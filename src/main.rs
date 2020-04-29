#![warn(rust_2018_idioms)]

use std::io::Read;
use std::fmt;

use minipb::{Status, ReadField};
use minipb::matcher_fields::{Matcher, Cont, Skip, MatcherFields};

struct HexOnly<'a>(&'a [u8]);

impl<'a> fmt::Debug for HexOnly<'a> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(fmt, "{:02x}", byte)?;
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + 'static>> {
    let stdin = std::io::stdin();
    let mut locked = stdin.lock();

    let mut buffer = Vec::new();
    locked.read_to_end(&mut buffer)?;

    //let mut reader = FieldReader::default();

    //let mut buf = &buffer[..];
    //let mut offset = 0;

    #[derive(Debug)]
    enum State {
        Top,
        Link { until: usize },
    }

    #[derive(Debug)]
    enum InterestingField {
        StartPbLink,
        EndPbLink,
        PbLinkHash,
        PbLinkName,
        PbLinkTotalSize,
    }

    impl Matcher for State {
        type Tag = InterestingField;

        fn decide_before(&mut self, offset: usize, read: &ReadField<'_>) -> Result<Cont<Self::Tag>, Skip> {
            //println!("decide({:?}, {}, {:?})", self, offset, read);
            match self {
                State::Top if read.field_id() == 2 => {
                    *self = State::Link { until: offset + read.bytes_to_skip() };
                    return Ok(Cont::Message(Some(InterestingField::StartPbLink)))
                },
                State::Top => {},
                State::Link { until } => {
                    assert!(offset < *until, "got up to {} but should had stopped at {}", until, offset);

                    return match read.field_id() {
                        1 => Ok(Cont::ReadSlice(InterestingField::PbLinkHash)),
                        2 => Ok(Cont::ReadSlice(InterestingField::PbLinkName)),
                        3 => Ok(Cont::ReadValue(InterestingField::PbLinkTotalSize)),
                        _ => Err(Skip),
                    };
                },
            }

            Err(Skip)
        }

        fn decide_after(&mut self, offset: usize) -> (bool, Option<Self::Tag>) {
            // println!("decide_at({:?}, {})", self, offset);
            match self {
                State::Link { until } if offset == *until => {
                    *self = State::Top;
                    (false, Some(InterestingField::EndPbLink))
                },
                _ => (false, None),
            }
        }
    }

    let mut fm = MatcherFields::new(State::Top);

    /*
    loop {
        match fm.next(&mut buf)? {
            Ok(matched) => println!("{:?}", matched),
            Err(x) => panic!("{:?}", x),
        }
    }*/

    let mut copies = Vec::new();
    let mut offset = 0;
    copies.push(buffer[offset]);
    offset += 1;

    loop {
        let orig_len = copies.len();
        let mut buf = &copies[..];
        match fm.next(&mut buf)? {
            Ok(matched) => {
                println!("{:?}", matched);
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
            },
            Err(Status::IdleAtEndOfBuffer) => {
                if offset != buffer.len() {
                    copies.push(buffer[offset]);
                    offset += 1;
                } else {
                    break;
                }
            },
            Err(Status::NeedMoreBytes) => {
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
                copies.push(buffer[offset]);
                offset += 1;
            },
        }
    }

    // TODO: BufRead integration
    // TODO: tokio integration
    // TODO: async-std integration

    Ok(())
}

