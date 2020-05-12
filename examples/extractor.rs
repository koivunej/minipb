#![allow(dead_code)]

use std::convert::TryFrom;
use std::fmt;
use minipb::{ReadField, DecodingError, WireType, FieldId, Status};
use minipb::matcher_fields::{MatcherFields, Matcher, Cont, Skip, Matched, Value};
use minipb::gather_fields::Slicer;

/// Takes an argument like `/a/b/c::type` to navigate a (an unsigned integer) as submessage, to
/// navigate b as a submessage, pick field c, then convert to it to `type` or error. Return all
/// matches, separated by environment variable $LFS.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::env::args;
    use std::io::Read;

    let mut args = args();
    let myself = args.next().expect("zeroeth argument must be present");
    let path = args.next().map(|s| {
        let ret = Path::try_from(s.as_str());
        match ret {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{}: failed to parse {:?}: {}", myself, s, e);
                std::process::exit(1);
            }
        }
    });

    if path.is_some() && matches!(args.next(), Some(_)) {
        eprintln!("{}: only supported argument is the path to match on; please pipe input as standard input", myself);
        std::process::exit(1);
    }

    let path = match path {
        Some(p) => p,
        None => {
            eprintln!(
                "USAGE: {} <PATH>\n\n\
                Where: \n\
                PATH is a slash separated path of field identifiers to match on (you'll need to consult the proto files for this)\n\n\
                Input is read from stdin.",
                myself);
            std::process::exit(1);
        },
    };

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();

    let grow_buffer_by = 64;

    let mut buffer = Vec::with_capacity(grow_buffer_by);
    let leaf_type = path.leaf_type().clone();
    let mut matcher_fields = MatcherFields::new(
        PathMatcher::new(
            path.into_components(),
            leaf_type.clone()));

    let mut exhausted = true;
    let mut need_to_keep_buffer = 0;
    let mut eof_after_buffer = false;

    let mut max_buffering_needed = 0;
    let mut elements = 0;

    loop {
        if exhausted {
            exhausted = false;
            let kept = buffer.len();
            if buffer.len() == buffer.capacity() {
                buffer.extend(std::iter::repeat(0).take(grow_buffer_by));
            }
            for _ in buffer.len()..buffer.capacity() {
                buffer.push(0);
            }
            match stdin.read(&mut buffer[kept..])? {
                0 => {
                    // hit eof
                    break
                },
                x if x + need_to_keep_buffer == buffer.capacity() => {
                    // requested amount of bytes were read ok
                },
                y => {
                    eof_after_buffer = true;
                    // remove zeroes from the end
                    buffer.drain(need_to_keep_buffer + y..);
                }
            }
        }

        let mut buf = &buffer[..];

        match matcher_fields.next(&mut buf)? {
            Ok(matched @ Matched { tag: Tag::Leaf, .. }) => {
                // FIXME: needing to know how to play between the original buffer and the adjusted
                // buffer is ... huge effort. I just lost a lot of time again.
                leaf_type.convert_to_stdout(matcher_fields.slicer(&buffer[..buffer.len() - buf.len()]), matched.value)?;
                elements += 1;
            },
            Ok(_) => {
                // ignore markers and ignored
            }
            Err(Status::NeedMoreBytes) => {
                // currently the buffer ends in between of a field, or it's tag, or possibly the
                // field length.
                if eof_after_buffer {
                    panic!("unexpected EOF");
                }
                exhausted = true;
            },
            Err(Status::IdleAtEndOfBuffer) => {
                if eof_after_buffer {
                    // the reader got past last field and ran out of bytes before starting to read
                    // the next field.
                    break;
                }
                exhausted = true;
            }
        }

        // we need to keep this much of bytes to interpret the whole field. if the user wanted to
        // match a megabyte sized field, currently the only way to get past is to match the whole
        // megabyte field. allowing fields to be returned per slice might be quite easily added.
        let consumed_until = buf.len();
        let consumed = buffer.len() - consumed_until;

        // we can drain the no longer needed bytes.
        // FIXME: this should just move the window over the buffer, not drain
        buffer.drain(..consumed);
        need_to_keep_buffer = buffer.len();

        max_buffering_needed = max_buffering_needed.max(need_to_keep_buffer);
    }

    eprintln!("{} elements read", elements);
    eprintln!("{} buffer size", buffer.capacity());
    eprintln!("{} max buffer size needed", max_buffering_needed);
    Ok(())
}

