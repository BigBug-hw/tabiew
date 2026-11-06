//! Lance v2 file structure parser.
//!
//! Validates the trailing `LANC` magic + footer, reads the column-metadata and
//! global-buffer offset tables, and decodes the Protobuf `ColumnMetadata`
//! messages and the global buffer containing the `FileDescriptor` (schema),
//! producing a [`LanceLayout`] that maps every interesting region back to a
//! byte span. We stop at the encoding-detail level — actual data buffer
//! decompression is out of scope (this is for learning the on-disk layout).
//!
//! Spec reference: `lance-format/protos/file2.proto`, `file.proto`, and
//! `encodings_v2_0.proto` (see `example/lance-proto-codegen/` for the
//! prost-generated Rust structs).
//!
//! File layout (all footer fields are little-endian unsigned integers):
//! ```text
//!   ├──────────────────────────────┤
//!   | Data Pages                   |   data buffers (referenced by columns)
//!   |   Data Buffer 0              |
//!   |   ...                        |
//!   |   Data Buffer BN             |
//!   ├──────────────────────────────┤
//!   | Global Buffers               |   (e.g. the FileDescriptor/Manifest)
//!   ├──────────────────────────────┤
//!   | Column Metadatas             |   one protobuf ColumnMetadata per column
//!   |   Column 0 Metadata          |
//!   |   Column 1 Metadata          |
//!   |   ...                        |
//!   |   Column CN Metadata          |
//!   ├──────────────────────────────┤
//!   | Column Metadata Offset Table |   CN × (u64 position, u64 size)
//!   ├──────────────────────────────┤
//!   | Global Buffers Offset Table  |   GN × (u64 position, u64 size)
//!   ├──────────────────────────────┤
//!   | Footer (40 bytes)            |
//!   |   u64  col_meta_0_offset (A) |
//!   |   u64  cmo_table_offset  (B) |
//!   |   u64  gbo_table_offset  (C) |
//!   |   u32  num_global_bufs       |
//!   |   u32  num_columns           |
//!   |   u16  major_version         |
//!   |   u16  minor_version         |
//!   |   "LANC"                     |
//!   ├──────────────────────────────┤
//! ```

use std::ops::Range;

use crate::binary::protobuf::{PbData, PbField, PbValue, decode_message_span_at};

/// Lance v2 magic bytes (`LANC`), trailing only.
pub const MAGIC: &[u8; 4] = b"LANC";

/// 256 MiB cap — files larger than this are rejected to avoid pathological
/// scans. Parquet uses the same limit.
const MAX_FILE_LEN: u64 = 256 * 1024 * 1024;

/// Footer is the trailing 40 bytes: 3× u64 offsets + 2× u32 counts +
/// 2× u16 version + 4-byte magic.
const FOOTER_LEN: u64 = 40;

/// A labelled byte region in the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub span: Range<u64>,
    pub kind: RegionKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Trailing `LANC` magic (last 4 bytes of the footer).
    Magic,
    /// 40-byte footer (offsets + counts + version + magic).
    Footer,
    /// Column Metadata Offset table (u64 LE (position, size) pairs).
    ColumnMetadataOffset,
    /// Global Buffers Offset table (u64 LE (position, size) pairs).
    GlobalBufferOffset,
    /// A single column's `ColumnMetadata` protobuf message.
    ColumnMetadata,
    /// A sub-field inside a `ColumnMetadata` protobuf (encoding, pages, …).
    ColumnMetadataField,
    /// A global buffer (e.g. the `FileDescriptor`/schema).
    GlobalBuffer,
    /// A sub-field inside the `FileDescriptor` protobuf (schema, fields, …).
    GlobalBufferField,
    /// Raw data page buffers (referenced by column pages).
    DataPages,
    /// A single data buffer within the Data Pages section.
    DataBuffer,
    Unknown,
}

/// A node in the human-readable structure tree shown in the right pane.
/// Reuses the Parquet `MetaNode` shape so the hex viewer can render trees
/// from either format without format-specific branching.
pub use crate::binary::parquet::MetaNode;

/// Result of parsing a Lance v2 file's structure.
#[derive(Debug, Default, Clone)]
pub struct LanceLayout {
    pub is_lance: bool,
    pub regions: Vec<Region>,
    pub tree: Vec<MetaNode>,
    pub error: Option<String>,
}

