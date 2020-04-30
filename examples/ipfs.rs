#![warn(rust_2018_idioms)]

use std::fmt;
use std::io::Read;
use std::borrow::Cow;
use std::ops::Range;

use minipb::matcher_fields::{Cont, Matcher, MatcherFields, Skip, Matched, Value, Gatherer, Slicer, Gathered};
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
    enum DagPbElement {
        StartPbLink,
        EndPbLink,
        PbLinkHash,
        PbLinkName,
        PbLinkTotalSize,
        PbLinkExtraField(FieldId),
        TopExtraField(FieldId),
    }

    impl Matcher for Document {
        type Tag = DagPbElement;

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
                    return Ok(Cont::Message(Some(DagPbElement::StartPbLink)));
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
                        1 => Ok(Cont::ReadSlice(DagPbElement::PbLinkHash)),
                        2 => Ok(Cont::ReadSlice(DagPbElement::PbLinkName)),
                        3 => Ok(Cont::ReadValue(DagPbElement::PbLinkTotalSize)),
                        x => Err(Skip(DagPbElement::PbLinkExtraField(x))),
                    };
                }
            }

            Err(Skip(DagPbElement::TopExtraField(read.field_id())))
        }

        fn decide_after(&mut self, offset: usize) -> (bool, Option<Self::Tag>) {
            use Document::*;
            // println!("decide_at({:?}, {})", self, offset);
            match self {
                Link { until } if offset == *until => {
                    *self = Top;
                    (false, Some(DagPbElement::EndPbLink))
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

    #[derive(Default)]
    struct PBLinkGatherer {
        hash: Option<Range<u64>>,
        name: Option<Range<u64>>,
        total_size: Option<u64>,
    }

    impl<'a> Gatherer<'a> for PBLinkGatherer {
        type Tag = DagPbElement;
        type Returned = PBLink<'a>;

        fn update(&mut self, matched: Matched<DagPbElement>, slicer: Slicer<'a>) -> Result<Option<Self::Returned>, DecodingError> {
            use DagPbElement::*;

            let (field, value) = match dbg!(matched) {
                Matched { tag: EndPbLink, .. } => {
                    if let (Some(_), Some(_), Some(_)) = (self.hash.as_ref(), self.name.as_ref(), self.total_size) {
                        let hr = self.hash.take().unwrap();
                        let nr = self.name.take().unwrap();
                        let total_size = self.total_size.take().unwrap();

                        let hash = Cow::Borrowed(slicer.as_slice(&hr));
                        let name = slicer.as_slice(&nr);
                        let name = Cow::Borrowed(std::str::from_utf8(name).unwrap());

                        return Ok(Some(dbg!(PBLink {
                            hash,
                            name,
                            total_size,
                        })));
                    } else {
                        println!("could not build a pblink: {:?}, {:?}, {:?}", self.hash, self.name, self.total_size);
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
                Matched { .. } => return Ok(None)
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

            let orig_len = buf.len();
            let used_len = tmp.len();

            let min = self.min_offset();

            if let Some(min) = min {
                //*buf = &buf[(self.reader.offset() - min) as usize..];
                //println!("found min={}, ignoring tmp.len()={} but setting buf to {} from {}", min, tmp.len(), buf.len(), orig_len);
                /*for range in self.lengths.iter_mut() {
                    match range {
                        Some(Range { ref mut start, ref mut end }) => {
                            *start -= min;
                            *end -= min;
                        },
                        _ => {}
                    }
                }*/
                if let Ok(Err(Status::NeedMoreBytes)) = &ret {
                    //println!("buf.len() was {}, tmp.len() == {}, marked min={} as used as buf.len() == {}", orig_len, tmp.len(), min, buf.len());
                }

            } else {
                *buf = tmp;

                if let Ok(Err(Status::NeedMoreBytes)) = &ret {
                    //println!("buf.len() was {}, tmp.len() == {}, now buf.len() == {}", orig_len, tmp.len(), buf.len());
                }
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

        fn min_offset(&self) -> Option<u64> {
            self.lengths.iter()
                .filter_map(|opt| if let Some(Range { start, .. }) = opt.as_ref() { Some(*start) } else { None })
                .min()
        }

        fn inner_next<'a>(
            &mut self,
            buf: &mut &'a [u8],
        ) -> Result<Result<PBLink<'a>, Status>, DecodingError> {
            use DagPbElement::*;
            use Status::*;

            let orig: &'a [u8] = *buf;

            if let Some(min) = self.min_offset() {
                // we need to advance, as the reader thinks the buf it get's passed starts from
                // where it last stopped at but we should had buffered (at caller) the amount
                // needed by our saved elements.
                *buf = &buf[(self.reader.offset() - min) as usize..];
            }

            loop {
                let (index, value) = match self.reader.next(buf)? {
                    Ok(Matched { tag: StartPbLink, .. }) => continue,
                    Ok(Matched { tag: EndPbLink, .. }) => {

                        // FIXME: this is definetly needed but ... I wonder if the matches change
                        // right before the end, will this be correct? Probably not

                        let min = self.min_offset();
                        let lens = (self.lengths[0].take(), self.lengths[1].take(), self.total_size.take(), min);

                        if let (Some(mut xr), Some(mut yr), Some(total_size), Some(min)) = lens {
                            // the issue here is that the indices are correct only on the first
                            // PBLink we return

                            xr.start -= min;
                            xr.end -= min;
                            yr.start -= min;
                            yr.end -= min;

                            let hash = Cow::Borrowed(&orig[xr.start as usize..xr.end as usize]);
                            let name = &orig[yr.start as usize..yr.end as usize];

                            let name = std::str::from_utf8(name)
                                .unwrap_or_else(|e| panic!("failed to convert {:?} to str: {}", HexOnly(name), e));

                            let name = Cow::Borrowed(name);
                            return Ok(Ok(PBLink {
                                hash,
                                name,
                                total_size,
                            }));
                        } else {
                            panic!("read partial pblink:\nlens:  {:?}\n", lens);
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
                    Value::Slice(range) => range,
                    _ => unreachable!()
                });
            }

        }
    }

    let mut g = Gathered::new(Document::Top, PBLinkGatherer::default());

    let mut copies = Vec::new();
    let mut offset = 0;
    copies.push(buffer[offset]);
    offset += 1;

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
                let before = copies.len();
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
