use std::convert::TryFrom;
use std::fmt;

pub mod field_reader;
pub mod gather_fields;
pub mod matcher_fields;

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
enum FieldValue {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    /// Length delimited data starts after two varints, tag and length. This offset points to the
    /// byte which starts the actual field, and continues for the length specified in the
    /// `FieldInfo::offset`.
    DataLength(u32),
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

/*
trait BlockingIOExt {
    // maybe two errors wasn't so good of an idea after all?
    fn from_read<R>(self, read: R) -> ReadWrapper<Self, R>
        where Self: Sized,
              R: std::io::Read;
}

impl<'a, It: Reader<'a>> BlockingIOExt for It {
    fn from_read<R>(self, read: R) -> ReadWrapper<Self, R>
        where Self: Sized,
              R: std::io::Read
    {
        todo!()
    }
}*/

/*
pub struct ReadWrapper<R, IO> {
    reader: R,
    inner: IO,
    buffer: Vec<u8>,
    grow_buffer_by: usize,
    need_to_keep_buffer: usize,
    exhausted: bool,
    eof_after_buffer: bool,
}

impl<IO: std::io::Read, R> ReadWrapper<R, IO>
    where for<'a> R: Reader<'a>
{
    pub fn new(reader: R, inner: IO, initial_buffer: usize) -> Self {
        Self {
            reader,
            inner,
            buffer: vec![0; initial_buffer],
            grow_buffer_by: initial_buffer,
            need_to_keep_buffer: 0,
            exhausted: true,
            eof_after_buffer: false,
        }
    }

    fn advance<'a>(&'a mut self) -> Result<Progress<<R as Reader<'a>>::Returned>, ReadError> {
        use std::iter::repeat;

        if self.exhausted {
            self.exhausted = false;
            let kept = self.buffer.len();
            if self.buffer.len() == self.buffer.capacity() {
                self.buffer.extend(repeat(0).take(self.grow_buffer_by));
            }
            for _ in self.buffer.len()..self.buffer.capacity() {
                self.buffer.push(0);
            }
            match self.inner.read(&mut self.buffer[kept..]) {
                Ok(0) => {
                    // hit eof
                    return Ok(Progress::EOF);
                },
                Ok(x) if x + self.need_to_keep_buffer == self.buffer.capacity() => {
                    // requested amount of bytes were read ok
                },
                Ok(y) => {
                    self.eof_after_buffer = true;
                    // remove zeroes from the end
                    self.buffer.drain(self.need_to_keep_buffer + y..);
                },
                Err(e) => return Err(ReadError::from(e)),
            }
        }

        let mut buf = &self.buffer[..];

        match self.reader.next(&mut buf)? {
            Ok(x) => {
                return Ok(Progress::Value(x));
            },
            Err(Status::NeedMoreBytes) => {
                // prepare to fill buffer on next round
                self.exhausted = true;
                if self.eof_after_buffer {
                    return Err(ReadError::UnexpectedEndOfFile);
                }
                return Ok(Progress::CallAgain);
            },
            Err(Status::IdleAtEndOfBuffer) => {
                if self.eof_after_buffer {
                    return Ok(Progress::EOF);
                }
                return Ok(Progress::CallAgain);
            },
        }
    }

    fn next<'a>(&'a mut self) -> Result<Progress<<R as Reader<'a>>::Returned>, ReadError> {
        loop {
            // ERROR: cannot borrow *self as mutable more than once at a time
            match self.advance() {
                Ok(Progress::CallAgain) => continue,
                Ok(p) => return Ok(p),
                Err(e) => return Err(e),
            }
        }
    }
}

/// Sad type required as no workaround for limiting the lifetime of `buf` inside `advance`.
enum Progress<T> {
    Value(T),
    CallAgain,
    EOF,
}



#[derive(Debug)]
enum ReadError {
    UnexpectedEndOfFile,
    Decoding(DecodingError),
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
*/