struct GrowingBufRead<R> {
    inner: R,
    grow_by: usize,
    consumed: usize,
    buffer: Vec<u8>,
}

impl<R: std::io::Read> GrowingBufRead<R> {
    fn new(reader: R, grow_by: usize) -> Self {
        Self {
            inner: reader,
            grow_by,
            consumed: 0,
            buffer: Vec::with_capacity(grow_by),
        }
    }

    // what kind of api could this have... maybe_buffer?
    // consumed, like bufread?
}

struct ReaderMatcherFields<R, M: Matcher> {
    /// The wrapped reader
    reader: R,
    /// Growable byte buffer. Growth happens by `grow_by` amount at a time.
    buffer: Vec<u8>,
    /// Processes the bytes read into the buffer.
    matcher: MatcherFields<M>,
    /// The amount to grow the buffer by. It will need to be grown for larger fields, as there
    /// currently isn't a way to read fields as slices.
    grow_by: usize,
    /// When true, need to read more bytes
    exhausted: bool,
    /// When true, any bytes in the buffer represent the last bytes of the input stream.
    eof_after_buffer: bool,
}

impl<R, M: Matcher> ReaderMatcherFields<R, M> {
    fn match_from(matcher: M, reader: R) -> Self {
        let grow_by = 64;
        Self {
            buffer: Vec::with_capacity(grow_by),
            reader,
            matcher: MatcherFields::new(matcher),
            grow_by,
            exhausted: true,
            eof_after_buffer: false,
        }
    }

    fn next<'a>(&'a mut self) -> Result<Matched<M::Tag>, ReadError> {
        loop {
            // YES this seems to work but it might not work for gathered...
            self.buffer.drain(..);
            let mut buf = &self.buffer[..];
            match self.matcher.next(&mut buf)? {
                Ok(m) => return Ok(m),
                Err(Status::IdleAtEndOfBuffer) => continue,
                Err(Status::NeedMoreBytes) => continue,
            }
        }
    }
}

/*
use minipb::gather_fields::{Gatherer, GatheredFields};

struct ReaderGatheredFields<R, M: Matcher, G> {
    /// The wrapped reader
    reader: R,
    /// Growable byte buffer. Growth happens by `grow_by` amount at a time.
    buffer: Vec<u8>,
    /// Processes the bytes read into the buffer.
    matcher: GatheredFields<M, G>,
    /// The amount to grow the buffer by. It will need to be grown for larger fields, as there
    /// currently isn't a way to read fields as slices.
    grow_by: usize,
    /// When true, need to read more bytes
    exhausted: bool,
    /// When true, any bytes in the buffer represent the last bytes of the input stream.
    eof_after_buffer: bool,
}

impl<R, M: Matcher, G> ReaderGatheredFields<R, M, G>
    where for<'a> G: Gatherer<'a, Tag = M::Tag>,
          for<'a> <G as Gatherer<'a>>::Returned: fmt::Debug
{
    fn match_from(reader: R, matcher: M, gatherer: G) -> Self {
        let grow_by = 64;
        Self {
            buffer: Vec::with_capacity(grow_by),
            reader,
            matcher: GatheredFields::new(matcher, gatherer),
            grow_by,
            exhausted: true,
            eof_after_buffer: false,
        }
    }

    fn next<'b>(&'b mut self) -> Result<<G as Gatherer<'b>>::Returned, ReadError> {
        loop {
            // this works with nightly and -Zpolonius
            self.buffer.drain(..);
            let mut buf = &self.buffer[..];
            match self.matcher.next(&mut buf)? {
                Ok(m) => return Ok(m),
                Err(Status::IdleAtEndOfBuffer) => continue,
                Err(Status::NeedMoreBytes) => continue,
            }
        }
    }
}
*/

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

