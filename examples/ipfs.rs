#![warn(rust_2018_idioms)]

use std::fmt;
use std::io::Read;
use std::borrow::Cow;
use std::ops::Range;

use minipb::matcher_fields::{Cont, Matcher, Skip, Matched, Value};
use minipb::gather_fields::{GatheredFields, Slicer, Gatherer};
use minipb::{ReadField, Status, DecodingError, FieldId};

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
    enum MerkleDag {
        Top,
        Link { until: usize },
        UserBytes { until: usize },
    }

    #[derive(Debug)]
    enum DagPbElement {
        StartPbLink,
        EndPbLink,
        PbLinkHash,
        PbLinkName,
        PbLinkTotalSize,
        PbLinkExtraField(FieldId),
        StartUserBytes,
        //StartUnixFs,
        UnixFsType,
        UnixFsData,
        UnixFsFileSize,
        UnixFsBlockSize,
        UnixFsField(FieldId),
        //EndUnixFsData,
        EndUserBytes,
        TopExtraField(FieldId),
    }

    impl Matcher for MerkleDag {
        type Tag = DagPbElement;

        fn decide_before(
            &mut self,
            offset: usize,
            read: &ReadField<'_>,
        ) -> Result<Result<Cont<Self::Tag>, Skip<Self::Tag>>, DecodingError> {
            use MerkleDag::*;
            //println!("decide({:?}, {}, {:?})", self, offset, read);
            match self {
                Top if read.field_id() == 1 => {
                    *self = UserBytes {
                        until: offset + read.bytes_to_skip(),
                    };
                    return Ok(Ok(Cont::Message(Some(DagPbElement::StartUserBytes))));
                },
                Top if read.field_id() == 2 => {
                    *self = Link {
                        until: offset + read.bytes_to_skip(),
                    };
                    return Ok(Ok(Cont::Message(Some(DagPbElement::StartPbLink))));
                },
                Top => {}
                Link { until } => {
                    if offset >= *until {
                        return Err(DecodingError::FailedMatcherNesting(offset, *until));
                    }

                    return Ok(match read.field_id() {
                        1 => Ok(Cont::ReadSlice(DagPbElement::PbLinkHash)),
                        2 => Ok(Cont::ReadSlice(DagPbElement::PbLinkName)),
                        3 => Ok(Cont::ReadValue(DagPbElement::PbLinkTotalSize)),
                        x => Err(Skip(DagPbElement::PbLinkExtraField(x))),
                    });
                },
                UserBytes { until } => {
                    if offset >= *until {
                        return Err(DecodingError::FailedMatcherNesting(offset, *until));
                    }

                    return Ok(match read.field_id() {
                        1 => Ok(Cont::ReadValue(DagPbElement::UnixFsType)),
                        2 => Ok(Cont::ReadSlice(DagPbElement::UnixFsData)),
                        3 => Ok(Cont::ReadValue(DagPbElement::UnixFsFileSize)),
                        4 => Ok(Cont::ReadValue(DagPbElement::UnixFsBlockSize)),
                        x => Err(Skip(DagPbElement::UnixFsField(x))),
                    });
                }
            }

            Ok(Err(Skip(DagPbElement::TopExtraField(read.field_id()))))
        }

        fn decide_after(&mut self, offset: usize) -> (bool, Option<Self::Tag>) {
            use MerkleDag::*;
            // println!("decide_at({:?}, {})", self, offset);
            match self {
                Link { until } if offset == *until => {
                    *self = Top;
                    (false, Some(DagPbElement::EndPbLink))
                }
                UserBytes { until } if offset == *until => {
                    *self = Top;
                    (false, Some(DagPbElement::EndUserBytes))
                }
                _ => (false, None),
            }
        }
    }

    struct PBLink<'a> {
        /// File offset
        offset: Range<u64>,
        hash: Cow<'a, [u8]>,
        name: Cow<'a, str>,
        total_size: u64,
    }

    impl fmt::Debug for PBLink<'_> {
        fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt.debug_struct("PBLink")
                .field("offset", &format_args!("{:?}", self.offset))
                .field("hash", &format_args!("{:?}", HexOnly(&*self.hash)))
                .field("name", &self.name)
                .field("total_size", &self.total_size)
                .finish()
        }
    }

    #[derive(Default)]
    struct PBLinkGatherer {
        start: Option<u64>,
        hash: Option<Range<u64>>,
        name: Option<Range<u64>>,
        total_size: Option<u64>,
    }

    impl<'a> Gatherer<'a> for PBLinkGatherer {
        type Tag = DagPbElement;
        type Returned = PBLink<'a>;

        fn update(&mut self, matched: Matched<DagPbElement>, slicer: Slicer<'a>) -> Result<Option<Self::Returned>, DecodingError> {
            use DagPbElement::*;

            let (field, value) = match matched {
                Matched { tag: EndPbLink, offset, .. } => {

                    // important that we don't keep these between links
                    let values = (self.start.take(), self.hash.take(), self.name.take(), self.total_size.take());

                    if let (Some(start), Some(hr), Some(nr), Some(total_size)) = values {

                        let hash = Cow::Borrowed(slicer.as_slice(&hr));
                        let name = slicer.as_slice(&nr);
                        let name = Cow::Borrowed(std::str::from_utf8(name).unwrap());

                        return Ok(Some(PBLink {
                            offset: start..offset,
                            hash,
                            name,
                            total_size,
                        }));
                    }

                    return Ok(None)
                },
                Matched { tag: PbLinkHash, value, .. } => (&mut self.hash, value),
                Matched { tag: PbLinkName, value, .. } => (&mut self.name, value),
                Matched { tag: PbLinkTotalSize, value, .. } => {
                    self.total_size = match value {
                        Value::Varint(x) => Some(x),
                        _ => None
                    };
                    return Ok(None)
                },
                Matched { tag: StartPbLink, offset, .. } => {
                    self.start = Some(offset);
                    return Ok(None)
                },
                Matched { tag, offset, value } => {
                    println!("skipped {:?} {:?} at {}", tag, value, offset);
                    return Ok(None)
                }
            };

            *field = match value {
                Value::Slice(range) => Some(range),
                _ => None,
            };

            return Ok(None)
        }

        fn min_offset(&self) -> Option<u64> {
            let h = self.hash.as_ref().map(|r| r.start);
            let n = self.name.as_ref().map(|r| r.start);
            match (h, n) {
                (Some(h), Some(n)) => Some(h.min(n)),
                (Some(x), _) | (_, Some(x)) => Some(x),
                _ => None
            }
        }
    }

    let mut g = GatheredFields::new(MerkleDag::Top, PBLinkGatherer::default());

    let mut copies = Vec::new();
    let mut offset = 0;
    copies.extend(&buffer[..64]);
    offset += 64;

    loop {
        let orig_len = copies.len();
        let mut buf = &copies[..];
        //println!("trying buf={:?}", HexOnly(buf));
        match g.next(&mut buf)? {
            Ok(matched) => {
                println!("{:?}", matched);
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
            }
            Err(Status::IdleAtEndOfBuffer) => {
                if offset != buffer.len() {
                    let consumed = orig_len - buf.len();
                    copies.drain(..consumed);
                    //println!("Err(IdleAtEndOfBuffer) pushing to copies.len()={} from {}", copies.len(), offset);
                    copies.push(buffer[offset]);
                    offset += 1;
                } else {
                    break;
                }
            }
            Err(Status::NeedMoreBytes) => {
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
                //println!("Err(NeedMoreBytes) drained from {}, now copies.len()={}, pushing from {}", before, copies.len(), offset);
                copies.push(buffer[offset]);
                offset += 1;
            }
        }
    }

    /*
    let mut links = Links::new();
    /*
    let mut buf = &buffer[..];

    loop {
        match links.next(&mut buf)? {
            Ok(matched) => println!("{:?}", matched),
            Err(x) => panic!("{:?}", x),
        }
    }
    */

    let mut copies = Vec::new();
    let mut offset = 0;
    copies.push(buffer[offset]);
    offset += 1;

    loop {
        let orig_len = copies.len();
        let mut buf = &copies[..];
        //println!("trying buf={:?}", HexOnly(buf));
        match links.next(&mut buf)? {
            Ok(matched) => {
                println!("{:?}", matched);
                let consumed = orig_len - buf.len();
                copies.drain(..consumed);
            }
            Err(Status::IdleAtEndOfBuffer) => {
                if offset != buffer.len() {
                    let consumed = orig_len - buf.len();
                    copies.drain(..consumed);
                    //println!("Err(IdleAtEndOfBuffer) pushing to copies.len()={} from {}", copies.len(), offset);
                    copies.push(buffer[offset]);
                    offset += 1;
                } else {
                    break;
                }
            }
            Err(Status::NeedMoreBytes) => {
                let consumed = orig_len - buf.len();
                let before = copies.len();
                copies.drain(..consumed);
                //println!("Err(NeedMoreBytes) drained from {}, now copies.len()={}, pushing from {}", before, copies.len(), offset);
                copies.push(buffer[offset]);
                offset += 1;
            }
        }
    }
    */

    /*
    let mut fm = MatcherFields::new(MerkleDag::Top);

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
                    println!("pushing to copies.len()={} from {}", copies.len(), offset);
                    copies.push(buffer[offset]);
                    offset += 1;
                } else {
                    break;
                }
            }
            Err(Status::NeedMoreBytes) => {
                let consumed = orig_len - buf.len();
                println!("draining consumed={} out of copies.len={}", consumed, copies.len());
                copies.drain(..consumed);
                println!("drained, now copies.len()={}, pushing from {}", copies.len(), offset);
                copies.push(buffer[offset]);
                offset += 1;
            }
        }
    }*/

    // TODO: BufRead integration
    // TODO: tokio integration
    // TODO: async-std integration

    Ok(())
}
