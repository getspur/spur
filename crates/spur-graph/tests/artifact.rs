use std::fs;

use spur_graph::validation::{
    compute_anchor_hash, validate_file, validate_symbol, FailureReason, ValidationOutcome,
};
use spur_graph::{
    load_artifact, read_artifact_header, GraphFileArtifact, GraphSymbolArtifact,
    CODE_FILE_URI_PREFIX, CODE_SYMBOL_URI_PREFIX,
};

#[test]
fn load_artifact_accepts_fixture_schema() {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("crates/spur-tui/tests/fixtures/graph_index/sample.json");
    let artifact = load_artifact(&fixture_path).expect("fixture should load");

    assert_eq!(artifact.header.graph_index_version, "fixture-2026-05-11");
    assert_eq!(artifact.header.content_hash_blake3, None);
    assert_eq!(artifact.files.len(), 2);
    assert_eq!(artifact.symbols.len(), 4);
    assert!(artifact.diagnostics.is_empty());
    assert!(artifact
        .files
        .iter()
        .any(|file| file.stable_file_id == "file-config"
            && file.file_path == "crates/example/src/config.rs"));
    assert!(artifact.symbols.iter().any(|symbol| {
        symbol.stable_symbol_id == "symbol-config-struct"
            && symbol.file_path == "crates/example/src/config.rs"
            && symbol.entity_name == "Config"
            && symbol.symbol_kind == "struct"
    }));
    assert!(artifact.symbols.iter().any(|symbol| {
        symbol.stable_symbol_id == "symbol-engine-run-method"
            && symbol.enclosing_scope.as_deref() == Some("impl GraphEngine")
    }));
    assert_eq!(CODE_FILE_URI_PREFIX, "graph://file/");
    assert_eq!(CODE_SYMBOL_URI_PREFIX, "graph://symbol/");
}

#[test]
fn load_artifact_defaults_missing_content_hash_to_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("legacy_no_hash.json");
    fs::write(
        &path,
        r#"{
          "header": {"graph_index_version": "v1"},
          "files": [],
          "symbols": []
        }"#,
    )
    .expect("write fixture");

    let artifact = load_artifact(&path).expect("legacy artifact should load");
    assert_eq!(artifact.header.graph_index_version, "v1");
    assert_eq!(artifact.header.content_hash_blake3, None);
}

#[test]
fn read_artifact_header_extracts_content_hash_when_present() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("with_hash.json");
    fs::write(
        &path,
        r#"{
          "header": {
            "graph_index_version": "v1",
            "content_hash_blake3": "abc123"
          },
          "manifest_version": "m1",
          "file_manifests": [],
          "files": [],
          "symbols": []
        }"#,
    )
    .expect("write fixture");

    let header = read_artifact_header(&path).expect("header should load");
    assert_eq!(header.graph_index_version, "v1");
    assert_eq!(header.content_hash_blake3.as_deref(), Some("abc123"));
}

#[test]
fn load_artifact_rejects_truncated_json_with_clear_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("truncated.json");
    fs::write(&path, r#"{"header":{"graph_index_version":"v1"},"#).expect("write fixture");

    let error = load_artifact(&path).expect_err("truncated JSON should fail");
    let message = error.to_string();

    assert!(
        message.contains("invalid graph index JSON"),
        "unexpected error: {message}"
    );
}

#[test]
fn load_artifact_deduplicates_duplicate_symbol_ids_with_diagnostic() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("duplicate.json");
    fs::write(
        &path,
        r#"{
          "header": {"graph_index_version": "v1"},
          "files": [
            {"stable_file_id": "file-a", "file_path": "src/a.rs"}
          ],
          "symbols": [
            {
              "stable_symbol_id": "sym-a",
              "file_path": "src/a.rs",
              "byte_range": [0, 10],
              "line_range": [1, 2],
              "entity_name": "First",
              "symbol_kind": "struct",
              "anchor_hash": "hash-first",
              "enclosing_scope": "module a"
            },
            {
              "stable_symbol_id": "sym-a",
              "file_path": "src/a.rs",
              "byte_range": [20, 30],
              "line_range": [4, 5],
              "entity_name": "Second",
              "symbol_kind": "struct",
              "anchor_hash": "hash-second",
              "enclosing_scope": "module a"
            }
          ]
        }"#,
    )
    .expect("write fixture");

    let artifact = load_artifact(&path).expect("duplicate ids should be diagnosed");

    assert_eq!(artifact.symbols.len(), 1);
    assert_eq!(artifact.symbols[0].entity_name, "First");
    assert_eq!(artifact.diagnostics.len(), 1);
    assert!(
        artifact.diagnostics[0].contains("duplicate stable_symbol_id `sym-a`"),
        "unexpected diagnostic: {:?}",
        artifact.diagnostics
    );
}

#[test]
fn load_artifact_rejects_reversed_byte_ranges_deterministically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("reversed.json");
    fs::write(
        &path,
        r#"{
          "header": {"graph_index_version": "v1"},
          "files": [
            {"stable_file_id": "file-a", "file_path": "src/a.rs"}
          ],
          "symbols": [
            {
              "stable_symbol_id": "sym-a",
              "file_path": "src/a.rs",
              "byte_range": [10, 9],
              "line_range": [1, 2],
              "entity_name": "Broken",
              "symbol_kind": "fn",
              "anchor_hash": "hash-broken",
              "enclosing_scope": null
            }
          ]
        }"#,
    )
    .expect("write fixture");

    let error = load_artifact(&path).expect_err("reversed byte range should fail");
    assert_eq!(
        error.to_string(),
        "graph index symbol `sym-a` has reversed byte_range [10, 9]"
    );
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
        stable_file_id: "file-lib".to_string(),
        file_path: "lib.rs".to_string(),
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
        stable_symbol_id: "symbol-run".to_string(),
        file_path: file_path.to_string(),
        byte_range,
        line_range: [1, 1],
        entity_name: entity_name.to_string(),
        symbol_kind: "fn".to_string(),
        anchor_hash: anchor_hash.to_string(),
        enclosing_scope: None,
    }
}
