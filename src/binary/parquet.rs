//! Parquet file structure parser.
//!
//! Validates the `PAR1` magic + footer, decodes the Thrift `FileMetaData`
//! (via [`crate::binary::thrift`]) and walks each column chunk's page headers,
//! producing a [`ParquetLayout`] that maps every interesting region back to a
//! byte span. We stop at the page-header level — page value decoding and
//! decompression are out of scope (this is for learning the on-disk layout).

use std::ops::Range;

use crate::binary::thrift::{WireField, WireKind, WireValue, decode_struct_span_at};

/// Parquet magic bytes (`PAR1`).
pub const MAGIC: &[u8; 4] = b"PAR1";

/// A labelled byte region in the file.
#[derive(Debug, Clone)]
pub struct Region {
    pub span: Range<u64>,
    pub kind: RegionKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Magic,
    FooterLength,
    Metadata,
    /// A sub-field inside FileMetaData (version, schema, num_rows, row_groups,
    /// created_by). Nested inside [`RegionKind::Metadata`].
    MetaField,
    RowGroup,
    ColumnChunk,
    PageHeader,
    PageData,
    Unknown,
}

/// A node in the human-readable structure tree shown in the right pane.
#[derive(Debug, Clone)]
pub struct MetaNode {
    pub depth: u8,
    pub label: String,
    pub span: Range<u64>,
    pub collapsible: bool,
}

/// Result of parsing a Parquet file's structure.
#[derive(Debug, Default, Clone)]
pub struct ParquetLayout {
    pub is_parquet: bool,
    pub regions: Vec<Region>,
    pub tree: Vec<MetaNode>,
    pub error: Option<String>,
}

impl ParquetLayout {
    fn err(msg: impl Into<String>) -> Self {
        Self {
            is_parquet: false,
            error: Some(msg.into()),
            ..Default::default()
        }
    }
}

