use std::convert::TryFrom;
use minipb::{ReadField, DecodingError, WireType, FieldId};
use minipb::matcher_fields::{Matcher, Cont, Skip, Value};

/// Takes an argument like `/a/b/c::type` to navigate a (an unsigned integer) as
/// submessage, to navigate b as a submessage, pick field c (hopefully a fixed32 wiretype), then
/// convert to it to `type` or error. Return all matches, separated by environment variable $LFS.
fn main() {

}

enum LeafType {
    Slice,
    Str,
    U64,
    I64,
    F64,
    F32,
    Bool
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
            _ => return Err(())
        })
    }
}

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
        use LeafType::Slice;

        let depth = self.position.len();

        let leaves = self.path.len() - 1;

        Ok(if depth == leaves {
            match &self.leaf_type {
                Slice if read.field_len() > 0 => Ok(Cont::ReadSlice(Tag::Leaf)),
                _ if read.field_len() == 0 => Ok(Cont::ReadValue(Tag::Leaf)),
                _ => Err(Skip(Tag::UnexpectedLeafType(read.wire_type()))),
            }
        } else {
            if read.field_id() == self.path[depth + 1] {
                if read.field_len() > 0 {
                    self.position.push(read.field_len() as usize);
                    Ok(Cont::Message(Some(Tag::Start)))
                } else {
                    Err(Skip(Tag::Ignored))
                }
            } else{
                Err(Skip(Tag::Ignored))
            }
        })
    }

    fn decide_after(&mut self, offset: usize) -> (bool, Option<Tag>) {
        if matches!(self.position.last(), Some(x) if *x == offset) {
            self.position.pop();
            (matches!(self.position.last(), Some(x) if *x == offset), Some(Tag::End))
        } else {
            (false, None)
        }
    }
}