impl LanceLayout {
    fn err(msg: impl Into<String>) -> Self {
        Self {
            is_lance: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }

    fn lance_err(msg: impl Into<String>) -> Self {
        Self {
            is_lance: true,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

/// Parsed footer fields (the 40 trailing bytes before the magic).
struct Footer {
    /// A: offset to column metadata 0.
    col_meta_0_offset: u64,
    /// B: offset to the Column Metadata Offset table.
    cmo_table_offset: u64,
    /// C: offset to the Global Buffers Offset table.
    gbo_table_offset: u64,
    num_global_bufs: u32,
    num_columns: u32,
    major_version: u16,
    minor_version: u16,
}

/// A data buffer reference collected from column page metadata.
/// `(column_index, page_index, buffer_index, file_offset, size)`.
type DataBufferRef = (usize, usize, usize, u64, u64);

/// Parse the Lance v2 structure of `bytes` (which must be the entire file).
pub fn parse(bytes: &[u8]) -> LanceLayout {
    let len = bytes.len() as u64;

    // ---- File-level validation -------------------------------------------
    if len < FOOTER_LEN {
        return LanceLayout::err("file too small to be Lance v2 (< 40 bytes)");
    }
    // Trailing LANC magic — Lance v2 has magic at the END, not the start.
    let magic_start = len - 4;
    if &bytes[magic_start as usize..len as usize] != MAGIC {
        return LanceLayout {
            is_lance: false,
            error: Some(format!(
                "not a Lance v2 file: bad trailing magic {:?}",
                &bytes[magic_start as usize..len as usize]
            )),
            ..Default::default()
        };
    }
    if len > MAX_FILE_LEN {
        return LanceLayout::lance_err(format!(
            "file too large ({len} bytes > {MAX_FILE_LEN} cap)"
        ));
    }

    let mut layout = LanceLayout {
        is_lance: true,
        ..Default::default()
    };

    // ---- Footer (40 bytes ending at EOF) ---------------------------------
    let footer_start = len - FOOTER_LEN;
    let footer = Footer {
        col_meta_0_offset: read_u64_le(bytes, footer_start),
        cmo_table_offset: read_u64_le(bytes, footer_start + 8),
        gbo_table_offset: read_u64_le(bytes, footer_start + 16),
        num_global_bufs: read_u32_le(bytes, footer_start + 24),
        num_columns: read_u32_le(bytes, footer_start + 28),
        major_version: read_u16_le(bytes, footer_start + 32),
        minor_version: read_u16_le(bytes, footer_start + 34),
    };

    // Footer region + tree node (physical byte order: footer is last, so we
    // push it now and sort the tree at the end).
    layout.regions.push(Region {
        span: footer_start..len,
        kind: RegionKind::Footer,
        label: format!(
            "Footer @0x{footer_start:08x}..0x{len:08x} [{FOOTER_LEN} B] (v{}.{}: {} cols, {} global bufs)",
            footer.major_version, footer.minor_version,
            footer.num_columns, footer.num_global_bufs
        ),
    });
    layout.tree.push(MetaNode {
        depth: 0,
        label: format!(
            "Footer @0x{footer_start:08x}..0x{len:08x} [{FOOTER_LEN} B]"
        ),
        span: footer_start..len,
        collapsible: true,
    });
    // Footer sub-fields (depth 1).
    push_footer_field(&mut layout, "col_meta_0_offset (A)", footer_start, footer.col_meta_0_offset);
    push_footer_field(&mut layout, "cmo_table_offset (B)", footer_start + 8, footer.cmo_table_offset);
    push_footer_field(&mut layout, "gbo_table_offset (C)", footer_start + 16, footer.gbo_table_offset);
    push_footer_field(&mut layout, "num_global_bufs", footer_start + 24, footer.num_global_bufs as u64);
    push_footer_field(&mut layout, "num_columns", footer_start + 28, footer.num_columns as u64);
    push_footer_field(&mut layout, "major_version", footer_start + 32, footer.major_version as u64);
    push_footer_field(&mut layout, "minor_version", footer_start + 34, footer.minor_version as u64);
    // Trailing magic.
    layout.regions.push(Region {
        span: magic_start..len,
        kind: RegionKind::Magic,
        label: "LANC (magic)".into(),
    });
    layout.tree.push(MetaNode {
        depth: 1,
        label: format!("LANC (magic) @0x{magic_start:08x}..0x{len:08x}"),
        span: magic_start..len,
        collapsible: false,
    });

    // ---- Global Buffers Offset table -------------------------------------
    let gbo_start = footer.gbo_table_offset;
    let gbo_end = footer_start;
    let gbo_count = footer.num_global_bufs as usize;
    let mut global_bufs: Vec<(u64, u64)> = Vec::new();
    if gbo_start < footer_start && gbo_end > gbo_start && (gbo_end - gbo_start) >= (gbo_count as u64) * 16 {
        let gbo_len = gbo_end - gbo_start;
        layout.regions.push(Region {
            span: gbo_start..gbo_end,
            kind: RegionKind::GlobalBufferOffset,
            label: format!(
                "Global Buffers Offset Table @0x{gbo_start:08x}..0x{gbo_end:08x} [{gbo_len} B, {gbo_count} buf(s)]"
            ),
        });
        layout.tree.push(MetaNode {
            depth: 0,
            label: format!(
                "Global Buffers Offset Table @0x{gbo_start:08x}..0x{gbo_end:08x} [{gbo_count} buf(s)]"
            ),
            span: gbo_start..gbo_end,
            collapsible: true,
        });
        // Read GBO entries and decode each global buffer.
        for i in 0..gbo_count {
            let pos = read_u64_le(bytes, gbo_start + (i as u64) * 16);
            let size = read_u64_le(bytes, gbo_start + (i as u64) * 16 + 8);
            global_bufs.push((pos, size));
            layout.tree.push(MetaNode {
                depth: 1,
                label: format!("global_buf[{i}] pos=0x{pos:08x} size={size}"),
                span: gbo_start + (i as u64) * 16..gbo_start + (i as u64) * 16 + 16,
                collapsible: false,
            });
        }
        // Decode each global buffer. Global buffer 0 typically holds the
        // `FileDescriptor` (schema). We attempt protobuf decode and label it.
        for (i, (pos, size)) in global_bufs.iter().enumerate() {
            if *pos < len && *size > 0 && pos + size <= len {
                global_buffer_node(&mut layout, bytes, i, *pos, *pos + size);
            }
        }
    }

    // ---- Column Metadata Offset table + Column Metadatas -----------------
    let cmo_start = footer.cmo_table_offset;
    let cmo_end = gbo_start.max(cmo_start);
    let col_count = footer.num_columns as usize;
    let mut col_metas: Vec<(u64, u64)> = Vec::new();
    let mut data_bufs: Vec<DataBufferRef> = Vec::new();

    if cmo_start < cmo_end && (cmo_end - cmo_start) >= (col_count as u64) * 16 {
        let cmo_len = cmo_end - cmo_start;
        layout.regions.push(Region {
            span: cmo_start..cmo_end,
            kind: RegionKind::ColumnMetadataOffset,
            label: format!(
                "Column Metadata Offset Table @0x{cmo_start:08x}..0x{cmo_end:08x} [{cmo_len} B, {col_count} col(s)]"
            ),
        });
        layout.tree.push(MetaNode {
            depth: 0,
            label: format!(
                "Column Metadata Offset Table @0x{cmo_start:08x}..0x{cmo_end:08x} [{col_count} col(s)]"
            ),
            span: cmo_start..cmo_end,
            collapsible: true,
        });
        // Read CMO entries.
        for i in 0..col_count {
            let pos = read_u64_le(bytes, cmo_start + (i as u64) * 16);
            let size = read_u64_le(bytes, cmo_start + (i as u64) * 16 + 8);
            col_metas.push((pos, size));
            layout.tree.push(MetaNode {
                depth: 1,
                label: format!("col[{i}] meta pos=0x{pos:08x} size={size}"),
                span: cmo_start + (i as u64) * 16..cmo_start + (i as u64) * 16 + 16,
                collapsible: false,
            });
        }

        // "Column Metadatas" parent node spanning all column metadata messages.
        let col_meta_start = col_metas.first().map(|(p, _)| *p).unwrap_or(cmo_start);
        let col_meta_end = col_metas.last().map(|(p, s)| p + s).unwrap_or(cmo_start);
        if col_meta_start < col_meta_end && col_meta_end <= cmo_start {
            layout.regions.push(Region {
                span: col_meta_start..col_meta_end,
                kind: RegionKind::ColumnMetadata,
                label: format!(
                    "Column Metadatas @0x{col_meta_start:08x}..0x{col_meta_end:08x} [{col_count} col(s)]"
                ),
            });
            layout.tree.push(MetaNode {
                depth: 0,
                label: format!("Column Metadatas @0x{col_meta_start:08x}..0x{col_meta_end:08x} [{col_count} col(s)]"),
                span: col_meta_start..col_meta_end,
                collapsible: true,
            });
            // Decode each column metadata as a depth-1 child.
            for (i, (pos, size)) in col_metas.iter().enumerate() {
                if *pos < len && *size > 0 && pos + size <= len {
                    column_metadata_node(&mut layout, bytes, i, *pos, *pos + size, &mut data_bufs);
                }
            }
        }
    }

    // ---- Data pages (everything before the first metadata/global buffer) ----
    // The data pages occupy the beginning of the file, before the column
    // metadatas and global buffers. We compute the boundary as the minimum
    // start offset of any column metadata or global buffer region.
    let data_end = layout
        .regions
        .iter()
        .filter(|r| matches!(r.kind, RegionKind::ColumnMetadata | RegionKind::GlobalBuffer))
        .map(|r| r.span.start)
        .min()
        .unwrap_or(footer.col_meta_0_offset);
    if data_end > 0 {
        layout.regions.push(Region {
            span: 0..data_end,
            kind: RegionKind::DataPages,
            label: format!("Data Pages @0x00000000..0x{data_end:08x} [{data_end} B]"),
        });
        layout.tree.push(MetaNode {
            depth: 0,
            label: format!("Data Pages @0x00000000..0x{data_end:08x} [{data_end} B]"),
            span: 0..data_end,
            collapsible: true,
        });
        // Render individual data buffers discovered from column page metadata,
        // sorted by file offset so they appear in physical byte order.
        data_bufs.sort_by_key(|(_, _, _, off, _)| *off);
        for (col_idx, page_idx, buf_idx, off, size) in &data_bufs {
            if *off + size > data_end {
                continue; // skip out-of-range buffers
            }
            layout.regions.push(Region {
                span: *off..*off + size,
                kind: RegionKind::DataBuffer,
                label: format!(
                    "Data Buffer (col[{col_idx}] page[{page_idx}] buf[{buf_idx}]) @0x{off:08x}..0x{:08x} [{size} B]",
                    off + size
                ),
            });
            layout.tree.push(MetaNode {
                depth: 1,
                label: format!(
                    "Data Buffer (col[{col_idx}] page[{page_idx}] buf[{buf_idx}]) @0x{off:08x}..0x{:08x} [{size} B]",
                    off + size
                ),
                span: *off..*off + size,
                collapsible: false,
            });
        }
    }

    // Keep regions sorted by start offset for [ / ] navigation.
    layout.regions.sort_by_key(|r| (r.span.start, r.span.end));

    // The tree was built in decode order (footer → GBO → CMO → data). Re-sort
    // it by physical byte order so the right-pane tree matches the left-pane
    // hex dump top-to-bottom. Sort by (span.start, depth) so a parent (smaller
    // depth) precedes its children when they share a start offset.
    layout
        .tree
        .sort_by_key(|n| (n.span.start, n.depth, n.span.end));

    layout
}

/// Push a footer sub-field as a depth-1 tree leaf (no separate region — the
/// whole footer is one region).
fn push_footer_field(layout: &mut LanceLayout, name: &str, off: u64, val: u64) {
    let span = match name {
        n if n.contains("major_version") || n.contains("minor_version") => off..off + 2,
        n if n.contains("num_") => off..off + 4,
        _ => off..off + 8,
    };
    layout.tree.push(MetaNode {
        depth: 1,
        label: format!("{name} = {val}"),
        span,
        collapsible: false,
    });
}

/// Decode a global buffer and append regions + tree nodes. Global buffer 0
/// typically holds the `FileDescriptor` (schema). We attempt protobuf decode
/// and label fields per `file.proto`.
fn global_buffer_node(layout: &mut LanceLayout, bytes: &[u8], idx: usize, start: u64, end: u64) {
    let len = (end - start) as usize;
    layout.regions.push(Region {
        span: start..end,
        kind: RegionKind::GlobalBuffer,
        label: format!("Global Buffer[{idx}] @0x{start:08x}..0x{end:08x} [{len} B]"),
    });
    // Try to decode as FileDescriptor { Schema schema = 1; uint64 length = 2; }
    let section = &bytes[start as usize..(start as usize + len)];
    match decode_message_span_at(section, start, 0) {
        Some((fields, _span)) => {
            // Detect if this looks like a FileDescriptor (field 1 = message =
            // Schema containing repeated Field messages with `name`).
            let is_file_descriptor = fields.iter().any(|f| {
                f.number == 1 && matches!(f.value.data, PbData::Message(_))
            });
            if is_file_descriptor {
                layout.tree.push(MetaNode {
                    depth: 0,
                    label: format!("FileDescriptor (global_buf[{idx}]) @0x{start:08x}..0x{end:08x} [{len} B]"),
                    span: start..end,
                    collapsible: true,
                });
                file_descriptor_node(layout, &fields);
            } else {
                layout.tree.push(MetaNode {
                    depth: 0,
                    label: format!("Global Buffer[{idx}] @0x{start:08x}..0x{end:08x} [{len} B]"),
                    span: start..end,
                    collapsible: true,
                });
                pb_fields_node(layout, &fields, 1, RegionKind::GlobalBufferField);
            }
        }
        None => {
            layout.tree.push(MetaNode {
                depth: 0,
                label: format!("Global Buffer[{idx}] @0x{start:08x}..0x{end:08x} [{len} B] (non-protobuf)"),
                span: start..end,
                collapsible: false,
            });
        }
    }
}

/// Render a `FileDescriptor` protobuf: field 1 = Schema, field 2 = length.
/// Per `file.proto`:
///   FileDescriptor { Schema schema = 1; uint64 length = 2; }
///   Schema { repeated Field fields = 1; map<string,bytes> metadata = 5; }
fn file_descriptor_node(layout: &mut LanceLayout, fields: &[PbField]) {
    for f in fields {
        match (f.number, &f.value.data) {
            (1, PbData::Message(schema_fields)) => {
                layout.regions.push(Region {
                    span: f.value.span.clone(),
                    kind: RegionKind::GlobalBufferField,
                    label: format!("Schema (fields[{}])", schema_fields.len()),
                });
                layout.tree.push(MetaNode {
                    depth: 1,
                    label: format!("Schema (fields[{}])", schema_fields.len()),
                    span: f.value.span.clone(),
                    collapsible: true,
                });
                // Each child of Schema is a `Field` message.
                for sf in schema_fields {
                    if let PbData::Message(field_inner) = &sf.value.data {
                        field_node(layout, sf, field_inner);
                    }
                }
            }
            (2, _) => {
                layout.regions.push(Region {
                    span: f.value.span.clone(),
                    kind: RegionKind::GlobalBufferField,
                    label: format!("length = {}", f.value.as_varint().unwrap_or(0)),
                });
                layout.tree.push(MetaNode {
                    depth: 1,
                    label: format!("length = {}", f.value.as_varint().unwrap_or(0)),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            _ => {
                pb_field_node(layout, f, 1, RegionKind::GlobalBufferField);
            }
        }
    }
}

/// Render a single `Field` message from the schema.
/// Per `file.proto`:
///   Field { Type type = 1; string name = 2; int32 id = 3; int32 parent_id = 4;
///          string logical_type = 5; bool nullable = 6; ... }
fn field_node(layout: &mut LanceLayout, sf: &PbField, field_inner: &[PbField]) {
    // Extract the name (field 2) for the label.
    let name = field_inner
        .iter()
        .find(|f| f.number == 2)
        .and_then(|f| f.value.as_string())
        .unwrap_or("?");
    let id = field_inner
        .iter()
        .find(|f| f.number == 3)
        .and_then(|f| f.value.as_varint())
        .unwrap_or(0);
    let logical_type = field_inner
        .iter()
        .find(|f| f.number == 5)
        .and_then(|f| f.value.as_string())
        .unwrap_or("?".into());
    layout.regions.push(Region {
        span: sf.value.span.clone(),
        kind: RegionKind::GlobalBufferField,
        label: format!("Field[{id}] \"{name}\" ({logical_type})"),
    });
    layout.tree.push(MetaNode {
        depth: 2,
        label: format!("Field[{id}] \"{name}\" ({logical_type})"),
        span: sf.value.span.clone(),
        collapsible: true,
    });
    // Render the Field's sub-fields (name, id, parent_id, logical_type, nullable, …).
    for ff in field_inner {
        let label = match ff.number {
            1 => {
                // type enum: 0=PARENT, 1=REPEATED, 2=LEAF
                let t = ff.value.as_varint().unwrap_or(0);
                let tname = match t {
                    0 => "PARENT",
                    1 => "REPEATED",
                    2 => "LEAF",
                    _ => "?",
                };
                format!("  type = {tname}")
            }
            2 => format!("  name = {:?}", ff.value.as_string().unwrap_or("?")),
            3 => format!("  id = {}", ff.value.as_varint().unwrap_or(0) as i32),
            4 => {
                // parent_id is int32 (plain varint, truncated to 32 bits).
                let raw = ff.value.as_varint().unwrap_or(0);
                format!("  parent_id = {}", raw as u32 as i32)
            }
            5 => format!("  logical_type = {:?}", ff.value.as_string().unwrap_or("?")),
            6 => format!("  nullable = {}", ff.value.as_varint().unwrap_or(0) != 0),
            7 => {
                // Deprecated: global encoding enum (NONE=0, PLAIN=1, VAR_BINARY=2,
                // DICTIONARY=3, RLE=4).
                let v = ff.value.as_varint().unwrap_or(0);
                let name = match v {
                    0 => "NONE",
                    1 => "PLAIN",
                    2 => "VAR_BINARY",
                    3 => "DICTIONARY",
                    4 => "RLE",
                    _ => "?",
                };
                format!("  encoding (deprecated) = {name}")
            }
            _ => pb_field_label(ff).replacen("field", "  field", 1),
        };
        layout.tree.push(MetaNode {
            depth: 3,
            label,
            span: ff.value.span.clone(),
            collapsible: false,
        });
    }
}

/// Decode a `ColumnMetadata` protobuf and append regions + tree nodes.
/// Per `file2.proto`:
///   ColumnMetadata {
///     Encoding encoding = 1;
///     repeated Page pages = 2;
///     repeated uint64 buffer_offsets = 3;
///     repeated uint64 buffer_sizes = 4;
///   }
///   Page {
///     repeated uint64 buffer_offsets = 1;
///     repeated uint64 buffer_sizes = 2;
///     uint64 length = 3;
///     Encoding encoding = 4;
///     uint64 priority = 5;
///   }
///
/// `data_bufs` collects (col, page, buf_idx, offset, size) for each page buffer
/// so the caller can render individual data buffers under the Data Pages node.
fn column_metadata_node(
    layout: &mut LanceLayout,
    bytes: &[u8],
    idx: usize,
    start: u64,
    end: u64,
    data_bufs: &mut Vec<DataBufferRef>,
) {
    let len = (end - start) as usize;
    layout.regions.push(Region {
        span: start..end,
        kind: RegionKind::ColumnMetadata,
        label: format!("ColumnMetadata[{idx}] @0x{start:08x}..0x{end:08x} [{len} B]"),
    });
    layout.tree.push(MetaNode {
        depth: 1,
        label: format!("ColumnMetadata[{idx}] @0x{start:08x}..0x{end:08x} [{len} B]"),
        span: start..end,
        collapsible: true,
    });
    let section = &bytes[start as usize..(start as usize + len)];
    match decode_message_span_at(section, start, 0) {
        Some((fields, _span)) => {
            column_metadata_fields_node(layout, bytes, &fields, idx, data_bufs);
        }
        None => {
            layout.tree.push(MetaNode {
                depth: 2,
                label: "(invalid Protobuf encoding)".into(),
                span: start..end,
                collapsible: false,
            });
            if layout.error.is_none() {
                layout
                    .error
                    .get_or_insert(format!("invalid ColumnMetadata[{idx}] Protobuf encoding"));
            }
        }
    }
}

/// Render the fields of a `ColumnMetadata` message with spec-correct labels.
fn column_metadata_fields_node(
    layout: &mut LanceLayout,
    file_bytes: &[u8],
    fields: &[PbField],
    col_idx: usize,
    data_bufs: &mut Vec<DataBufferRef>,
) {
    // Track page index for the repeated Page field.
    let mut page_idx: usize = 0;
    for f in fields {
        match (f.number, &f.value.data) {
            (1, PbData::Message(_)) => {
                // Encoding (column-level)
                layout.regions.push(Region {
                    span: f.value.span.clone(),
                    kind: RegionKind::ColumnMetadataField,
                    label: "encoding (ColumnEncoding)".into(),
                });
                layout.tree.push(MetaNode {
                    depth: 2,
                    label: "encoding (ColumnEncoding)".into(),
                    span: f.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(enc_fields) = &f.value.data {
                    encoding_node(layout, enc_fields, 3, EncodingKind::Column);
                }
            }
            (2, PbData::Message(_)) => {
                // A Page message (repeated)
                if let PbData::Message(page_fields) = &f.value.data {
                    let length = page_fields
                        .iter()
                        .find(|pf| pf.number == 3)
                        .and_then(|pf| pf.value.as_varint())
                        .unwrap_or(0);
                    layout.regions.push(Region {
                        span: f.value.span.clone(),
                        kind: RegionKind::ColumnMetadataField,
                        label: format!("Page[{page_idx}] (length={length})"),
                    });
                    layout.tree.push(MetaNode {
                        depth: 2,
                        label: format!("Page[{page_idx}] (length={length})"),
                        span: f.value.span.clone(),
                        collapsible: true,
                    });
                    // Collect data buffer refs and render page sub-fields.
                    page_node(layout, file_bytes, page_fields, col_idx, page_idx, data_bufs);
                    page_idx += 1;
                }
            }
            (3, _) => {
                // repeated uint64 buffer_offsets (packed)
                let vals = packed_u64_preview(f, file_bytes);
                layout.regions.push(Region {
                    span: f.value.span.clone(),
                    kind: RegionKind::ColumnMetadataField,
                    label: format!("buffer_offsets = {vals}"),
                });
                layout.tree.push(MetaNode {
                    depth: 2,
                    label: format!("buffer_offsets = {vals}"),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            (4, _) => {
                let vals = packed_u64_preview(f, file_bytes);
                layout.regions.push(Region {
                    span: f.value.span.clone(),
                    kind: RegionKind::ColumnMetadataField,
                    label: format!("buffer_sizes = {vals}"),
                });
                layout.tree.push(MetaNode {
                    depth: 2,
                    label: format!("buffer_sizes = {vals}"),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            _ => {
                pb_field_node(layout, f, 2, RegionKind::ColumnMetadataField);
            }
        }
    }
}

/// Which kind of `Encoding` we are rendering — column-level (`ColumnEncoding`)
/// or page-level (`ArrayEncoding`). The wire format is identical (an `Encoding`
/// oneof with `indirect`/`direct`/`none`), but the inner `DirectEncoding`
/// payload carries a different protobuf message.
#[derive(Clone, Copy)]
enum EncodingKind {
    Column,
    Page,
}

/// Render an `Encoding` message.
/// Per `file2.proto`:
///   Encoding { oneof location { DeferredEncoding indirect = 1;
///                               DirectEncoding direct = 2;
///                               google.protobuf.Empty none = 3; } }
/// `DirectEncoding.encoding` is a `bytes` field that contains a protobuf `Any`
/// (field 1 = type_url string, field 2 = value message). The value message is
/// a `ColumnEncoding` (column-level) or `ArrayEncoding` (page-level), whose
/// own oneof field identifies the concrete encoding variant.
fn encoding_node(layout: &mut LanceLayout, fields: &[PbField], depth: u8, kind: EncodingKind) {
    for f in fields {
        match (f.number, &f.value.data) {
            (1, PbData::Message(_)) => {
                // DeferredEncoding { uint64 buffer_location = 1; uint64 buffer_length = 2; }
                layout.tree.push(MetaNode {
                    depth,
                    label: "indirect (DeferredEncoding)".into(),
                    span: f.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(inner) = &f.value.data {
                    for ff in inner {
                        let nm = match ff.number {
                            1 => "buffer_location",
                            2 => "buffer_length",
                            _ => "field",
                        };
                        layout.tree.push(MetaNode {
                            depth: depth + 1,
                            label: format!("{nm} = {}", ff.value.as_varint().unwrap_or(0)),
                            span: ff.value.span.clone(),
                            collapsible: false,
                        });
                    }
                }
            }
            (2, PbData::Message(_)) => {
                // DirectEncoding { bytes encoding = 1; }
                // The `encoding` bytes decode as a protobuf `Any`-like message:
                //   Any { string type_url = 1; bytes value = 2; }
                // The `value` is a ColumnEncoding or ArrayEncoding message.
                layout.tree.push(MetaNode {
                    depth,
                    label: "direct (DirectEncoding)".into(),
                    span: f.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(inner) = &f.value.data {
                    direct_encoding_node(layout, inner, depth + 1, kind);
                }
            }
            (3, _) => {
                layout.tree.push(MetaNode {
                    depth,
                    label: "none (no encoding)".into(),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            _ => {
                layout.tree.push(MetaNode {
                    depth,
                    label: pb_field_label(f),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
        }
    }
}

/// Render a `DirectEncoding`'s `encoding` bytes, which contain a protobuf
/// `Any`-like message: `{ string type_url = 1; bytes value = 2; }`.
/// The `value` is a `ColumnEncoding` or `ArrayEncoding` (depending on `kind`),
/// whose own oneof identifies the concrete variant.
fn direct_encoding_node(
    layout: &mut LanceLayout,
    direct_fields: &[PbField],
    depth: u8,
    kind: EncodingKind,
) {
    for f in direct_fields {
        if f.number != 1 {
            pb_field_node(layout, f, depth, RegionKind::ColumnMetadataField);
            continue;
        }
        // Field 1 = the `encoding` bytes, which we decode as Any{type_url, value}.
        let (type_url, value_fields) = decode_any(&f.value);
        let type_label = type_url
            .as_deref()
            .map(strip_type_prefix)
            .unwrap_or_else(|| format!("bytes[{}]", any_raw_len(&f.value)));
        layout.tree.push(MetaNode {
            depth,
            label: type_label.clone(),
            span: f.value.span.clone(),
            collapsible: true,
        });
        // Render the Any's type_url.
        if let Some(url) = &type_url {
            layout.tree.push(MetaNode {
                depth: depth + 1,
                label: format!("type_url = {:?}", url),
                span: f.value.span.clone(),
                collapsible: false,
            });
        }
        // Render the value message's oneof variant.
        if let Some(vfields) = &value_fields {
            match kind {
                EncodingKind::Column => column_encoding_node(layout, vfields, depth + 1),
                EncodingKind::Page => array_encoding_node(layout, vfields, depth + 1),
            }
        }
    }
}

/// Decode a protobuf `Any`-like value: field 1 = type_url (string), field 2 =
/// value (message). Returns (type_url, value_fields).
fn decode_any(value: &PbValue) -> (Option<String>, Option<Vec<PbField>>) {
    let inner = match &value.data {
        PbData::Message(fields) => fields,
        PbData::Bytes(b) => {
            // Raw bytes — try to decode as a message.
            return match decode_message_span_at(b, value.span.start, 0) {
                Some((fields, _)) => extract_any_fields(&fields),
                None => (None, None),
            };
        }
        _ => return (None, None),
    };
    extract_any_fields(inner)
}

fn extract_any_fields(fields: &[PbField]) -> (Option<String>, Option<Vec<PbField>>) {
    let type_url = fields
        .iter()
        .find(|f| f.number == 1)
        .and_then(|f| f.value.as_string())
        .map(|s| s.to_string());
    let value_fields = fields
        .iter()
        .find(|f| f.number == 2)
        .and_then(|f| match &f.value.data {
            PbData::Message(vf) => Some(vf.clone()),
            PbData::Bytes(b) => decode_message_span_at(b, f.value.span.start, 0).map(|(vf, _)| vf),
            _ => None,
        });
    (type_url, value_fields)
}

/// Render a `ColumnEncoding` message oneof.
/// Per `encodings_v2_0.proto`:
///   ColumnEncoding { oneof { Values (empty) = 1; ZoneIndex = 2; Blob = 3; } }
fn column_encoding_node(layout: &mut LanceLayout, fields: &[PbField], depth: u8) {
    for f in fields {
        let label = match f.number {
            1 => "Values".to_string(),
            2 => "ZoneIndex".to_string(),
            3 => "Blob".to_string(),
            _ => pb_field_label(f),
        };
        let collapsible = matches!(f.value.data, PbData::Message(_));
        layout.tree.push(MetaNode {
            depth,
            label,
            span: f.value.span.clone(),
            collapsible,
        });
        // For ZoneIndex/Blob, render the inner fields.
        if let PbData::Message(inner) = &f.value.data {
            for ff in inner {
                let nm = match ff.number {
                    1 => "rows_per_zone",
                    2 => "zone_map_buffer",
                    3 => "inner",
                    _ => "field",
                };
                if ff.number == 2 {
                    // Buffer message
                    layout.tree.push(MetaNode {
                        depth: depth + 1,
                        label: format!("{nm} (Buffer)"),
                        span: ff.value.span.clone(),
                        collapsible: matches!(ff.value.data, PbData::Message(_)),
                    });
                    if let PbData::Message(buf_fields) = &ff.value.data {
                        for bf in buf_fields {
                            let bnm = match bf.number {
                                1 => "buffer_index",
                                2 => "buffer_type",
                                _ => "field",
                            };
                            let bval = match &bf.value.data {
                                PbData::Varint(v) => {
                                    if bf.number == 2 {
                                        // BufferType enum: 0=Page, 1=Column, 2=File
                                        let tn = match *v {
                                            0 => "Page",
                                            1 => "Column",
                                            2 => "File",
                                            _ => "?",
                                        };
                                        format!(" = {tn}")
                                    } else {
                                        format!(" = {v}")
                                    }
                                }
                                _ => format!(" = {}", pb_field_label_raw(&bf.value)),
                            };
                            layout.tree.push(MetaNode {
                                depth: depth + 2,
                                label: format!("{bnm}{bval}"),
                                span: bf.value.span.clone(),
                                collapsible: false,
                            });
                        }
                    }
                } else {
                    layout.tree.push(MetaNode {
                        depth: depth + 1,
                        label: format!("{nm} = {}", pb_field_label_raw(&ff.value)),
                        span: ff.value.span.clone(),
                        collapsible: false,
                    });
                }
            }
        }
    }
}

/// Render an `ArrayEncoding` message oneof.
/// Per `encodings_v2_0.proto`, ArrayEncoding has 21 variants (tags 1..21):
///   Flat(1), Nullable(2), FixedSizeList(3), List(4), Struct(5), Binary(6),
///   Dictionary(7), Fsst(8), PackedStruct(9), Bitpacked(10), FixedSizeBinary(11),
///   BitpackedForNonNeg(12), Constant(13), InlineBitpacking(14),
///   OutOfLineBitpacking(15), Variable(16), PackedStructFixedWidthMiniBlock(17),
///   Block(18), Rle(19), GeneralMiniBlock(20), ByteStreamSplit(21).
fn array_encoding_node(layout: &mut LanceLayout, fields: &[PbField], depth: u8) {
    for f in fields {
        let label = array_encoding_variant_name(f.number);
        let collapsible = matches!(f.value.data, PbData::Message(_));
        layout.tree.push(MetaNode {
            depth,
            label: label.clone(),
            span: f.value.span.clone(),
            collapsible,
        });
        // Render a few well-known inner structures.
        if let PbData::Message(inner) = &f.value.data {
            array_encoding_inner(layout, f.number, inner, depth + 1);
        }
    }
}

/// Map an ArrayEncoding oneof tag number to a display name.
fn array_encoding_variant_name(tag: u32) -> String {
    let name = match tag {
        1 => "Flat",
        2 => "Nullable",
        3 => "FixedSizeList",
        4 => "List",
        5 => "Struct (SimpleStruct)",
        6 => "Binary",
        7 => "Dictionary",
        8 => "Fsst",
        9 => "PackedStruct",
        10 => "Bitpacked",
        11 => "FixedSizeBinary",
        12 => "BitpackedForNonNeg",
        13 => "Constant",
        14 => "InlineBitpacking",
        15 => "OutOfLineBitpacking",
        16 => "Variable",
        17 => "PackedStructFixedWidthMiniBlock",
        18 => "Block",
        19 => "Rle",
        20 => "GeneralMiniBlock",
        21 => "ByteStreamSplit",
        _ => return format!("field {}", tag),
    };
    name.to_string()
}

/// Render the inner fields of a known ArrayEncoding variant.
fn array_encoding_inner(
    layout: &mut LanceLayout,
    tag: u32,
    inner: &[PbField],
    depth: u8,
) {
    match tag {
        1 => {
            // Flat { uint64 bits_per_value = 1; Buffer buffer = 2; Compression compression = 3; }
            flat_inner(layout, inner, depth);
        }
        2 => {
            // Nullable { oneof Nullability { NoNull(1), SomeNull(2), AllNull(3) } }
            nullable_inner(layout, inner, depth);
        }
        6 => {
            // Binary { ArrayEncoding indices = 1; ArrayEncoding bytes = 2; uint64 null_adjustment = 3; }
            for ff in inner {
                match ff.number {
                    1 | 2 => {
                        let nm = if ff.number == 1 { "indices" } else { "bytes" };
                        layout.tree.push(MetaNode {
                            depth,
                            label: format!("{nm} (ArrayEncoding)"),
                            span: ff.value.span.clone(),
                            collapsible: true,
                        });
                        if let PbData::Message(sub) = &ff.value.data {
                            array_encoding_node(layout, sub, depth + 1);
                        }
                    }
                    3 => {
                        layout.tree.push(MetaNode {
                            depth,
                            label: format!("null_adjustment = {}", ff.value.as_varint().unwrap_or(0)),
                            span: ff.value.span.clone(),
                            collapsible: false,
                        });
                    }
                    _ => pb_field_node_no_region(layout, ff, depth),
                }
            }
        }
        10 => {
            // Bitpacked { uint64 compressed_bits_per_value = 1; uint64 uncompressed_bits_per_value = 2; Buffer buffer = 3; bool signed = 4; }
            bitpacked_inner(layout, inner, depth);
        }
        12 => {
            // BitpackedForNonNeg { uint64 compressed_bits_per_value = 1; uint64 uncompressed_bits_per_value = 2; Buffer buffer = 3; }
            bitpacked_inner(layout, inner, depth);
        }
        19 => {
            // Rle { uint64 bits_per_value = 1; }
            for ff in inner {
                let nm = match ff.number {
                    1 => "bits_per_value",
                    _ => "field",
                };
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("{nm} = {}", ff.value.as_varint().unwrap_or(0)),
                    span: ff.value.span.clone(),
                    collapsible: false,
                });
            }
        }
        16 => {
            // Variable { uint32 bits_per_offset = 1; }
            for ff in inner {
                let nm = match ff.number {
                    1 => "bits_per_offset",
                    _ => "field",
                };
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("{nm} = {}", ff.value.as_varint().unwrap_or(0)),
                    span: ff.value.span.clone(),
                    collapsible: false,
                });
            }
        }
        _ => {
            // Generic fallback: render all inner fields.
            for ff in inner {
                pb_field_node_no_region(layout, ff, depth);
            }
        }
    }
}

/// Render a `Flat` ArrayEncoding's inner fields.
fn flat_inner(layout: &mut LanceLayout, inner: &[PbField], depth: u8) {
    for ff in inner {
        match ff.number {
            1 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("bits_per_value = {}", ff.value.as_varint().unwrap_or(0)),
                    span: ff.value.span.clone(),
                    collapsible: false,
                });
            }
            2 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: "buffer (Buffer)".into(),
                    span: ff.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(buf_fields) = &ff.value.data {
                    buffer_inner(layout, buf_fields, depth + 1);
                }
            }
            3 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: "compression (Compression)".into(),
                    span: ff.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(c_fields) = &ff.value.data {
                    for cf in c_fields {
                        let nm = match cf.number {
                            1 => "scheme",
                            2 => "level",
                            _ => "field",
                        };
                        let val = match &cf.value.data {
                            PbData::String(s) => format!(" = {:?}", s),
                            _ => format!(" = {}", pb_field_label_raw(&cf.value)),
                        };
                        layout.tree.push(MetaNode {
                            depth: depth + 1,
                            label: format!("{nm}{val}"),
                            span: cf.value.span.clone(),
                            collapsible: false,
                        });
                    }
                }
            }
            _ => pb_field_node_no_region(layout, ff, depth),
        }
    }
}

/// Render a `Nullable` ArrayEncoding's oneof Nullability.
fn nullable_inner(layout: &mut LanceLayout, inner: &[PbField], depth: u8) {
    for ff in inner {
        let label = match ff.number {
            1 => "NoNulls (NoNull)",
            2 => "SomeNulls (SomeNull)",
            3 => "AllNulls (AllNull)",
            _ => "field",
        };
        layout.tree.push(MetaNode {
            depth,
            label: label.into(),
            span: ff.value.span.clone(),
            collapsible: matches!(ff.value.data, PbData::Message(_)),
        });
        if let PbData::Message(sub) = &ff.value.data {
            // NoNull { ArrayEncoding values = 1; }
            // SomeNull { ArrayEncoding validity = 1; ArrayEncoding values = 2; }
            for sf in sub {
                let nm = match sf.number {
                    1 => if ff.number == 2 { "validity" } else { "values" },
                    2 => "values",
                    _ => "field",
                };
                layout.tree.push(MetaNode {
                    depth: depth + 1,
                    label: format!("{nm} (ArrayEncoding)"),
                    span: sf.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(sub2) = &sf.value.data {
                    array_encoding_node(layout, sub2, depth + 2);
                }
            }
        }
    }
}

/// Render a `Bitpacked`/`BitpackedForNonNeg` inner.
fn bitpacked_inner(layout: &mut LanceLayout, inner: &[PbField], depth: u8) {
    for ff in inner {
        match ff.number {
            1 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("compressed_bits_per_value = {}", ff.value.as_varint().unwrap_or(0)),
                    span: ff.value.span.clone(),
                    collapsible: false,
                });
            }
            2 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("uncompressed_bits_per_value = {}", ff.value.as_varint().unwrap_or(0)),
                    span: ff.value.span.clone(),
                    collapsible: false,
                });
            }
            3 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: "buffer (Buffer)".into(),
                    span: ff.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(buf_fields) = &ff.value.data {
                    buffer_inner(layout, buf_fields, depth + 1);
                }
            }
            4 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("signed = {}", ff.value.as_varint().unwrap_or(0) != 0),
                    span: ff.value.span.clone(),
                    collapsible: false,
                });
            }
            _ => pb_field_node_no_region(layout, ff, depth),
        }
    }
}

/// Render a `Buffer` message: { uint32 buffer_index = 1; BufferType buffer_type = 2; }
fn buffer_inner(layout: &mut LanceLayout, buf_fields: &[PbField], depth: u8) {
    for bf in buf_fields {
        match bf.number {
            1 => {
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("buffer_index = {}", bf.value.as_varint().unwrap_or(0)),
                    span: bf.value.span.clone(),
                    collapsible: false,
                });
            }
            2 => {
                let v = bf.value.as_varint().unwrap_or(0);
                let tn = match v {
                    0 => "Page",
                    1 => "Column",
                    2 => "File",
                    _ => "?",
                };
                layout.tree.push(MetaNode {
                    depth,
                    label: format!("buffer_type = {tn}"),
                    span: bf.value.span.clone(),
                    collapsible: false,
                });
            }
            _ => pb_field_node_no_region(layout, bf, depth),
        }
    }
}

/// Render a `Page` message's fields.
fn page_node(
    layout: &mut LanceLayout,
    file_bytes: &[u8],
    fields: &[PbField],
    col_idx: usize,
    page_idx: usize,
    data_bufs: &mut Vec<DataBufferRef>,
) {
    // Collect buffer_offsets and buffer_sizes so we can pair them.
    let buffer_offsets: Vec<u64> = fields
        .iter()
        .find(|f| f.number == 1)
        .map(|f| packed_u64_list(f, file_bytes))
        .unwrap_or_default();
    let buffer_sizes: Vec<u64> = fields
        .iter()
        .find(|f| f.number == 2)
        .map(|f| packed_u64_list(f, file_bytes))
        .unwrap_or_default();

    for f in fields {
        match f.number {
            1 => {
                layout.tree.push(MetaNode {
                    depth: 3,
                    label: format!("buffer_offsets = {}", packed_u64_preview(f, file_bytes)),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            2 => {
                layout.tree.push(MetaNode {
                    depth: 3,
                    label: format!("buffer_sizes = {}", packed_u64_preview(f, file_bytes)),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            3 => {
                layout.tree.push(MetaNode {
                    depth: 3,
                    label: format!("length = {}", f.value.as_varint().unwrap_or(0)),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            4 => {
                // Encoding (page-level)
                layout.tree.push(MetaNode {
                    depth: 3,
                    label: "encoding (ArrayEncoding)".into(),
                    span: f.value.span.clone(),
                    collapsible: true,
                });
                if let PbData::Message(enc_fields) = &f.value.data {
                    encoding_node(layout, enc_fields, 4, EncodingKind::Page);
                }
            }
            5 => {
                layout.tree.push(MetaNode {
                    depth: 3,
                    label: format!("priority = {}", f.value.as_varint().unwrap_or(0)),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
            _ => {
                layout.tree.push(MetaNode {
                    depth: 3,
                    label: pb_field_label(f),
                    span: f.value.span.clone(),
                    collapsible: false,
                });
            }
        }
    }

    // Collect data buffer references: pair buffer_offsets with buffer_sizes.
    for (bi, (&off, &size)) in buffer_offsets.iter().zip(buffer_sizes.iter()).enumerate() {
        data_bufs.push((col_idx, page_idx, bi, off, size));
    }
}

/// Decode a packed `repeated uint64` field into a `Vec<u64>`.
/// In proto3, scalar repeated fields default to packed encoding: the entire
/// list is stored as a single length-delimited field whose payload is the
/// concatenation of the individual varints.
///
/// **Pitfall:** the protobuf decoder may misinterpret packed varint bytes as a
/// nested `Message` if the bytes happen to form valid protobuf tags (e.g.
/// `40 80 01` decodes as `field 8 = varint 128`). When this happens we fall
/// back to reading the raw bytes from the field's span within `file_bytes`.
fn packed_u64_list(f: &PbField, file_bytes: &[u8]) -> Vec<u64> {
    let bytes: Vec<u8> = match &f.value.data {
        PbData::Bytes(b) => b.clone(),
        PbData::String(s) => s.as_bytes().to_vec(),
        PbData::Varint(v) => return vec![*v],
        PbData::Fixed32(v) => return vec![*v as u64],
        PbData::Fixed64(v) => return vec![*v],
        PbData::Message(_) => {
            // The decoder misinterpreted the packed varints as a message.
            // Recover the raw payload from the span. The span covers
            // tag + length + payload, so we need to skip the tag and length
            // prefix to get to the payload bytes.
            let span = &f.value.span;
            if span.start >= span.end || span.end > file_bytes.len() as u64 {
                return Vec::new();
            }
            let raw = &file_bytes[span.start as usize..span.end as usize];
            // Skip the tag (1+ bytes varint) and length (1+ bytes varint)
            // to reach the payload. We re-decode the tag and length.
            let mut pos = 0;
            // Skip tag varint.
            while pos < raw.len() {
                let b = raw[pos];
                pos += 1;
                if b & 0x80 == 0 {
                    break;
                }
            }
            // Read length varint.
            let mut len: usize = 0;
            let mut shift: u32 = 0;
            while pos < raw.len() {
                let b = raw[pos];
                pos += 1;
                len |= ((b & 0x7f) as usize) << shift;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            raw.get(pos..pos + len)
                .map(|s| s.to_vec())
                .unwrap_or_default()
        }
    };
    let bytes = bytes.as_slice();
    let mut vals: Vec<u64> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        let mut ok = false;
        while pos < bytes.len() {
            let b = bytes[pos];
            pos += 1;
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                ok = true;
                break;
            }
            shift += 7;
            if shift >= 64 {
                break;
            }
        }
        if !ok {
            break;
        }
        vals.push(result);
        if vals.len() >= 1024 {
            break; // safety cap
        }
    }
    vals
}

/// Decode a packed `repeated uint64` field into a bracketed list of values
/// for display. Returns a string like `[0, 100]`.
fn packed_u64_preview(f: &PbField, file_bytes: &[u8]) -> String {
    let vals = packed_u64_list(f, file_bytes);
    if vals.is_empty() {
        return "[]".into();
    }
    if vals.len() <= 8 {
        format!(
            "[{}]",
            vals.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
        )
    } else {
        format!(
            "[{}, … ({} total)]",
            vals.iter()
                .take(8)
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            vals.len()
        )
    }
}

/// Strip the leading "/" from a protobuf type URL for cleaner display.
fn strip_type_prefix(url: &str) -> String {
    url.strip_prefix('/').unwrap_or(url).to_string()
}

/// Get the raw byte length of an `Any`-encoded value (for fallback labels).
fn any_raw_len(value: &PbValue) -> usize {
    match &value.data {
        PbData::Bytes(b) => b.len(),
        PbData::Message(m) => m.len(),
        PbData::String(s) => s.len(),
        _ => 0,
    }
}

/// Recursively render generic protobuf fields as tree nodes + regions.
fn pb_fields_node(
    layout: &mut LanceLayout,
    fields: &[PbField],
    depth: u8,
    kind: RegionKind,
) {
    for f in fields {
        pb_field_node(layout, f, depth, kind);
        if let PbData::Message(inner) = &f.value.data {
            if depth < 6 {
                pb_fields_node(layout, inner, depth + 1, kind);
            }
        }
    }
}

/// Push a single generic protobuf field as a tree node + region.
fn pb_field_node(layout: &mut LanceLayout, f: &PbField, depth: u8, kind: RegionKind) {
    let label = pb_field_label(f);
    layout.regions.push(Region {
        span: f.value.span.clone(),
        kind,
        label: label.clone(),
    });
    layout.tree.push(MetaNode {
        depth,
        label,
        span: f.value.span.clone(),
        collapsible: matches!(f.value.data, PbData::Message(_)),
    });
}

/// Push a single generic protobuf field as a tree node (no region).
fn pb_field_node_no_region(layout: &mut LanceLayout, f: &PbField, depth: u8) {
    layout.tree.push(MetaNode {
        depth,
        label: pb_field_label(f),
        span: f.value.span.clone(),
        collapsible: matches!(f.value.data, PbData::Message(_)),
    });
}

/// Produce a human-readable label for a protobuf field (generic, by number).
fn pb_field_label(f: &PbField) -> String {
    format!("field {} = {}", f.number, pb_field_label_raw(&f.value))
}

fn pb_field_label_raw(value: &PbValue) -> String {
    match &value.data {
        PbData::Varint(v) => v.to_string(),
        PbData::Fixed32(v) => format!("0x{v:08x}"),
        PbData::Fixed64(v) => format!("0x{v:016x}"),
        PbData::Bytes(b) => {
            let preview = b
                .iter()
                .take(16)
                .map(|x| format!("{x:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let extra = if b.len() > 16 { " …" } else { "" };
            format!("bytes[{}] {preview}{extra}", b.len())
        }
        PbData::String(s) => {
            let preview: String = s.chars().take(32).collect();
            let extra = if s.chars().count() > 32 { "…" } else { "" };
            format!("{preview:?}{extra}")
        }
        PbData::Message(inner) => format!("message[{} fields]", inner.len()),
    }
}

fn read_u64_le(bytes: &[u8], off: u64) -> u64 {
    let o = off as usize;
    if o + 8 > bytes.len() {
        return 0;
    }
    u64::from_le_bytes([
        bytes[o],
        bytes[o + 1],
        bytes[o + 2],
        bytes[o + 3],
        bytes[o + 4],
        bytes[o + 5],
        bytes[o + 6],
        bytes[o + 7],
    ])
}

fn read_u32_le(bytes: &[u8], off: u64) -> u32 {
    let o = off as usize;
    if o + 4 > bytes.len() {
        return 0;
    }
    u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
}

fn read_u16_le(bytes: &[u8], off: u64) -> u16 {
    let o = off as usize;
    if o + 2 > bytes.len() {
        return 0;
    }
    u16::from_le_bytes([bytes[o], bytes[o + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_too_small() {
        let bytes = [0u8; 8];
        let layout = parse(&bytes);
        assert!(!layout.is_lance);
        assert!(layout.error.unwrap().contains("too small"));
    }

    #[test]
    fn rejects_non_lance_magic() {
        // 40 bytes but wrong trailing magic.
        let mut bytes = vec![0u8; 40];
        bytes[36..40].copy_from_slice(b"PAR1");
        let layout = parse(&bytes);
        assert!(!layout.is_lance);
        assert!(layout.error.unwrap().contains("bad trailing magic"));
    }

    #[test]
    fn rejects_too_large_cap_constant() {
        assert_eq!(MAX_FILE_LEN, 256 * 1024 * 1024);
        assert_eq!(FOOTER_LEN, 40);
    }

    #[test]
    fn parses_dentry_lance_fixture() {
        let bytes = std::fs::read("example/dentry.lance").expect("fixture missing");
        let layout = parse(&bytes);
        assert!(layout.is_lance, "is_lance should be true; error={:?}", layout.error);

        // Must have all spec-defined region kinds.
        assert!(layout.regions.iter().any(|r| r.kind == RegionKind::Magic));
        assert!(layout.regions.iter().any(|r| r.kind == RegionKind::Footer));
        assert!(
            layout
                .regions
                .iter()
                .any(|r| r.kind == RegionKind::ColumnMetadataOffset),
            "missing ColumnMetadataOffset region"
        );
        assert!(
            layout
                .regions
                .iter()
                .any(|r| r.kind == RegionKind::GlobalBufferOffset),
            "missing GlobalBufferOffset region"
        );
        assert!(layout.regions.iter().any(|r| r.kind == RegionKind::ColumnMetadata));
        assert!(layout.regions.iter().any(|r| r.kind == RegionKind::GlobalBuffer));
        assert!(layout.regions.iter().any(|r| r.kind == RegionKind::DataPages));
        // Data buffers should be discovered from column page metadata.
        assert!(
            layout.regions.iter().any(|r| r.kind == RegionKind::DataBuffer),
            "missing DataBuffer regions — page buffer_offsets/sizes not parsed"
        );

        // Tree should contain spec field names.
        let tree_text: String = layout
            .tree
            .iter()
            .map(|n| n.label.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        // FileDescriptor / Schema / Field names.
        assert!(tree_text.contains("FileDescriptor"), "tree missing FileDescriptor: {tree_text}");
        assert!(tree_text.contains("Schema"), "tree missing Schema");
        assert!(tree_text.contains("parent_ino"), "tree missing parent_ino");
        assert!(tree_text.contains("name"), "tree missing name");
        assert!(tree_text.contains("ino"), "tree missing ino");
        assert!(tree_text.contains("uint64"), "tree missing logical_type uint64");
        assert!(tree_text.contains("string"), "tree missing logical_type string");
        // ColumnMetadata / Page / Encoding.
        assert!(tree_text.contains("Column Metadatas"), "tree missing 'Column Metadatas' parent");
        assert!(tree_text.contains("ColumnMetadata"), "tree missing ColumnMetadata");
        assert!(tree_text.contains("ColumnEncoding"), "tree missing ColumnEncoding");
        assert!(tree_text.contains("ArrayEncoding"), "tree missing ArrayEncoding");
        assert!(tree_text.contains("Page"), "tree missing Page");
        // Footer fields.
        assert!(tree_text.contains("col_meta_0_offset"), "tree missing col_meta_0_offset");
        assert!(tree_text.contains("cmo_table_offset"), "tree missing cmo_table_offset");
        assert!(tree_text.contains("gbo_table_offset"), "tree missing gbo_table_offset");
        assert!(tree_text.contains("num_columns"), "tree missing num_columns");
        assert!(tree_text.contains("major_version"), "tree missing major_version");
        // Encoding variant names from encodings_v2_0.proto.
        assert!(tree_text.contains("Values"), "tree missing ColumnEncoding::Values");
        // type_url display.
        assert!(tree_text.contains("lance.encodings.ColumnEncoding"), "tree missing type_url");
        assert!(tree_text.contains("lance.encodings.ArrayEncoding"), "tree missing type_url");

        // Regions sorted by start.
        let mut sorted = layout.regions.clone();
        sorted.sort_by_key(|r| (r.span.start, r.span.end));
        assert_eq!(layout.regions, sorted);

        // Tree must follow physical byte order.
        let tree_starts: Vec<u64> = layout.tree.iter().map(|n| n.span.start).collect();
        let mut expected = tree_starts.clone();
        expected.sort();
        assert_eq!(
            tree_starts, expected,
            "tree nodes must be in physical byte order"
        );
        // First depth-0 node = Data Pages @0, last = Footer.
        let depth0: Vec<&MetaNode> = layout.tree.iter().filter(|n| n.depth == 0).collect();
        assert!(!depth0.is_empty());
        assert_eq!(depth0.first().unwrap().span.start, 0, "first node should be Data Pages @0");
        assert!(
            depth0.last().unwrap().span.start >= 755,
            "last depth-0 node should be Footer, got start={}",
            depth0.last().unwrap().span.start
        );

        // ColumnMetadata nodes must be depth 1 (under "Column Metadatas" parent).
        let col_meta_nodes: Vec<&MetaNode> = layout
            .tree
            .iter()
            .filter(|n| n.label.starts_with("ColumnMetadata["))
            .collect();
        assert!(!col_meta_nodes.is_empty(), "no ColumnMetadata[] nodes");
        for n in &col_meta_nodes {
            assert_eq!(
                n.depth, 1,
                "ColumnMetadata node should be depth 1 (under Column Metadatas), got depth {}: {}",
                n.depth, n.label
            );
        }

        // Data Buffer nodes must be depth 1 (under Data Pages parent).
        let data_buf_nodes: Vec<&MetaNode> = layout
            .tree
            .iter()
            .filter(|n| n.label.starts_with("Data Buffer"))
            .collect();
        assert!(!data_buf_nodes.is_empty(), "no Data Buffer nodes");
        for n in &data_buf_nodes {
            assert_eq!(
                n.depth, 1,
                "Data Buffer node should be depth 1 (under Data Pages), got depth {}: {}",
                n.depth, n.label
            );
        }
    }

    #[test]
    fn footer_fields_match_spec_offsets() {
        let bytes = std::fs::read("example/dentry.lance").expect("fixture missing");
        let layout = parse(&bytes);
        assert!(layout.is_lance);
        // The footer label should mention v0.3, 3 cols, 1 global buf.
        let footer_region = layout
            .regions
            .iter()
            .find(|r| r.kind == RegionKind::Footer)
            .unwrap();
        assert!(footer_region.label.contains("v0.3"), "footer label: {}", footer_region.label);
        assert!(footer_region.label.contains("3 cols"));
        assert!(footer_region.label.contains("1 global buf"));
    }

    #[test]
    fn data_buffers_match_page_metadata() {
        let bytes = std::fs::read("example/dentry.lance").expect("fixture missing");
        let layout = parse(&bytes);
        assert!(layout.is_lance);
        // Col[0] page[0]: buffer_offsets=[0], buffer_sizes=[40]
        // Col[1] page[0]: buffer_offsets=[64,128], buffer_sizes=[40,44]
        // Col[2] page[0]: buffer_offsets=[192], buffer_sizes=[40]
        let data_bufs: Vec<&Region> = layout
            .regions
            .iter()
            .filter(|r| r.kind == RegionKind::DataBuffer)
            .collect();
        assert!(
            data_bufs.iter().any(|r| r.span == (0..40)),
            "missing data buffer @0..40 (col[0] page[0] buf[0])"
        );
        assert!(
            data_bufs.iter().any(|r| r.span == (64..104)),
            "missing data buffer @64..104 (col[1] page[0] buf[0])"
        );
        assert!(
            data_bufs.iter().any(|r| r.span == (128..172)),
            "missing data buffer @128..172 (col[1] page[0] buf[1])"
        );
        assert!(
            data_bufs.iter().any(|r| r.span == (192..232)),
            "missing data buffer @192..232 (col[2] page[0] buf[0])"
        );
    }

    #[test]
    fn truncated_lance_does_not_panic() {
        let bytes = std::fs::read("example/dentry.lance").expect("fixture missing");
        let half = &bytes[..bytes.len() / 2];
        let layout = parse(half);
        let _ = layout.error;
    }
}
