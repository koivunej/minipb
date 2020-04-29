#![warn(rust_2018_idioms)]
#![allow(dead_code, unused)]

use std::io::{self, BufRead, Read};
use std::fmt;
use std::convert::TryFrom;

struct HexOnly<'a>(&'a [u8]);

impl<'a> fmt::Debug for HexOnly<'a> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(fmt, "{:02x}", byte)?;
        }
        Ok(())
    }
}

// 2 -> 1' hash
// 2 -> 2' name
// 2 -> 3' tsize

fn main() -> Result<(), Box<dyn std::error::Error + 'static>> {
    let stdin = std::io::stdin();
    let mut locked = stdin.lock();

    let mut buffer = Vec::new();
    locked.read_to_end(&mut buffer)?;

    let mut reader = FieldReader::default();

    let mut buf = &buffer[..];
    let mut offset = 0;

    #[derive(Debug)]
    enum State {
        Top,
        Link { until: usize },
    }

    #[derive(Debug)]
    enum InterestingField {
        PbLinkHash,
        PbLinkName,
        PbLinkTotalSize,
    }

    impl Matcher for State {
        type Tag = InterestingField;

        fn decide_before(&mut self, offset: usize, read: &ReadField<'_>) -> Result<Cont<Self::Tag>, Skip> {
            //println!("decide({:?}, {}, {:?})", self, offset, read);
            match self {
                State::Top if read.field.id == 2 => {
                    *self = State::Link { until: offset + read.bytes_to_skip() };
                    return Ok(Cont::Message)
                },
                State::Top => {},
                State::Link { until } => {
                    assert!(offset < *until, "got up to {} but should had stopped at {}", until, offset);

                    return match read.field.id {
                        1 => Ok(Cont::ReadVec(InterestingField::PbLinkHash)),
                        2 => Ok(Cont::ReadStr(InterestingField::PbLinkName)),
                        3 => Ok(Cont::ReadValue(InterestingField::PbLinkTotalSize)),
                        _ => Err(Skip),
                    };
                },
                _ => unreachable!()
            }

            Err(Skip)
        }

        fn decide_after(&mut self, offset: usize) {
            // println!("decide_at({:?}, {})", self, offset);
            match self {
                State::Link { until } if offset == *until => {
                    *self = State::Top;
                },
                _ => {}
            }
        }
    }

    let mut state = State::Top;

    let mut fm = MatcherFields::new(State::Top);

    loop {
        match fm.next(&mut buf)? {
            Ok(matched) => println!("{:?}", matched),
            Err(x) => panic!("{:?}", x),
        }
    }

    Ok(())
}

trait Matcher {
    type Tag;
    fn decide_before(&mut self, offset: usize, read: &ReadField<'_>) -> Result<Cont<Self::Tag>, Skip>;
    fn decide_after(&mut self, offset: usize);
}

struct MatcherFields<M> {
    offset: u64,
    reader: FieldReader,
    matcher: M,
    state: State,
}

enum State {
    Ready,
    Gathering(u64),
    Skipping(u64),
}

impl<M: Matcher> MatcherFields<M> {
    fn new(matcher: M) -> Self {
        Self {
            offset: 0,
            reader: FieldReader::default(),
            matcher,
            state: State::Ready,
        }

    }

