#![warn(rust_2018_idioms)]

use std::fmt;
use std::io::Read;
use std::borrow::Cow;
use std::ops::Range;

use minipb::matcher_fields::{Cont, Matcher, MatcherFields, Skip, Matched, Value};
use minipb::{ReadField, Status, DecodingError, FieldId, Reader};

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
    enum Document {
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
        PbLinkExtraField(FieldId),
        TopExtraField(FieldId),
    }

    impl Matcher for Document {
        type Tag = InterestingField;

        fn decide_before(
            &mut self,
            offset: usize,
            read: &ReadField<'_>,
        ) -> Result<Cont<Self::Tag>, Skip<Self::Tag>> {
            use Document::*;
            //println!("decide({:?}, {}, {:?})", self, offset, read);
            match self {
                Top if read.field_id() == 2 => {
                    *self = Document::Link {
                        until: offset + read.bytes_to_skip(),
                    };
                    return Ok(Cont::Message(Some(InterestingField::StartPbLink)));
                }
                Top => {}
                Link { until } => {
                    assert!(
                        offset < *until,
                        "got up to {} but should had stopped at {}",
                        offset,
                        until,
                    );

                    return match read.field_id() {
                        1 => Ok(Cont::ReadSlice(InterestingField::PbLinkHash)),
                        2 => Ok(Cont::ReadSlice(InterestingField::PbLinkName)),
                        3 => Ok(Cont::ReadValue(InterestingField::PbLinkTotalSize)),
                        x => Err(Skip(InterestingField::PbLinkExtraField(x))),
                    };
                }
            }

            Err(Skip(InterestingField::TopExtraField(read.field_id())))
        }

        fn decide_after(&mut self, offset: usize) -> (bool, Option<Self::Tag>) {
            use Document::*;
            // println!("decide_at({:?}, {})", self, offset);
            match self {
                Link { until } if offset == *until => {
                    *self = Top;
                    (false, Some(InterestingField::EndPbLink))
                }
                _ => (false, None),
            }
        }
    }

    struct PBLink<'a> {
        hash: Cow<'a, [u8]>,
        name: Cow<'a, str>,
        total_size: u64,
    }

    impl fmt::Debug for PBLink<'_> {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt.debug_struct("PBLink")
                .field("hash", &format_args!("{:?}", HexOnly(&*self.hash)))
                .field("name", &self.name)
                .field("total_size", &self.total_size)
                .finish()
        }
    }

    struct Links {
        reader: MatcherFields<Document>,
        lengths: [Option<Range<u64>>; 2],
        total_size: Option<u64>,
    }

    impl<'a> Reader<'a, PBLink<'a>> for Links {
        fn next(
            &mut self,
            buf: &mut &'a [u8],
        ) -> Result<Result<PBLink<'a>, Status>, DecodingError> {
            let mut tmp = *buf;
            let ret = self.inner_next(&mut tmp);
            // if buf was changed here, it would need to modify all of the ranges as well

            let min = self.lengths.iter()
                .filter_map(|opt| if let Some(Range { start, .. }) = opt.as_ref() { Some(*start) } else { None })
                .min();

            if let Some(min) = min {
                *buf = &buf[min as usize..];
                for range in self.lengths.iter_mut() {
                    match range {
                        Some(Range { ref mut start, ref mut end }) => {
                            *start -= min;
                            *end -= min;
                        },
                        _ => {}
                    }
                }
            } else {
                *buf = tmp;
            }

            ret
        }
    }

    impl Links {

        fn new() -> Self {
            Links {
                reader: MatcherFields::new(Document::Top),
                lengths: [None, None],
                total_size: None,
            }
        }


        fn inner_next<'a>(
            &mut self,
            buf: &mut &'a [u8],
        ) -> Result<Result<PBLink<'a>, Status>, DecodingError> {
            use InterestingField::*;
            use Status::*;

            let orig: &'a [u8] = *buf;
            let mut tmp: &'a [u8] = *buf;

            let start = self.reader.offset();

            // before returning this function must save the length of buf to know how long the
            // inner reader has gone.
            //
            // when entering the next time, the tmp received by the self.reader must be have some
            // amount of bytes skipped from the front

            let min = self.lengths.iter()
                .filter_map(|opt| if let Some(Range { start, .. }) = opt.as_ref() { Some(*start) } else { None })
                .min();

            if let Some(min) = min {
                tmp = &tmp[min as usize..];
            }

            loop {
                let (index, value) = match self.reader.next(&mut tmp)? {
                    Ok(Matched { tag: StartPbLink, .. }) => continue,
                    Ok(Matched { tag: EndPbLink, .. }) => {
                        let lens = (self.lengths[0].take(), self.lengths[1].take(), self.total_size.take());

                        if let (Some(xr), Some(yr), Some(total_size)) = lens {
                            // the issue here is that the indices are correct only on the first
                            // PBLink we return

                            let hash = Cow::Borrowed(&orig[xr.start as usize..xr.end as usize]);
                            let name = &orig[yr.start as usize..yr.end as usize];

                            let name = std::str::from_utf8(name)
                                .unwrap_or_else(|e| panic!("failed to convert {:?} to str: {}", HexOnly(name), e));

                            let name = Cow::Borrowed(name);
                            *buf = tmp;
                            return Ok(Ok(PBLink {
                                hash,
                                name,
                                total_size,
                            }));
                        } else {
                            panic!("read partial pblink:\n\
                                lens:  {:?}\n", lens);
                        }
                    }
                    Err(IdleAtEndOfBuffer) => return Ok(Err(IdleAtEndOfBuffer)),
                    Err(NeedMoreBytes) => return Ok(Err(NeedMoreBytes)),
                    Ok(Matched { tag: PbLinkHash, value, .. }) => {
                        (0, value)
                    }
                    Ok(Matched { tag: PbLinkName, value, .. }) => {
                        (1, value)
                    }
                    Ok(Matched { tag: PbLinkTotalSize, value, .. }) => {
                        if let Value::Varint(value) = value {
                            self.total_size = Some(value);
                        }
                        continue;
                    }
                    Ok(ignored) => {
                        println!("ignored {:?}", ignored);
                        continue;
                    }
                };

                self.lengths[index] = Some(match value {
                    Value::Slice(Range { start: s, end: e }) => (s - start)..(e - start),
                    _ => unreachable!()
                });
            }

        }
    }

    let mut links = Links::new();
    let mut buf = &buffer[..];

    /*loop {
        match links.next(&mut buf)? {
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
        match links.next(&mut buf)? {
            Ok(matched) => {
                println!("{:?}", matched);
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
            }
            Err(Status::IdleAtEndOfBuffer) => {
                if offset != buffer.len() {
                    copies.push(buffer[offset]);
                    offset += 1;
                } else {
                    break;
                }
            }
            Err(Status::NeedMoreBytes) => {
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
                copies.push(buffer[offset]);
                offset += 1;
            }
        }
    }

    let mut fm = MatcherFields::new(Document::Top);

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
            }
            Err(Status::IdleAtEndOfBuffer) => {
                if offset != buffer.len() {
                    copies.push(buffer[offset]);
                    offset += 1;
                } else {
                    break;
                }
            }
            Err(Status::NeedMoreBytes) => {
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
                copies.push(buffer[offset]);
                offset += 1;
            }
        }
    }

    // TODO: BufRead integration
    // TODO: tokio integration
    // TODO: async-std integration

    Ok(())
}
