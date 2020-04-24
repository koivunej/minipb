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

[protobuf is not good for large files or messages]: https://developers.google.com/protocol-buffers/docs/techniques#large-data
