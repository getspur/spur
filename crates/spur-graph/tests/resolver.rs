use std::fs;
use std::path::{Path, PathBuf};

use spur_graph::{
    resolve_artifact_location, write_current_pointer, ArtifactFormat, GraphIndexPointer,
    ResolvedArtifact, SourceKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedChoice {
    Explicit,
    Current,
    Pointer,
    Legacy,
    Missing,
}

#[derive(Debug)]
struct ResolverCase {
    explicit_override: bool,
    current: bool,
    pointer: bool,
    legacy: bool,
}

#[test]
fn explicit_current_pointer_legacy() {
    assert_case(ResolverCase::new(true, true, true, true));
}

#[test]
fn explicit_current_pointer_no_legacy() {
    assert_case(ResolverCase::new(true, true, true, false));
}

#[test]
fn explicit_current_no_pointer_legacy() {
    assert_case(ResolverCase::new(true, true, false, true));
}

#[test]
fn explicit_current_no_pointer_no_legacy() {
    assert_case(ResolverCase::new(true, true, false, false));
}

#[test]
fn explicit_no_current_pointer_legacy() {
    assert_case(ResolverCase::new(true, false, true, true));
}

#[test]
fn explicit_no_current_pointer_no_legacy() {
    assert_case(ResolverCase::new(true, false, true, false));
}

#[test]
fn explicit_no_current_no_pointer_legacy() {
    assert_case(ResolverCase::new(true, false, false, true));
}

#[test]
fn explicit_no_current_no_pointer_no_legacy() {
    assert_case(ResolverCase::new(true, false, false, false));
}

#[test]
fn no_explicit_current_pointer_legacy() {
    assert_case(ResolverCase::new(false, true, true, true));
}

#[test]
fn no_explicit_current_pointer_no_legacy() {
    assert_case(ResolverCase::new(false, true, true, false));
}

#[test]
fn no_explicit_current_no_pointer_legacy() {
    assert_case(ResolverCase::new(false, true, false, true));
}

#[test]
fn no_explicit_current_no_pointer_no_legacy() {
    assert_case(ResolverCase::new(false, true, false, false));
}

#[test]
fn no_explicit_no_current_pointer_legacy() {
    assert_case(ResolverCase::new(false, false, true, true));
}

#[test]
fn no_explicit_no_current_pointer_no_legacy() {
    assert_case(ResolverCase::new(false, false, true, false));
}

#[test]
fn no_explicit_no_current_no_pointer_legacy() {
    assert_case(ResolverCase::new(false, false, false, true));
}

#[test]
fn no_explicit_no_current_no_pointer_no_legacy() {
    assert_case(ResolverCase::new(false, false, false, false));
}

#[test]
fn explicit_override_accepts_parquet_directory() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let worktree = tempdir.path();
    let parquet_dir = worktree.join("explicit.parquet");
    write_parquet_manifest(&parquet_dir, "explicit-parquet-hash");

    let resolved = resolve_artifact_location(worktree, Some(&parquet_dir))
        .expect("explicit parquet should resolve");

    assert_resolved(
        &resolved,
        &parquet_dir,
        ArtifactFormat::Parquet,
        "explicit-parquet-hash",
    );
}

impl ResolverCase {
    fn new(explicit_override: bool, current: bool, pointer: bool, legacy: bool) -> Self {
        Self {
            explicit_override,
            current,
            pointer,
            legacy,
        }
    }
}

fn assert_case(case: ResolverCase) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let worktree = tempdir.path();
    let paths = FixturePaths::new(worktree);

    if case.explicit_override {
        write_legacy_json(&paths.explicit);
    }
    if case.current {
        write_parquet_manifest(&paths.current, "current-hash");
        write_current_pointer(worktree, &paths.current).expect("write CURRENT");
    }
    if case.pointer {
        write_parquet_manifest(&paths.pointer, "pointer-hash");
        write_pointer_file(worktree, &paths.pointer);
    }
    if case.legacy {
        write_legacy_json(&paths.legacy);
    }

    let explicit = case.explicit_override.then_some(paths.explicit.as_path());
    let expected = expected_choice(case);
    let actual = resolve_artifact_location(worktree, explicit);

    match expected {
        ExpectedChoice::Explicit => {
            let resolved = actual.expect("explicit override should resolve");
            assert_resolved(&resolved, &paths.explicit, ArtifactFormat::LegacyJson, "");
        }
        ExpectedChoice::Current => {
            let resolved = actual.expect("CURRENT should resolve");
            assert_resolved(
                &resolved,
                &paths.current,
                ArtifactFormat::Parquet,
                "current-hash",
            );
        }
        ExpectedChoice::Pointer => {
            let resolved = actual.expect("pointer file should resolve");
            assert_resolved(
                &resolved,
                &paths.pointer,
                ArtifactFormat::Parquet,
                "pointer-hash",
            );
        }
        ExpectedChoice::Legacy => {
            let resolved = actual.expect("legacy JSON should resolve");
            assert_resolved(&resolved, &paths.legacy, ArtifactFormat::LegacyJson, "");
        }
        ExpectedChoice::Missing => {
            let err = actual.expect_err("missing artifacts should fail");
            assert!(
                err.to_string().contains("no valid spur graph artifact"),
                "unexpected error: {err:#}"
            );
        }
    }
}