/// Parse the parquet structure of `bytes` (which must be the entire file).
pub fn parse(bytes: &[u8]) -> ParquetLayout {
    let len = bytes.len() as u64;

    // ---- File-level validation -------------------------------------------
    if len < 8 {
        return ParquetLayout::err("file too small to be Parquet (< 8 bytes)");
    }
    if &bytes[0..4] != MAGIC {
        return ParquetLayout {
            is_parquet: false,
            error: Some(format!(
                "not a Parquet file: bad header magic {:?}",
                &bytes[0..4]
            )),
            ..Default::default()
        };
    }
    if &bytes[(len - 4) as usize..len as usize] != MAGIC {
        return ParquetLayout {
            is_parquet: false,
            error: Some("not a Parquet file: bad footer magic".into()),
            ..Default::default()
        };
    }

    let mut layout = ParquetLayout {
        is_parquet: true,
        ..Default::default()
    };
    layout.regions.push(Region {
        span: 0..4,
        kind: RegionKind::Magic,
        label: "PAR1 (header magic)".into(),
    });
    layout.tree.push(MetaNode {
        depth: 0,
        label: "PAR1 (header magic) @0x00000000..0x00000004".into(),
        span: 0..4,
        collapsible: false,
    });
    layout.regions.push(Region {
        span: len - 4..len,
        kind: RegionKind::Magic,
        label: "PAR1 (footer magic)".into(),
    });

    // ---- Footer length (LE u32 just before the trailing PAR1) ------------
    let footer_len_off = len - 8;
    let footer_len = u32::from_le_bytes([
        bytes[footer_len_off as usize],
        bytes[footer_len_off as usize + 1],
        bytes[footer_len_off as usize + 2],
        bytes[footer_len_off as usize + 3],
    ]) as u64;
    if footer_len > len - 8 {
        layout.error = Some(format!(
            "footer length {footer_len} exceeds available bytes {}",
            len - 8
        ));
        layout.regions.push(Region {
            span: footer_len_off..footer_len_off + 4,
            kind: RegionKind::FooterLength,
            label: format!("footer length = {footer_len} (invalid)"),
        });
        layout.tree.push(MetaNode {
            depth: 0,
            label: format!("footer length = {footer_len} (invalid)"),
            span: footer_len_off..footer_len_off + 4,
            collapsible: false,
        });
        return layout;
    }
    let meta_start = len - 8 - footer_len;
    let meta_end = footer_len_off;

    // File-level regions (always present for a valid Parquet file).
    layout.regions.push(Region {
        span: footer_len_off..footer_len_off + 4,
        kind: RegionKind::FooterLength,
        label: format!("footer length = {footer_len}"),
    });
    layout.regions.push(Region {
        span: meta_start..meta_end,
        kind: RegionKind::Metadata,
        label: format!("FileMetaData @0x{meta_start:08x}..0x{meta_end:08x} [{footer_len} B]"),
    });

    // ---- Decode FileMetaData ---------------------------------------------
    let Some((fields, _meta_span)) = decode_struct_span_at(bytes, 0, meta_start) else {
        layout.error = Some("invalid FileMetaData Thrift encoding".into());
        return layout;
    };

    // ---- Build physical-layout tree --------------------------------------
    // The tree mirrors the byte layout on disk, not the Thrift logical order:
    //   PAR1 header
    //   [data region: RowGroup > ColumnChunk > Page > (PageHeader, PageData)]
    //   FileMetaData (Thrift footer)
    //     └─ RowGroup metadata > ColumnChunk metadata (points back at the data span)
    //   footer length (LE u32)
    //   PAR1 footer
    //
    // The data region is only walked if FileMetaData has a row_groups list;
    // its bytes physically sit between the header magic and FileMetaData.

    // (1) header magic — already pushed above
    // (2) data region (row groups → column chunks → pages)
    data_region_node(&mut layout, &fields, bytes, meta_start);

    // (3) FileMetaData footer
    layout.tree.push(MetaNode {
        depth: 0,
        label: format!("FileMetaData @0x{meta_start:08x}..0x{meta_end:08x} [{footer_len} B]"),
        span: meta_start..meta_end,
        collapsible: true,
    });
    for f in &fields {
        match f.id {
            1 => {
                if let Some(v) = f.value.as_i64() {
                    push_meta_field(&mut layout, 1, &format!("version = {v}"), &f.value.span);
                }
            }
            2 => {
                if let WireKind::List(items) = &f.value.kind {
                    push_meta_field_region_only(
                        &mut layout,
                        &format!("schema[{}]", items.len()),
                        &f.value.span,
                    );
                    schema_node(&mut layout.tree, items, &f.value.span);
                }
            }
            3 => {
                if let Some(v) = f.value.as_i64() {
                    push_meta_field(&mut layout, 1, &format!("num_rows = {v}"), &f.value.span);
                }
            }
            4 => {
                if let WireKind::List(items) = &f.value.kind {
                    push_meta_field_region_only(
                        &mut layout,
                        &format!("row_groups[{}]", items.len()),
                        &f.value.span,
                    );
                    row_groups_metadata_node(&mut layout.tree, items, &f.value.span);
                }
            }
            5 => {
                if let WireKind::List(items) = &f.value.kind {
                    push_meta_field_region_only(
                        &mut layout,
                        &format!("key_value_metadata[{}]", items.len()),
                        &f.value.span,
                    );
                    kv_node(&mut layout.tree, items, &f.value.span);
                }
            }
            6 => {
                if let Some(b) = f.value.as_binary() {
                    let s = String::from_utf8_lossy(b).to_string();
                    push_meta_field(
                        &mut layout,
                        1,
                        &format!("created_by = {s:?}"),
                        &f.value.span,
                    );
                }
            }
            _ => {}
        }
    }

    // (4) footer length — physically sits between FileMetaData and footer magic
    layout.tree.push(MetaNode {
        depth: 0,
        label: format!("footer length = {footer_len} @0x{footer_len_off:08x}"),
        span: footer_len_off..footer_len_off + 4,
        collapsible: false,
    });

    // (5) footer magic
    layout.tree.push(MetaNode {
        depth: 0,
        label: format!("PAR1 (footer magic) @0x{:08x}..0x{:08x}", len - 4, len),
        span: len - 4..len,
        collapsible: false,
    });

    // Keep regions sorted by start offset for [ / ] navigation.
    layout.regions.sort_by_key(|r| (r.span.start, r.span.end));
    layout
}

fn push_leaf(tree: &mut Vec<MetaNode>, depth: u8, label: &str, span: &Range<u64>) {
    tree.push(MetaNode {
        depth,
        label: label.to_string(),
        span: span.clone(),
        collapsible: false,
    });
}

/// Push a FileMetaData sub-field as both a tree leaf (depth 1) and a
/// `MetaField` region, so bytes inside it get a distinct color in the hex
/// pane.
fn push_meta_field(layout: &mut ParquetLayout, depth: u8, label: &str, span: &Range<u64>) {
    push_leaf(&mut layout.tree, depth, label, span);
    layout.regions.push(Region {
        span: span.clone(),
        kind: RegionKind::MetaField,
        label: label.to_string(),
    });
}

