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
//! `encodings_v2_0.proto`, `encodings_v2_1.proto`, and `encodings_v2_3.proto`.
//!
//! File layout (all footer fields are little-endian unsigned integers):
//! ```text
//!   ├──────────────────────────────┤
//!   | Data Pages                   |   data buffers (referenced by columns)
//!   |   Data Buffer 0              |
//!   |   ...                        |
//!   |   Data Buffer BN             |
//!   ├──────────────────────────────┤
//!   | Global Buffers               |   references may point anywhere in data area
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

use crate::binary::protobuf::{PbData, PbField, decode_message_span_at};

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

/// Parsed physical structure. References keep metadata at its actual location.
#[derive(Debug, Default, Clone)]
pub struct LanceLayout {
    pub is_lance: bool,
    pub regions: Vec<Region>,
    pub tree: Vec<MetaNode>,
    pub error: Option<String>,
    pub references: Vec<crate::binary::parquet::Reference>,
    render_bytes: usize,
}
impl LanceLayout {
    fn diagnostic(&mut self, message: impl Into<String>) {
        let message = message.into();
        if self
            .error
            .as_ref()
            .is_some_and(|s| s.len() >= 4096 || s.contains(&message))
        {
            return;
        }
        self.error = Some(match self.error.take() {
            Some(old) => format!("{old}; {message}"),
            None => message,
        });
    }
}
fn node(
    l: &mut LanceLayout,
    depth: u8,
    label: impl Into<String>,
    span: Range<u64>,
    kind: RegionKind,
    collapsible: bool,
) {
    if l.tree.len() >= 200_000 {
        if l.tree.len() == 200_000 && !l.error.as_deref().unwrap_or("").contains("node limit") {
            l.diagnostic("metadata node limit reached (200000)");
        }
        return;
    }
    let label = label.into();
    l.tree.push(MetaNode {
        depth,
        label: format!(
            "{label} @0x{:08x}..0x{:08x} [{} B]",
            span.start,
            span.end,
            span.end.saturating_sub(span.start)
        ),
        span: span.clone(),
        collapsible,
    });
    l.regions.push(Region { span, kind, label });
}
fn range(start: u64, size: u64, limit: u64) -> Option<Range<u64>> {
    let end = start.checked_add(size)?;
    (end <= limit).then_some(start..end)
}
fn read64(b: &[u8], off: u64) -> Option<u64> {
    let s = usize::try_from(off).ok()?;
    Some(u64::from_le_bytes(
        b.get(s..s.checked_add(8)?)?.try_into().ok()?,
    ))
}
fn read32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn message(f: &PbField) -> Option<Vec<PbField>> {
    let raw = f.value.as_bytes()?;
    decode_message_span_at(raw, f.value.payload_span.start, 0).map(|(fields, _)| fields)
}

fn packed(f: &PbField) -> Option<Vec<u64>> {
    if let Some(v) = f.value.as_varint() {
        return Some(vec![v]);
    }
    let mut b = f.value.as_bytes()?;
    let mut values = Vec::new();
    while !b.is_empty() {
        let mut value = 0;
        let mut consumed = 0;
        for shift in (0..70).step_by(7) {
            let x = *b.get(consumed)?;
            consumed += 1;
            if shift == 63 && x > 1 {
                return None;
            }
            value |= ((x & 127) as u64) << shift;
            if x < 128 {
                break;
            }
            if shift == 63 {
                return None;
            }
        }
        b = &b[consumed..];
        values.push(value);
        if values.len() > 1_000_000 {
            return None;
        }
    }
    Some(values)
}
fn repeated_values(fields: &[PbField], n: u32) -> Option<Vec<(u64, Range<u64>)>> {
    let mut result = Vec::new();
    for f in fields.iter().filter(|f| f.number == n) {
        if let Some(value) = f.value.as_varint() {
            result.push((value, f.value.span.clone()));
        } else {
            let values = packed(f)?;
            let raw = f.value.as_bytes()?;
            let mut offset = 0;
            for value in values {
                let start = offset;
                loop {
                    let byte = *raw.get(offset)?;
                    offset += 1;
                    if byte < 128 {
                        break;
                    }
                }
                result.push((
                    value,
                    f.value.payload_span.start + start as u64
                        ..f.value.payload_span.start + offset as u64,
                ));
            }
        }
        if result.len() > 1_000_000 {
            return None;
        }
    }
    Some(result)
}