    fn next<'a>(&mut self, buf: &mut &'a [u8]) -> Result<Result<Matched<'a, M::Tag>, Status>, DecodingError> {
        loop {
            match self.reader.next(buf)? {
                Err(Status::IdleAtEndOfBuffer) => unreachable!(),
                Err(Status::NeedMoreBytes) => unreachable!(),
                Ok(read) => {

                    let consumed = read.consumed();
                    let decoded = &buf[..consumed];
                    *buf = &buf[consumed..];
                    let read_at = self.offset;
                    self.offset += consumed as u64;

                    //println!("{:?}:", HexOnly(decoded));
                    //println!("offset={:<8x} field={:?}", offset, read.field.id);

                    let decision = self.matcher.decide_before(read_at as usize, &read);

                    //println!("decision = {:?} and {:?}", decision, state);

                    match decision {
                        Ok(Cont::Message) => {},
                        Ok(Cont::ReadVec(tag)) => {
                            let bytes = &buf[..read.field_len()];
                            *buf = &buf[bytes.len()..];
                            self.offset += bytes.len() as u64;
                            /*println!("{:indent$}{:<4} {:?}", "", bytes.len(), HexOnly(bytes), indent = 8);*/
                            assert_eq!(bytes.len(), read.field_len());
                            self.matcher.decide_after(self.offset as usize);

                            return Ok(Ok(Matched {
                                tag,
                                offset: read_at,
                                value: Value::Slice(bytes),
                            }));
                        },
                        Ok(Cont::ReadStr(tag)) => {
                            let bytes = &buf[..read.field_len()];
                            *buf = &buf[bytes.len()..];
                            self.offset += bytes.len() as u64;
                            /*if let Ok(s) = std::str::from_utf8(bytes) {
                                println!("{:indent$}{:?}", "", s, indent = 8);
                            } else {
                                println!("{:indent$}{:?}", "", HexOnly(bytes), indent = 8);
                            }*/
                            self.matcher.decide_after(self.offset as usize);

                            return Ok(Ok(Matched {
                                tag,
                                offset: read_at,
                                value: Value::Slice(bytes)
                            }));
                        },
                        Ok(Cont::ReadValue(tag)) => {
                            let value = match &read.field.value {
                                FieldValue::Varint(x) => Value::Varint(*x),
                                FieldValue::Fixed64(x) => Value::Fixed64(*x),
                                FieldValue::Fixed32(x) => Value::Fixed32(*x),
                                x => unreachable!("unexpected {:?}", x),
                            };

                            self.matcher.decide_after(self.offset as usize);
                            // println!("{:indent$}{:?}", "", value, indent = 8);

                            return Ok(Ok(Matched {
                                tag,
                                offset: read_at,
                                value
                            }));
                        },
                        Err(Skip) => {
                            let skipped = read.field_len();
                            *buf = &buf[..skipped];

                            //println!("{:indent$}skipped {}", "", skipped, indent = 8);
                        }
                    }
                    // how to know we are now dropping multiple levels?
                },
            }
        }
    }
}

#[derive(Debug)]
struct Matched<'a, T> {
    tag: T,
    offset: u64,
    value: Value<'a>,
}

#[derive(Debug)]
enum Cont<T> {
    Message,
    ReadStr(T),
    ReadVec(T),
    ReadValue(T),
}

#[derive(Debug)]
enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Slice(&'a [u8]),
}

#[derive(Debug)]
struct Skip;

pub type FieldId = u32;

#[derive(Debug, Clone, Copy, PartialEq)]
enum WireType {
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

struct LengthDelimited;

/// All of the bytes still remaining in the buffer need to be kept, but more bytes should be read.
#[derive(Debug)]
struct NeedMoreBytes;

#[derive(Debug)]
enum Status {
    /// Would like to read the next item but there are no more bytes in the buffer. This could be
    /// because the input has been fully exhausted (end of file).
    IdleAtEndOfBuffer,
    /// Reading a variable length integer, for example the field id and type or the value or the
    /// length.
    NeedMoreBytes,
}


#[derive(Debug)]
struct ReadField<'a> {
    /// How many bytes were consumed from the beginning of the buffer
    consumed: usize,
    /// The actual read field, which can be used to skip the field.
    field: &'a FieldInfo
}

