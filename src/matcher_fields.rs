use crate::field_reader::FieldReader;
use crate::{DecodingError, FieldValue, ReadField, Slicer, Status};
use std::ops::Range;

/// State machine one needs to write in order to know how to handle nested fields.
pub trait Matcher {
    /// Tag describing to caller how to process the field
    type Tag: 'static;

    /// Advance the matcher on a new field read.
    ///
    /// Returns the direction to take with the field with either Cont or Skip. Cont'd fields need
    /// to be tagged.
    fn decide_before(
        &mut self,
        offset: usize,
        read: &ReadField<'_>,
    ) -> Result<Result<Cont<Self::Tag>, Skip<Self::Tag>>, DecodingError>;

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

#[derive(Debug)]
enum State<T> {
    /// Initial state and where we can also stop because of EOF.
    Ready,
    /// Entered after processing a field value (nested message, slice, or scalar) and stayed as
    /// long as matcher.decide_after returns `(true, _)` as multiple nested messages will stop on
    /// the same byte offset.
    DecidingAfter,
    /// Entered to buffer up a complete slice (bytes or str).
    Buffering(T, u64, u64, u64),
    /// Skipping a complete field, which can be long.
    Skipping(T, u64, u64, u64),
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

    fn advance<'a>(
        &mut self,
        buf: &mut &'a [u8],
    ) -> Result<Result<Option<Matched<M::Tag>>, Status>, DecodingError> {
        match &mut self.state {
            State::Ready => match self.reader.next(buf)? {
                Err(s) => Ok(Err(s)),
                Ok(read) => {
                    let consumed = read.consumed();
                    let _decoded = &buf[..consumed];
                    *buf = &buf[consumed..];
                    let read_at = self.offset;
                    self.offset += consumed as u64;

                    // when possibly going deeper, only one decision is enough.
                    let decision = self.matcher.decide_before(read_at as usize, &read)?;

                    let ret = match decision {
                        Ok(Cont::Message(maybe_tag)) => maybe_tag.map(|tag| Matched {
                            tag,
                            offset: read_at,
                            value: Value::Marker,
                        }),
                        Ok(Cont::ReadValue(tag)) => {
                            // why isn't this a move? because FieldReader owns the FieldInfo
                            let value = match &read.field.value {
                                FieldValue::Varint(x) => Value::Varint(*x),
                                FieldValue::Fixed64(x) => Value::Fixed64(*x),
                                FieldValue::Fixed32(x) => Value::Fixed32(*x),
                                x => panic!("{:?} returned for a length delimited field which is invalid. Either Cont::ReadSlice or Skip the field.", x),
                            };

                            Some(Matched {
                                tag,
                                offset: read_at,
                                value,
                            })
                        }
                        Ok(Cont::ReadSlice(tag)) => {
                            self.state = State::Buffering(
                                tag,
                                read_at,
                                self.offset,
                                read.field_len() as u64,
                            );
                            return Ok(Ok(None));
                        }
                        Err(Skip(tag)) => {
                            let total = read.field_len();
                            self.state = State::Skipping(tag, read_at, self.offset, total as u64);
                            return Ok(Ok(None));
                        }
                    };

                    self.state = State::DecidingAfter;

                    Ok(Ok(ret))
                }
            },
            State::DecidingAfter => {
                let (again, maybe_tag) = self.matcher.decide_after(self.offset as usize);

                if !again {
                    // the `again` would had been true if multiple levels of nested messages ended
                    // at the same byte
                    self.state = State::Ready;
                }

                if let Some(tag) = maybe_tag {
                    Ok(Ok(Some(Matched {
                        tag,
                        offset: self.offset,
                        value: Value::Marker,
                    })))
                } else {
                    Ok(Ok(None))
                }
            }
            State::Buffering(_, _, _, amount) => {
                if (buf.len() as u64) < *amount {
                    // TODO: it'd be great to tell how many we are expecting, a size hint, so that
                    // the caller could bail out on too large payloads.
                    return Ok(Err(Status::NeedMoreBytes));
                }

                let amount = *amount;

                let bytes = &buf[..amount as usize];
                assert_eq!(bytes.len() as u64, amount);

                *buf = &buf[amount as usize..];
                self.offset += amount as u64;

                // this trick is needed to avoid Matcher::Tag: Copy
                let (tag, read_at, start) =
                    match std::mem::replace(&mut self.state, State::DecidingAfter) {
                        State::Buffering(tag, read_at, start, _) => (tag, read_at, start),
                        _ => unreachable!(),
                    };

                let ret = Matched {
                    tag,
                    offset: read_at,
                    value: Value::Slice(start..self.offset),
                };

                self.state = State::DecidingAfter;

                Ok(Ok(Some(ret)))
            }
            State::Skipping(_, _, _, amount) => {
                let amt = *amount;

                let skipped = amt.min(buf.len() as u64);

                self.offset += skipped as u64;
                *buf = &buf[skipped as usize..];

                let remaining = amt - skipped;

                if remaining == 0 {
                    let (tag, read_at, start) =
                        match std::mem::replace(&mut self.state, State::DecidingAfter) {
                            State::Skipping(tag, read_at, start, _) => (tag, read_at, start),
                            _ => unreachable!(),
                        };
                    let ret = Matched {
                        tag,
                        offset: read_at,
                        value: Value::Slice(start..self.offset),
                    };
                    return Ok(Ok(Some(ret)));
                }

                *amount = remaining;

                // TODO: again, a size hint wouldn't hurt, especially if the user is reading from
                // std::io::Seek or similar; these could just be not read at all.
                Ok(Err(Status::NeedMoreBytes))
            }
        }
    }

