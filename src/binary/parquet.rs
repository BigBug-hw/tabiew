//! Physical Parquet layout and complete Thrift metadata inspection.
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
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone)]
pub struct Reference {
    pub source: Range<u64>,
    pub target: Range<u64>,
}

impl ParquetLayout {
    fn err(msg: impl Into<String>) -> Self {
        Self {
            error: Some(msg.into()),
            ..Self::default()
        }
    }
    fn diagnostic(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.error = Some(match self.error.take() {
            Some(old) => format!("{old}; {msg}"),
            None => msg,
        });
    }
}
fn node(
    layout: &mut ParquetLayout,
    depth: u8,
    label: String,
    span: Range<u64>,
    kind: RegionKind,
    collapsible: bool,
) {
    layout.tree.push(MetaNode {
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
    layout.regions.push(Region { span, kind, label });
}
fn i64_field(fields: &[WireField], id: i16) -> Option<i64> {
    fields.iter().find(|f| f.id == id)?.value.as_i64()
}
fn positive(fields: &[WireField], id: i16) -> Option<u64> {
    u64::try_from(i64_field(fields, id)?).ok()
}
fn list(fields: &[WireField], id: i16) -> &[WireValue] {
    fields
        .iter()
        .find(|f| f.id == id)
        .and_then(|f| f.value.as_list())
        .unwrap_or(&[])
}
fn structure(fields: &[WireField], id: i16) -> Option<&[WireField]> {
    fields.iter().find(|f| f.id == id)?.value.as_struct()
}
fn binary(fields: &[WireField], id: i16) -> Option<&[u8]> {
    fields.iter().find(|f| f.id == id)?.value.as_binary()
}
fn chunk_span(cm: &[WireField], limit: u64) -> Option<Range<u64>> {
    let data = positive(cm, 9)?;
    let start = positive(cm, 11)
        .filter(|v| *v > 0)
        .map_or(data, |dict| dict.min(data));
    let size = positive(cm, 7)?;
    let end = start.checked_add(size)?;
    (start >= 4 && end <= limit && end > start).then_some(start..end)
}

/// Inspect a complete file. All nested decoders are bounded to their declared region.
pub fn parse(bytes: &[u8]) -> ParquetLayout {
    let len = bytes.len() as u64;
    if len < 12 {
        return ParquetLayout::err("file too small to be Parquet");
    }
    if !bytes.starts_with(MAGIC) || !bytes.ends_with(MAGIC) {
        return ParquetLayout::err("not a Parquet file: bad magic");
    }
    let mut l = ParquetLayout {
        is_parquet: true,
        ..Default::default()
    };
    node(
        &mut l,
        0,
        "PAR1 (header magic)".into(),
        0..4,
        RegionKind::Magic,
        false,
    );
    let footer_off = len - 8;
    let footer_len =
        u32::from_le_bytes(bytes[bytes.len() - 8..bytes.len() - 4].try_into().unwrap()) as u64;
    let Some(meta_start) = footer_off.checked_sub(footer_len).filter(|v| *v >= 4) else {
        l.diagnostic("footer length overlaps header or exceeds file");
        return l;
    };
    let Some((fields, decoded)) = decode_struct_span_at(
        &bytes[meta_start as usize..footer_off as usize],
        meta_start,
        0,
    ) else {
        l.diagnostic("invalid FileMetaData Thrift encoding");
        return l;
    };
    if decoded.end != footer_off {
        l.diagnostic("FileMetaData does not consume declared footer length");
    }
    data_layout(&mut l, &fields, bytes, meta_start);
    // Extra structures have explicit references and can occur between chunks or after data.
    auxiliary_layout(&mut l, &fields, bytes, meta_start);
    // Sort complete physical subtrees, never sort individual children across their parents.
    sort_roots(&mut l.tree);
    node(
        &mut l,
        0,
        "FileMetaData".into(),
        meta_start..footer_off,
        RegionKind::Metadata,
        true,
    );
    render_fields(&mut l, &fields, "FileMetaData", 1);
    node(
        &mut l,
        0,
        format!("footer length = {footer_len}"),
        footer_off..len - 4,
        RegionKind::FooterLength,
        false,
    );
    node(
        &mut l,
        0,
        "PAR1 (footer magic)".into(),
        len - 4..len,
        RegionKind::Magic,
        false,
    );
    add_gaps(&mut l, meta_start);
    sort_roots(&mut l.tree);
    l.regions.sort_by_key(|r| (r.span.start, r.span.end));
    l
}
fn sort_roots(tree: &mut Vec<MetaNode>) {
    let mut roots: Vec<Vec<MetaNode>> = Vec::new();
    for n in std::mem::take(tree) {
        if n.depth == 0 || roots.is_empty() {
            roots.push(Vec::new());
        }
        roots.last_mut().unwrap().push(n);
    }
    roots.sort_by_key(|g| g[0].span.start);
    *tree = roots.into_iter().flatten().collect();
}
fn add_gaps(l: &mut ParquetLayout, end: u64) {
    let mut ranges: Vec<_> = l
        .regions
        .iter()
        .filter(|r| matches!(r.kind, RegionKind::ColumnChunk | RegionKind::Unknown))
        .map(|r| r.span.clone())
        .collect();
    ranges.sort_by_key(|r| r.start);
    let mut cursor = 4;
    for r in ranges {
        if r.start > cursor {
            node(
                l,
                0,
                "uninterpreted bytes".into(),
                cursor..r.start,
                RegionKind::Unknown,
                false,
            );
        }
        cursor = cursor.max(r.end);
    }
    if cursor < end {
        node(
            l,
            0,
            "uninterpreted bytes".into(),
            cursor..end,
            RegionKind::Unknown,
            false,
        );
    }
}
fn data_layout(l: &mut ParquetLayout, fields: &[WireField], bytes: &[u8], limit: u64) {
    let mut occupied: Vec<Range<u64>> = Vec::new();
    for (rg, item) in list(fields, 4).iter().enumerate() {
        let Some(rgf) = item.as_struct() else {
            continue;
        };
        let columns = list(rgf, 1);
        let mut chunks = Vec::new();
        for (ci, col) in columns.iter().enumerate() {
            let Some(cf) = col.as_struct() else { continue };
            if binary(cf, 1).is_some_and(|p| !p.is_empty()) {
                continue;
            }
            let Some(cm) = structure(cf, 3) else { continue };
            let Some(span) = chunk_span(cm, limit) else {
                l.diagnostic(format!(
                    "invalid ColumnChunk range: row_group={rg} column={ci}"
                ));
                continue;
            };
            if occupied
                .iter()
                .any(|r| r.start < span.end && span.start < r.end)
            {
                l.diagnostic(format!(
                    "overlapping ColumnChunk: row_group={rg} column={ci}"
                ));
                continue;
            }
            occupied.push(span.clone());
            chunks.push((ci, cm, span));
        }
        if chunks.is_empty() {
            continue;
        }
        chunks.sort_by_key(|(_, _, s)| s.start);
        let rg_span = chunks.first().unwrap().2.start..chunks.last().unwrap().2.end;
        node(
            l,
            0,
            format!(
                "RowGroup #{rg} (data) num_rows={} total_byte_size={}",
                i64_field(rgf, 3).unwrap_or(0),
                i64_field(rgf, 2).unwrap_or(0)
            ),
            rg_span,
            RegionKind::RowGroup,
            true,
        );
        for (ci, cm, span) in chunks {
            let path = list(cm, 3)
                .iter()
                .filter_map(|v| v.as_binary())
                .map(|b| String::from_utf8_lossy(b))
                .collect::<Vec<_>>()
                .join(".");
            let codec = positive(cm, 4)
                .and_then(|v| codec_name(v as i64))
                .unwrap_or("unknown");
            node(
                l,
                1,
                format!("ColumnChunk #{ci} path_in_schema={path} codec={codec}"),
                span.clone(),
                RegionKind::ColumnChunk,
                true,
            );
            // A physical node links back to its metadata, not a fake child located in the footer.
            l.references.push(Reference {
                source: span.clone(),
                target: cm.first().map_or(item.span.clone(), |f| {
                    f.span.start..cm.last().unwrap().span.end
                }),
            });
            for f in cm.iter().filter(|f| matches!(f.id, 9..=11)) {
                if let Some(offset) = f
                    .value
                    .as_i64()
                    .and_then(|n| u64::try_from(n).ok())
                    .filter(|n| *n >= span.start && *n < span.end)
                {
                    l.references.push(Reference {
                        source: f.span.clone(),
                        target: offset..span.end,
                    });
                }
            }
            pages(l, cm, bytes, span, codec);
        }
    }
}
fn pages(l: &mut ParquetLayout, cm: &[WireField], bytes: &[u8], span: Range<u64>, codec: &str) {
    let mut cursor = span.start;
    let mut count = 0;
    while cursor < span.end {
        if count >= 10_000 {
            l.diagnostic("page count limit reached");
            break;
        }
        let Some((pf, hs)) =
            decode_struct_span_at(&bytes[cursor as usize..span.end as usize], cursor, 0)
        else {
            l.diagnostic(format!("invalid PageHeader @0x{cursor:x}"));
            break;
        };
        let Some(size) = positive(&pf, 3) else {
            l.diagnostic("missing or negative compressed_page_size");
            break;
        };
        let Some(end) = hs.end.checked_add(size).filter(|end| *end <= span.end) else {
            l.diagnostic("PageData exceeds ColumnChunk");
            break;
        };
        let Some(pt) = i64_field(&pf, 1) else {
            l.diagnostic("missing PageHeader.type");
            break;
        };
        let name = page_type_name(pt).unwrap_or("unknown");
        node(
            l,
            2,
            format!(
                "Page #{count} {name} codec={codec} uncompressed_page_size={} compressed_page_size={size}",
                i64_field(&pf, 2).unwrap_or(0)
            ),
            cursor..end,
            RegionKind::PageData,
            true,
        );
        node(
            l,
            3,
            format!("PageHeader {name}"),
            hs.clone(),
            RegionKind::PageHeader,
            true,
        );
        render_fields(l, &pf, "PageHeader", 4);
        if size > 0 {
            node(
                l,
                3,
                format!("PageData {name}"),
                hs.end..end,
                RegionKind::PageData,
                pt == 3,
            );
        }
        if pt == 3
            && let Some(v2) = structure(&pf, 8)
        {
            let rl = positive(v2, 6);
            let dl = positive(v2, 5);
            if let (Some(rl), Some(dl)) = (rl, dl) {
                if let Some(level_end) = hs
                    .end
                    .checked_add(rl)
                    .and_then(|n| n.checked_add(dl))
                    .filter(|n| *n <= end)
                {
                    if rl > 0 {
                        node(
                            l,
                            4,
                            "repetition_levels (RLE)".into(),
                            hs.end..hs.end + rl,
                            RegionKind::PageData,
                            false,
                        );
                    }
                    if dl > 0 {
                        node(
                            l,
                            4,
                            "definition_levels (RLE)".into(),
                            hs.end + rl..level_end,
                            RegionKind::PageData,
                            false,
                        );
                    }
                    if level_end < end {
                        node(
                            l,
                            4,
                            "encoded values".into(),
                            level_end..end,
                            RegionKind::PageData,
                            false,
                        );
                    }
                } else {
                    l.diagnostic("V2 level lengths exceed PageData");
                }
            }
        }
        cursor = end;
        count += 1;
    }
    let _ = cm;
}

fn render_fields(l: &mut ParquetLayout, fields: &[WireField], ty: &str, depth: u8) {
    if depth > 64 {
        return;
    }
    for f in fields {
        let (name, ft) = super::parquet_schema::field(ty, f.id)
            .map(|(a, b)| (a.to_owned(), b))
            .unwrap_or_else(|| {
                (
                    format!("field_{}", f.id),
                    match &f.value.kind {
                        WireKind::Bool(_) => "wire:bool",
                        WireKind::Byte(_) => "wire:byte",
                        WireKind::I16(_) => "wire:i16",
                        WireKind::I32(_) => "wire:i32",
                        WireKind::I64(_) => "wire:i64",
                        WireKind::Double(_) => "wire:double",
                        WireKind::Binary(_) => "wire:binary",
                        WireKind::List(_) => "wire:list",
                        WireKind::Map(_) => "wire:map",
                        WireKind::Struct(_) => "wire:struct",
                    },
                )
            });
        render_value(l, &f.value, &name, ft, depth, f.span.clone());
    }
    if ty == "DataPageHeaderV2" && !fields.iter().any(|f| f.id == 7) {
        // Defaults are annotations on the structure rather than fake zero-length fields.
        if let Some(n) = l
            .tree
            .iter_mut()
            .rev()
            .find(|n| n.depth == depth.saturating_sub(1))
        {
            n.label
                .push_str(" · is_compressed=true (default, not stored)");
        }
    }
}
fn render_value(
    l: &mut ParquetLayout,
    v: &WireValue,
    name: &str,
    ty: &str,
    depth: u8,
    span: Range<u64>,
) {
    let enum_name = v
        .as_i64()
        .and_then(|n| super::parquet_schema::enumeration(ty, n));
    let summary = match &v.kind {
        WireKind::Bool(b) => b.to_string(),
        WireKind::Byte(n) => n.to_string(),
        WireKind::I16(n) | WireKind::I32(n) | WireKind::I64(n) => {
            enum_name.map_or_else(|| n.to_string(), |e| format!("{e} ({n})"))
        }
        WireKind::Double(n) => n.to_string(),
        WireKind::Binary(b) if ty == "string" => format!("{:?}", String::from_utf8_lossy(b)),
        WireKind::Binary(b) => format!(
            "{} bytes: {}{}",
            b.len(),
            b.iter()
                .take(128)
                .map(|n| format!("{n:02x}"))
                .collect::<Vec<_>>()
                .join(" "),
            if b.len() > 128 {
                " … (see raw bytes)"
            } else {
                ""
            }
        ),
        WireKind::List(v) => format!("{} items", v.len()),
        WireKind::Map(v) => format!("{} entries", v.len()),
        WireKind::Struct(_) => String::new(),
    };
    let container = matches!(
        v.kind,
        WireKind::Struct(_) | WireKind::List(_) | WireKind::Map(_)
    );
    let label = if let WireKind::List(items) = &v.kind {
        format!("{name}[{}] ({ty})", items.len())
    } else if summary.is_empty() {
        format!("{name} ({ty})")
    } else {
        format!("{name} ({ty}) = {summary}")
    };
    node(l, depth, label, span, RegionKind::MetaField, container);
    match &v.kind {
        WireKind::Struct(fields) => render_fields(l, fields, ty, depth + 1),
        WireKind::List(items) => {
            let inner = ty
                .strip_prefix("list<")
                .or_else(|| ty.strip_prefix("set<"))
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("unknown");
            for (i, v) in items.iter().enumerate() {
                render_value(l, v, &format!("[{i}]"), inner, depth + 1, v.span.clone());
            }
        }
        WireKind::Map(items) => {
            let (kt, vt) = ty
                .strip_prefix("map<")
                .and_then(|s| s.strip_suffix('>'))
                .and_then(|s| s.split_once(','))
                .unwrap_or(("unknown", "unknown"));
            for (i, (k, v)) in items.iter().enumerate() {
                render_value(l, k, &format!("[{i}].key"), kt, depth + 1, k.span.clone());
                render_value(l, v, &format!("[{i}].value"), vt, depth + 1, v.span.clone());
            }
        }
        _ => {}
    }
}
fn auxiliary_layout(l: &mut ParquetLayout, fields: &[WireField], bytes: &[u8], limit: u64) {
    for rg in list(fields, 4) {
        let Some(rgf) = rg.as_struct() else { continue };
        for col in list(rgf, 1) {
            let Some(cf) = col.as_struct() else { continue };
            if binary(cf, 1).is_some_and(|p| !p.is_empty()) {
                continue;
            }
            for (offset_id, length_id, ty) in [(4, 5, "OffsetIndex"), (6, 7, "ColumnIndex")] {
                if let (Some(start), Some(size)) =
                    (positive(cf, offset_id), positive(cf, length_id))
                {
                    auxiliary(
                        l,
                        bytes,
                        start,
                        size,
                        limit,
                        ty,
                        cf.iter()
                            .find(|f| f.id == offset_id)
                            .map(|f| f.span.clone()),
                    );
                }
            }
            if let Some(cm) = structure(cf, 3)
                && let Some(start) = positive(cm, 14)
            {
                let length = positive(cm, 15).unwrap_or_else(|| limit.saturating_sub(start));
                let source = cm.iter().find(|f| f.id == 14).map(|f| f.span.clone());
                auxiliary(l, bytes, start, length, limit, "BloomFilterHeader", source);
            }
        }
    }
}
fn auxiliary(
    l: &mut ParquetLayout,
    bytes: &[u8],
    start: u64,
    size: u64,
    limit: u64,
    ty: &str,
    source: Option<Range<u64>>,
) {
    let Some(end) = start
        .checked_add(size)
        .filter(|e| start >= 4 && *e <= limit && size > 0)
    else {
        l.diagnostic(format!("invalid {ty} range"));
        return;
    };
    let Some((fields, span)) =
        decode_struct_span_at(&bytes[start as usize..end as usize], start, 0)
    else {
        l.diagnostic(format!("invalid {ty} Thrift encoding"));
        return;
    };
    let actual_end = if ty == "BloomFilterHeader" {
        positive(&fields, 1)
            .and_then(|n| span.end.checked_add(n))
            .filter(|e| *e <= end)
    } else {
        Some(end)
    };
    let Some(actual_end) = actual_end else {
        l.diagnostic("Bloom filter bitset exceeds declared region");
        return;
    };
    node(
        l,
        0,
        ty.into(),
        start..actual_end,
        RegionKind::Unknown,
        true,
    );
    if let Some(source) = source {
        l.references.push(Reference {
            source,
            target: start..actual_end,
        });
    }
    render_fields(l, &fields, ty, 1);
    if ty == "BloomFilterHeader" && span.end < actual_end {
        node(
            l,
            1,
            "Bloom filter bitset".into(),
            span.end..actual_end,
            RegionKind::Unknown,
            false,
        );
    }
    if ty != "BloomFilterHeader" && span.end != end {
        l.diagnostic(format!("{ty} does not consume declared length"));
    }
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

#[cfg(test)]
mod structure_regressions {
    use super::*;
    #[test]
    fn v2_control_fields_and_nested_ranges() {
        // PageHeader: DATA_PAGE_V2, sizes=5, header_v2: five values, no nulls,
        // five rows, PLAIN, definition bytes=2, repetition bytes=1, false compression.
        let encoded = [
            0x15, 6, 0x15, 10, 0x15, 10, 0x5c, 0x15, 10, 0x15, 0, 0x15, 10, 0x15, 0, 0x15, 4, 0x15,
            2, 0x12, 0, 0,
        ];
        let (fields, span) = decode_struct_span_at(&encoded, 100, 0).unwrap();
        let mut layout = ParquetLayout::default();
        render_fields(&mut layout, &fields, "PageHeader", 0);
        for expected in [
            "definition_levels_byte_length (i32) = 2",
            "repetition_levels_byte_length (i32) = 1",
            "is_compressed (bool) = false",
        ] {
            assert!(
                layout.tree.iter().any(|n| n.label.contains(expected)),
                "{expected}"
            );
        }
        let parent = layout
            .tree
            .iter()
            .find(|n| n.label.starts_with("data_page_header_v2"))
            .unwrap();
        assert_eq!(parent.span, 106..121);
        assert_eq!(span, 100..122);
        assert!(
            layout
                .tree
                .iter()
                .filter(|n| n.depth == 1)
                .all(|n| n.span.start >= parent.span.start && n.span.end <= parent.span.end)
        );
    }
    #[test]
    fn missing_optional_dictionary_offset_is_not_invented() {
        let (fields, _) = decode_struct_span_at(&[0x96, 8, 0], 0, 0).unwrap();
        let mut layout = ParquetLayout::default();
        render_fields(&mut layout, &fields, "ColumnMetaData", 0);
        assert!(
            !layout
                .tree
                .iter()
                .any(|n| n.label.starts_with("dictionary_page_offset"))
        );
    }
    #[test]
    fn sample_dictionary_pages_belong_to_their_own_chunks() {
        let layout = parse(include_bytes!("../../tests/fixtures/sample-nested.parquet"));
        assert!(layout.error.is_none(), "{:?}", layout.error);
        let chunks: Vec<_> = layout
            .regions
            .iter()
            .filter(|r| r.kind == RegionKind::ColumnChunk)
            .collect();
        assert_eq!(
            chunks.iter().map(|r| r.span.start).collect::<Vec<_>>(),
            [4, 119, 208, 302, 421, 552, 651]
        );
        assert_eq!(chunks[0].span, 4..119);
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.starts_with("dictionary_page_offset (i64) = 4"))
        );
        assert!(
            layout
                .regions
                .iter()
                .any(|r| r.kind == RegionKind::PageHeader && r.span.start == 4)
        );
    }
}

#[cfg(test)]
mod auxiliary_regressions {
    use super::*;
    #[test]
    fn offset_index_locations_have_official_names_and_references() {
        let mut bytes = b"PAR1".to_vec();
        bytes.extend_from_slice(&[0x19, 0x1c, 0x16, 8, 0x15, 20, 0x16, 0, 0, 0]);
        let mut layout = ParquetLayout::default();
        auxiliary(&mut layout, &bytes, 4, 10, 14, "OffsetIndex", Some(20..22));
        assert!(layout.error.is_none(), "{:?}", layout.error);
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.starts_with("page_locations[1]"))
        );
        assert!(
            layout
                .tree
                .iter()
                .any(|n| n.label.starts_with("first_row_index (i64) = 0"))
        );
        assert_eq!(layout.references[0].target, 4..14);
    }
    #[test]
    fn bloom_header_and_bitset_have_distinct_physical_spans() {
        let mut bytes = b"PAR1".to_vec();
        bytes.extend_from_slice(&[
            0x15, 8, 0x1c, 0x1c, 0, 0, 0x1c, 0x1c, 0, 0, 0x1c, 0x1c, 0, 0, 0,
        ]);
        bytes.extend_from_slice(&[0; 4]);
        let mut layout = ParquetLayout::default();
        auxiliary(&mut layout, &bytes, 4, 19, 23, "BloomFilterHeader", None);
        assert!(layout.error.is_none(), "{:?}", layout.error);
        for field in ["numBytes", "algorithm", "hash", "compression"] {
            assert!(layout.tree.iter().any(|n| n.label.starts_with(field)));
        }
        let bitset = layout
            .tree
            .iter()
            .find(|n| n.label.starts_with("Bloom filter bitset"))
            .unwrap();
        assert_eq!(bitset.span, 19..23);
    }
    #[test]
    fn unknown_fields_preserve_id_wire_type_value_and_header_span() {
        let (fields, _) = decode_struct_span_at(&[0x15, 84, 0], 40, 0).unwrap();
        let mut layout = ParquetLayout::default();
        render_fields(&mut layout, &fields, "FutureStruct", 0);
        assert!(layout.tree[0].label.starts_with("field_1 (wire:i32) = 42"));
        assert_eq!(layout.tree[0].span, 40..42);
    }
}