impl ReadField<'_> {
    fn consumed(&self) -> usize {
        self.consumed
    }

    fn bytes_to_skip(&self) -> usize {
        self.consumed + self.field.bytes_to_skip()
    }

    fn field_len(&self) -> usize {
        self.field.bytes_to_skip()
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

#[derive(Default)]
struct FieldReader {
    field: Option<FieldInfo>,
}

impl FieldReader {
    /// Reads the first bytes as any field. After returning a length delimited field, the data must
    /// be skipped for 'ReadField::bytes_to_skip` to avoid interpreting the field as a nested message.
    fn next<'a>(&'a mut self, data: &[u8]) -> Result<Result<ReadField<'a>, Status>, DecodingError> {
        macro_rules! launder {
            ($x:expr) => {
                match $x {
                    Ok(x) => x,
                    Err(NeedMoreBytes) => return Ok(Err(Status::NeedMoreBytes)),
                }
            }
        }

        let (consumed, tag) = launder!(read_varint32(data)?);

        let data = &data[consumed..];

        let field = tag >> 3;
        let kind = WireType::try_from(tag)?;

        let (additional, value) = match &kind {
            WireType::Varint => {
                let (consumed, val) = launder!(read_varint64(data)?);
                (consumed, FieldValue::Varint(val))
            },
            WireType::Fixed32 => {
                let (consumed, val) = launder!(read_fixed32(data));
                (consumed, FieldValue::Fixed32(val))
            },
            WireType::Fixed64 => {
                let (consumed, val) = launder!(read_fixed64(data));
                (consumed, FieldValue::Fixed64(val))
            },
            WireType::LengthDelimited => {
                let (consumed, len) = launder!(read_varint32(data)?);
                (consumed, FieldValue::DataLength(len))
            }
        };

        let consumed = consumed + additional;
        let data = &data[consumed..];

        self.field = Some(FieldInfo {
            id: field,
            kind,
            value
        });

        let field = self.field.as_ref().unwrap();

        Ok(Ok(ReadField {
            consumed,
            field
        }))
    }
}

fn read_varint32(data: &[u8]) -> Result<Result<(usize, u32), NeedMoreBytes>, DecodingError> {
    match read_varint(data, 5)? {
        Ok((bytes, val)) => Ok(Ok((bytes, val as u32))),
        Err(e) => Ok(Err(e)),
    }
}

fn read_varint64(data: &[u8]) -> Result<Result<(usize, u64), NeedMoreBytes>, DecodingError> {
    read_varint(data, 10)
}

fn read_fixed32(data: &[u8]) -> Result<(usize, u32), NeedMoreBytes> {
    if data.len() < 4 {
        Err(NeedMoreBytes)
    } else {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&data[..4]);
        Ok((4, u32::from_le_bytes(bytes)))
    }
}

fn read_fixed64(data: &[u8]) -> Result<(usize, u64), NeedMoreBytes> {
    if data.len() < 8 {
        Err(NeedMoreBytes)
    } else {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&data[..8]);
        Ok((8, u64::from_le_bytes(bytes)))
    }
}

fn read_varint(data: &[u8], max_bytes: usize) -> Result<Result<(usize, u64), NeedMoreBytes>, DecodingError> {
    let mask = 0x7f;

    let mut val = 0u64;

    let mut count = 0;

    for b in data.iter().take(max_bytes) {
        val |= ((b & mask) as u64) << count * 7;
        count += 1;

        if b & 0x80 == 0 {
            return Ok(Ok((count, val)));
        }
    }

    if count < max_bytes {
        Ok(Err(NeedMoreBytes))
    } else if max_bytes == 4 {
        Err(DecodingError::TooManyVarint32Bytes)
    } else {
        Err(DecodingError::TooManyVarint64Bytes)
    }
}

#[derive(Debug)]
enum DecodingError {
    UnsupportedGroupWireType(u32),
    UnknownWireType(u32),
    TooManyVarint32Bytes,
    TooManyVarint64Bytes,
    InvalidUtf8
}

impl fmt::Display for DecodingError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        use DecodingError::*;
        match *self {
            UnsupportedGroupWireType(tag) => write!(fmt, "groups are not supported: {:02x}", tag),
            UnknownWireType(tag) => write!(fmt, "unsupported wire type in {:02x} of tag {:02x}", tag & 0x7, tag),
            TooManyVarint32Bytes => write!(fmt, "too many bytes read for 32-bit varint"),
            TooManyVarint64Bytes => write!(fmt, "too many bytes read for 64-bit varint"),
            InvalidUtf8 => write!(fmt, "Invalid UTF8"),
        }
    }
}

impl std::error::Error for DecodingError {}

#[derive(Debug)]
enum Error {
    Decoding(DecodingError),
    IO(std::io::Error),
}

impl From<DecodingError> for Error {
    fn from(e: DecodingError) -> Self {
        Self::Decoding(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::IO(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Error::*;
        match *self {
            Decoding(ref e) => write!(fmt, "Decoding: {:?}", e),
            IO(ref e) => write!(fmt, "IO: {}", e),
        }
    }
}

impl std::error::Error for Error {}
