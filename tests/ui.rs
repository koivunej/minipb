#[test]
fn these_should_fail_to_compile() {
    let t = trybuild::TestCases::new();
    // these tests are related to trying to cover the unsafe use in
    // minipb::io_ext::read::ReadWrapper
    t.compile_fail("tests/ui/*.rs");
}
