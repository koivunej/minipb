use crate::field_reader::FieldReader;
use crate::{DecodingError, FieldValue, ReadField, Status};
use std::borrow::Cow;

/// State machine one needs to write in order to know how to handle nested fields.
pub trait Matcher {
    /// Tag describing to caller how to process the field
    type Tag;

    /// Advance the matcher on a new field read.
    ///
    /// Returns the direction to take with the field with either Cont or Skip. Cont'd fields need
    /// to be tagged.
    fn decide_before(
        &mut self,
        offset: usize,
        read: &ReadField<'_>,
    ) -> Result<Cont<Self::Tag>, Skip>;

    /// Advance the matcher after a field has been processed. Depending on the return value this
    /// can be called many times in order for the Matcher ot highligh which objects have ended at
    /// this location with valueless tags.
    ///
    /// Return `(true, _)` if this method needs to be called again on the same offset, `(false, _)`
    /// otherwise.
    fn decide_after(&mut self, offset: usize) -> (bool, Option<Self::Tag>);
}

/// Uses an [`Matcher`] to match tagged fields from a [`FieldReader`].
pub struct MatcherFields<M: Matcher> {
    offset: u64,
    reader: FieldReader,
    matcher: M,
    state: State<M::Tag>,
}

enum State<T> {
    Ready,
    DecidingAfter,
    Gathering(T, u64, u64),
    Skipping(u64),
}

impl<M: Matcher> MatcherFields<M> {
    pub fn new(matcher: M) -> Self {
        Self {
            offset: 0,
            reader: FieldReader::default(),
            matcher,
            state: State::Ready,
        }
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn is_idle(&self) -> bool {
        match self.state {
            State::Ready => true,
            _ => false,
        }
    }

    pub fn next<'a>(
        &mut self,
        buf: &mut &'a [u8],
    ) -> Result<Result<Matched<'a, M::Tag>, Status>, DecodingError> {
        loop {
            match &self.state {
                State::DecidingAfter => {
                    let (again, maybe_tag) = self.matcher.decide_after(self.offset as usize);

                    if !again {
                        self.state = State::Ready;
                    }

                    if let Some(tag) = maybe_tag {
                        return Ok(Ok(Matched {
                            tag,
                            offset: self.offset,
                            value: Value::Marker,
                        }));
                    } else {
                        continue;
                    }
                }
                State::Gathering(_, _, amount) => {
                    if (buf.len() as u64) < *amount {
                        return Ok(Err(Status::NeedMoreBytes));
                    }

                    let amount = *amount;

                    let bytes = &buf[..amount as usize];
                    *buf = &buf[amount as usize..];
                    self.offset += bytes.len() as u64;
                    assert_eq!(bytes.len() as u64, amount);

                    // this trick is needed to avoid Matcher::Tag: Copy
                    let (tag, read_at) =
                        match std::mem::replace(&mut self.state, State::DecidingAfter) {
                            State::Gathering(tag, read_at, _) => (tag, read_at),
                            _ => unreachable!(),
                        };

                    let ret = Matched {
                        tag,
                        offset: read_at,
                        value: Value::Slice(Cow::Borrowed(bytes)),
                    };

                    self.state = State::DecidingAfter;

                    return Ok(Ok(ret));
                }
                State::Skipping(amount) => {
                    let amount = *amount;

                    if buf.len() as u64 >= amount {
                        *buf = &buf[amount as usize..];
                        self.offset += amount as u64;
                        self.state = State::Ready;
                    } else {
                        let available = buf.len();
                        *buf = &buf[buf.len()..];
                        self.offset += available as u64;
                        self.state = State::Skipping(amount - available as u64);
                        return Ok(Err(Status::NeedMoreBytes));
                    }
                }
                State::Ready => match self.reader.next(buf)? {
                    Err(s) => return Ok(Err(s)),
                    Ok(read) => {
                        let consumed = read.consumed();
                        let _decoded = &buf[..consumed];
                        *buf = &buf[consumed..];
                        let read_at = self.offset;
                        self.offset += consumed as u64;

                        let decision = self.matcher.decide_before(read_at as usize, &read);

                        let ret = match decision {
                            Ok(Cont::Message(maybe_tag)) => {
                                if let Some(tag) = maybe_tag {
                                    Matched {
                                        tag,
                                        offset: read_at,
                                        value: Value::Marker,
                                    }
                                } else {
                                    continue;
                                }
                            }
                            Ok(Cont::ReadSlice(tag)) => {
                                self.state =
                                    State::Gathering(tag, read_at, read.field_len() as u64);
                                continue;
                            }
                            Ok(Cont::ReadValue(tag)) => {
                                let value = match &read.field.value {
                                    FieldValue::Varint(x) => Value::Varint(*x),
                                    FieldValue::Fixed64(x) => Value::Fixed64(*x),
                                    FieldValue::Fixed32(x) => Value::Fixed32(*x),
                                    x => unreachable!("unexpected {:?}", x),
                                };

                                Matched {
                                    tag,
                                    offset: read_at,
                                    value,
                                }
                            }
                            Err(Skip) => {
                                let total = read.field_len();
                                self.state = State::Skipping(total as u64);
                                continue;
                            }
                        };

                        self.state = State::DecidingAfter;

                        return Ok(Ok(ret));
                    }
                },
            }
        }
    }

    pub fn into_matcher(self) -> M {
        self.matcher
    }
}

impl<M: Matcher + PartialEq> MatcherFields<M> {
    // TODO: something to advance to wanted matcher state
}

/// An item tagged by a [`Matcher`] from the stream of fields read by
/// [`MatcherFields`].
#[derive(Debug)]
pub struct Matched<'a, T> {
    pub tag: T,
    pub offset: u64,
    pub value: Value<'a>,
}

/// Instruction to process the field as follows, with the given tag.
#[derive(Debug)]
pub enum Cont<T> {
    /// Start processing the field as a nested message. Outputs the given tag to mark this.
    Message(Option<T>),
    /// Process the field as an opaque slice. Bytes will be buffered until there's at least this
    /// amount available. This will require the caller to buffer this much data.
    ReadSlice(T),
    // FIXME: here could be a ReadPartialSlice to stream bytes when they arrive, that will require
    // though cloneable tags, which wouldn't be a huge deal.
    /// Process the field as non-length delimited field with the given tag.
    ReadValue(T),
}

/// Represents a matched value.
#[derive(Debug)]
pub enum Value<'a> {
    /// Value does not exist in the stream, but it represents a state change taken by the
    /// [`Matcher`].
    Marker,
    /// Number read as a [`WireType::Varint`]
    Varint(u64),
    /// Value read as a [`WireType::Fixed64`]
    Fixed64(u64),
    /// Value read as a [`WireType::Fixed32`]
    Fixed32(u32),
    /// A length delimited field read as slice.
    Slice(Cow<'a, [u8]>),
}

/// Represents an instruction to skip the current field. Good default.
#[derive(Debug)]
pub struct Skip;
