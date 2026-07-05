use std::fs;

#[path = "fixtures/mod.rs"]
mod fixtures;

use fixtures::crate_path;

#[test]
fn step16_moves_large_inline_tests_to_focused_test_directories() {
    for dir in [
        "tests/fixtures",
        "tests/pack",
        "tests/search",
        "tests/embedding",
        "tests/paths",
    ] {
        assert!(crate_path(dir).is_dir(), "missing {dir}");
    }

    assert!(
        crate_path("tests/fixtures/mod.rs").is_file(),
        "missing shared integration-test fixtures module"
    );

    let pack_service =
        fs::read_to_string(crate_path("src/pack/service.rs")).expect("read pack service source");
    assert!(
        !pack_service.contains("#[cfg(test)]\nmod tests"),
        "pack service should not carry the large inline integration suite"
    );
}