    pub fn into_parts(self) -> (u64, M) {
        (self.offset, self.matcher)
    }

    /// Needs to be called with **the buffer before the previous call** to `next` has advanced it.
    pub fn slicer<'a>(&self, buf: &'a [u8]) -> Slicer<'a> {
        Slicer::wrap(buf, self.offset)
    }

    pub fn into_sliced(self) -> SlicedMatcherFields<M> {
        SlicedMatcherFields { inner: self }
    }
}

impl<'a, M: Matcher> crate::Reader<'a> for MatcherFields<M> {
    type Returned = Matched<M::Tag>;

    fn next(
        &mut self,
        buf: &mut &'a [u8],
    ) -> Result<Result<Matched<M::Tag>, Status>, DecodingError> {
        loop {
            match self.advance(buf)? {
                Ok(Some(m)) => return Ok(Ok(m)),
                Ok(None) => continue,
                Err(e) => return Ok(Err(e)),
            }
        }
    }
}

/// MatcherFields but will return `SlicedMatched` instead of `Matched`.
pub struct SlicedMatcherFields<M: Matcher> {
    inner: MatcherFields<M>,
}

impl<'a, M: Matcher> crate::Reader<'a> for SlicedMatcherFields<M> {
    type Returned = SlicedMatched<'a, M::Tag>;

    fn next(
        &mut self,
        buf: &mut &'a [u8],
    ) -> Result<Result<SlicedMatched<'a, M::Tag>, Status>, DecodingError> {
        // store for later slicing
        let orig: &'a [u8] = *buf;
        match self.inner.next(buf)? {
            Ok(Matched {
                tag,
                offset,
                value: Value::Slice(range),
            }) => {
                let slicer = self.inner.slicer(&orig[..(orig.len() - buf.len())]);
                let bytes = slicer.as_slice(&range);

                Ok(Ok(SlicedMatched {
                    tag,
                    offset,
                    value: SlicedValue::Slice(range, bytes),
                }))
            }
            Ok(Matched { tag, offset, value }) => Ok(Ok(SlicedMatched {
                tag,
                offset,
                value: match value {
                    Value::Marker => SlicedValue::Marker,
                    Value::Varint(x) => SlicedValue::Varint(x),
                    Value::Fixed64(x) => SlicedValue::Fixed64(x),
                    Value::Fixed32(x) => SlicedValue::Fixed32(x),
                    Value::Slice(_) => unreachable!("already matched it in an earlier arm"),
                },
            })),
            Err(e) => Ok(Err(e)),
        }
    }
}

/// An item tagged by a [`Matcher`] from the stream of fields read by
/// [`MatcherFields`].
#[derive(Debug)]
pub struct Matched<T> {
    pub tag: T,
    pub offset: u64,
    pub value: Value,
}

/// An item tagged by a [`Matcher`] from the stream of fields read by
/// [`MatcherFields`] with Value::Slice turned into a byte slice.
#[derive(Debug)]
pub struct SlicedMatched<'a, T> {
    pub tag: T,
    pub offset: u64,
    pub value: SlicedValue<'a>,
}

/// Represents a matched value.
#[derive(Debug, Clone)]
pub enum Value {
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
    Slice(Range<u64>),
}

/// Represents a sliced matched value.
#[derive(Debug, Clone)]
pub enum SlicedValue<'a> {
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
    Slice(Range<u64>, &'a [u8]),
}

impl From<SlicedValue<'_>> for Value {
    fn from(sv: SlicedValue<'_>) -> Self {
        match sv {
            SlicedValue::Marker => Self::Marker,
            SlicedValue::Varint(x) => Self::Varint(x),
            SlicedValue::Fixed64(x) => Self::Fixed64(x),
            SlicedValue::Fixed32(x) => Self::Fixed32(x),
            SlicedValue::Slice(range, _) => Self::Slice(range),
        }
    }
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

impl Value {
    pub fn slice_len(&self) -> Result<usize, ()> {
        match self {
            Value::Slice(Range { start, end }) => Ok((end - start) as usize),
            _ => Err(()),
        }
    }
}

/// Represents an instruction to skip the current field. Good default.
#[derive(Debug)]
pub struct Skip<T>(pub T);