#[derive(Clone)]
enum LeafType {
    Slice,
    Str,
    U64,
    I64,
    F64,
    F32,
    Bool,
    Debug
}

#[derive(Debug)]
struct ConversionError(Value, &'static str);

impl fmt::Display for ConversionError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "conversion of {:?} failed: {}", self.0, self.1)
    }
}

impl std::error::Error for ConversionError {}

impl LeafType {
    fn convert_to_stdout(&self, slicer: Slicer<'_>, value: Value) -> Result<(), ConversionError> {
        use LeafType::*;
        match (&self, value) {
            (Slice, Value::Slice(range))
            | (Debug, Value::Slice(range)) => {
                let slice = slicer.as_slice(&range);
                for b in slice {
                    print!("{:02x}", b);
                }
                println!();
            },
            (Str, Value::Slice(range)) => {
                let slice = slicer.as_slice(&range);
                match std::str::from_utf8(slice) {
                    Ok(s) => println!("{}", s),
                    Err(_) => return Err(ConversionError(Value::Slice(range), "invalid utf8")),
                }
            },
            (U64, Value::Varint(x)) | (U64, Value::Fixed64(x)) => println!("{}", x),
            (U64, Value::Fixed32(x)) => println!("{}", x),
            (I64, Value::Varint(_x)) | (I64, Value::Fixed64(_x)) => {
                todo!("zigzag")
            }
            (F32, Value::Fixed32(x)) => {
                println!("{}", f32::from_bits(x))
            },
            (F64, Value::Fixed64(x)) => {
                println!("{}", f64::from_bits(x))
            },
            (Bool, Value::Varint(x)) => println!("{}", x == 1),
            (Debug, value) => println!("{:?}", value),
            _ => todo!()
        }

        Ok(())
    }

        /*
    fn accepts(&self, value: &Value) -> bool {
        use LeafType::*;
        match self {
            Slice | Str | Debug if value.field_len() > 0 => true,
            _ => false,
        }
    }
        */
}

impl TryFrom<&'_ str> for LeafType {
    type Error = ();
    fn try_from(s: &'_ str) -> Result<Self, Self::Error> {
        Ok(match s {
            "slice" | "bytes" => LeafType::Slice,
            "str" | "string" => LeafType::Str,
            "u64" | "uint64" | "u32" | "uint32" | "fixed32" | "fixed64" => LeafType::U64,
            "i64" | "sint64" | "i32" | "sint32" | "sfixed32" | "sfixed64" => LeafType::I64,
            "double" => LeafType::F64,
            "float" => LeafType::F32,
            "any" => LeafType::Debug,
            _ => return Err(())
        })
    }
}

struct Path(Vec<FieldId>, LeafType);

impl Path {
    fn leaf_type(&self) -> &LeafType {
        &self.1
    }

    fn into_components(self) -> Vec<FieldId> {
        self.0
    }
}

#[derive(Debug)]
enum PathParseError<'a> {
    InvalidField(&'a str),
    MissingLeafType,
    UnsupportedLeafType(&'a str),
    Empty,
}

impl fmt::Display for PathParseError<'_> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        use PathParseError::*;
        match self {
            InvalidField(field) => write!(fmt, "invalid field: {:?}", field),
            MissingLeafType => write!(fmt, "path ended in double colon but no type is missing"),
            UnsupportedLeafType(leaf_type) => write!(fmt, "unsupported leaf type: {:?}", leaf_type),
            Empty => write!(fmt, "no path specified"),
        }
    }
}

impl std::error::Error for PathParseError<'_> {}

impl<'a> TryFrom<&'a str> for Path {
    type Error = PathParseError<'a>;

