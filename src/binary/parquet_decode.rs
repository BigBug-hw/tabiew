//! On-demand Parquet page inspection. File offsets and decompressed offsets are distinct.
use super::thrift::{WireField, decode_struct_span_at};
use polars_parquet::parquet::{
    compression::{self, Compression, DecompressionContext},
    read::read_metadata,
    schema::{
        Repetition,
        types::{ParquetType, PhysicalType},
    },
};
use std::{io::Cursor, ops::Range};

pub const MAX_PAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 1_000_000;
#[derive(Debug, Clone)]
pub struct PageDetails {
    pub source_span: Range<u64>,
    pub decompressed: Vec<u8>,
    pub controls: Vec<String>,
    pub values: Vec<String>,
}
fn integer(f: &[WireField], id: i16) -> Option<i64> {
    f.iter().find(|f| f.id == id)?.value.as_i64()
}
fn nested(f: &[WireField], id: i16) -> Option<&[WireField]> {
    f.iter().find(|f| f.id == id)?.value.as_struct()
}
fn size(v: i64) -> Result<usize, String> {
    usize::try_from(v).map_err(|_| "negative size".into())
}
fn take<'a>(b: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
    if n > b.len() {
        return Err("truncated encoded data".into());
    }
    let (v, r) = b.split_at(n);
    *b = r;
    Ok(v)
}
fn varint(b: &mut &[u8]) -> Result<u64, String> {
    let mut v = 0;
    for s in (0..70).step_by(7) {
        let x = take(b, 1)?[0];
        if s == 63 && x > 1 {
            return Err("varint overflow".into());
        }
        v |= ((x & 127) as u64) << s;
        if x < 128 {
            return Ok(v);
        }
    }
    Err("varint overflow".into())
}
fn zigzag(b: &mut &[u8]) -> Result<i64, String> {
    let n = varint(b)?;
    Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
}
fn bits(b: &[u8], width: usize, count: usize) -> Result<Vec<u64>, String> {
    if width > 64 || count > MAX_ITEMS {
        return Err("bit width or item limit exceeded".into());
    }
    let needed = width
        .checked_mul(count)
        .ok_or("bit length overflow")?
        .div_ceil(8);
    if needed > b.len() {
        return Err("truncated packed values".into());
    }
    Ok((0..count)
        .map(|i| {
            let mut v = 0;
            for j in 0..width {
                let p = i * width + j;
                v |= (((b[p / 8] >> (p % 8)) & 1) as u64) << j;
            }
            v
        })
        .collect())
}
fn legacy_bitpacked(b: &[u8], width: usize, count: usize) -> Result<Vec<u64>, String> {
    if width > 64
        || count > MAX_ITEMS
        || width
            .checked_mul(count)
            .is_none_or(|n| n.div_ceil(8) > b.len())
    {
        return Err("truncated legacy BIT_PACKED levels".into());
    }
    Ok((0..count)
        .map(|i| {
            let mut v = 0;
            for j in 0..width {
                let p = i * width + j;
                v = (v << 1) | ((b[p / 8] >> (7 - p % 8)) & 1) as u64;
            }
            v
        })
        .collect())
}
fn hybrid(
    mut b: &[u8],
    width: usize,
    count: usize,
    controls: &mut Vec<String>,
) -> Result<Vec<u64>, String> {
    if width > 32 || count > MAX_ITEMS {
        return Err("RLE bit width or item limit exceeded".into());
    }
    let mut out = Vec::new();
    while out.len() < count {
        let header = varint(&mut b)?;
        let n = usize::try_from(header >> 1).map_err(|_| "run overflow")?;
        if n == 0 {
            return Err("empty RLE run".into());
        }
        if header & 1 == 0 {
            let raw = take(&mut b, width.div_ceil(8))?;
            let mut v = 0;
            for (i, x) in raw.iter().enumerate() {
                v |= (*x as u64) << (i * 8)
            }
            if controls.len() < 4096 {
                controls.push(format!("run_header={header}: RLE count={n}, value={v}"));
            }
            out.resize(out.len() + n.min(count - out.len()), v)
        } else {
            let bytes = n.checked_mul(width).ok_or("packed run overflow")?;
            let raw = take(&mut b, bytes)?;
            let num = n
                .checked_mul(8)
                .ok_or("packed count overflow")?
                .min(count - out.len());
            if controls.len() < 4096 {
                controls.push(format!(
                    "run_header={header}: bit-packed groups={n}, bit_width={width}"
                ));
            }
            out.extend(bits(raw, width, num)?)
        }
    }
    Ok(out)
}
fn delta(b: &mut &[u8], controls: &mut Vec<String>) -> Result<Vec<i64>, String> {
    let block = size(varint(b)? as i64)?;
    let minis = size(varint(b)? as i64)?;
    let count = size(varint(b)? as i64)?;
    if block == 0
        || block % 128 != 0
        || minis == 0
        || block % minis != 0
        || (block / minis) % 32 != 0
        || count > MAX_ITEMS
        || minis > 65536
    {
        return Err("invalid delta block parameters".into());
    }
    let first = zigzag(b)?;
    controls.push(format!("delta block_size={block}, miniblocks={minis}, total_value_count={count}, first_value={first}"));
    let mut out = Vec::new();
    if count == 0 {
        return Ok(out);
    }
    out.push(first);
    while out.len() < count {
        let min = zigzag(b)?;
        let widths = take(b, minis)?.to_vec();
        if controls.len() < 4096 {
            controls.push(format!(
                "min_delta={min}, miniblock_bit_widths={:?} ({} total)",
                &widths[..widths.len().min(64)],
                widths.len()
            ));
        }
        for width in widths {
            if width > 64 {
                return Err("invalid delta bit width".into());
            }
            let per = block / minis;
            let raw = take(
                b,
                per.checked_mul(width as usize)
                    .ok_or("delta overflow")?
                    .div_ceil(8),
            )?;
            let n = per.min(count - out.len());
            for d in bits(raw, width as usize, n)? {
                let v = out
                    .last()
                    .copied()
                    .unwrap()
                    .wrapping_add(min)
                    .wrapping_add(d as i64);
                out.push(v)
            }
        }
    }
    Ok(out)
}
fn binary_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => format!("{s:?}"),
        Err(_) => format!(
            "0x{}",
            b.iter().map(|v| format!("{v:02x}")).collect::<String>()
        ),
    }
}
fn bounded_binary(b: &[u8], used: usize) -> Result<String, String> {
    if b.len()
        .checked_mul(4)
        .and_then(|n| n.checked_add(used))
        .is_none_or(|n| n > MAX_PAGE_BYTES)
    {
        return Err("decoded text exceeds 64 MiB inspection limit".into());
    }
    Ok(binary_text(b))
}
fn plain(mut b: &[u8], ty: PhysicalType, count: usize) -> Result<Vec<String>, String> {
    if count > MAX_ITEMS {
        return Err("decoded item limit exceeded (1000000)".into());
    }
    let mut out = Vec::new();
    if ty == PhysicalType::Boolean {
        return bits(b, 1, count).map(|v| v.into_iter().map(|v| (v != 0).to_string()).collect());
    }
    let mut text_bytes = 0usize;
    for _ in 0..count {
        out.push(match ty {
            PhysicalType::Int32 => {
                i32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap()).to_string()
            }
            PhysicalType::Int64 => {
                i64::from_le_bytes(take(&mut b, 8)?.try_into().unwrap()).to_string()
            }
            PhysicalType::Float => {
                f32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap()).to_string()
            }
            PhysicalType::Double => {
                f64::from_le_bytes(take(&mut b, 8)?.try_into().unwrap()).to_string()
            }
            PhysicalType::Int96 => binary_text(take(&mut b, 12)?),
            PhysicalType::FixedLenByteArray(n) => bounded_binary(take(&mut b, n)?, text_bytes)?,
            PhysicalType::ByteArray => {
                let n = u32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
                bounded_binary(take(&mut b, n)?, text_bytes)?
            }
            PhysicalType::Boolean => unreachable!(),
        });
        text_bytes = text_bytes
            .checked_add(out.last().unwrap().len())
            .ok_or("decoded text size overflow")?;
        if text_bytes > MAX_PAGE_BYTES {
            return Err("decoded text exceeds 64 MiB inspection limit".into());
        }
    }
    Ok(out)
}
fn decode_values(
    mut b: &[u8],
    ty: PhysicalType,
    encoding: i64,
    count: usize,
    dict: Option<&[String]>,
    controls: &mut Vec<String>,
) -> Result<(Vec<String>, Option<Vec<u64>>), String> {
    if count > MAX_ITEMS {
        return Err("decoded item limit exceeded (1000000)".into());
    }
    if count == 0 {
        return Ok((Vec::new(), None));
    }
    match encoding {
        0 => Ok((plain(b, ty, count)?, None)),
        2 | 8 => {
            let width = take(&mut b, 1)?[0] as usize;
            controls.push(format!("dictionary_index_bit_width={width}"));
            let ids = hybrid(b, width, count, controls)?;
            let dict = dict.ok_or("dictionary page not available")?;
            let mut text_bytes = 0usize;
            for i in &ids {
                let v = dict
                    .get(*i as usize)
                    .ok_or("dictionary index out of bounds")?;
                text_bytes = text_bytes
                    .checked_add(v.len() + 128)
                    .ok_or("decoded text overflow")?;
                if text_bytes > MAX_PAGE_BYTES {
                    return Err(
                        "decoded dictionary expansion exceeds 64 MiB inspection limit".into(),
                    );
                }
            }
            let vals = ids
                .iter()
                .map(|i| {
                    dict.get(*i as usize)
                        .cloned()
                        .ok_or_else(|| format!("dictionary index {i} out of bounds"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((vals, Some(ids)))
        }
        3 if ty == PhysicalType::Boolean => {
            let n = u32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap()) as usize;
            let raw = take(&mut b, n)?;
            Ok((
                hybrid(raw, 1, count, controls)?
                    .into_iter()
                    .map(|v| (v != 0).to_string())
                    .collect(),
                None,
            ))
        }
        4 if ty == PhysicalType::Boolean => Ok((
            bits(b, 1, count)?
                .into_iter()
                .map(|v| (v != 0).to_string())
                .collect(),
            None,
        )),
        5 if matches!(ty, PhysicalType::Int32 | PhysicalType::Int64) => {
            let v = delta(&mut b, controls)?;
            if v.len() != count {
                return Err("delta value count mismatch".into());
            }
            Ok((
                v.into_iter()
                    .map(|v| {
                        if ty == PhysicalType::Int32 {
                            (v as i32).to_string()
                        } else {
                            v.to_string()
                        }
                    })
                    .collect(),
                None,
            ))
        }
        6 if ty == PhysicalType::ByteArray => {
            let lengths = delta(&mut b, controls)?;
            if lengths.len() != count {
                return Err("delta lengths count mismatch".into());
            }
            let mut out = Vec::new();
            let mut used = 0;
            for n in lengths {
                let value = bounded_binary(take(&mut b, size(n)?)?, used)?;
                used += value.len() + 128;
                out.push(value)
            }
            Ok((out, None))
        }
        7 if matches!(
            ty,
            PhysicalType::ByteArray | PhysicalType::FixedLenByteArray(_)
        ) =>
        {
            let prefix = delta(&mut b, controls)?;
            let suffix = delta(&mut b, controls)?;
            if prefix.len() != count || suffix.len() != count {
                return Err("delta strings count mismatch".into());
            }
            let mut last = Vec::new();
            let mut out = Vec::new();
            let mut used = 0;
            for (p, n) in prefix.into_iter().zip(suffix) {
                let p = size(p)?;
                if p > last.len() {
                    return Err("delta prefix exceeds previous value".into());
                }
                last.truncate(p);
                let suffix = take(&mut b, size(n)?)?;
                if last
                    .len()
                    .checked_add(suffix.len())
                    .ok_or("string length overflow")?
                    > MAX_PAGE_BYTES
                {
                    return Err("decoded string limit exceeded".into());
                }
                last.extend_from_slice(suffix);
                let value = bounded_binary(&last, used)?;
                used += value.len() + 128;
                out.push(value);
            }
            Ok((out, None))
        }
        9 => {
            let width = match ty {
                PhysicalType::Int32 | PhysicalType::Float => 4,
                PhysicalType::Int64 | PhysicalType::Double => 8,
                PhysicalType::FixedLenByteArray(n) => n,
                _ => return Err("unsupported BYTE_STREAM_SPLIT physical type".into()),
            };
            let n = width.checked_mul(count).ok_or("split length overflow")?;
            if n > b.len() || n > MAX_PAGE_BYTES {
                return Err("truncated or oversized BYTE_STREAM_SPLIT data".into());
            }
            controls.push(format!(
                "byte_stream_split byte_streams={width}, values_per_stream={count}"
            ));
            let mut raw = vec![0; n];
            for i in 0..count {
                for j in 0..width {
                    raw[i * width + j] = b[j * count + i]
                }
            }
            Ok((plain(&raw, ty, count)?, None))
        }
        _ => Err(format!("unsupported encoding {encoding} for {ty:?}")),
    }
}
type UnpackedPage = (Vec<WireField>, Vec<u8>, Range<u64>);
fn unpack_page(
    bytes: &[u8],
    off: u64,
    end: u64,
    codec: Compression,
) -> Result<UnpackedPage, String> {
    let start = usize::try_from(off).map_err(|_| "page offset overflow")?;
    let limit = usize::try_from(end).map_err(|_| "page end overflow")?;
    let slice = bytes.get(start..limit).ok_or("page outside column")?;
    let (fields, span) = decode_struct_span_at(slice, off, 0).ok_or("invalid page header")?;
    let compressed = size(integer(&fields, 3).ok_or("missing compressed_page_size")?)?;
    let uncompressed = size(integer(&fields, 2).ok_or("missing uncompressed_page_size")?)?;
    if uncompressed > MAX_PAGE_BYTES || compressed > MAX_PAGE_BYTES {
        return Err("page exceeds 64 MiB inspection limit".into());
    }
    let body_start = span.end as usize;
    let body_end = body_start
        .checked_add(compressed)
        .ok_or("page length overflow")?;
    if body_end > limit {
        return Err("page exceeds column range".into());
    }
    let body = bytes.get(body_start..body_end).ok_or("page outside file")?;
    let mut output = vec![0; uncompressed];
    let v2 = nested(&fields, 8);
    let prefix = if let Some(v) = v2 {
        size(integer(v, 5).ok_or("missing definition_levels_byte_length")?)?
            .checked_add(size(
                integer(v, 6).ok_or("missing repetition_levels_byte_length")?,
            )?)
            .ok_or("levels overflow")?
    } else {
        0
    };
    if prefix > body.len() || prefix > output.len() {
        return Err("levels exceed page body".into());
    }
    let compress = v2
        .and_then(|v| v.iter().find(|f| f.id == 7))
        .and_then(|v| v.value.as_bool())
        .unwrap_or(true);
    if codec == Compression::Uncompressed || !compress {
        if body.len() != output.len() {
            return Err("uncompressed page size mismatch".into());
        }
        output.copy_from_slice(body)
    } else {
        output[..prefix].copy_from_slice(&body[..prefix]);
        compression::decompress(
            codec,
            &body[prefix..],
            &mut output[prefix..],
            &mut DecompressionContext::Unset,
        )
        .map_err(|e| e.to_string())?
    }
    Ok((fields, output, off..body_end as u64))
}
// First absent ancestor determines whether a missing leaf denotes null or an empty collection.
fn absent_reason(root: &ParquetType, path: &[&str], definition: u64) -> String {
    let mut node = root;
    let mut threshold = 0;
    let mut names = Vec::new();
    for (i, name) in path.iter().enumerate() {
        if node.name() != *name {
            if let ParquetType::GroupType { fields, .. } = node {
                let Some(child) = fields.iter().find(|f| f.name() == *name) else {
                    break;
                };
                node = child;
            } else {
                break;
            }
        }
        names.push(*name);
        match node.get_field_info().repetition {
            Repetition::Optional => {
                threshold += 1;
                if definition < threshold {
                    return format!("null at {}", names.join("."));
                }
            }
            Repetition::Repeated => {
                threshold += 1;
                if definition < threshold {
                    return format!("empty collection at {}", names[..i].join("."));
                }
            }
            Repetition::Required => (),
        }
    }
    "absent leaf".into()
}
fn validate_column_ranges(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 12 || &bytes[..4] != b"PAR1" || &bytes[bytes.len() - 4..] != b"PAR1" {
        return Err("invalid Parquet magic".into());
    }
    let end = bytes.len() - 8;
    let len = u32::from_le_bytes(bytes[end..end + 4].try_into().unwrap()) as usize;
    let start = end
        .checked_sub(len)
        .filter(|s| *s >= 4)
        .ok_or("invalid footer length")?;
    let (fields, _) =
        decode_struct_span_at(&bytes[start..end], start as u64, 0).ok_or("invalid footer")?;
    let groups = fields
        .iter()
        .find(|f| f.id == 4)
        .and_then(|f| f.value.as_list())
        .ok_or("missing row_groups")?;
    for group in groups {
        let fields = group.as_struct().ok_or("invalid row group")?;
        let columns = fields
            .iter()
            .find(|f| f.id == 1)
            .and_then(|f| f.value.as_list())
            .ok_or("missing columns")?;
        for column in columns {
            let fields = column.as_struct().ok_or("invalid column")?;
            if fields
                .iter()
                .any(|f| f.id == 1 && f.value.as_binary().is_some_and(|s| !s.is_empty()))
            {
                return Err("external column files are not decoded".into());
            }
            let cm = nested(fields, 3).ok_or("encrypted or missing column metadata")?;
            let data = integer(cm, 9).ok_or("missing data_page_offset")?;
            let off = integer(cm, 11).map(|d| d.min(data)).unwrap_or(data);
            let n = integer(cm, 7).ok_or("missing total_compressed_size")?;
            let off = u64::try_from(off).map_err(|_| "negative column offset")?;
            let n = u64::try_from(n).map_err(|_| "negative column size")?;
            if off < 4 || off.checked_add(n).is_none_or(|e| e > start as u64) {
                return Err("column range outside data region".into());
            }
        }
    }
    Ok(())
}
/// Decode a physical page. Compressed bytes never acquire fictitious per-value file offsets.
pub fn decode_page(bytes: &[u8], page_offset: u64) -> Result<PageDetails, String> {
    if bytes.len() > 256 * 1024 * 1024 {
        return Err("file exceeds 256 MiB structural inspection limit".into());
    }
    validate_column_ranges(bytes)?;
    let metadata = read_metadata(&mut Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let column = metadata
        .row_groups
        .iter()
        .flat_map(|rg| rg.parquet_columns())
        .find(|c| c.byte_range().contains(&page_offset))
        .ok_or("no column owns this page")?;
    let range = column.byte_range();
    if range.end > bytes.len() as u64 {
        return Err("column outside file".into());
    }
    // Confirm that the requested offset is an actual page boundary, not an arbitrary body byte.
    let mut current = range.start;
    let mut found = false;
    for _ in 0..10000 {
        if current == page_offset {
            found = true;
            break;
        }
        if current > page_offset || current >= range.end {
            break;
        }
        let (h, s) =
            decode_struct_span_at(&bytes[current as usize..range.end as usize], current, 0)
                .ok_or("invalid preceding page header")?;
        let n = size(integer(&h, 3).ok_or("missing compressed size")?)?;
        current = s.end.checked_add(n as u64).ok_or("page length overflow")?;
    }
    if !found {
        return Err("offset is not a page boundary".into());
    }
    let (h, raw, source_span) = unpack_page(bytes, page_offset, range.end, column.compression())?;
    let ty = column.physical_type();
    let mut controls = vec![
        format!(
            "source file range: 0x{:x}..0x{:x}; decompressed offsets start at 0",
            source_span.start, source_span.end
        ),
        format!("physical_type={ty:?}; codec={:?}", column.compression()),
        format!(
            "path_in_schema={}",
            column
                .descriptor()
                .path_in_schema
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(".")
        ),
    ];
    let mut details = PageDetails {
        source_span,
        decompressed: raw,
        controls: Vec::new(),
        values: Vec::new(),
    };
    if integer(&h, 1) == Some(2) {
        let dh = nested(&h, 7).ok_or("missing dictionary_page_header")?;
        let count = size(integer(dh, 1).ok_or("missing dictionary count")?)?;
        controls.push(format!("dictionary num_values={count}"));
        details.values = plain(&details.decompressed, ty, count)?
            .into_iter()
            .enumerate()
            .map(|(i, v)| format!("dictionary[{i}] = {v}"))
            .collect();
        details.controls = controls;
        return Ok(details);
    }
    let v2 = nested(&h, 8);
    let dh = v2
        .or_else(|| nested(&h, 5))
        .ok_or("unsupported page type")?;
    let count = size(integer(dh, 1).ok_or("missing num_values")?)?;
    if count > MAX_ITEMS || count.saturating_mul(192) > MAX_PAGE_BYTES {
        return Err("page exceeds 1000000 decoded items inspection limit".into());
    }
    let desc = &column.descriptor().descriptor;
    let max_rep =
        usize::try_from(desc.max_rep_level).map_err(|_| "negative max repetition level")?;
    let max_def =
        usize::try_from(desc.max_def_level).map_err(|_| "negative max definition level")?;
    let mut body = details.decompressed.as_slice();
    let mut levels = Vec::new();
    for (name, max, id, encoding_id) in [
        ("repetition_levels", max_rep, 6, 4),
        ("definition_levels", max_def, 5, 3),
    ] {
        let region_start = details.decompressed.len() - body.len();
        let len = if let Some(v) = v2 {
            size(integer(v, id).ok_or("missing levels length")?)?
        } else if max == 0 {
            0
        } else if integer(dh, encoding_id) == Some(4) {
            count
                .checked_mul((usize::BITS - max.leading_zeros()) as usize)
                .ok_or("levels size overflow")?
                .div_ceil(8)
        } else {
            u32::from_le_bytes(take(&mut body, 4)?.try_into().unwrap()) as usize
        };
        let encoded = take(&mut body, len)?;
        let width = (usize::BITS - max.leading_zeros()) as usize;
        controls.push(format!(
            "{name}: decompressed @+0x{region_start:x}, byte_length={len}, max_level={max}, bit_width={width}"
        ));
        let values = if max == 0 {
            vec![0; count]
        } else if v2.is_none() && integer(dh, encoding_id) == Some(4) {
            legacy_bitpacked(encoded, width, count)?
        } else {
            hybrid(encoded, width, count, &mut controls)?
        };
        if values.iter().any(|v| *v > max as u64) {
            return Err("level exceeds schema maximum".into());
        }
        levels.push(values);
    }
    let present = levels[1].iter().filter(|v| **v == max_def as u64).count();
    let encoding = integer(dh, if v2.is_some() { 4 } else { 2 }).ok_or("missing value encoding")?;
    controls.push(format!(
        "values: decompressed @+0x{:x}, num_values={count}, non_null_values={present}, encoding={encoding}", details.decompressed.len()-body.len()
    ));
    let dict = if present > 0 && matches!(encoding, 2 | 8) {
        let off = column
            .dictionary_page_offset()
            .ok_or("dictionary offset missing")?;
        let (dh, raw, _) = unpack_page(
            bytes,
            u64::try_from(off).map_err(|_| "negative dictionary offset")?,
            range.end,
            column.compression(),
        )?;
        let inner = nested(&dh, 7).ok_or("missing dictionary header")?;
        Some(plain(
            &raw,
            ty,
            size(integer(inner, 1).ok_or("missing dictionary length")?)?,
        )?)
    } else {
        None
    };
    let absence_path = column
        .descriptor()
        .path_in_schema
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>();
    let (vals, indices) =
        match decode_values(body, ty, encoding, present, dict.as_deref(), &mut controls) {
            Ok(v) => v,
            Err(e) => {
                controls.push(format!("Value decoding unavailable: {e}"));
                let mut rendered = 0usize;
                for (i, (&rep, &def)) in levels[0].iter().zip(&levels[1]).enumerate() {
                    let state = if def == max_def as u64 {
                        "present; value unavailable".into()
                    } else {
                        absent_reason(&column.descriptor().base_type, &absence_path, def)
                    };
                    let row =
                        format!("[{i}] repetition_level={rep}, definition_level={def}: {state}");
                    rendered = rendered
                        .checked_add(row.len())
                        .ok_or("decoded output overflow")?;
                    if rendered > MAX_PAGE_BYTES {
                        return Err("decoded levels output exceeds 64 MiB inspection limit".into());
                    }
                    details.values.push(row);
                }
                details.controls = controls;
                return Ok(details);
            }
        };
    let mut output_bytes = vals.iter().map(|s| s.len()).sum::<usize>();
    output_bytes = output_bytes
        .checked_add(count.checked_mul(192).ok_or("output size overflow")?)
        .ok_or("output size overflow")?;
    if let Some(d) = &dict {
        output_bytes = output_bytes
            .checked_add(d.iter().map(|s| s.len() + 32).sum::<usize>())
            .ok_or("dictionary output size overflow")?;
    }
    if output_bytes > MAX_PAGE_BYTES {
        return Err("formatted page output exceeds 64 MiB inspection limit".into());
    }
    if let Some(dict) = dict {
        for (i, v) in dict.iter().enumerate() {
            details.values.push(format!("dictionary[{i}] = {v}"));
        }
    }
    let mut rendered = details.values.iter().map(|s| s.len()).sum::<usize>();
    let mut vi = 0;
    for (i, (&rep, &def)) in levels[0].iter().zip(&levels[1]).enumerate() {
        let v = if def == max_def as u64 {
            let value = &vals[vi];
            let text = if let Some(ids) = &indices {
                format!("dictionary_index={} → {value}", ids[vi])
            } else {
                value.clone()
            };
            vi += 1;
            text
        } else {
            absent_reason(&column.descriptor().base_type, &absence_path, def)
        };
        let row = format!("[{i}] repetition_level={rep}, definition_level={def}: {v}");
        rendered = rendered
            .checked_add(row.len())
            .ok_or("decoded output overflow")?;
        if rendered > MAX_PAGE_BYTES {
            return Err("formatted page output exceeds 64 MiB inspection limit".into());
        }
        details.values.push(row);
    }
    details.controls = controls;
    Ok(details)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rle_and_bitpacked_runs() {
        let mut c = Vec::new();
        assert_eq!(hybrid(&[6, 2], 2, 3, &mut c).unwrap(), vec![2, 2, 2]);
        assert_eq!(
            hybrid(&[3, 0b10101010], 1, 8, &mut c).unwrap(),
            vec![0, 1, 0, 1, 0, 1, 0, 1]
        );
        assert!(hybrid(&[0], 1, 1, &mut c).is_err());
    }
    #[test]
    fn dictionary_indices() {
        let dict = vec!["red".into(), "blue".into()];
        let (v, i) = decode_values(
            &[1, 3, 0b10101010],
            PhysicalType::ByteArray,
            8,
            4,
            Some(&dict),
            &mut vec![],
        )
        .unwrap();
        assert_eq!(v, vec!["red", "blue", "red", "blue"]);
        assert_eq!(i.unwrap(), vec![0, 1, 0, 1]);
    }
    #[test]
    fn plain_and_split() {
        assert_eq!(
            plain(&[42, 0, 0, 0], PhysicalType::Int32, 1).unwrap(),
            vec!["42"]
        );
        let (v, _) = decode_values(
            &[1, 2, 0, 0, 0, 0, 0, 0],
            PhysicalType::Int32,
            9,
            2,
            None,
            &mut vec![],
        )
        .unwrap();
        assert_eq!(v, vec!["1", "2"]);
        assert!(plain(&[1], PhysicalType::Int32, 1).is_err());
    }
    #[test]
    fn delta_and_empty_dictionary() {
        let mut raw = &[128, 1, 4, 3, 20, 4, 1, 0, 0, 0, 2, 0, 0, 0][..];
        assert_eq!(delta(&mut raw, &mut vec![]).unwrap(), vec![10, 12, 15]);
        assert_eq!(
            decode_values(&[], PhysicalType::Int32, 8, 0, None, &mut vec![])
                .unwrap()
                .0
                .len(),
            0
        );
    }
    #[test]
    fn decodes_nested_fixture_pages() {
        let bytes = include_bytes!("../../tests/fixtures/sample-nested.parquet");
        let metadata = read_metadata(&mut Cursor::new(bytes.as_slice())).unwrap();
        let mut decoded = 0;
        let mut absent = Vec::new();
        for column in metadata.row_groups[0].parquet_columns() {
            let page = decode_page(bytes, column.data_page_offset() as u64).unwrap();
            assert!(
                !page.controls.iter().any(|s| s.contains("unavailable")),
                "{:?}",
                page.controls
            );
            decoded += page.values.iter().filter(|s| s.starts_with('[')).count();
            absent.extend(
                page.values
                    .into_iter()
                    .filter(|s| s.contains("null at") || s.contains("empty collection")),
            );
            if let Some(off) = column.dictionary_page_offset() {
                assert!(!decode_page(bytes, off as u64).unwrap().values.is_empty());
            }
        }
        assert!(decoded >= 28);
        assert!(!absent.is_empty());
    }
    #[test]
    fn v2_levels_and_uncompressed_flag() {
        // DATA_PAGE_V2: two rows, one null, plain INT32; levels remain uncompressed.
        let mut bytes = vec![
            0x15, 6, 0x15, 16, 0x15, 16, 0x5c, 0x15, 4, 0x15, 2, 0x15, 4, 0x15, 0, 0x15, 8, 0x15,
            0, 0x12, 0, 0,
        ];
        bytes.extend_from_slice(&[2, 1, 2, 0, 42, 0, 0, 0]);
        let (header, raw, span) =
            unpack_page(&bytes, 0, bytes.len() as u64, Compression::Snappy).unwrap();
        assert_eq!(span.end, bytes.len() as u64);
        assert_eq!(integer(nested(&header, 8).unwrap(), 5), Some(4));
        assert_eq!(hybrid(&raw[..4], 1, 2, &mut vec![]).unwrap(), vec![1, 0]);
        assert_eq!(
            plain(&raw[4..], PhysicalType::Int32, 1).unwrap(),
            vec!["42"]
        );
    }
    #[test]
    fn bounds_dictionary_expansion_before_cloning() {
        let dictionary = vec!["x".repeat(1024 * 1024)];
        assert!(
            decode_values(
                &[1, 200, 1, 0],
                PhysicalType::ByteArray,
                8,
                100,
                Some(&dictionary),
                &mut vec![]
            )
            .unwrap_err()
            .contains("64 MiB")
        );
    }
    #[test]
    fn delta_strings_and_legacy_levels() {
        // DELTA_LENGTH_BYTE_ARRAY lengths=[2,3], followed by concatenated bytes.
        let mut raw = vec![128, 1, 4, 2, 4, 2, 0, 0, 0, 0];
        raw.extend_from_slice(b"abcde");
        let (v, _) = decode_values(&raw, PhysicalType::ByteArray, 6, 2, None, &mut vec![]).unwrap();
        assert_eq!(v, vec!["\"ab\"", "\"cde\""]);
        let mut raw = vec![128, 1, 4, 2, 0, 2, 0, 0, 0, 0];
        raw.extend_from_slice(&[128, 1, 4, 2, 4, 0, 0, 0, 0, 0]);
        raw.extend_from_slice(b"abcd");
        let (v, _) = decode_values(&raw, PhysicalType::ByteArray, 7, 2, None, &mut vec![]).unwrap();
        assert_eq!(v, vec!["\"ab\"", "\"acd\""]);
        assert_eq!(
            legacy_bitpacked(&[0b00011011], 2, 4).unwrap(),
            vec![0, 1, 2, 3]
        );
    }
    #[test]
    fn reject_corrupt() {
        assert!(decode_page(b"PAR1", 0).is_err());
        assert!(hybrid(&[2], 33, 1, &mut vec![]).is_err());
        assert!(bits(&[], 64, 1).is_err());
    }
}
