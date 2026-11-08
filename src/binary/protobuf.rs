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
    Varint = 0,          // int32, int64, uint32, uint64, sint32, sint64, bool, enum
    SixtyFourBit = 1,    // fixed64, sfixed64, double
    LengthDelimited = 2, // string, bytes, embedded messages, packed repeated
    ThirtyTwoBit = 5,    // fixed32, sfixed32, float
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
    /// Value bytes, excluding tag and the length prefix for length-delimited fields.
    pub payload_span: Range<u64>,
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
            PbData::Bytes(b) => std::str::from_utf8(b).ok(),
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

/// Decode exactly the bounded message in `bytes[offset..]`.
///
/// Length-delimited fields remain raw bytes: only the schema can distinguish
/// strings, packed scalars, bytes and embedded messages. To decode a known
/// embedded message, pass its bytes and `payload_span.start` as the base.
/// Invalid tags, truncated fields and absolute-offset overflow return `None`.
pub fn decode_message_span_at(
    bytes: &[u8],
    base: u64,
    offset: u64,
) -> Option<(Vec<PbField>, Range<u64>)> {
    let off = usize::try_from(offset).ok()?;
    let input = bytes.get(off..)?;
    let start = base.checked_add(offset)?;
    let end = start.checked_add(u64::try_from(input.len()).ok()?)?;
    let mut dec = Decoder {
        bytes: input,
        base: start,
        pos: 0,
    };
    let mut fields = Vec::new();
    while dec.pos < input.len() {
        if fields.len() >= MAX_FIELDS {
            return None;
        }
        fields.push(dec.read_field()?);
    }
    Some((fields, start..end))
}

struct Decoder<'a> {
    bytes: &'a [u8],
    base: u64,
    pos: usize,
}
const MAX_BINARY_LEN: usize = 64 * 1024 * 1024;
const MAX_FIELDS: usize = 1_000_000;

impl<'a> Decoder<'a> {
    fn abs(&self, pos: usize) -> u64 {
        // The entire input range was checked before constructing the decoder.
        self.base + pos as u64
    }
    fn read_byte(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn read_bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }
    fn read_varint(&mut self) -> Option<u64> {
        let mut result = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.read_byte()?;
            if shift == 63 && b > 1 {
                return None;
            }
            result |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(result);
            }
        }
        None
    }
    fn read_field(&mut self) -> Option<PbField> {
        let start = self.pos;
        let tag = self.read_varint()?;
        let number = tag >> 3;
        if number == 0 || number > 0x1fff_ffff {
            return None;
        }
        let wire_type = PbWireType::from_u8((tag & 7) as u8)?;
        let payload_start;
        let data = match wire_type {
            PbWireType::Varint => {
                payload_start = self.pos;
                PbData::Varint(self.read_varint()?)
            }
            PbWireType::SixtyFourBit => {
                payload_start = self.pos;
                PbData::Fixed64(u64::from_le_bytes(self.read_bytes(8)?.try_into().ok()?))
            }
            PbWireType::ThirtyTwoBit => {
                payload_start = self.pos;
                PbData::Fixed32(u32::from_le_bytes(self.read_bytes(4)?.try_into().ok()?))
            }
            PbWireType::LengthDelimited => {
                let len = usize::try_from(self.read_varint()?).ok()?;
                if len > MAX_BINARY_LEN {
                    return None;
                }
                payload_start = self.pos;
                PbData::Bytes(self.read_bytes(len)?.to_vec())
            }
        };
        Some(PbField {
            number: number as u32,
            value: PbValue {
                span: self.abs(start)..self.abs(self.pos),
                payload_span: self.abs(payload_start)..self.abs(self.pos),
                wire_type,
                data,
            },
        })
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
        let value = &fields[0].value;
        let (inner, inner_span) =
            decode_message_span_at(value.as_bytes().unwrap(), value.payload_span.start, 0).unwrap();
        assert_eq!(inner_span, 2..9);
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
    #[test]
    fn strings_that_look_like_messages_remain_bytes() {
        for text in ["mtime", "hi", "hello", ""] {
            let mut bytes = vec![0x12, text.len() as u8];
            bytes.extend_from_slice(text.as_bytes());
            let (fields, span) = decode_message_span_at(&bytes, 100, 0).unwrap();
            assert!(matches!(fields[0].value.data, PbData::Bytes(_)));
            assert_eq!(fields[0].value.as_bytes(), Some(text.as_bytes()));
            assert_eq!(fields[0].value.as_string(), Some(text));
            assert_eq!(fields[0].value.payload_span, 102..span.end);
        }
    }
    #[test]
    fn rejects_bad_tags_and_trailing_zero() {
        for bytes in [
            vec![0],
            vec![8, 1, 0],
            vec![1; 9],
            vec![0x0b],
            vec![0x80, 0x80, 0x80, 0x80, 0x10, 0],
        ] {
            assert!(decode_message_span_at(&bytes, 0, 0).is_none(), "{bytes:?}");
        }
    }
    #[test]
    fn rejects_truncation_and_varint_overflow() {
        for bytes in [
            vec![8],
            vec![10, 2, 1],
            vec![13, 1],
            vec![9, 1, 2],
            [vec![8], vec![0xff; 9], vec![2]].concat(),
        ] {
            assert!(decode_message_span_at(&bytes, 0, 0).is_none(), "{bytes:?}");
        }
    }
    #[test]
    fn validates_absolute_range_and_empty_payload() {
        assert!(decode_message_span_at(&[8, 1], u64::MAX, 0).is_none());
        assert!(decode_message_span_at(&[8, 1], u64::MAX - 1, 1).is_none());
        let (fields, span) = decode_message_span_at(&[10, 0], 10, 0).unwrap();
        assert_eq!(span, 10..12);
        assert_eq!(fields[0].value.payload_span, 12..12);
        assert_eq!(fields[0].value.as_bytes(), Some([].as_slice()));
        assert!(
            decode_message_span_at(fields[0].value.as_bytes().unwrap(), 12, 0)
                .unwrap()
                .0
                .is_empty()
        );
    }
}
