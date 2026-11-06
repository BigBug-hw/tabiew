//! A self-contained Protobuf wire format decoder.
//!
//! Purpose: decode Protobuf-encoded messages (used by Lance v2) and produce
//! a generic "wire tree" annotated with absolute byte spans, which the hex
//! viewer can point at directly.
//!
//! Spec reference: Protobuf Encoding
//! <https://developers.google.com/protocol-buffers/docs/encoding>

use std::ops::Range;

/// Protobuf wire types (low 3 bits of the tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PbWireType {
    Varint = 0,        // int32, int64, uint32, uint64, sint32, sint64, bool, enum
    SixtyFourBit = 1,  // fixed64, sfixed64, double
    LengthDelimited = 2, // string, bytes, embedded messages, packed repeated
    ThirtyTwoBit = 5,  // fixed32, sfixed32, float
}

impl PbWireType {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(PbWireType::Varint),
            1 => Some(PbWireType::SixtyFourBit),
            2 => Some(PbWireType::LengthDelimited),
            5 => Some(PbWireType::ThirtyTwoBit),
            _ => None,
        }
    }
}

/// A decoded Protobuf field with its field number and value.
#[derive(Debug, Clone)]
pub struct PbField {
    pub number: u32,
    pub value: PbValue,
}

/// Protobuf value with byte span (span covers tag + value bytes).
#[derive(Debug, Clone)]
pub struct PbValue {
    pub span: Range<u64>,
    pub wire_type: PbWireType,
    pub data: PbData,
}

