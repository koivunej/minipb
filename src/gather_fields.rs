use std::fmt;
use std::ops::Range;
use crate::{DecodingError, Status};
use crate::matcher_fields::{Matcher, MatcherFields, Matched};

mod read;
pub use read::ReaderGatheredFields;

/// Gathers multiple tagged values into single returned value. This is needed because the fields in
/// protobuf files can come at any order, and previous values can be overridden by later fields.
pub trait Gatherer<'a> {
    /// The marker type matched by this gatherer
    type Tag: 'static;

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
    // what file offset the buffer[0] corresponds to
    first_offset: u64,
}

impl<'a> Slicer<'a> {
    pub(crate) fn wrap(buffer: &'a [u8], last_offset: u64) -> Self {
        // println!("{:?} = {} - {}", last_offset.saturating_sub(buffer.len() as u64), last_offset, buffer.len());
        let first_offset = last_offset.saturating_sub(buffer.len() as u64);
        Self {
            buffer,
            first_offset
        }
    }

    pub fn as_slice(&self, range: &Range<u64>) -> &'a [u8] {
        // println!("adjusted_range = ({} - {})..({} - {})", range.start, self.first_offset, range.end, self.first_offset);
        let start = (range.start - self.first_offset) as usize;
        let end = (range.end - self.first_offset) as usize;
        let adjusted_range = start..end;
        assert_eq!(range.end - range.start, (adjusted_range.end - adjusted_range.start) as u64);
        let ret = &self.buffer[adjusted_range];

        /*
        print!("buffer: ");
        for b in self.buffer {
            print!("{:02x}", b);
        }
        println!();

        print!("sliced: {:indent$}", "", indent = start * 2);
        for b in ret {
            print!("{:02x}", b);
        }
        println!();
        */

        ret
    }
}

/// Combines [`Matcher`] and [`Gatherer`] to allow recognizing full values combined of parts of the
/// buffer. Maintains the buffer so that while inner reader can advance, enough bytes are buffered
/// to allow reconstructing the gathered value when the time comes.
pub struct GatheredFields<M: Matcher, G> {
    reader: MatcherFields<M>,
    gatherer: G,
    cached_min_offset: Option<u64>,
}

impl<M: Matcher, G> GatheredFields<M, G>
    where for<'a> G: Gatherer<'a, Tag = M::Tag>
{
    pub fn new(matcher: M, gatherer: G) -> Self {
        Self {
            reader: MatcherFields::new(matcher),
            gatherer,
            cached_min_offset: None
        }
    }

}

impl<'a, M: Matcher, G> crate::Reader<'a> for GatheredFields<M, G>
    where G: Gatherer<'a, Tag = M::Tag>
{
    type Returned = G::Returned;

    fn next(&mut self, buf: &mut &'a [u8]) -> Result<Result<<G as Gatherer<'a>>::Returned, Status>, DecodingError> {
        let mut tmp = if let Some(min) = self.cached_min_offset {
            // this means that min is stored at buf[0] and buf[diff] is the next byte the inner
            // reader(s) need to look at
            let diff = (self.reader.offset() - min) as usize;
            &buf[diff..]
        } else {
            &buf[..]
        };

        loop {
            let ret = match self.reader.next(&mut tmp)? {
                Ok(m) => {
                    // FIXME: it's easy to not notice that tmp is passed to inner instead of buf
                    // and even that is wrong in the case of reading more than 1 byte at a time!

                    let end = buf.len() - tmp.len();
                    let sliced_from = &buf[..end];

                    // println!("{:02x?}", sliced_from);

                    let slicer = Slicer::wrap(sliced_from, self.reader.offset());
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
                    *buf = tmp;
                }

                return ret;
            }
        }
    }
}
