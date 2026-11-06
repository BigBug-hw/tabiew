//! A self-contained Thrift Compact Protocol decoder.
//!
//! Purpose: teach the reader how the bytes on disk map to Parquet's
//! `FileMetaData`. We deliberately avoid pulling in a Thrift crate so the
//! decoder produces a generic "wire tree" annotated with absolute byte spans,
//! which the hex viewer can point at directly.
//!
//! Spec reference: Apache Thrift Compact Protocol
//! <https://github.com/apache/thrift/blob/master/doc/specs/thrift-compact-protocol.md>

use std::ops::Range;

/// Compact-protocol wire type ids (low nibble of field / list / map headers).
pub mod ct {
    pub const STOP: u8 = 0;
    pub const BOOL_TRUE: u8 = 1;
    pub const BOOL_FALSE: u8 = 2;
    pub const BYTE: u8 = 3;
    pub const I16: u8 = 4;
    pub const I32: u8 = 5;
    pub const I64: u8 = 6;
    pub const DOUBLE: u8 = 7;
    pub const BINARY: u8 = 8;
    pub const LIST: u8 = 9;
    pub const SET: u8 = 10;
    pub const MAP: u8 = 11;
    pub const STRUCT: u8 = 12;
}

/// Human-readable name for a compact wire type id.
pub fn type_name(t: u8) -> &'static str {
    match t {
        ct::STOP => "stop",
        ct::BOOL_TRUE => "bool(true)",
        ct::BOOL_FALSE => "bool(false)",
        ct::BYTE => "i8",
        ct::I16 => "i16",
        ct::I32 => "i32",
        ct::I64 => "i64",
        ct::DOUBLE => "double",
        ct::BINARY => "binary",
        ct::LIST => "list",
        ct::SET => "set",
        ct::MAP => "map",
        ct::STRUCT => "struct",
        _ => "unknown",
    }
}

/// A decoded value with its absolute byte span.
#[derive(Debug, Clone)]
pub struct WireValue {
    pub span: Range<u64>,
    pub kind: WireKind,
}

#[derive(Debug, Clone)]
pub enum WireKind {
    Bool(bool),
    Byte(i8),
    I16(i64),
    I32(i64),
    I64(i64),
    Double(f64),
    Binary(Vec<u8>),
    List(Vec<WireValue>),
    Map(Vec<(WireValue, WireValue)>),
    Struct(Vec<WireField>),
}