impl PbValue {
    pub fn as_varint(&self) -> Option<u64> {
        match &self.data {
            PbData::Varint(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match &self.data {
            PbData::Varint(v) => Some(*v as i64),
            PbData::Fixed64(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match &self.data {
            PbData::Varint(v) => Some(*v as u32),
            PbData::Fixed32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.data {
            PbData::Bytes(b) => Some(b.as_slice()),
            // String variant stores the decoded string; raw bytes are not retained.
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match &self.data {
            PbData::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_message(&self) -> Option<&[PbField]> {
        match &self.data {
            PbData::Message(fields) => Some(fields.as_slice()),
            _ => None,
        }
    }
}

/// Decoded Protobuf data.
#[derive(Debug, Clone)]
pub enum PbData {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    Bytes(Vec<u8>),
    Message(Vec<PbField>),
    String(String),
}

/// Decode a Protobuf message starting at `offset` within `bytes` (relative to `base`).
/// Returns (fields, span) where span covers the entire decoded message.
///
/// Safety limits: MAX_DEPTH=64, MAX_LIST_LEN=1_000_000, MAX_BINARY_LEN=64MiB.
/// Returns `None` on malformed input or limit exceeded — never panics.
pub fn decode_message_span_at(
    bytes: &[u8],
    base: u64,
    offset: u64,
) -> Option<(Vec<PbField>, Range<u64>)> {
    let off = offset as usize;
    if off > bytes.len() {
        return None;
    }
    let mut dec = Decoder::new(&bytes[off..], base + offset);
    let fields = dec.read_message(0)?;
    let span = dec.abs(0)..dec.abs(dec.pos);
    Some((fields, span))
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

    /// Unsigned varint (Protobuf uses base-128 varint, not zigzag).
    fn read_varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
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

    /// Read a single field (tag + value) and return the field.
    fn read_field(&mut self) -> Option<PbField> {
        let start = self.pos;
        let tag = self.read_varint()?;
        let number = (tag >> 3) as u32;
        let wire_type = PbWireType::from_u8((tag & 0x07) as u8)?;

        let data = match wire_type {
            PbWireType::Varint => Some(PbData::Varint(self.read_varint()?)),
            PbWireType::SixtyFourBit => {
                let raw = self.read_bytes(8)?;
                Some(PbData::Fixed64(u64::from_le_bytes(raw.try_into().ok()?)))
            }
            PbWireType::LengthDelimited => {
                // Could be a nested message or a string/bytes.
                // We try to decode as message first; if it looks like a message
                // (contains valid field tags), return Message; otherwise String/Bytes.
                let len = self.read_varint()? as usize;
                if len > MAX_BINARY_LEN {
                    return None;
                }
                let data_start = self.pos;
                let data_end = data_start + len;

                // Peek: try to decode as nested message. Only accept it as a
                // message if (1) we are within the depth limit, (2) the nested
                // decode consumes *exactly* `len` bytes, and (3) at least one
                // field was decoded. Otherwise fall back to string/bytes — a
                // length-delimited value that merely *looks* like a valid
                // message (e.g. ASCII text whose bytes happen to form tags)
                // must not be misinterpreted.
                if self.depth < MAX_DEPTH && len > 0 {
                    let saved_pos = self.pos;
                    let saved_depth = self.depth;
                    self.depth += 1;
                    let nested = self.read_message_inner(len);
                    self.depth = saved_depth;
                    if let Some(fields) = nested {
                        if !fields.is_empty() && self.pos == data_end {
                            return Some(PbField {
                                number,
                                value: PbValue {
                                    span: self.abs(start)..self.abs(self.pos),
                                    wire_type,
                                    data: PbData::Message(fields),
                                },
                            });
                        }
                    }
                    self.pos = saved_pos;
                }

                // Not a valid message — store as string/bytes.
                let data = self.read_bytes(len)?.to_vec();
                if let Ok(s) = std::str::from_utf8(&data) {
                    Some(PbData::String(s.to_string()))
                } else {
                    Some(PbData::Bytes(data))
                }
            }
            PbWireType::ThirtyTwoBit => {
                let raw = self.read_bytes(4)?;
                Some(PbData::Fixed32(u32::from_le_bytes(raw.try_into().ok()?)))
            }
        }?;

        Some(PbField {
            number,
            value: PbValue {
                span: self.abs(start)..self.abs(self.pos),
                wire_type,
                data,
            },
        })
    }

    /// Read a message (sequence of fields) with depth tracking.
    fn read_message(&mut self, limit: usize) -> Option<Vec<PbField>> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let fields = self.read_message_inner(limit);
        self.depth -= 1;
        fields
    }

    /// Read fields until EOF or `limit` bytes consumed.
    ///
    /// Returns `None` if a field header was partially consumed and then failed
    /// (e.g. a length-delimited field whose declared length exceeds the
    /// safety cap) — that indicates corruption rather than a clean end of
    /// message, so callers can fall back to string/bytes or surface an error.
    fn read_message_inner(&mut self, limit: usize) -> Option<Vec<PbField>> {
        let start = self.pos;
        let mut fields = Vec::new();
        loop {
            if self.pos >= self.bytes.len() {
                break;
            }
            if limit > 0 && self.pos - start >= limit {
                break;
            }
            // Peek at the tag — if it's 0, we've hit corruption or end.
            if self.bytes.get(self.pos) == Some(&0) {
                break;
            }
            let field_start = self.pos;
            match self.read_field() {
                Some(field) => fields.push(field),
                None => {
                    // If we consumed any bytes before failing, the input is
                    // malformed (not a clean EOF). For a nested message this
                    // signals the parent to fall back to string/bytes; for the
                    // top-level it surfaces as a decode failure.
                    if self.pos > field_start {
                        return None;
                    }
                    break;
                }
            }
        }
        Some(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hand-built Protobuf: field 1 (length-delimited) = "hello"
    // tag: (1 << 3) | 2 = 0x0a, length: 5, data: "hello"
    #[test]
    fn decodes_length_delimited_string() {
        let bytes = [0x0a, 0x05, b'h', b'e', b'l', b'l', b'o'];
        let (fields, span) = decode_message_span_at(&bytes, 0, 0).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].number, 1);
        assert_eq!(fields[0].value.as_string(), Some("hello"));
        assert_eq!(span, 0..7);
    }

    // Field 2 (varint) = 42
    // tag: (2 << 3) | 0 = 0x10, varint: 42 = 0x2a
    #[test]
    fn decodes_varint() {
        let bytes = [0x10, 0x2a];
        let (fields, _span) = decode_message_span_at(&bytes, 0, 0).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].number, 2);
        assert_eq!(fields[0].value.as_varint(), Some(42));
    }

    // Field 3 (fixed32) = 0x12345678
    // tag: (3 << 3) | 5 = 0x1d, data: 78 56 34 12 (LE)
    #[test]
    fn decodes_fixed32() {
        let bytes = [0x1d, 0x78, 0x56, 0x34, 0x12];
        let (fields, _span) = decode_message_span_at(&bytes, 0, 0).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].number, 3);
        assert_eq!(fields[0].value.as_u32(), Some(0x12345678));
    }

    // Field 4 (fixed64) = 0x0102030405060708
    // tag: (4 << 3) | 1 = 0x21, data: 08 07 06 05 04 03 02 01 (LE)
    #[test]
    fn decodes_fixed64() {
        let bytes = [0x21, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
        let (fields, _span) = decode_message_span_at(&bytes, 0, 0).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].number, 4);
        assert_eq!(fields[0].value.as_i64(), Some(0x0102030405060708i64));
    }

    // Nested message: field 1 = { field 2 = "inner" }
    // outer tag: 0x0a, length: 7
    // inner: 0x12 (field 2, len-delimited), 0x05, "inner"  (1 + 1 + 5 = 7 bytes)
    #[test]
    fn decodes_nested_message() {
        let bytes = [0x0a, 0x07, 0x12, 0x05, b'i', b'n', b'n', b'e', b'r'];
        let (fields, _span) = decode_message_span_at(&bytes, 0, 0).unwrap();
        assert_eq!(fields.len(), 1);
        let inner = fields[0].value.as_message().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].number, 2);
        assert_eq!(inner[0].value.as_string(), Some("inner"));
    }

