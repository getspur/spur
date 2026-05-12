use std::fs;

use spur_tui::mentions::code_graph::{load_artifact, CODE_FILE_URI_PREFIX, CODE_SYMBOL_URI_PREFIX};

#[test]
fn load_artifact_accepts_fixture_schema() {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/graph_index/sample.json");
    let artifact = load_artifact(&fixture_path).expect("fixture should load");

    assert_eq!(artifact.header.graph_index_version, "fixture-2026-05-11");
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