fn expected_choice(case: ResolverCase) -> ExpectedChoice {
    if case.explicit_override {
        ExpectedChoice::Explicit
    } else if case.current {
        ExpectedChoice::Current
    } else if case.pointer {
        ExpectedChoice::Pointer
    } else if case.legacy {
        ExpectedChoice::Legacy
    } else {
        ExpectedChoice::Missing
    }
}

fn assert_resolved(
    resolved: &ResolvedArtifact,
    expected_path: &Path,
    expected_format: ArtifactFormat,
    expected_parquet_hash: &str,
) {
    assert_eq!(resolved.path, canonicalize(expected_path));
    assert_eq!(resolved.format, expected_format);
    match resolved.format {
        ArtifactFormat::LegacyJson => match &resolved.cache_key {
            spur_graph::ArtifactCacheKey::LegacyJson { path, .. } => {
                assert_eq!(path, &canonicalize(expected_path));
            }
            other => panic!("expected legacy cache key, got {other:?}"),
        },
        ArtifactFormat::Parquet => match &resolved.cache_key {
            spur_graph::ArtifactCacheKey::Parquet { graph_content_hash } => {
                assert_eq!(graph_content_hash, expected_parquet_hash);
            }
            other => panic!("expected parquet cache key, got {other:?}"),
        },
    }
}

struct FixturePaths {
    explicit: PathBuf,
    current: PathBuf,
    pointer: PathBuf,
    legacy: PathBuf,
}

impl FixturePaths {
    fn new(worktree: &Path) -> Self {
        Self {
            explicit: worktree.join("explicit.json"),
            current: worktree.join("artifacts/current.parquet"),
            pointer: worktree.join("artifacts/pointer.parquet"),
            legacy: worktree.join(".spur/graph-index.json"),
        }
    }
}

fn write_legacy_json(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir legacy parent");
    }
    fs::write(
        path,
        r#"{
  "header": { "graph_index_version": "legacy" },
  "files": [],
  "symbols": []
}"#,
    )
    .expect("write legacy JSON");
}

fn write_parquet_manifest(path: &Path, graph_content_hash: &str) {
    fs::create_dir_all(path).expect("mkdir parquet dir");
    fs::write(
        path.join("manifest.json"),
        format!(
            r#"{{
  "graph_index_version": "spur-graph-phase2",
  "schema_version": "spur-graph-schema-v5",
  "manifest_version": "manifest-test",
  "graph_content_hash": "{graph_content_hash}",
  "indexed_commit_oid": null,
  "extractor_version": "test-extractor",
  "complete": true,
  "row_counts": {{
    "nodes": 0,
    "edges": 0,
    "edges_by_dst": null,
    "edges_unresolved": 0,
    "files": 0,
    "file_manifests": 0,
    "tombstones": 0
  }},
  "parquet_writer": {{
    "compression": "zstd-3",
    "row_group_size": 16384
  }},
  "edges_by_dst_present": false
}}"#
        ),
    )
    .expect("write parquet manifest");
}

fn write_pointer_file(worktree: &Path, canonical_artifact_path: &Path) {
    let pointer_path = worktree.join(".spur/graph-index.pointer.json");
    fs::create_dir_all(pointer_path.parent().expect("pointer parent")).expect("mkdir .spur");
    let pointer = GraphIndexPointer {
        schema: "spur-graph-pointer-v1".to_string(),
        graph_content_hash: "pointer-hash".to_string(),
        manifest_version: "manifest-test".to_string(),
        source_kind: SourceKind::Git,
        indexed_commit_oid: Some("test-head".to_string()),
        canonical_artifact_path: canonical_artifact_path.to_path_buf(),
    };
    fs::write(
        pointer_path,
        serde_json::to_vec_pretty(&pointer).expect("encode pointer"),
    )
    .expect("write pointer");
}

fn canonicalize(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|err| panic!("canonicalize `{}`: {err}", path.display()))
}