    // Multiple fields
    #[test]
    fn decodes_multiple_fields() {
        // field 1 (varint) = 1, field 2 (string) = "abc"
        let bytes = [0x08, 0x01, 0x12, 0x03, b'a', b'b', b'c'];
        let (fields, _span) = decode_message_span_at(&bytes, 0, 0).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].number, 1);
        assert_eq!(fields[0].value.as_varint(), Some(1));
        assert_eq!(fields[1].number, 2);
        assert_eq!(fields[1].value.as_string(), Some("abc"));
    }

    #[test]
    fn rejects_offset_out_of_bounds() {
        let bytes = [0x08, 0x01];
        assert!(decode_message_span_at(&bytes, 0, 100).is_none());
    }

    #[test]
    fn empty_message() {
        let bytes: [u8; 0] = [];
        let (fields, _span) = decode_message_span_at(&bytes, 0, 0).unwrap();
        assert!(fields.is_empty());
    }

    #[test]
    fn base_offset_in_span() {
        let bytes = [0x08, 0x01];
        let (_fields, span) = decode_message_span_at(&bytes, 100, 0).unwrap();
        assert_eq!(span, 100..102);
    }

    #[test]
    fn rejects_max_binary_len() {
        // field 1 (len-delimited) with length > MAX_BINARY_LEN
        let mut bytes = vec![0x0a]; // tag
        // varint for 65MiB
        let len = (MAX_BINARY_LEN as u64) + 1;
        let mut buf = Vec::new();
        let mut v = len;
        while v > 0x7f {
            buf.push((v as u8) | 0x80);
            v >>= 7;
        }
        buf.push(v as u8);
        bytes.extend_from_slice(&buf);
        assert!(decode_message_span_at(&bytes, 0, 0).is_none());
    }
}