/// Push only the `MetaField` region (no tree node). Used for fields whose
/// sub-renderer (e.g. `schema_node`, `row_groups_metadata_node`) already adds
/// its own container tree node, to avoid a duplicate.
fn push_meta_field_region_only(layout: &mut ParquetLayout, label: &str, span: &Range<u64>) {
    layout.regions.push(Region {
        span: span.clone(),
        kind: RegionKind::MetaField,
        label: label.to_string(),
    });
}

fn schema_node(tree: &mut Vec<MetaNode>, items: &[WireValue], list_span: &Range<u64>) {
    tree.push(MetaNode {
        depth: 1,
        label: format!("schema[{}]", items.len()),
        span: list_span.clone(),
        collapsible: true,
    });
    for (i, item) in items.iter().enumerate() {
        if let Some(fields) = item.as_struct() {
            let name = field_str(fields, 4).unwrap_or_else(|| format!("<#{i}>"));
            let kind = field_i64(fields, 1)
                .and_then(parquet_type_name)
                .unwrap_or("?");
            let rep = field_i64(fields, 3).and_then(repetition_name).unwrap_or("");
            let nch = field_i64(fields, 5);
            let label = if let Some(n) = nch {
                format!(
                    "SchemaElement \"{}\" [{}] type={} rep={}",
                    name, n, kind, rep
                )
            } else {
                format!("SchemaElement \"{}\" type={} rep={}", name, kind, rep)
            };
            tree.push(MetaNode {
                depth: 2,
                label,
                span: item.span.clone(),
                collapsible: false,
            });
        }
    }
}

fn kv_node(tree: &mut Vec<MetaNode>, items: &[WireValue], list_span: &Range<u64>) {
    tree.push(MetaNode {
        depth: 1,
        label: format!("key_value_metadata[{}]", items.len()),
        span: list_span.clone(),
        collapsible: true,
    });
    for item in items {
        if let Some(fields) = item.as_struct() {
            let key = field_str(fields, 1).unwrap_or_default();
            let val = field_str(fields, 2);
            let label = if let Some(v) = val {
                format!("{key} = {v:?}")
            } else {
                key
            };
            tree.push(MetaNode {
                depth: 2,
                label,
                span: item.span.clone(),
                collapsible: false,
            });
        }
    }
}

/// Build the physical data-region sub-tree (RowGroup → ColumnChunk → Page →
/// PageHeader/PageData). These bytes sit between the header magic and
/// FileMetaData. The tree node spans are the *physical* byte ranges, not the
/// Thrift-struct spans (which live inside FileMetaData).
fn data_region_node(
    layout: &mut ParquetLayout,
    fields: &[WireField],
    bytes: &[u8],
    meta_start: u64,
) {
    let Some(rg_list) = fields
        .iter()
        .find(|f| f.id == 4)
        .and_then(|f| f.value.as_list())
    else {
        return;
    };
    for (rg_idx, item) in rg_list.iter().enumerate() {
        let Some(rg_fields) = item.as_struct() else {
            continue;
        };
        let num_rows = field_i64(rg_fields, 3).unwrap_or(0);
        let total_bytes = field_i64(rg_fields, 2).unwrap_or(0);
        let columns = field_list(rg_fields, 1).unwrap_or_default();

        // Compute the physical byte span of each column chunk from its page
        // offsets. `total_compressed_size` is unreliable for deriving the chunk
        // end (some writers, e.g. DuckDB, report only the data-page bytes,
        // excluding dictionary pages), so we bound each chunk by the *next*
        // chunk's start (or meta_start for the last chunk) instead.
        let mut chunk_starts: Vec<u64> = Vec::new();
        let mut chunk_meta: Vec<&[WireField]> = Vec::new();
        for col in columns {
            let Some(fc) = col.as_struct() else { continue };
            let Some(cm) = field_struct(fc, 3) else {
                continue;
            };
            let dict = field_i64(cm, 12).unwrap_or(0) as u64;
            let data = field_i64(cm, 9).unwrap_or(0) as u64;
            let start = if dict > 0 && (data == 0 || dict < data) {
                dict
            } else {
                data
            };
            if start == 0 {
                continue;
            }
            chunk_starts.push(start);
            chunk_meta.push(cm);
        }
        // Sort chunks by start offset so each chunk's end = next chunk's start.
        let mut order: Vec<usize> = (0..chunk_starts.len()).collect();
        order.sort_by_key(|&i| chunk_starts[i]);
        let mut chunk_ends: Vec<u64> = vec![0; chunk_starts.len()];
        for (pos, &i) in order.iter().enumerate() {
            chunk_ends[i] = if pos + 1 < order.len() {
                chunk_starts[order[pos + 1]]
            } else {
                meta_start
            };
        }
        let rg_start = chunk_starts.iter().copied().min().unwrap_or(0);
        let rg_end = chunk_ends.iter().copied().max().unwrap_or(0);
        if rg_end <= rg_start {
            continue;
        }
        let rg_span = rg_start..rg_end;
        layout.tree.push(MetaNode {
            depth: 0,
            label: format!(
                "RowGroup #{rg_idx} (data) rows={num_rows} bytes={total_bytes} cols={}",
                columns.len()
            ),
            span: rg_span.clone(),
            collapsible: true,
        });
        layout.regions.push(Region {
            span: rg_span,
            kind: RegionKind::RowGroup,
            label: format!("RowGroup #{rg_idx} rows={num_rows}"),
        });

        for (c_idx, cm) in chunk_meta.iter().enumerate() {
            let start = chunk_starts[c_idx];
            let end = chunk_ends[c_idx];
            column_chunk_data_node(layout, c_idx, start, end, cm, bytes);
        }
    }
}

