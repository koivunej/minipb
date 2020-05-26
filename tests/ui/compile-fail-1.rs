use minipb::{DecodingError, Status, Reader};
use minipb::io_ext::read::ReadWrapper;

#[derive(Default)]
struct NoopReader<'a>(std::marker::PhantomData<&'a ()>);

impl<'a> Reader<'a> for NoopReader<'a> {
    type Returned = (); // it doesn't matter if Returned is 'static
    fn next(&mut self, _buf: &mut &'a [u8]) -> Result<Result<(), Status>, DecodingError> {
        todo!()
    }
}

fn test(read: impl std::io::Read) {
    let mut items = ReadWrapper::new(read, NoopReader::default());
    let first = items.read_next();
    let second = items.read_next();
    // with NoopReader<'a> as in, if it would store a reference to the buffer this should fail
    println!("{:?}, {:?}", first, second);
}

fn main() {}
