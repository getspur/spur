use std::fs;

use spur_graph::validation::{
    compute_anchor_hash, validate_file, validate_symbol, FailureReason, ValidationOutcome,
};
use spur_graph::{
    GraphFileArtifact, GraphSymbolArtifact, CODE_FILE_URI_PREFIX, CODE_SYMBOL_URI_PREFIX,
};

#[test]
fn code_uri_prefixes_remain_stable() {
    assert_eq!(CODE_FILE_URI_PREFIX, "graph://file/");
    assert_eq!(CODE_SYMBOL_URI_PREFIX, "graph://symbol/");
}

#[test]
fn validate_symbol_passes_for_exact_fixture_source_range() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "impl GraphEngine {\n    fn run(&self) {}\n}\n";
    fs::create_dir_all(dir.path().join("src")).expect("mkdir");
    fs::write(dir.path().join("src/lib.rs"), source).expect("write source");
    let start = source.find("fn run").expect("symbol start");
    let end = source.find("}\n}").expect("symbol end");
    let slice = &source[start..end];
    let payload = symbol_payload(
        "src/lib.rs",
        [start, end],
        "run",
        compute_anchor_hash(slice),
    );

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Pass
    );
}

#[test]
fn validate_symbol_fails_when_byte_range_is_out_of_bounds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "fn run() {}\n";
    fs::write(dir.path().join("lib.rs"), source).expect("write source");
    let payload = symbol_payload("lib.rs", [0, source.len() + 1], "run", 0);

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::RangeOutOfBounds)
    );
}

#[test]
fn validate_symbol_fails_when_slice_does_not_contain_entity_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "fn run() {}\nfn stop() {}\n";
    fs::write(dir.path().join("lib.rs"), source).expect("write source");
    let start = source.find("fn stop").expect("wrong symbol start");
    let end = source.len();
    let slice = &source[start..end];
    let payload = symbol_payload("lib.rs", [start, end], "run", compute_anchor_hash(slice));

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::NameNotFound)
    );
}

#[test]
fn validate_symbol_fails_when_entity_name_contains_path_separator() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "fn run() {}\n";
    fs::write(dir.path().join("lib.rs"), source).expect("write source");
    let payload = symbol_payload("lib.rs", [0, source.len()], "src/run", 0);

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::NameNotFound)
    );
}

#[test]
fn validate_symbol_fails_when_anchor_hash_mismatches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "fn run() {\n    old();\n}\n";
    fs::write(dir.path().join("lib.rs"), source).expect("write source");
    let start = source.find("fn run").expect("symbol start");
    let end = source.len();
    let payload = symbol_payload("lib.rs", [start, end], "run", 42);

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::AnchorHashMismatch)
    );
}

#[test]
fn validate_symbol_fails_when_byte_range_splits_utf8_character() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = "fn café() {}\n";
    fs::write(dir.path().join("lib.rs"), source).expect("write source");
    let split_inside_e_acute = source.find('é').expect("accented char") + 1;
    let payload = symbol_payload("lib.rs", [0, split_inside_e_acute], "café", 0);

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::Utf8Boundary)
    );
}

#[test]
fn validate_symbol_fails_when_file_path_is_deleted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("lib.rs");
    fs::write(&path, "fn run() {}\n").expect("write source");
    fs::remove_file(&path).expect("delete source");
    let payload = symbol_payload("lib.rs", [0, 11], "run", 0);

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::FileMissing)
    );
}

#[test]
fn validate_symbol_fails_when_file_path_was_renamed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original = dir.path().join("old.rs");
    let renamed = dir.path().join("new.rs");
    fs::write(&original, "fn run() {}\n").expect("write source");
    fs::rename(&original, &renamed).expect("rename source");
    let payload = symbol_payload("old.rs", [0, 11], "run", 0);

    assert_eq!(
        validate_symbol(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::FileMissing)
    );
}

#[test]
fn validate_file_fails_when_regular_file_is_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = GraphFileArtifact {
        stable_file_id: "file-lib".to_owned(),
        file_path: "lib.rs".to_owned(),
    };

    assert_eq!(
        validate_file(&payload, dir.path()),
        ValidationOutcome::Fail(FailureReason::FileMissing)
    );
}

fn symbol_payload(
    file_path: &str,
    byte_range: [usize; 2],
    entity_name: &str,
    anchor_hash: u64,
) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: "symbol-run".to_owned(),
        file_path: file_path.to_owned(),
        byte_range,
        line_range: [1, 1],
        entity_name: entity_name.to_owned(),
        qualified_name: entity_name.to_owned(),
        symbol_kind: "fn".to_owned(),
        anchor_hash: anchor_hash.to_string(),
        enclosing_scope: None,
    }
}
