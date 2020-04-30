use crate::field_reader::FieldReader;
use crate::{DecodingError, FieldValue, ReadField, Status};
use std::ops::Range;
use std::fmt;

/// State machine one needs to write in order to know how to handle nested fields.
pub trait Matcher {
    /// Tag describing to caller how to process the field
    type Tag: fmt::Debug;

    /// Advance the matcher on a new field read.
    ///
    /// Returns the direction to take with the field with either Cont or Skip. Cont'd fields need
    /// to be tagged.
    fn decide_before(
        &mut self,
        offset: usize,
        read: &ReadField<'_>,
    ) -> Result<Cont<Self::Tag>, Skip<Self::Tag>>;

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
    Ready,
    DecidingAfter,
    Gathering(T, u64, u64, u64),
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
        //println!("{:<4} {:<4} {:?}", self.offset, buf.len(), self.state);
        match &mut self.state {
            State::DecidingAfter => {
                let (again, maybe_tag) = self.matcher.decide_after(self.offset as usize);

                if !again {
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
            State::Gathering(_, _, _, amount) => {
                if (buf.len() as u64) < *amount {
                    //println!("    => need {} more bytes", *amount - buf.len() as u64);
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
                        State::Gathering(tag, read_at, start, _) => (tag, read_at, start),
                        _ => unreachable!(),
                    };

                let ret = Matched {
                    tag,
                    offset: read_at,
                    value: Value::Slice(start..self.offset),
                };

                self.state = State::DecidingAfter;
                //println!("    => {:?} and returning Ok(Ok({:?}))", self.state, ret);

                return Ok(Ok(Some(ret)));
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
                    //println!("    => {:?} and returning Ok(Ok({:?}))", self.state, ret);
                    return Ok(Ok(Some(ret)));
                }

                *amount = remaining;
                return Ok(Err(Status::NeedMoreBytes));
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
                    //println!("    => decision before {:?}", decision);

                    let ret = match decision {
                        Ok(Cont::Message(maybe_tag)) => {
                            //println!("    => starting submessage with buf.len() = {}", buf.len());
                            maybe_tag.map(|tag| Matched {
                                tag,
                                offset: read_at,
                                value: Value::Marker,
                            })
                        }
                        Ok(Cont::ReadValue(tag)) => {
                            let value = match &read.field.value {
                                FieldValue::Varint(x) => Value::Varint(*x),
                                FieldValue::Fixed64(x) => Value::Fixed64(*x),
                                FieldValue::Fixed32(x) => Value::Fixed32(*x),
                                x => unreachable!("unexpected {:?}", x),
                            };

                            Some(Matched {
                                tag,
                                offset: read_at,
                                value,
                            })
                        }
                        Ok(Cont::ReadSlice(tag)) => {
                            //println!("    => starting to gather with buf.len() = {}", buf.len());
                            self.state = State::Gathering(
                                tag, read_at, self.offset, read.field_len() as u64);
                            return Ok(Ok(None));
                        }
                        Err(Skip(tag)) => {
                            let total = read.field_len();
                            self.state = State::Skipping(tag, read_at, self.offset, total as u64);
                            return Ok(Ok(None))
                        }
                    };

                    self.state = State::DecidingAfter;

                    //println!("    => {:?} and returning Ok(Ok({:?}))", self.state, ret);

                    return Ok(Ok(ret));
                }
            },
        }
    }

    pub fn next<'a>(
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

    pub fn into_parts(self) -> (u64, M) {
        (self.offset, self.matcher)
    }
}

impl<M: Matcher + PartialEq> MatcherFields<M> {
    // TODO: something to advance to wanted matcher state
}

