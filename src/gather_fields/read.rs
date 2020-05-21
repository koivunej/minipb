use crate::gather_fields::{Gatherer, GatheredFields};
use crate::matcher_fields::Matcher;
use crate::{ReadError, Status};

/// A poor mans `std::io::BufRead` but with a growing buffer.
pub struct ReaderGatheredFields<R, M: Matcher, G> {
    /// The wrapped reader
    reader: R,
    /// Growable byte buffer. Growth happens by `grow_by` amount at a time.
    buffer: Vec<u8>,
    /// Processes the bytes read into the buffer.
    matcher: GatheredFields<M, G>,
    /// The amount to grow the buffer by. It will need to be grown for larger fields, as there
    /// currently isn't a way to read fields as slices.
    grow_by: usize,
    /// Where in the buffer did we last get to
    at_offset: usize,
    /// When true, need to read more bytes
    exhausted: bool,
    /// When true, any bytes in the buffer represent the last bytes of the input stream.
    eof_after_buffer: bool,
}

impl<R, M: Matcher, G> ReaderGatheredFields<R, M, G>
    where R: std::io::Read,
          for<'a> G: Gatherer<'a, Tag = M::Tag>
{
    pub fn new(reader: R, matcher: GatheredFields<M, G>) -> Self {
        let grow_by = 64;
        Self {
            buffer: Vec::with_capacity(grow_by),
            reader,
            matcher,
            grow_by,
            at_offset: 0,
            // start with this false so that we dont grow on the first round
            exhausted: false,
            eof_after_buffer: false,
        }
    }

    #[cfg(polonius)]
    fn next(&mut self) -> Result<<G as Gatherer<'_>>::Returned, ReadError> {
        loop {
            self.maybe_fill()?;

            // this would work out of the box with -Zpolonius
            let mut buf = &self.buffer[..];
            match self.matcher.next(&mut buf)? {
                Ok(m) => return Ok(m),
                Err(Status::IdleAtEndOfBuffer) => {},
                Err(Status::NeedMoreBytes) => {},
            }
        }
    }

    /// There might be Interrupted errors while reading, which are **not** ignored like the
    /// `std::io::BufRead` does for example. After the interruption the next can be called again
    /// only if the inner `std::io::Read` can continue reading where it was left off.
    #[cfg(not(polonius))]
    pub fn next<'a>(&'a mut self) -> Result<Option<<G as Gatherer<'a>>::Returned>, ReadError> {
        use std::mem::transmute;
        loop {
            self.maybe_fill()?;

            unsafe {
                // We are trying to work around a compiler "bug" which would otherwise grow the
                // region of shared borrow of `buf` to whole function (NLL limitation). I think
                // this is NLL problem #3 in Nikos polonius/NLL related blog posts and different
                // variants of it have been reported and have the tag 'NLL-polonius'.
                //
                // The shared borrow is changed into 'static for the *duration* of matcher.next
                // call so that it doesn't become hit the NLL limitation. When we cast it back to
                // the "shorter" lifetime right before returning, it should have the same effect as
                // using the `'a` the whole time.
                //
                // I am a bit worried of someone coming over and writing a Gatherer<'static>.. But
                // not sure if that could fit here, wouldn't &'static mut self become an issue?
                let mut buf = transmute::<&'_ _, &'static [u8]>(&self.buffer[self.at_offset..]);

                // the matcher might advance this
                let original_len = buf.len();

                // FIXME: there's probably a bug here when the buf does get moved but we don't move
                // the internal accounting?
                let ret = self.matcher.next(&mut buf)?;

                let buf_len = buf.len();
                let consumed = original_len - buf_len;
                self.at_offset += consumed;

                match ret {
                    Ok(m) => return Ok(Some(m)),
                    Err(Status::IdleAtEndOfBuffer) if self.eof_after_buffer => return Ok(None),
                    Err(Status::NeedMoreBytes) if self.eof_after_buffer => return Err(ReadError::UnexpectedEndOfFile),
                    Err(Status::IdleAtEndOfBuffer)
                    | Err(Status::NeedMoreBytes) => self.exhausted = true,
                }
            }
        }
    }

    fn maybe_fill(&mut self) -> Result<(), ReadError> {
        use std::iter::repeat;

        if self.exhausted {
            let mut len_before = self.buffer.len();
            print!("len before={}; ", len_before);

            let mut needed_zeroes = self.buffer.capacity() - len_before;

            if needed_zeroes == 0 {
                if self.at_offset != 0 {
                    print!("draining ..{}; ", self.at_offset);
                    self.buffer.drain(..self.at_offset);
                    self.at_offset = 0;
                    len_before = self.buffer.len();
                    print!("len before={}; ", len_before);
                }

                needed_zeroes = self.buffer.capacity() - len_before;

                if needed_zeroes == 0 {
                    // growing only after we are certain there's no other way might cause some
                    // reprocessing but might be the optimal strategy, or silly either way
                    needed_zeroes += self.grow_by;
                }
            }

            // only read one byte at a time
            //needed_zeroes = needed_zeroes.min(8);

            print!("needed_zeroes={}; ", needed_zeroes);

            self.buffer.extend(repeat(0).take(needed_zeroes));

            print!("len_after={}; reading max {} over {}..; ", self.buffer.len(), self.buffer[len_before..].len(), len_before);

            let bytes = self.reader.read(&mut self.buffer[len_before..])?;

            self.eof_after_buffer = bytes == 0;
            self.buffer.truncate(len_before + bytes);

            println!("read {}; len after={}", bytes, self.buffer.len());
        }
        Ok(())
    }
}