/// One ColumnChunk in the physical data region: walks its page headers and
/// emits PageHeader / PageData children.
fn column_chunk_data_node(
    layout: &mut ParquetLayout,
    c_idx: usize,
    span_start: u64,
    span_end: u64,
    cc_meta: &[WireField],
    bytes: &[u8],
) {
    let path = column_path(cc_meta);
    let codec = field_i64(cc_meta, 4).and_then(codec_name).unwrap_or("?");
    let num_values = field_i64(cc_meta, 5).unwrap_or(0);
    let total_compressed = field_i64(cc_meta, 7).unwrap_or(0);
    let total_uncompressed = field_i64(cc_meta, 6).unwrap_or(0);
    let cc_span = span_start..span_end;

    layout.tree.push(MetaNode {
        depth: 1,
        label: format!("ColumnChunk #{c_idx} path={path}"),
        span: cc_span.clone(),
        collapsible: true,
    });
    // Detail leaves.
    push_leaf(
        &mut layout.tree,
        2,
        &format!("codec = {codec}"),
        &field_offset_span(cc_meta, 4),
    );
    push_leaf(
        &mut layout.tree,
        2,
        &format!("num_values = {num_values}"),
        &field_offset_span(cc_meta, 5),
    );
    push_leaf(
        &mut layout.tree,
        2,
        &format!("total_compressed = {total_compressed}B"),
        &field_offset_span(cc_meta, 7),
    );
    push_leaf(
        &mut layout.tree,
        2,
        &format!("total_uncompressed = {total_uncompressed}B"),
        &field_offset_span(cc_meta, 6),
    );
    layout.regions.push(Region {
        span: cc_span,
        kind: RegionKind::ColumnChunk,
        label: format!("ColumnChunk #{c_idx} {path}"),
    });

    pages_node(layout, span_start, span_end, cc_meta, bytes);
}

/// Walk the page headers inside one column chunk. Each page produces a
/// `Page #N` node with `PageHeader` (the inline Thrift struct) and `PageData`
/// (the compressed values: RL+DL+values for data pages; dictionary entries for
/// dictionary pages) children.
fn pages_node(
    layout: &mut ParquetLayout,
    _chunk_start: u64,
    chunk_end: u64,
    cc_meta: &[WireField],
    bytes: &[u8],
) {
    let dict_off = field_i64(cc_meta, 12).unwrap_or(0) as u64;
    let data_off = field_i64(cc_meta, 9).unwrap_or(0) as u64;
    let start = if dict_off > 0 && (data_off == 0 || dict_off < data_off) {
        dict_off
    } else if data_off > 0 {
        data_off
    } else {
        return;
    };
    let page_cap = 10_000usize;
    let mut cur = start;
    let mut count = 0usize;
    while cur < chunk_end && count < page_cap {
        let Some((pf, struct_span)) = decode_struct_span_at(bytes, 0, cur) else {
            break;
        };
        let ptype = field_i64(&pf, 1).and_then(page_type_name).unwrap_or("?");
        let uncomp = field_i64(&pf, 2).unwrap_or(0);
        let comp = field_i64(&pf, 3).unwrap_or(0);
        // The struct span includes the trailing STOP byte, so this is the true
        // end of the page header.
        let header_end = struct_span.end;
        let data_start = header_end;
        let data_end = (data_start + comp as u64).min(chunk_end);
        let page_span = cur..data_end.max(header_end);

        layout.tree.push(MetaNode {
            depth: 2,
            label: format!("Page #{count} {ptype} uncomp={uncomp}B comp={comp}B @0x{cur:08x}"),
            span: page_span.clone(),
            collapsible: true,
        });
        layout.tree.push(MetaNode {
            depth: 3,
            label: format!(
                "PageHeader {ptype} @0x{cur:08x}..0x{header_end:08x} [{header_size} B]",
                header_size = header_end - cur
            ),
            span: cur..header_end,
            collapsible: true,
        });
        page_header_fields_node(&mut layout.tree, &pf);
        if data_end > data_start {
            layout.tree.push(MetaNode {
                depth: 3,
                label: format!("PageData {ptype} [{comp} B] @0x{data_start:08x}..0x{data_end:08x}"),
                span: data_start..data_end,
                collapsible: false,
            });
        }
        layout.regions.push(Region {
            span: cur..header_end,
            kind: RegionKind::PageHeader,
            label: format!("PageHeader #{count} {ptype}"),
        });
        if data_end > data_start {
            layout.regions.push(Region {
                span: data_start..data_end,
                kind: RegionKind::PageData,
                label: format!("PageData #{count} {ptype} [{comp} B]"),
            });
        }
        let next = data_end.max(header_end);
        if next <= cur {
            break;
        }
        cur = next;
        count += 1;
    }
}