/// An item tagged by a [`Matcher`] from the stream of fields read by
/// [`MatcherFields`].
#[derive(Debug)]
pub struct Matched<T> {
    pub tag: T,
    pub offset: u64,
    pub value: Value,
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

impl Value {
    pub fn slice_len(&self) -> Result<usize, ()> {
        match self {
            Value::Slice(Range { start, end }) => Ok((end - start) as usize),
            _ => Err(())
        }
    }
}

/// Represents an instruction to skip the current field. Good default.
#[derive(Debug)]
pub struct Skip<T>(pub T);

/// Gathers multiple tagged values into single returned value. This is needed because the fields in
/// protobuf files can come at any order, and previous values can be overridden by later fields.
pub trait Gatherer<'a> {
    /// The marker type matched by this gatherer
    type Tag;

    /// Returned combined value from this gatherer
    type Returned: 'a;

    /// Records field matches required for `Returned` and possibly returns when needed.
    fn update(&mut self, matched: Matched<Self::Tag>, slicer: Slicer<'a>) -> Result<Option<Self::Returned>, DecodingError>;

    /// Returns the minimum stored input offset or None
    fn min_offset(&self) -> Option<u64>;
}

/// Slicer helps to map the bytes in the current buffer into the offset ranges of Value::Slice.
pub struct Slicer<'a> {
    buffer: &'a [u8],
    first_offset: u64,
}

impl<'a> Slicer<'a> {
    pub(crate) fn wrap(buffer: &'a [u8], last_offset: u64) -> Self {
        println!("{:?} = {} - {}", last_offset.checked_sub(buffer.len() as u64), last_offset, buffer.len());
        let first_offset = last_offset - buffer.len() as u64;
        Self {
            buffer,
            first_offset
        }
    }

    pub fn as_slice(&self, range: &Range<u64>) -> &'a [u8] {
        let adjusted_range = (range.start - self.first_offset) as usize..(range.end - self.first_offset) as usize;
        println!("slicing {:?} into {:?} from buf.len() == {}", range, adjusted_range, self.buffer.len());
        &self.buffer[adjusted_range]
    }
}

/// Combines [`Matcher`] and [`Gatherer`] to allow recognizing full values combined of parts of the
/// buffer. Maintains the buffer so that while inner reader can advance, enough bytes are buffered
/// to allow reconstructing the gathered value when the time comes.
pub struct Gathered<M: Matcher, G> {
    reader: MatcherFields<M>,
    gatherer: G,
    cached_min_offset: Option<u64>,
}

impl<'a, M: Matcher, G> Gathered<M, G>
    where G: Gatherer<'a, Tag = M::Tag>,
          G::Returned: fmt::Debug
{
    pub fn new(matcher: M, gatherer: G) -> Self {
        Gathered {
            reader: MatcherFields::new(matcher),
            gatherer,
            cached_min_offset: None
        }
    }

    pub fn next(&mut self, buf: &mut &'a [u8]) -> Result<Result<G::Returned, Status>, DecodingError> {
        println!("buf.len() == {}", buf.len());

        let mut tmp = if let Some(min) = self.cached_min_offset {
            // this means that min is stored at buf[0] and buf[diff] is the next byte the inner
            // reader(s) need to look at
            let diff = (self.reader.offset() - min) as usize;
            println!("attemping to slice buf.len() = {} at {}..", buf.len(), diff);
            &buf[diff..]
        } else {
            &buf[..]
        };

        loop {
            let ret = match self.reader.next(&mut tmp)? {
                Ok(m) => {
                    let slicer = Slicer::wrap(buf, self.reader.offset());
                    let ret = self.gatherer.update(m, slicer)?.map(|r| Ok(Ok(r)));
                    // invalidate the cached value
                    self.cached_min_offset.take();
                    ret
                },
                Err(e) => Some(Ok(Err(e)))
            };

            if let Some(ret) = ret {
                // FIXME: check if the min changed, update needed
                if self.cached_min_offset.is_none() {
                    self.cached_min_offset = self.gatherer.min_offset();
                }

                if self.cached_min_offset.is_none() {
                    println!("cached min is none, *buf = {} from {}, ret = {:?}", tmp.len(), buf.len(), ret);
                    *buf = tmp;
                } else {
                    println!("buffering: {}", buf.len());
                }

                // FIXME: adjust the buf to contain needed again

                return ret;
            }
        }
    }
}
