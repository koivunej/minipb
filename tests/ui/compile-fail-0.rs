use minipb::{Status, DecodingError, Reader, io_ext::read::ReadWrapper};

use std::borrow::Cow;

/// Making sure that Reader<'static> cannot be passed over to ReadWrapper. If it could be, the
/// returned lifetime would not be valid after mutation but luckily the read_next() binding `'a` to
/// `&mut self` stops it.
struct BadReader;

impl Reader<'static> for BadReader {
    type Returned = Cow<'static, [u8]>;

    fn next(&mut self, buf: &mut &'static [u8]) -> Result<Result<Self::Returned, Status>, DecodingError> {
        Ok(Ok(Cow::Borrowed(buf)))
    }
}

fn test(read: impl std::io::Read + 'static) {
    let mut rw = ReadWrapper::new(read, BadReader);
    rw.read_next();
}

fn main() {}
