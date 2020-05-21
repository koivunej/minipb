#![allow(dead_code)]

use std::convert::TryFrom;
use std::fmt;
use minipb::{ReadField, DecodingError, WireType, FieldId, Status, ReadError, Reader};
use minipb::matcher_fields::{MatcherFields, Matcher, Cont, Skip, SlicedMatched, SlicedValue, Value};
use minipb::gather_fields::{Slicer, ReaderGatheredFields};

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
    let stdin = stdin.lock();
    let leaf_type = path.leaf_type().clone();
    let matcher_fields = MatcherFields::new(
        PathMatcher::new(
            path.into_components(),
            leaf_type.clone()));

    let mut reader = ReaderGatheredFields::new(stdin, matcher_fields.as_sliced());
    let mut elements = 0;

    loop {
        match reader.next()? {
            Some(matched @ SlicedMatched { tag: Tag::Leaf, .. }) => {
                leaf_type.convert_to_stdout(matched.value)?;
                elements += 1;
            },
            Some(_) => {}
            None => break,
        }
    }

    eprintln!("{} elements read", elements);
    Ok(())
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
    fn convert_to_stdout(&self, value: SlicedValue<'_>) -> Result<(), ConversionError> {
        use LeafType::*;
        use SlicedValue::*;
        match (&self, value) {
            (LeafType::Slice, SlicedValue::Slice(_, slice))
            | (Debug, SlicedValue::Slice(_, slice)) => {
                for b in slice {
                    print!("{:02x}", b);
                }
                println!();
            },
            (Str, SlicedValue::Slice(range, slice)) => {
                match std::str::from_utf8(slice) {
                    Ok(s) => println!("{}", s),
                    Err(_) => return Err(ConversionError(Value::Slice(range), "invalid utf8")),
                }
            },
            (U64, Varint(x)) | (U64, Fixed64(x)) => println!("{}", x),
            (U64, Fixed32(x)) => println!("{}", x),
            (I64, Varint(_x)) | (I64, Fixed64(_x)) => {
                todo!("zigzag")
            }
            (F32, Fixed32(x)) => {
                println!("{}", f32::from_bits(x))
            },
            (F64, Fixed64(x)) => {
                println!("{}", f64::from_bits(x))
            },
            (Bool, Varint(x)) => println!("{}", x == 1),
            (Debug, value) => println!("{:?}", value),
            _ => todo!()
        }

        Ok(())
    }
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
