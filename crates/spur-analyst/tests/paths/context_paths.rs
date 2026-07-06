use crate::fixtures;

#[test]
fn paths_test_directory_exists_for_context_path_coverage() {
    assert!(fixtures::crate_path("tests/paths").is_dir());
}
