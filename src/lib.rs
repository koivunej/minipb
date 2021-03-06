use std::convert::TryFrom;
use std::fmt;

pub mod field_reader;
pub mod gather_fields;
pub mod matcher_fields;

pub mod io_ext;

pub use gather_fields::Slicer;

pub(crate) mod pb;

#[derive(Debug)]
pub struct ReadField<'a> {
    /// How many bytes were consumed from the beginning of the buffer
    consumed: usize,
    /// The actual read field, which can be used to skip the field.
    field: &'a FieldInfo,
}

impl ReadField<'_> {
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    pub fn field_id(&self) -> FieldId {
        self.field.id
    }

    pub fn bytes_to_skip(&self) -> usize {
        self.consumed + self.field.bytes_to_skip()
    }

    pub fn field_len(&self) -> usize {
        self.field.bytes_to_skip()
    }

    pub fn wire_type(&self) -> WireType {
        self.field.kind
    }

    pub fn is_length_delimited(&self) -> bool {
        match self.field.kind {
            WireType::LengthDelimited => true,
            _ => false,
        }
    }

    pub fn value(&self) -> &FieldValue {
        &self.field.value
    }
}

#[derive(Debug)]
struct FieldInfo {
    /// Offset where the field tag (index and wiretype) starts.
    // tag_at_offset: u64,
    /// Field identifier as in the .proto file.
    id: FieldId,
    /// Kind of the field, determines the kind of `value` field.
    kind: WireType,
    /// Total length of the field from it's beginning. For varint this will be amount of varint
    /// bytes read for the field, for fixed32 and fixed64 constants 4 and 8 will be given. For
    /// length delimited fields this the length of the field without preceding tag and length
    /// fields.
    //length: u32,
    /// Value determined by the `kind` field.
    value: FieldValue,
}

pub type FieldId = u32;

/// Supported protobuf wire types. Note, that BeginGroup and EndGroup **are not supported**.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WireType {
    Varint,
    Fixed64,
    LengthDelimited,
    Fixed32,
}

impl TryFrom<u32> for WireType {
    type Error = DecodingError;

    fn try_from(tag: u32) -> Result<WireType, Self::Error> {
        Ok(match tag & 0x7 {
            0 => WireType::Varint,
            1 => WireType::Fixed64,
            2 => WireType::LengthDelimited,
            3 | 4 => return Err(DecodingError::UnsupportedGroupWireType(tag)),
            5 => WireType::Fixed32,
            _ => return Err(DecodingError::UnknownWireType(tag)),
        })
    }
}

impl FieldInfo {
    fn bytes_to_skip(&self) -> usize {
        match self.value {
            FieldValue::DataLength(x) => x as usize,
            _ => 0,
        }
    }
}

#[derive(Debug)]
pub enum FieldValue {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    /// Length delimited data starts after two varints, tag and length. This offset points to the
    /// byte which starts the actual field, and continues for the length specified in the
    /// `FieldInfo::offset`.
    DataLength(u32),
}

#[cfg(test)]
impl FieldValue {
    fn output_with_field_id(&self, id: FieldId) -> impl Iterator<Item = u8> {
        use either::Either;
        use stackvector::StackVec;
        use FieldValue::*;

        // sorry, this is some maniac code right here. wanted to try this out
        let lowest_bits = match self {
            Varint(_) => 0,
            Fixed64(_) => 1,
            DataLength(_) => 2,
            Fixed32(_) => 5,
        };

        let id = (id << 3) | lowest_bits;

        let field_bytes = VarintBytes(id as u64);

        let mut tmp = StackVec::<[u8; 8]>::new();

        let payload = match self {
            Varint(x) => Either::Left(VarintBytes(*x)),
            Fixed64(x) => {
                tmp.extend_from_slice(&x.to_le_bytes());
                Either::Right(tmp.into_iter())
            }
            Fixed32(x) => {
                tmp.extend_from_slice(&x.to_le_bytes());
                Either::Right(tmp.into_iter())
            }
            DataLength(x) => Either::Left(VarintBytes(*x as u64)),
        };

        field_bytes.chain(payload)
    }
}

#[cfg(test)]
struct VarintBytes(u64);

#[cfg(test)]
impl Iterator for VarintBytes {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0 == 0 {
            None
        } else {
            let ret = (self.0 & 0x7f) as u8;
            self.0 >>= 7;
            Some(if self.0 != 0 { ret | 0x80 } else { ret })
        }
    }
}

#[test]
fn test_varint_bytes() {
    assert_eq!(&VarintBytes(227).collect::<Vec<_>>(), &[0xe3, 0x01]);
    assert_eq!(&VarintBytes(242).collect::<Vec<_>>(), &[0xf2, 0x01]);
}

/// All of the bytes still remaining in the buffer need to be kept, but more bytes should be read.
#[derive(Debug)]
pub struct NeedMoreBytes;

#[derive(Debug)]
pub enum Status {
    /// Would like to read the next item but there are no more bytes in the buffer. This could be
    /// because the input has been fully exhausted (end of file).
    IdleAtEndOfBuffer,
    /// Reading a variable length integer, for example the field id and type or the value or the
    /// length.
    NeedMoreBytes,
}

/// Represents either a bug in this crate, or an error in the protobuf bytes.
#[derive(Debug)]
pub enum DecodingError {
    UnsupportedGroupWireType(u32),
    UnknownWireType(u32),
    TooManyVarint32Bytes,
    TooManyVarint64Bytes,
    InvalidUtf8,
    FailedMatcherNesting(usize, usize),
}

impl fmt::Display for DecodingError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        use DecodingError::*;
        match *self {
            UnsupportedGroupWireType(tag) => write!(fmt, "groups are not supported: {:02x}", tag),
            UnknownWireType(tag) => write!(
                fmt,
                "unsupported wire type in {:02x} of tag {:02x}",
                tag & 0x7,
                tag
            ),
            TooManyVarint32Bytes => write!(fmt, "too many bytes read for 32-bit varint"),
            TooManyVarint64Bytes => write!(fmt, "too many bytes read for 64-bit varint"),
            InvalidUtf8 => write!(fmt, "Invalid UTF8"),
            FailedMatcherNesting(offset, limit) => write!(
                fmt,
                "nested field was read until {}, should had ended at {}",
                offset, limit
            ),
        }
    }
}

impl std::error::Error for DecodingError {}

// a single method trait would allow easy extension adapters, still not 100% convinced this *can't*
// work but it'll take some iterations
pub trait Reader<'a> {
    type Returned: 'a;
    fn next(&mut self, buf: &mut &'a [u8])
        -> Result<Result<Self::Returned, Status>, DecodingError>;
}

/// Errors which can happen when reading from an std::io::Read.
#[derive(Debug)]
pub enum ReadError {
    /// More bytes could not be read from the source but were expected
    UnexpectedEndOfFile,
    /// Decoding the input failed
    Decoding(DecodingError),
    /// An IO error occured
    IO(std::io::Error),
}

impl fmt::Display for ReadError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ReadError::*;
        match self {
            UnexpectedEndOfFile => write!(fmt, "unexpected end of file"),
            Decoding(e) => write!(fmt, "decoding failed: {}", e),
            IO(e) => write!(fmt, "{}", e),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<DecodingError> for ReadError {
    fn from(e: DecodingError) -> Self {
        ReadError::Decoding(e)
    }
}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> Self {
        ReadError::IO(e)
    }
}