impl WireValue {
    pub fn as_struct(&self) -> Option<&[WireField]> {
        match &self.kind {
            WireKind::Struct(fields) => Some(fields),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[WireValue]> {
        match &self.kind {
            WireKind::List(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match &self.kind {
            WireKind::I16(v) | WireKind::I32(v) | WireKind::I64(v) => Some(*v),
            WireKind::Byte(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_binary(&self) -> Option<&[u8]> {
        match &self.kind {
            WireKind::Binary(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match &self.kind {
            WireKind::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// A decoded struct field: `id` plus its value (the span covers the value
/// only; the field header bytes are tracked separately by the decoder if
/// needed, but Parquet's field ids are stable so we mostly care about the
/// value span for "jump to bytes").
#[derive(Debug, Clone)]
pub struct WireField {
    pub id: i16,
    pub value: WireValue,
}

/// Decode a top-level struct from `bytes`, starting at `base` absolute offset.
///
/// Returns `None` if the input is malformed or exceeds the configured safety
/// limits. The caller is expected to handle the failure gracefully (e.g. show
/// an "invalid metadata" message) — we never panic.
pub fn decode_struct(bytes: &[u8], base: u64) -> Option<Vec<WireField>> {
    let mut dec = Decoder::new(bytes, base);
    dec.read_struct_body(0).map(|w| match w.kind {
        WireKind::Struct(fields) => fields,
        _ => Vec::new(),
    })
}

/// Convenience: decode a struct at an absolute file offset. Returns the
/// fields **and** the full struct span (including the trailing STOP byte) so
/// callers like the page-header scanner can advance past the whole header.
pub fn decode_struct_span_at(
    bytes: &[u8],
    base: u64,
    offset: u64,
) -> Option<(Vec<WireField>, std::ops::Range<u64>)> {
    let off = offset as usize;
    if off > bytes.len() {
        return None;
    }
    let mut dec = Decoder::new(&bytes[off..], base + offset);
    let w = dec.read_struct_body(0)?;
    Some(match w.kind {
        WireKind::Struct(fields) => (fields, w.span),
        _ => (Vec::new(), w.span),
    })
}

struct Decoder<'a> {
    bytes: &'a [u8],
    /// Absolute offset of `bytes[0]` in the original file.
    base: u64,
    /// Current cursor within `bytes`.
    pos: usize,
    /// Depth guard.
    depth: u32,
}

const MAX_DEPTH: u32 = 64;
const MAX_LIST_LEN: usize = 1_000_000;
const MAX_BINARY_LEN: usize = 64 * 1024 * 1024;

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8], base: u64) -> Self {
        Self {
            bytes,
            base,
            pos: 0,
            depth: 0,
        }
    }

    fn abs(&self, pos: usize) -> u64 {
        self.base + pos as u64
    }

    fn read_byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn read_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos.checked_add(n)? > self.bytes.len() {
            return None;
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }

    /// Unsigned LEB128 varint.
    fn read_varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.read_byte()?;
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }

    /// ZigZag-decoded varint → i64.
    fn read_zigzag(&mut self) -> Option<i64> {
        let u = self.read_varint()?;
        Some(((u >> 1) as i64) ^ -((u & 1) as i64))
    }

    fn read_struct_body(&mut self, _field_id_hint: i16) -> Option<WireValue> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let start = self.pos;
        let mut fields = Vec::new();
        let mut last_field_id: i16 = 0;
        loop {
            let header = self.read_byte()?;
            if header == ct::STOP {
                break;
            }
            let wire_type = header & 0x0f;
            let delta = (header >> 4) & 0x0f;
            let field_id = if delta == 0 {
                self.read_zigzag()? as i16
            } else {
                last_field_id.wrapping_add(delta as i16)
            };
            last_field_id = field_id;
            let value = self.read_value(wire_type)?;
            fields.push(WireField {
                id: field_id,
                value,
            });
        }
        self.depth -= 1;
        Some(WireValue {
            span: self.abs(start)..self.abs(self.pos),
            kind: WireKind::Struct(fields),
        })
    }

    fn read_value(&mut self, wire_type: u8) -> Option<WireValue> {
        let start = self.pos;
        match wire_type {
            ct::BOOL_TRUE => Some(WireValue {
                span: self.abs(start)..self.abs(start),
                kind: WireKind::Bool(true),
            }),
            ct::BOOL_FALSE => Some(WireValue {
                span: self.abs(start)..self.abs(start),
                kind: WireKind::Bool(false),
            }),
            ct::BYTE => {
                let b = self.read_byte()?;
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::Byte(b as i8),
                })
            }
            ct::I16 => {
                let v = self.read_zigzag()?;
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::I16(v),
                })
            }
            ct::I32 => {
                let v = self.read_zigzag()?;
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::I32(v),
                })
            }
            ct::I64 => {
                let v = self.read_zigzag()?;
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::I64(v),
                })
            }
            ct::DOUBLE => {
                let raw = self.read_bytes(8)?;
                let v = f64::from_le_bytes(raw.try_into().ok()?);
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::Double(v),
                })
            }
            ct::BINARY => {
                let len = self.read_varint()? as usize;
                if len > MAX_BINARY_LEN {
                    return None;
                }
                let data = self.read_bytes(len)?.to_vec();
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::Binary(data),
                })
            }
            ct::LIST | ct::SET => {
                let header = self.read_byte()?;
                let elem_type = header & 0x0f;
                let size_low = (header >> 4) & 0x0f;
                let size = if size_low == 0x0f {
                    self.read_varint()? as usize
                } else {
                    size_low as usize
                };
                if size > MAX_LIST_LEN {
                    return None;
                }
                let mut items = Vec::with_capacity(size.min(1024));
                for _ in 0..size {
                    items.push(self.read_value(elem_type)?);
                }
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::List(items),
                })
            }
            ct::MAP => {
                let size = self.read_varint()? as usize;
                if size > MAX_LIST_LEN {
                    return None;
                }
                let kv_header = self.read_byte()?;
                let _key_type = kv_header >> 4;
                let val_type = kv_header & 0x0f;
                let mut entries = Vec::with_capacity(size.min(1024));
                for _ in 0..size {
                    let k = self.read_value(kv_header >> 4)?;
                    let v = self.read_value(val_type)?;
                    entries.push((k, v));
                }
                Some(WireValue {
                    span: self.abs(start)..self.abs(self.pos),
                    kind: WireKind::Map(entries),
                })
            }
            ct::STRUCT => self.read_struct_body(0),
            ct::STOP => None,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built Compact Protocol struct:
    ///   struct {
    ///     1: i32 = 1            -> 15 02           (delta=1, type=i32(5), zigzag(1)=2)
    ///     2: binary "hi"        -> 18 02 68 69     (delta=1, type=binary(8), len=2, "hi")
    ///     3: list<i32> [1,2]    -> 19 25 02 04     (delta=1, type=list(9), list header (2<<4)|i32=0x25, [zigzag1, zigzag2])
    ///     0x00 (stop)
    ///   }
    #[test]
    fn decode_simple_struct() {
        let bytes = [
            0x15, 0x02, 0x18, 0x02, b'h', b'i', 0x19, 0x25, 0x02, 0x04, 0x00,
        ];
        let fields = decode_struct(&bytes, 0).unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].id, 1);
        assert_eq!(fields[0].value.as_i64(), Some(1));
        assert_eq!(fields[1].id, 2);
        assert_eq!(fields[1].value.as_binary(), Some(b"hi".as_ref()));
        assert_eq!(fields[2].id, 3);
        let list = fields[2].value.as_list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].as_i64(), Some(1));
        assert_eq!(list[1].as_i64(), Some(2));
        // Field 3 value spans the list header + 2 values: bytes 7..10.
        // (The field header byte at offset 6 is not part of the value span.)
        let last = &fields[2].value;
        assert_eq!(last.span, 7..10);
    }

    #[test]
    fn decode_bool_in_header() {
        // struct { 1: bool true, 2: bool false, stop }
        let bytes = [0x11, 0x12, 0x00];
        let fields = decode_struct(&bytes, 0).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].value.as_bool(), Some(true));
        assert_eq!(fields[1].value.as_bool(), Some(false));
    }

    #[test]
    fn decode_long_field_id_delta_zero() {
        // struct { 100: i32 = 1 }  -> delta=0, type=i32, zigzag(100)=200, zigzag(1)=2
        let bytes = [0x05, 0xc8, 0x01, 0x02, 0x00];
        let fields = decode_struct(&bytes, 0).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].id, 100);
        assert_eq!(fields[0].value.as_i64(), Some(1));
    }

    #[test]
    fn decode_nested_struct() {
        // struct {
        //   1: struct { 1: i32 = 7 }  -> 1c (delta=1,type=struct) 15 0e 00 00
        //   stop
        // }
        let bytes = [0x1c, 0x15, 0x0e, 0x00, 0x00];
        let fields = decode_struct(&bytes, 0).unwrap();
        assert_eq!(fields.len(), 1);
        let inner = fields[0].value.as_struct().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].id, 1);
        assert_eq!(inner[0].value.as_i64(), Some(7));
    }

    #[test]
    fn decode_map() {
        // struct { 1: map<binary, i32> { "a": 1 } }
        // field header: (1<<4)|map(0x0b) = 0x1b
        // size varint: 01
        // kv header: (binary(8)<<4)|i32(5) = 0x85
        // key: 01 'a'
        // val: 02
        // stop
        let bytes = [0x1b, 0x01, 0x85, 0x01, b'a', 0x02, 0x00];
        let fields = decode_struct(&bytes, 0).unwrap();
        assert_eq!(fields.len(), 1);
        match &fields[0].value.kind {
            WireKind::Map(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0.as_binary(), Some(b"a".as_ref()));
                assert_eq!(entries[0].1.as_i64(), Some(1));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn decode_truncated_returns_none() {
        let bytes = [0x15]; // field header but no value
        assert!(decode_struct(&bytes, 0).is_none());
    }

    #[test]
    fn decode_empty_struct() {
        let bytes = [0x00];
        let fields = decode_struct(&bytes, 0).unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn decode_zigzag_and_varint_edge() {
        // i64 = -1 -> zigzag = 1
        let bytes = [0x16, 0x01, 0x00]; // field 1: i64 = -1, stop
        let fields = decode_struct(&bytes, 0).unwrap();
        assert_eq!(fields[0].value.as_i64(), Some(-1));
    }

    #[test]
    fn decode_double() {
        // field 1: double = 1.0
        let mut bytes = vec![0x17]; // delta=1, type=double
        bytes.extend_from_slice(&1.0f64.to_le_bytes());
        bytes.push(0x00);
        let fields = decode_struct(&bytes, 0).unwrap();
        match &fields[0].value.kind {
            WireKind::Double(v) => assert!((v - 1.0).abs() < f64::EPSILON),
            _ => panic!("expected double"),
        }
    }
}