/// Expand a decoded `PageHeader` Thrift struct into tree leaf nodes (depth 4).
///
/// Shows the page-type-specific sub-header fields:
/// - DATA_PAGE (field 5 → DataPageHeader): num_values, encoding,
///   definition_level_encoding, repetition_level_encoding
/// - DICTIONARY_PAGE (field 7 → DictionaryPageHeader): num_values, encoding
/// - DATA_PAGE_V2 (field 8 → DataPageHeaderV2): num_values, num_nulls,
///   num_rows, is_compressed
///
/// Each leaf's span is the Thrift field's own byte span, so cursor navigation
/// lands on the exact encoded bytes.
fn page_header_fields_node(tree: &mut Vec<MetaNode>, pf: &[WireField]) {
    // Top-level PageHeader fields.
    push_i64_leaf(tree, 4, pf, 1, |v| {
        let name = page_type_name(v).unwrap_or("?");
        format!("type = {v} ({name})")
    });
    push_i64_leaf(tree, 4, pf, 2, |v| format!("uncompressed_page_size = {v}"));
    push_i64_leaf(tree, 4, pf, 3, |v| format!("compressed_page_size = {v}"));
    push_i64_leaf(tree, 4, pf, 4, |v| format!("crc = 0x{v:08x}"));

    // DataPageHeader (field 5) — for DATA_PAGE.
    if let Some(dph) = field_struct(pf, 5) {
        push_subheader(
            tree,
            "DataPageHeader",
            dph,
            &[
                (1, "num_values", FieldFmt::I64),
                (2, "encoding", FieldFmt::Encoding),
                (3, "definition_level_encoding", FieldFmt::Encoding),
                (4, "repetition_level_encoding", FieldFmt::Encoding),
            ],
        );
    }
    // DictionaryPageHeader (field 7) — for DICTIONARY_PAGE.
    if let Some(dph) = field_struct(pf, 7) {
        push_subheader(
            tree,
            "DictionaryPageHeader",
            dph,
            &[
                (1, "num_values", FieldFmt::I64),
                (2, "encoding", FieldFmt::Encoding),
            ],
        );
    }
    // DataPageHeaderV2 (field 8) — for DATA_PAGE_V2.
    if let Some(dph) = field_struct(pf, 8) {
        push_subheader(
            tree,
            "DataPageHeaderV2",
            dph,
            &[
                (1, "num_values", FieldFmt::I64),
                (2, "num_nulls", FieldFmt::I64),
                (3, "num_rows", FieldFmt::I64),
                (4, "encoding", FieldFmt::Encoding),
                (5, "is_compressed", FieldFmt::Bool),
            ],
        );
    }
}

/// Look up an i64 Thrift field by id and push a formatted leaf node.
fn push_i64_leaf(
    tree: &mut Vec<MetaNode>,
    depth: u8,
    fields: &[WireField],
    id: i16,
    fmt: impl Fn(i64) -> String,
) {
    if let Some(f) = fields.iter().find(|f| f.id == id)
        && let Some(v) = f.value.as_i64()
    {
        push_leaf(tree, depth, &fmt(v), &f.value.span);
    }
}

/// Field value formatter for sub-header leaves.
enum FieldFmt {
    I64,
    Encoding,
    Bool,
}