    fn try_from(s: &'a str) -> Result<Self, Self::Error> {
        let mut split = s.split('/').enumerate().peekable();
        let mut components = Vec::new();

        while let Some((i, maybe_last)) = split.next() {
            let is_last = split.peek().is_none();

            if !is_last {
                if i == 0 && maybe_last.is_empty() {
                    // allow paths like `a/b/c` in addition to `/a/b/c`
                    continue;
                }
                // this should only be an FieldId, no subscripts yet
                let id = maybe_last.parse::<FieldId>()
                    .map_err(|_| PathParseError::InvalidField(maybe_last))?;
                components.push(id);
            } else {
                // there may be the "cast"
                let mut split = maybe_last.split("::");
                let last = split.next().expect("there is always the first element");

                let last = last.parse::<FieldId>()
                    .map_err(|_| PathParseError::InvalidField(last))?;

                let leaf_type = match split.next() {
                    Some("") => return Err(PathParseError::MissingLeafType),
                    Some(leaf_type) => LeafType::try_from(leaf_type)
                        .map_err(|_| PathParseError::UnsupportedLeafType(leaf_type))?,
                    None => LeafType::Debug,
                };

                components.push(last);

                return Ok(Path(components, leaf_type));
            }
        }

        Err(PathParseError::Empty)
    }
}

/// Matches the "fields delimited by slash" path syntax. Is not able to do the "to leaftype"
/// conversion at least yet.
struct PathMatcher {
    /// What we want to find
    path: Vec<FieldId>,
    /// Leaf type we want to find, other parts of the path are submessages
    leaf_type: LeafType,
    /// Stacked ending offsets for the matched path elements
    position: Vec<usize>,

}

impl PathMatcher {
    fn new(path: Vec<FieldId>, leaf_type: LeafType) -> PathMatcher {
        let position = Vec::with_capacity(path.len() - 1);
        PathMatcher {
            path,
            leaf_type,
            position,
        }
    }
}

#[derive(Debug)]
enum Tag {
    Start,
    Leaf,
    End,
    Ignored,
    UnexpectedLeafType(WireType),
}

impl Matcher for PathMatcher {
    type Tag = Tag;

    fn decide_before(
        &mut self,
        offset: usize,
        read: &ReadField<'_>,
    ) -> Result<Result<Cont<Tag>, Skip<Tag>>, DecodingError> {
        use LeafType::*;

        let depth = self.position.len();
        let leaves = self.path.len() - 1;

        let decision = if depth == leaves {
            if read.field_id() == self.path[depth] {
                match &self.leaf_type {
                    Debug | Slice | Str if read.is_length_delimited() => Ok(Cont::ReadSlice(Tag::Leaf)),
                    _ if !read.is_length_delimited() => Ok(Cont::ReadValue(Tag::Leaf)),
                    _ => Err(Skip(Tag::UnexpectedLeafType(read.wire_type()))),
                }
            } else {
                Err(Skip(Tag::Ignored))
            }
        } else {
            if read.field_id() == self.path[depth] {
                if read.field_len() > 0 {
                    // FIXME: this offset + bytes_to_skip needs to be easier to handle
                    self.position.push(offset + read.bytes_to_skip());
                    Ok(Cont::Message(Some(Tag::Start)))
                } else {
                    Err(Skip(Tag::Ignored))
                }
            } else{
                Err(Skip(Tag::Ignored))
            }
        };

        // println!("offset = {}, {:?} ==> {:?}, path = {:?}", offset, read, decision, self.position);
        Ok(decision)
    }

    fn decide_after(&mut self, offset: usize) -> (bool, Option<Tag>) {
        match self.position.last() {
            Some(x) if *x == offset => {
                self.position.pop();
                (matches!(self.position.last(), Some(x) if *x == offset), Some(Tag::End))
            },
            Some(x) => {
                assert!(*x > offset, "got up to {} but should had stopped at {}", offset, x);
                (false, None)
            },
            None => (false, None)
        }
    }
}