/// Inspect the Lance v2 container; encoding semantics are selected by explicit type URL.
pub fn parse(bytes: &[u8]) -> LanceLayout {
    let mut l = LanceLayout::default();
    let len = bytes.len() as u64;
    if len < FOOTER_LEN {
        l.diagnostic("file too small to be Lance v2 (< 40 bytes)");
        return l;
    }
    if !bytes.ends_with(MAGIC) {
        l.diagnostic("not Lance: bad trailing magic");
        return l;
    }
    l.is_lance = true;
    if len > MAX_FILE_LEN {
        l.diagnostic("file exceeds 256 MiB inspection limit");
        return l;
    }
    let footer = len - FOOTER_LEN;
    let fs = footer as usize;
    let a = read64(bytes, footer).unwrap();
    let b = read64(bytes, footer + 8).unwrap();
    let c = read64(bytes, footer + 16).unwrap();
    let ng = read32(bytes, fs + 24);
    let nc = read32(bytes, fs + 28);
    if u64::from(nc) + u64::from(ng) > 50_000 {
        l.diagnostic("offset table entry inspection limit exceeded (50000)");
        return l;
    }
    let major = u16::from_le_bytes(bytes[fs + 32..fs + 34].try_into().unwrap());
    let minor = u16::from_le_bytes(bytes[fs + 34..fs + 36].try_into().unwrap());
    node(
        &mut l,
        0,
        format!("Footer (v{major}.{minor}: {nc} cols, {ng} global bufs)"),
        footer..len,
        RegionKind::Footer,
        true,
    );
    for (name, off, size, value) in [
        ("col_meta_0_offset (A)", 0, 8, a),
        ("cmo_table_offset (B)", 8, 8, b),
        ("gbo_table_offset (C)", 16, 8, c),
        ("num_global_bufs", 24, 4, ng as u64),
        ("num_columns", 28, 4, nc as u64),
        ("major_version", 32, 2, major as u64),
        ("minor_version", 34, 2, minor as u64),
    ] {
        node(
            &mut l,
            1,
            format!("{name} = {value}"),
            footer + off..footer + off + size,
            RegionKind::Footer,
            false,
        );
    }
    node(
        &mut l,
        1,
        "LANC (magic)",
        len - 4..len,
        RegionKind::Magic,
        false,
    );
    if !(a <= b && b <= c && c <= footer) {
        l.diagnostic("invalid footer ordering: A <= B <= C <= footer required");
        sort_roots(&mut l.tree);
        return l;
    }
    let Some(cmo) = range(b, u64::from(nc) * 16, c) else {
        l.diagnostic("column metadata offset table exceeds bounds");
        sort_roots(&mut l.tree);
        return l;
    };
    let Some(gbo) = range(c, u64::from(ng) * 16, footer) else {
        l.diagnostic("global buffer offset table exceeds bounds");
        sort_roots(&mut l.tree);
        return l;
    };
    let mut cols = Vec::new();
    let mut globals = Vec::new();
    for (label, table, count, kind, entries) in [
        (
            "Column Metadata Offset Table",
            cmo,
            nc,
            RegionKind::ColumnMetadataOffset,
            &mut cols,
        ),
        (
            "Global Buffers Offset Table",
            gbo,
            ng,
            RegionKind::GlobalBufferOffset,
            &mut globals,
        ),
    ] {
        if table.is_empty() {
            continue;
        }
        node(&mut l, 0, label, table.clone(), kind, true);
        for i in 0..count {
            let off = table.start + u64::from(i) * 16;
            let pos = read64(bytes, off).unwrap();
            let size = read64(bytes, off + 8).unwrap();
            node(
                &mut l,
                1,
                format!("[{i}] position={pos}, size={size}"),
                off..off + 16,
                kind,
                true,
            );
            node(
                &mut l,
                2,
                format!("position = {pos}"),
                off..off + 8,
                kind,
                false,
            );
            node(
                &mut l,
                2,
                format!("size = {size}"),
                off + 8..off + 16,
                kind,
                false,
            );
            if let Some(span) = range(pos, size, b) {
                entries.push((i, span.clone()));
                l.references.push(crate::binary::parquet::Reference {
                    source: off..off + 16,
                    target: span,
                });
            } else {
                l.diagnostic(format!("invalid {label} entry[{i}] range"));
            }
        }
    }
    if cols.first().is_some_and(|(_, r)| r.start != a) {
        l.diagnostic("A does not match column metadata 0 position");
    }
    let mut physical = Vec::new();
    for (_, r) in cols.iter().chain(globals.iter()) {
        if !r.is_empty() {
            physical.push(r.clone());
        }
    }
    physical.sort_by_key(|r| r.start);
    for pair in physical.windows(2) {
        if pair[0].end > pair[1].start {
            l.diagnostic("overlapping metadata/global buffers");
        }
    }
    let data_end = cols.iter().map(|(_, r)| r.start).min().unwrap_or(a);
    let mut buffers = Vec::new();
    for (i, span) in &globals {
        if span.is_empty() {
            continue;
        }
        let fields = decode_message_span_at(
            &bytes[span.start as usize..span.end as usize],
            span.start,
            0,
        );
        if *i == 0 {
            node(
                &mut l,
                0,
                "FileDescriptor (global_buf[0])",
                span.clone(),
                RegionKind::GlobalBuffer,
                true,
            );
            match fields {
                Some((fields, _)) => render_fields(
                    &mut l,
                    &fields,
                    "lance.file.FileDescriptor",
                    1,
                    RegionKind::GlobalBufferField,
                    0,
                ),
                None => l.diagnostic("invalid FileDescriptor protobuf"),
            }
        } else {
            node(
                &mut l,
                0,
                format!("global_buffers[{i}] (raw bytes; type not declared)"),
                span.clone(),
                RegionKind::GlobalBuffer,
                false,
            );
        }
    }
    for (i, span) in &cols {
        node(
            &mut l,
            0,
            format!("ColumnMetadata[{i}]"),
            span.clone(),
            RegionKind::ColumnMetadata,
            true,
        );
        if let Some((fields, _)) = decode_message_span_at(
            &bytes[span.start as usize..span.end as usize],
            span.start,
            0,
        ) {
            render_fields(
                &mut l,
                &fields,
                "lance.file.v2.ColumnMetadata",
                1,
                RegionKind::ColumnMetadataField,
                0,
            );
            collect_buffers(&mut l, &fields, *i, None, data_end, &mut buffers);
            for (page, f) in fields.iter().filter(|f| f.number == 2).enumerate() {
                if l.references.len() >= 200_000 {
                    l.diagnostic("buffer reference inspection limit reached");
                    break;
                }
                if let Some(fields) = message(f) {
                    collect_buffers(&mut l, &fields, *i, Some(page), data_end, &mut buffers);
                } else {
                    l.diagnostic("invalid pages message");
                }
            }
        } else {
            l.diagnostic(format!("invalid ColumnMetadata[{i}] protobuf"));
        }
    }
    let mut globals_ranges: Vec<_> = globals
        .iter()
        .filter(|(_, r)| !r.is_empty())
        .map(|(_, r)| r.clone())
        .collect();
    globals_ranges.sort_by_key(|r| r.start);
    let mut intervals = Vec::new();
    let mut start = 0;
    for r in &globals_ranges {
        if start < r.start {
            intervals.push(start..r.start.min(data_end));
        }
        start = start.max(r.end);
    }
    if start < data_end {
        intervals.push(start..data_end);
    }
    buffers.sort_by_key(|(_, r)| r.start);
    for interval in intervals.into_iter().filter(|r| !r.is_empty()) {
        node(
            &mut l,
            0,
            "Data Pages (buffers and alignment gaps)",
            interval.clone(),
            RegionKind::DataPages,
            true,
        );
        let mut cursor = interval.start;
        for (label, span) in buffers
            .iter()
            .filter(|(_, r)| r.start >= interval.start && r.end <= interval.end)
        {
            if span.start > cursor {
                node(
                    &mut l,
                    1,
                    "uninterpreted bytes (possible alignment gap)",
                    cursor..span.start,
                    RegionKind::Unknown,
                    false,
                );
            }
            cursor = cursor.max(span.end);
            node(
                &mut l,
                1,
                label,
                span.clone(),
                RegionKind::DataBuffer,
                false,
            );
        }
        if cursor < interval.end {
            node(
                &mut l,
                1,
                "uninterpreted bytes (possible alignment gap)",
                cursor..interval.end,
                RegionKind::Unknown,
                false,
            );
        }
    }
    for (_, span) in &buffers {
        if globals_ranges
            .iter()
            .any(|r| r.start < span.end && span.start < r.end)
        {
            l.diagnostic("data buffer overlaps global buffer");
        }
    }
    sort_roots(&mut l.tree);
    l.regions.sort_by_key(|r| (r.span.start, r.span.end));
    l
}
fn collect_buffers(
    l: &mut LanceLayout,
    fields: &[PbField],
    column: u32,
    page: Option<usize>,
    limit: u64,
    buffers: &mut Vec<(String, Range<u64>)>,
) {
    let (on, sn) = if page.is_some() { (1, 2) } else { (3, 4) };
    let (Some(offsets), Some(sizes)) = (repeated_values(fields, on), repeated_values(fields, sn))
    else {
        l.diagnostic("invalid repeated buffer offsets/sizes");
        return;
    };
    if offsets.len() != sizes.len() {
        l.diagnostic("buffer_offsets and buffer_sizes have different lengths");
        return;
    }
    for (i, ((offset, offset_span), (size, size_span))) in offsets.iter().zip(&sizes).enumerate() {
        if l.references.len() >= 200_000 {
            l.diagnostic("buffer reference inspection limit reached (200000)");
            break;
        }
        if let Some(span) = range(*offset, *size, limit) {
            let scope = page.map_or_else(
                || format!("column[{column}]"),
                |p| format!("column[{column}].pages[{p}]"),
            );
            if !span.is_empty() {
                buffers.push((format!("Data Buffer ({scope}.buffers[{i}])"), span.clone()));
            }
            for source in [offset_span, size_span] {
                l.references.push(crate::binary::parquet::Reference {
                    source: source.clone(),
                    target: span.clone(),
                });
            }
        } else {
            l.diagnostic(format!("buffer[{i}] exceeds data region"));
        }
    }
}
fn sort_roots(tree: &mut Vec<MetaNode>) {
    let mut roots: Vec<Vec<MetaNode>> = Vec::new();
    for n in std::mem::take(tree) {
        if n.depth == 0 || roots.is_empty() {
            roots.push(Vec::new());
        }
        roots.last_mut().unwrap().push(n);
    }
    roots.sort_by_key(|r| r[0].span.start);
    let mut grouped: Vec<MetaNode> = Vec::new();
    let mut parent = None;
    for mut subtree in roots {
        if subtree[0].label.starts_with("ColumnMetadata[") {
            let index = *parent.get_or_insert_with(|| {
                let index = grouped.len();
                grouped.push(MetaNode {
                    depth: 0,
                    label: "Column Metadata Table".into(),
                    span: subtree[0].span.clone(),
                    collapsible: true,
                });
                index
            });
            grouped[index].span.end = grouped[index].span.end.max(subtree[0].span.end);
            for node in &mut subtree {
                node.depth += 1;
            }
        } else {
            parent = None;
        }
        grouped.extend(subtree);
    }
    *tree = grouped;
}
fn preview_bytes(b: &[u8]) -> String {
    let s = b
        .iter()
        .take(32)
        .map(|v| format!("{v:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "bytes[{}] {s}{}",
        b.len(),
        if b.len() > 32 {
            " … (full bytes in field details)"
        } else {
            ""
        }
    )
}
fn scalar_text(f: &PbField, ty: &str) -> String {
    match &f.value.data {
        PbData::Varint(v) => {
            if let Some(name) = super::lance_schema::enum_value(ty, *v) {
                return format!("{name} ({v})");
            }
            match ty {
                "bool" => format!("{}", *v != 0),
                "int32" => format!("{}", *v as i32),
                "int64" => format!("{}", *v as i64),
                "sint32" | "sint64" => format!("{}", ((v >> 1) as i64) ^ -((v & 1) as i64)),
                _ => v.to_string(),
            }
        }
        PbData::Fixed32(v) => match ty {
            "float" => f32::from_bits(*v).to_string(),
            "sfixed32" => (*v as i32).to_string(),
            _ => v.to_string(),
        },
        PbData::Fixed64(v) => match ty {
            "double" => f64::from_bits(*v).to_string(),
            "sfixed64" => (*v as i64).to_string(),
            _ => v.to_string(),
        },
        PbData::Bytes(b) if ty == "string" => match std::str::from_utf8(b) {
            Ok(s) => format!("{s:?}"),
            Err(_) => format!("invalid UTF-8: {}", preview_bytes(b)),
        },
        PbData::Bytes(b) => preview_bytes(b),
        PbData::String(s) => format!("{s:?}"),
        PbData::Message(_) => "legacy message".into(),
    }
}
fn render_fields(
    l: &mut LanceLayout,
    fields: &[PbField],
    ty: &str,
    depth: u8,
    kind: RegionKind,
    recursion: usize,
) {
    let amount = fields
        .iter()
        .map(|f| f.value.span.end.saturating_sub(f.value.span.start) as usize)
        .sum::<usize>();
    l.render_bytes = l.render_bytes.saturating_add(amount);
    if l.render_bytes > 64 * 1024 * 1024 {
        l.diagnostic("nested metadata byte inspection limit reached (64 MiB)");
        return;
    }
    if recursion >= 48 {
        l.diagnostic("encoding/schema recursion inspection limit reached");
        return;
    }
    if ty == "lance.encodings.Buffer" {
        let mut defaults = Vec::new();
        if !fields.iter().any(|f| f.number == 1) {
            defaults.push("buffer_index=0");
        }
        if !fields.iter().any(|f| f.number == 2) {
            defaults.push("buffer_type=page");
        }
        if !defaults.is_empty()
            && let Some(parent) = l
                .tree
                .iter_mut()
                .rev()
                .find(|n| n.depth == depth.saturating_sub(1))
        {
            parent.label.push_str(&format!(
                " · {} (implicit default; not stored)",
                defaults.join(", ")
            ));
        }
    }
    let mut counts = std::collections::HashMap::<u32, usize>::new();
    for f in fields {
        if l.tree.len() >= 200_000 {
            l.diagnostic("metadata node limit reached");
            return;
        }
        let Some((name, ft, repeated)) = super::lance_schema::field(ty, f.number) else {
            node(
                l,
                depth,
                format!(
                    "field_{} (wire {:?}) = {}",
                    f.number,
                    f.value.wire_type,
                    scalar_text(f, "unknown")
                ),
                f.value.span.clone(),
                kind,
                false,
            );
            continue;
        };
        let count = counts.entry(f.number).or_default();
        let label = if repeated {
            format!("{name}[{}]", *count)
        } else {
            name.to_string()
        };
        *count += 1;
        if ty == "lance.file.v2.DirectEncoding" && f.number == 1 {
            render_any(l, f, depth, kind, recursion + 1);
            continue;
        }
        if ft.starts_with("map<") {
            node(
                l,
                depth,
                format!("{label} ({ft})"),
                f.value.span.clone(),
                kind,
                true,
            );
            if let Some(inner) = message(f) {
                let types = ft
                    .trim_start_matches("map<")
                    .trim_end_matches('>')
                    .split(',')
                    .collect::<Vec<_>>();
                for member in &inner {
                    let (n, t) = if member.number == 1 {
                        ("key", types[0])
                    } else if member.number == 2 {
                        ("value", types[1])
                    } else {
                        ("unknown", "unknown")
                    };
                    node(
                        l,
                        depth + 1,
                        format!("{n} = {}", scalar_text(member, t)),
                        member.value.span.clone(),
                        kind,
                        false,
                    );
                }
            } else {
                l.diagnostic(format!("invalid {label} map entry"));
            }
        } else if super::lance_schema::is_message(ft) || ft == "google.protobuf.Empty" {
            node(
                l,
                depth,
                format!("{label} ({})", ft.rsplit('.').next().unwrap_or(ft)),
                f.value.span.clone(),
                kind,
                true,
            );
            if let Some(inner) = message(f) {
                render_fields(l, &inner, ft, depth + 1, kind, recursion + 1);
            } else {
                l.diagnostic(format!("invalid message {ft}"));
            }
        } else if repeated && ft != "string" && ft != "bytes" && f.value.as_bytes().is_some() {
            if let Some(values) = packed(f) {
                let items = values
                    .iter()
                    .take(16)
                    .map(|v| {
                        super::lance_schema::enum_value(ft, *v).map_or_else(
                            || match ft {
                                "int32" => (*v as i32).to_string(),
                                "int64" => (*v as i64).to_string(),
                                _ => v.to_string(),
                            },
                            str::to_owned,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                node(
                    l,
                    depth,
                    format!(
                        "{name} = [{items}{}] ({} values, packed {ft})",
                        if values.len() > 16 { ", …" } else { "" },
                        values.len()
                    ),
                    f.value.span.clone(),
                    kind,
                    true,
                );
                if let Some(elements) = repeated_values(std::slice::from_ref(f), f.number) {
                    for (i, (value, span)) in elements.into_iter().enumerate() {
                        if l.tree.len() >= 200_000 {
                            l.diagnostic("metadata node limit reached");
                            break;
                        }
                        let text = super::lance_schema::enum_value(ft, value).map_or_else(
                            || match ft {
                                "int32" => (value as i32).to_string(),
                                "int64" => (value as i64).to_string(),
                                "bool" => (value != 0).to_string(),
                                "sint32" | "sint64" => {
                                    (((value >> 1) as i64) ^ -((value & 1) as i64)).to_string()
                                }
                                _ => value.to_string(),
                            },
                            str::to_owned,
                        );
                        node(
                            l,
                            depth + 1,
                            format!("[{i}] ({ft}) = {text}"),
                            span,
                            kind,
                            false,
                        );
                    }
                }
            } else {
                l.diagnostic(format!("invalid packed {ft}"));
                node(
                    l,
                    depth,
                    format!("{label} = {}", scalar_text(f, "bytes")),
                    f.value.span.clone(),
                    kind,
                    false,
                );
            }
        } else {
            node(
                l,
                depth,
                format!("{label} ({ft}) = {}", scalar_text(f, ft)),
                f.value.span.clone(),
                kind,
                false,
            );
        }
    }
}
fn render_any(l: &mut LanceLayout, f: &PbField, depth: u8, kind: RegionKind, recursion: usize) {
    node(
        l,
        depth,
        "encoding (google.protobuf.Any)",
        f.value.span.clone(),
        kind,
        true,
    );
    let Some(fields) = message(f) else {
        l.diagnostic("invalid DirectEncoding.encoding Any");
        return;
    };
    let url = fields
        .iter()
        .find(|f| f.number == 1)
        .and_then(|f| f.value.as_string());
    for field in &fields {
        if field.number == 1 {
            node(
                l,
                depth + 1,
                format!("type_url (string) = {}", scalar_text(field, "string")),
                field.value.span.clone(),
                kind,
                false,
            );
        } else if field.number == 2 {
            let ty = url.and_then(|s| s.rsplit('/').next()).unwrap_or("");
            if super::lance_schema::is_message(ty) && ty.starts_with("lance.encodings") {
                node(
                    l,
                    depth + 1,
                    format!("value ({ty})"),
                    field.value.span.clone(),
                    kind,
                    true,
                );
                if let Some(inner) = message(field) {
                    render_fields(l, &inner, ty, depth + 2, kind, recursion + 1);
                } else {
                    l.diagnostic(format!("invalid encoding payload {ty}"));
                }
            } else {
                node(
                    l,
                    depth + 1,
                    format!(
                        "value (unknown encoding {ty:?}) = {}",
                        scalar_text(field, "bytes")
                    ),
                    field.value.span.clone(),
                    kind,
                    false,
                );
            }
        } else {
            node(
                l,
                depth + 1,
                format!("field_{} = {}", field.number, scalar_text(field, "unknown")),
                field.value.span.clone(),
                kind,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn vi(mut n: u64) -> Vec<u8> {
        let mut b = Vec::new();
        while n >= 128 {
            b.push(n as u8 | 128);
            n >>= 7;
        }
        b.push(n as u8);
        b
    }
    fn binary(id: u64, b: &[u8]) -> Vec<u8> {
        let mut out = vi(id * 8 + 2);
        out.extend(vi(b.len() as u64));
        out.extend(b);
        out
    }
    fn value(id: u64, n: u64) -> Vec<u8> {
        let mut out = vi(id * 8);
        out.extend(vi(n));
        out
    }
    #[test]
    fn example_fixtures_have_real_names_and_buffers() {
        let inode = parse(include_bytes!("../../tests/fixtures/inode.lance"));
        assert!(inode.error.is_none(), "{:?}", inode.error);
        assert!(inode.tree.iter().any(|n| n.label.contains("\"mtime\"")));
        let dentry = parse(include_bytes!("../../tests/fixtures/dentry.lance"));
        assert!(dentry.error.is_none(), "{:?}", dentry.error);
        for expected in [0..8, 64..72, 128..132, 192..200] {
            assert!(
                dentry
                    .regions
                    .iter()
                    .any(|r| r.kind == RegionKind::DataBuffer && r.span == expected)
            );
        }
        assert!(
            dentry
                .tree
                .iter()
                .any(|n| n.label.contains("minor_version = 3"))
        );
    }
    #[test]
    fn repeated_fields_accumulate_packed_segments_and_unpacked_values() {
        let b = [binary(1, &[0, 8]), value(1, 16), binary(1, &[24, 32])].concat();
        let (fields, _) = decode_message_span_at(&b, 100, 0).unwrap();
        let out = repeated_values(&fields, 1).unwrap();
        assert_eq!(
            out.iter().map(|(v, _)| *v).collect::<Vec<_>>(),
            [0, 8, 16, 24, 32]
        );
        assert_eq!(out[0].1, 102..103);
        assert_eq!(out[1].1, 103..104);
        assert_eq!(out[2].1, 104..106);
    }
    #[test]
    fn repeated_buffers_are_not_silently_truncated_at_1024() {
        let raw = vec![0; 1100];
        let b = binary(1, &raw);
        let (fields, _) = decode_message_span_at(&b, 0, 0).unwrap();
        assert_eq!(repeated_values(&fields, 1).unwrap().len(), 1100);
    }
    #[test]
    fn type_url_selects_newer_encoding_schema() {
        let payload = binary(1, &[]);
        let any = [
            binary(1, b"type.googleapis.com/lance.encodings21.PageLayout"),
            binary(2, &payload),
        ]
        .concat();
        let b = binary(1, &any);
        let (fields, _) = decode_message_span_at(&b, 100, 0).unwrap();
        let mut l = LanceLayout::default();
        render_fields(
            &mut l,
            &fields,
            "lance.file.v2.DirectEncoding",
            0,
            RegionKind::ColumnMetadataField,
            0,
        );
        let text = l
            .tree
            .iter()
            .map(|n| n.label.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("mini_block_layout"), "{text}");
        assert!(!text.contains("Flat"));
    }
    #[test]
    fn corrupt_offsets_report_error_instead_of_panicking() {
        let bytes = include_bytes!("../../tests/fixtures/dentry.lance");
        for off in [755, 763, 771, 691, 699, 739, 747] {
            let mut b = bytes.to_vec();
            b[off..off + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            assert!(parse(&b).error.is_some());
        }
    }
    #[test]
    fn hostile_table_counts_are_bounded_before_allocation() {
        let mut b = include_bytes!("../../tests/fixtures/dentry.lance").to_vec();
        let offset = b.len() - 40 + 28;
        b[offset..offset + 4].copy_from_slice(&50_001u32.to_le_bytes());
        let l = parse(&b);
        assert!(
            l.error
                .as_deref()
                .unwrap()
                .contains("entry inspection limit")
        );
        assert!(l.tree.is_empty());
    }
    #[test]
    fn data_buffers_after_a_global_buffer_keep_their_physical_location() {
        let descriptor = value(2, 1);
        let mut b = vec![0; 128];
        b[..descriptor.len()].copy_from_slice(&descriptor);
        let page = [binary(1, &[64]), binary(2, &[8]), value(3, 1)].concat();
        let column = binary(2, &page);
        let a = b.len() as u64;
        b.extend(&column);
        let cmo = b.len() as u64;
        b.extend(a.to_le_bytes());
        b.extend((column.len() as u64).to_le_bytes());
        let gbo = b.len() as u64;
        b.extend(0u64.to_le_bytes());
        b.extend((descriptor.len() as u64).to_le_bytes());
        b.extend(a.to_le_bytes());
        b.extend(cmo.to_le_bytes());
        b.extend(gbo.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend(0u16.to_le_bytes());
        b.extend(3u16.to_le_bytes());
        b.extend(b"LANC");
        let l = parse(&b);
        assert!(l.error.is_none(), "{:?}", l.error);
        assert!(
            l.regions
                .iter()
                .any(|r| r.kind == RegionKind::DataBuffer && r.span == (64..72))
        );
    }
    #[test]
    fn column_metadata_group_preserves_regions() {
        let layout = parse(include_bytes!("../../tests/fixtures/dentry.lance"));
        let parent = layout
            .tree
            .iter()
            .find(|n| n.label == "Column Metadata Table")
            .unwrap();
        assert_eq!(parent.span, 356..691);
        let children: Vec<_> = layout
            .tree
            .iter()
            .filter(|n| n.label.starts_with("ColumnMetadata["))
            .collect();
        assert_eq!(children.len(), 3);
        assert!(children.iter().all(|n| n.depth == 1));
        assert!(
            !layout
                .regions
                .iter()
                .any(|r| r.label == "Column Metadata Table")
        );
    }
}
