use crate::{ReadError, Reader, Status};

/// A poor mans `std::io::BufRead` but with a growing buffer.
pub struct ReadWrapper<IO, R> {
    /// The wrapped reader
    inner: IO,
    /// Growable byte buffer. Growth happens by `grow_by` amount at a time.
    buffer: Vec<u8>,
    /// Processes the bytes read into the buffer.
    matcher: R,
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

impl<'a, IO, R> ReadWrapper<IO, R>
where
    IO: std::io::Read,
    R: Reader<'a>,
{
    pub fn new(inner: IO, matcher: R) -> Self {
        let grow_by = 8192;
        Self {
            inner,
            buffer: Vec::with_capacity(grow_by),
            matcher,
            grow_by,
            at_offset: 0,
            exhausted: false,
            eof_after_buffer: false,
        }
    }

    /// There might be Interrupted errors while reading, which are **not** ignored like the
    /// `std::io::BufRead` does for example. After the interruption the next can be called again
    /// only if the inner `std::io::Read` can continue reading where it was left off.
    ///
    /// # Safety
    ///
    /// The current implementation requires the use of `unsafe`, so this method might not be sound.
    /// Any obvious cases dont seem to compile, please see `tests/ui` for those and report an issue
    /// if you find a new case which shouldn't work!
    pub fn read_next(&'a mut self) -> Result<Option<R::Returned>, ReadError> {
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

                // consumed can be zero, in case the gatherer would only need more buffer
                self.at_offset += consumed;

                match ret {
                    Ok(m) => return Ok(Some(m)),
                    Err(Status::IdleAtEndOfBuffer) if self.eof_after_buffer => return Ok(None),
                    Err(Status::NeedMoreBytes) if self.eof_after_buffer => {
                        return Err(ReadError::UnexpectedEndOfFile)
                    }
                    Err(Status::IdleAtEndOfBuffer) | Err(Status::NeedMoreBytes) => {
                        self.exhausted = true
                    }
                }
            }
        }
    }

    fn maybe_fill(&mut self) -> Result<(), ReadError> {
        use std::iter::repeat;

        if self.exhausted && !self.eof_after_buffer {
            let mut len_before = self.buffer.len();
            let mut needed_zeroes = self.buffer.capacity() - len_before;

            if needed_zeroes == 0 {
                if self.at_offset != 0 {
                    // these first bytes haven't been needed for a long time
                    self.buffer.drain(..self.at_offset);
                    self.at_offset = 0;
                    len_before = self.buffer.len();
                }

                needed_zeroes = self.buffer.capacity() - len_before;

                if needed_zeroes == 0 {
                    // growing only after we are certain there's no other way might cause some
                    // reprocessing but might be the optimal strategy, or silly either way
                    needed_zeroes += self.grow_by;
                }
            }

            // only read N bytes at a time
            //needed_zeroes = needed_zeroes.min(8);

            self.buffer.extend(repeat(0).take(needed_zeroes));

            let bytes = self.inner.read(&mut self.buffer[len_before..])?;

            self.eof_after_buffer = bytes == 0;
            self.buffer.truncate(len_before + bytes);
        }
        Ok(())
    }

    pub fn into_inner(self) -> IO {
        self.inner
    }
}
