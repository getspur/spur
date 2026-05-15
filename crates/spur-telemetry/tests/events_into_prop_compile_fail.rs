#[test]
fn pathbuf_and_string_do_not_implement_into_prop() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/into_prop_string_fail.rs");
    t.compile_fail("tests/ui/into_prop_pathbuf_fail.rs");
}
