# Minipb

Another take on protobuf deserialization (not serialization). This is just a
prototype, for more complete solutions look at:

* [prost](https://crates.io/crates/prost)
* [quick-protobuf](https://crates.io/crates/quick-protobuf)
* [rust-protobuf](https://crates.io/crates/protobuf)

While it says on the protobuf documentation that [protobuf is not good for
large files or messages] it can certainly be used for such. If you find youself
in a situation where you'd like to partially process a large protobuf file,
this crate might be interesting for you. The idea is to provide an
implementation which can be used to help seeking a large file and pull only the
interesting parts out.

## Introduction

The crate provides three levels of abstractions (lowest to highest):

 1. `FieldReader`
 2. `MatcherFields` with the help of `Matcher` trait
 3. `GatheredFields` with the help of `Matcher` and `Gatherer`

`FieldReader` is barely usable without additional help to decide what to do
with length delimited fields. This help is provided by `MatcherFields` which
can guide the `FieldReader` over the input, skipping fields and reading fields
as bytes, as directed by the `Matcher`. `Matcher` itself represents a
deterministic finite automaton and it recognizes the interesting parts of the
document.

Third level is the `GatheredFields` which, with the help of `Matcher` and
`Gatherer` makes sure the buffer keeps enough bytes buffered so that a complete
value with multiple fields can be "gathered".

Examples of the above:

 * `Matcher`: `PathMatcher` in `examples/extractor.rs`
   * `Matcher::Tag`: `Tag` marks the elements
   * internal state on top of `Vec`
 * `Matcher`: `MerkleDag` in `examples/ipfs.rs`
   * `Matcher::Tag`: `DagPbElement` marks the elements
 * `Gatherer`: `PBLinkGatherer` in `examples/ipfs.rs`
   * produces `PBLink<'_>`

The `MatcherFields`, and it's sibling `SlicedMatcherFields`, and
`GathererFields` implement the `minipb::Reader` abstraction which might work to
support actual byte sources such as `std::io::Read`.

## User visible conventions

 * `u64` is a file (or input) offset
 * `usize` is used when the user could have to slice something up

## Implementation

There are no tests yet, but examples contains a semi-useful `extractor` which
can parts of the document and one day format all of the fields as expected. The
path syntax could be similar to XPath, if you squint hard enough. The other
example is ipfs which does a similar thing, but gathers PBLinks out of an ipfs
dag-pb document.

Currently everything works with a dreaded `buf: &mut &[u8]`. After having
succesfully made progress, the `buf` is made shorter. To get anything useful
out of the system requires calculating these buffer offsets a lot, and it's
very easy to make off-by-N mistakes. While I couldn't see a better option, I
recognise this must be changed in order to support ring buffers `&mut (&'a
[u8], &'a [u8])`.

The aim of the crate is to have a core which would be `no_std` and provide all
kinds of wrappers which would allow you to consume what ever kind of
`std::io::Read`, `AsyncRead` du jour and so on. Currently barely
`std::io::Read` support has been implemented and even that might require
`unsafe` to get around NLL limitations (problem #3 to be specific, looping).

## TODO

* [x] get `std::io::Read` support working
* [x] major cleanup
* [x] move the `minipb::gather_fields::read` into more suitable place
* [ ] explore ringbuffer support
* [ ] read a field as bytes without buffering it at once
  * this shouldn't be hard to implement, not sure how useful it would be though
* [ ] packed repeated fields
* [ ] tests
  * read prime bytes at a time?
* [ ] benchmarks
  * maybe using OSM data ([format](https://wiki.openstreetmap.org/wiki/PBF_Format))?
* [ ] separate the `no_std` core and or provide a feature?
* [ ] get rid of unsafe when polonius hits the stable
* [ ] quick-protobuf parser integration for matcher and gatherer generation!
* [ ] world domination

# License and legal

Not yet decided. This has been a side project of mine at [Equilibrium](https://github.com/eqlabs).

[protobuf is not good for large files or messages]: https://developers.google.com/protocol-buffers/docs/techniques#large-data