fn push_subheader(
    tree: &mut Vec<MetaNode>,
    name: &str,
    fields: &[WireField],
    specs: &[(i16, &str, FieldFmt)],
) {
    tree.push(MetaNode {
        depth: 4,
        label: name.to_string(),
        span: fields.first().map(|f| f.value.span.clone()).unwrap_or(0..0),
        collapsible: true,
    });
    for (id, label, fmt) in specs {
        let Some(f) = fields.iter().find(|f| f.id == *id) else {
            continue;
        };
        let val_str = match fmt {
            FieldFmt::I64 => f.value.as_i64().map(|v| v.to_string()),
            FieldFmt::Encoding => f
                .value
                .as_i64()
                .and_then(encoding_name)
                .map(|n| n.to_string()),
            FieldFmt::Bool => f.value.as_bool().map(|b| b.to_string()),
        };
        if let Some(v) = val_str {
            push_leaf(tree, 5, &format!("{label} = {v}"), &f.value.span);
        }
    }
}

/// FileMetaData's `row_groups` list — pure metadata. Each RowGroup entry
/// contains ColumnChunk entries whose `meta_data` holds file_offset, codec,
/// num_values, total_compressed, and the dict/data page offsets. We surface
/// those fields and the *referenced* data span, but the bytes themselves live
/// in the data region above (the struct span here is the Thrift encoding).
fn row_groups_metadata_node(tree: &mut Vec<MetaNode>, items: &[WireValue], list_span: &Range<u64>) {
    tree.push(MetaNode {
        depth: 1,
        label: format!("row_groups[{}] (metadata)", items.len()),
        span: list_span.clone(),
        collapsible: true,
    });
    for (rg_idx, item) in items.iter().enumerate() {
        let Some(fields) = item.as_struct() else {
            continue;
        };
        let num_rows = field_i64(fields, 3).unwrap_or(0);
        let total_bytes = field_i64(fields, 2).unwrap_or(0);
        let columns = field_list(fields, 1).unwrap_or_default();
        tree.push(MetaNode {
            depth: 2,
            label: format!(
                "RowGroup #{rg_idx} rows={num_rows} bytes={total_bytes} cols={}",
                columns.len()
            ),
            span: item.span.clone(),
            collapsible: true,
        });
        for (c_idx, col) in columns.iter().enumerate() {
            let Some(fc) = col.as_struct() else { continue };
            let file_offset = field_i64(fc, 2).unwrap_or(0);
            let Some(cm) = field_struct(fc, 3) else {
                continue;
            };
            let path = column_path(cm);
            let codec = field_i64(cm, 4).and_then(codec_name).unwrap_or("?");
            let num_values = field_i64(cm, 5).unwrap_or(0);
            let total_compressed = field_i64(cm, 7).unwrap_or(0);
            let total_uncompressed = field_i64(cm, 6).unwrap_or(0);
            let dict_off = field_i64(cm, 12).unwrap_or(0);
            let data_off = field_i64(cm, 9).unwrap_or(0);
            let data_start = if dict_off > 0 && (data_off == 0 || dict_off < data_off) {
                dict_off
            } else {
                data_off
            };
            tree.push(MetaNode {
                depth: 3,
                label: format!("ColumnChunk #{c_idx} path={path} → data @0x{data_start:08x}"),
                span: col.span.clone(),
                collapsible: true,
            });
            push_leaf(
                tree,
                4,
                &format!("file_offset = {file_offset}"),
                &field_offset_span(fc, 2),
            );
            push_leaf(
                tree,
                4,
                &format!("codec = {codec}"),
                &field_offset_span(cm, 4),
            );
            push_leaf(
                tree,
                4,
                &format!("num_values = {num_values}"),
                &field_offset_span(cm, 5),
            );
            push_leaf(
                tree,
                4,
                &format!("total_compressed = {total_compressed}B"),
                &field_offset_span(cm, 7),
            );
            push_leaf(
                tree,
                4,
                &format!("total_uncompressed = {total_uncompressed}B"),
                &field_offset_span(cm, 6),
            );
            push_leaf(
                tree,
                4,
                &format!("data_page_offset = 0x{data_off:08x}"),
                &field_offset_span(cm, 9),
            );
            push_leaf(
                tree,
                4,
                &format!("dictionary_page_offset = 0x{dict_off:08x}"),
                &field_offset_span(cm, 12),
            );
        }
    }
}

/// Shared helper: render the `path_in_schema` list as a dotted string.
fn column_path(cc_meta: &[WireField]) -> String {
    field_list(cc_meta, 3)
        .map(|l| {
            l.iter()
                .filter_map(|v| v.as_binary())
                .map(|b| String::from_utf8_lossy(b).to_string())
                .collect::<Vec<_>>()
                .join(".")
        })
        .unwrap_or_default()
}

/// Look up a Thrift field by id and return its span (for showing the exact
/// bytes of a metadata field inside FileMetaData). Returns an empty span if
/// the field is absent.
fn field_offset_span(fields: &[WireField], id: i16) -> Range<u64> {
    fields
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.value.span.clone())
        .unwrap_or(0..0)
}

// ---- Thrift field helpers ------------------------------------------------

fn field_struct(fields: &[WireField], id: i16) -> Option<&[WireField]> {
    fields
        .iter()
        .find(|f| f.id == id)
        .and_then(|f| f.value.as_struct())
}

fn field_list(fields: &[WireField], id: i16) -> Option<&[WireValue]> {
    fields
        .iter()
        .find(|f| f.id == id)
        .and_then(|f| f.value.as_list())
}

fn field_i64(fields: &[WireField], id: i16) -> Option<i64> {
    fields
        .iter()
        .find(|f| f.id == id)
        .and_then(|f| f.value.as_i64())
}

fn field_str(fields: &[WireField], id: i16) -> Option<String> {
    fields
        .iter()
        .find(|f| f.id == id)
        .and_then(|f| f.value.as_binary())
        .map(|b| String::from_utf8_lossy(b).into_owned())
}

// ---- Parquet enum name maps ---------------------------------------------

pub fn parquet_type_name(t: i64) -> Option<&'static str> {
    Some(match t {
        0 => "BOOLEAN",
        1 => "INT32",
        2 => "INT64",
        3 => "INT96",
        4 => "FLOAT",
        5 => "DOUBLE",
        6 => "BYTE_ARRAY",
        7 => "FIXED_LEN_BYTE_ARRAY",
        _ => return None,
    })
}

pub fn repetition_name(t: i64) -> Option<&'static str> {
    Some(match t {
        0 => "REQUIRED",
        1 => "OPTIONAL",
        2 => "REPEATED",
        _ => return None,
    })
}

pub fn codec_name(t: i64) -> Option<&'static str> {
    Some(match t {
        0 => "UNCOMPRESSED",
        1 => "SNAPPY",
        2 => "GZIP",
        3 => "LZO",
        4 => "BROTLI",
        5 => "LZ4",
        6 => "ZSTD",
        7 => "LZ4_RAW",
        _ => return None,
    })
}

pub fn page_type_name(t: i64) -> Option<&'static str> {
    Some(match t {
        0 => "DATA_PAGE",
        1 => "INDEX_PAGE",
        2 => "DICTIONARY_PAGE",
        3 => "DATA_PAGE_V2",
        _ => return None,
    })
}

pub fn encoding_name(t: i64) -> Option<&'static str> {
    Some(match t {
        0 => "PLAIN",
        1 => "GROUP_VAR_INT",
        2 => "PLAIN_DICTIONARY",
        3 => "RLE",
        4 => "BIT_PACKED",
        5 => "DELTA_BINARY_PACKED",
        6 => "DELTA_LENGTH_BYTE_ARRAY",
        7 => "DELTA_BYTE_ARRAY",
        8 => "RLE_DICTIONARY",
        9 => "BYTE_STREAM_SPLIT",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use polars::df;
    use polars::prelude::*;

    fn make_parquet_bytes() -> Vec<u8> {
        let mut df = df! {
            "id" => [1i64, 2, 3],
            "name" => ["a", "b", "c"],
        }
        .unwrap();
        let mut buf = Vec::new();
        ParquetWriter::new(&mut buf).finish(&mut df).unwrap();
        buf
    }

    #[test]
    fn parses_real_parquet_footer() {
        let bytes = make_parquet_bytes();
        let layout = parse(&bytes);
        assert!(layout.is_parquet, "error: {:?}", layout.error);
        assert!(layout.error.is_none(), "{:?}", layout.error);
        assert!(layout.regions.iter().any(|r| r.kind == RegionKind::Magic));
        assert!(
            layout
                .regions
                .iter()
                .any(|r| r.kind == RegionKind::FooterLength)
        );
        assert!(
            layout
                .regions
                .iter()
                .any(|r| r.kind == RegionKind::Metadata)
        );
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.starts_with("FileMetaData"))
        );
        assert!(layout.tree.iter().any(|n| n.label.starts_with("schema[")));
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.starts_with("row_groups["))
        );
        assert!(
            layout
                .regions
                .iter()
                .any(|r| r.kind == RegionKind::ColumnChunk)
        );
        assert!(
            layout
                .regions
                .iter()
                .any(|r| r.kind == RegionKind::PageHeader)
        );
        // File-level regions should appear as tree nodes too.
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.contains("PAR1 (header magic)"))
        );
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.contains("PAR1 (footer magic)"))
        );
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.starts_with("footer length"))
        );
    }

    /// A wider DataFrame produces multiple column chunks; every one should
    /// have its pages scanned (regression for the STOP-byte drift bug where
    /// only the first couple of chunks got page headers).
    #[test]
    fn all_column_chunks_have_pages() {
        let mut df = df! {
            "id"   => [1i64, 2, 3, 4, 5],
            "name" => ["a", "b", "c", "d", "e"],
            "val"  => [10.0f64, 20.0, 30.0, 40.0, 50.0],
            "flag" => [true, false, true, false, true],
        }
        .unwrap();
        let mut buf = Vec::new();
        ParquetWriter::new(&mut buf).finish(&mut df).unwrap();
        let layout = parse(&buf);
        assert!(layout.is_parquet);

        let chunk_count = layout
            .regions
            .iter()
            .filter(|r| r.kind == RegionKind::ColumnChunk)
            .count();
        assert!(
            chunk_count >= 4,
            "expected >=4 column chunks, got {chunk_count}"
        );

        // Each column chunk region should be followed by at least one page
        // header region whose span is inside the chunk's span.
        for cc in layout
            .regions
            .iter()
            .filter(|r| r.kind == RegionKind::ColumnChunk)
        {
            let has_page = layout.regions.iter().any(|r| {
                r.kind == RegionKind::PageHeader
                    && r.span.start >= cc.span.start
                    && r.span.end <= cc.span.end
            });
            assert!(
                has_page,
                "column chunk {:?} has no page header region",
                cc.label
            );
        }
    }

    #[test]
    fn rejects_non_parquet() {
        let bytes = b"hello, world! this is not parquet at all".to_vec();
        let layout = parse(&bytes);
        assert!(!layout.is_parquet);
        assert!(layout.error.is_some());
    }

    #[test]
    fn rejects_too_small() {
        let bytes = vec![0u8; 4];
        let layout = parse(&bytes);
        assert!(!layout.is_parquet);
        assert!(layout.error.unwrap().contains("too small"));
    }

    #[test]
    fn enum_name_maps() {
        assert_eq!(parquet_type_name(6), Some("BYTE_ARRAY"));
        assert_eq!(codec_name(1), Some("SNAPPY"));
        assert_eq!(page_type_name(0), Some("DATA_PAGE"));
        assert_eq!(repetition_name(1), Some("OPTIONAL"));
        assert!(parquet_type_name(99).is_none());
    }

    /// Regression: page scanner must never cross into FileMetaData. Some
    /// writers (duckdb/polars) report `total_compressed_size` larger than the
    /// actual page bytes, so the chunk span and page-scan loop must be
    /// hard-capped at the metadata start.
    #[test]
    fn page_regions_never_cross_into_metadata() {
        // A wide DataFrame with mixed types forces multiple column chunks and
        // dictionary pages, mirroring the house-price.parquet layout that
        // originally triggered the bug.
        let mut df = df! {
            "i1" => (0..200i64).collect::<Vec<_>>(),
            "i2" => (0..200i64).map(|x| x * 2).collect::<Vec<_>>(),
            "i3" => (0..200i64).map(|x| x * 3).collect::<Vec<_>>(),
            "s1" => (0..200).map(|x| format!("row{x}")).collect::<Vec<_>>(),
            "s2" => (0..200).map(|x| if x % 2 == 0 { "a" } else { "b" }).collect::<Vec<_>>(),
            "s3" => (0..200).map(|x| format!("val{x}")).collect::<Vec<_>>(),
        }
        .unwrap();
        let mut buf = Vec::new();
        ParquetWriter::new(&mut buf).finish(&mut df).unwrap();
        let layout = parse(&buf);
        assert!(layout.is_parquet);

        let meta = layout
            .regions
            .iter()
            .find(|r| r.kind == RegionKind::Metadata)
            .expect("metadata region");
        let meta_start = meta.span.start;

        // No ColumnChunk / PageHeader / PageData region may start at or past
        // the metadata start, nor end after it.
        for r in &layout.regions {
            match r.kind {
                RegionKind::ColumnChunk
                | RegionKind::PageHeader
                | RegionKind::PageData
                | RegionKind::RowGroup => {
                    assert!(
                        r.span.start < meta_start,
                        "{:?} {:?} starts at/after meta_start 0x{:x}",
                        r.kind,
                        r.span,
                        meta_start
                    );
                    assert!(
                        r.span.end <= meta_start,
                        "{:?} {:?} ends after meta_start 0x{:x}",
                        r.kind,
                        r.span,
                        meta_start
                    );
                }
                _ => {}
            }
        }
    }
}
