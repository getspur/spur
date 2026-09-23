//! sud-m2a `MentionEngine::query` contract tests: typed error taxonomy
//! (root / traversal / source-build), required-vs-optional source policy,
//! ranking integration, tier policy, and result shaping (2026-08-27
//! mentions spec §2 and the error-handling table).

mod common;

use std::sync::Arc;

use common::{entry, legacy_file_uri, ScriptedSource, TempTree};
use spur_mentions::{
    FileMentionSource, FilesystemProfile, ManualClock, MentionEngine, MentionError, MentionKind,
    QueryOptions, RankOptions, TierPolicy,
};

fn options_with_limit(limit: Option<usize>) -> QueryOptions {
    QueryOptions {
        filesystem_profile: FilesystemProfile::tui(),
        rank: RankOptions {
            limit,
            tiers: TierPolicy::default(),
        },
    }
}

#[test]
fn missing_root_is_a_typed_root_error() {
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    let (source, _handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    engine.register_source(Box::new(source), true);

    let missing = std::env::temp_dir().join("spur-mentions-definitely-missing-root");
    let error = engine
        .query(&missing, "a", &QueryOptions::default())
        .expect_err("missing root must fail typed");
    assert!(matches!(error, MentionError::Root(_)));
}

#[test]
fn file_root_is_a_typed_root_error() {
    let tree = TempTree::new("root-is-file");
    let file = tree.file("not-a-dir.txt", "");
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    let (source, _handle) = ScriptedSource::new("fixture", vec![Vec::new()]);
    engine.register_source(Box::new(source), true);

    let error = engine
        .query(&file, "a", &QueryOptions::default())
        .expect_err("non-directory root must fail typed");
    assert!(matches!(error, MentionError::Root(_)));
}

#[test]
fn required_source_failure_fails_the_whole_query() {
    let tree = TempTree::new("required-fail");
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);
    handle
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let error = engine
        .query(tree.root(), "a", &QueryOptions::default())
        .expect_err("required source failure must surface");
    match error {
        MentionError::SourceBuild(failure) => {
            assert_eq!(failure.source_key, "fixture");
            assert_eq!(failure.message, "scripted build failure");
        }
        other => panic!("expected SourceBuild, got {other:?}"),
    }
}

#[test]
fn required_traversal_failure_maps_to_the_traversal_variant() {
    let tree = TempTree::new("required-traversal");
    let (source, handle) = ScriptedSource::new("walker", vec![Vec::new()]);
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);
    handle
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);
    handle
        .traversal
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let error = engine
        .query(tree.root(), "a", &QueryOptions::default())
        .expect_err("traversal failure must surface");
    match error {
        MentionError::Traversal(failure) => {
            assert_eq!(failure.source_key, "walker");
            assert_eq!(failure.message, "scripted traversal failure");
        }
        other => panic!("expected Traversal, got {other:?}"),
    }
}

#[test]
fn optional_source_failure_degrades_to_diagnostics() {
    let tree = TempTree::new("optional-degrade");
    let (main, _main_handle) = ScriptedSource::new(
        "main",
        vec![vec![entry(0, MentionKind::File, "file:///ab.rs", "ab.rs")]],
    );
    let (aux, aux_handle) = ScriptedSource::new("aux", vec![Vec::new()]);
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(main), true);
    engine.register_source(Box::new(aux), false);
    aux_handle
        .fail
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let result = engine
        .query(tree.root(), "ab", &QueryOptions::default())
        .expect("optional failure must not fail the query");
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].uri, "file:///ab.rs");
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].source_key, "aux");
    assert_eq!(result.diagnostics[0].message, "scripted build failure");
}

#[test]
fn query_ranks_matches_with_the_full_sort_reference() {
    let tree = TempTree::new("rank");
    let entries = vec![
        entry(0, MentionKind::File, "file:///a-x-b.txt", "a-x-b.txt"),
        entry(1, MentionKind::File, "file:///ab.rs", "ab.rs"),
        entry(2, MentionKind::File, "file:///zz.txt", "zz.txt"),
    ];
    let (source, _handle) = ScriptedSource::new("fixture", vec![entries]);
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    let result = engine
        .query(tree.root(), "ab", &options_with_limit(None))
        .expect("query succeeds");
    let uris: Vec<&str> = result.entries.iter().map(|e| e.uri.as_str()).collect();
    // Prefix match ranks above the scattered match; the non-match drops.
    assert_eq!(uris, vec!["file:///ab.rs", "file:///a-x-b.txt"]);
}

#[test]
fn empty_query_orders_by_tier_then_stable_key() {
    let tree = TempTree::new("empty-query");
    let entries = vec![
        entry(
            0,
            MentionKind::Custom("worker".into()),
            "worker://b",
            "worker:b",
        ),
        entry(1, MentionKind::File, "file:///b.rs", "b.rs"),
        entry(2, MentionKind::CodeSymbol, "graph://s", "zz-sym"),
        entry(3, MentionKind::File, "file:///a.rs", "a.rs"),
        entry(
            4,
            MentionKind::Custom("worker".into()),
            "worker://a",
            "worker:a",
        ),
        entry(5, MentionKind::Directory, "file:///d", "d/"),
    ];
    let (source, _handle) = ScriptedSource::new("fixture", vec![entries]);
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    let result = engine
        .query(tree.root(), "", &options_with_limit(None))
        .expect("empty query succeeds");
    let displays: Vec<&str> = result.entries.iter().map(|e| e.display.as_str()).collect();
    // Every row survives with rank 0: file tree first, caller kinds next
    // (tier policy), code kinds last; within a tier the stable tie key
    // (display) orders lexically.
    assert_eq!(
        displays,
        vec!["a.rs", "b.rs", "d/", "worker:a", "worker:b", "zz-sym"]
    );
}

#[test]
fn custom_tier_policy_orders_caller_defined_kinds() {
    let tree = TempTree::new("tiers");
    let entries = vec![
        entry(0, MentionKind::Custom("issue".into()), "issue://1", "i-1"),
        entry(1, MentionKind::Custom("worker".into()), "worker://a", "w-a"),
        entry(2, MentionKind::Custom("other".into()), "other://x", "o-x"),
        entry(3, MentionKind::File, "file:///a.rs", "a.rs"),
        entry(4, MentionKind::CodeSymbol, "graph://s", "c-sym"),
    ];
    let (source, _handle) = ScriptedSource::new("fixture", vec![entries]);
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    let mut options = options_with_limit(None);
    options.rank.tiers = TierPolicy {
        custom: [("worker".into(), 1u8), ("issue".into(), 2u8)]
            .into_iter()
            .collect(),
        custom_fallback: 3,
    };

    let result = engine
        .query(tree.root(), "", &options)
        .expect("tiered query succeeds");
    let displays: Vec<&str> = result.entries.iter().map(|e| e.display.as_str()).collect();
    assert_eq!(displays, vec!["a.rs", "w-a", "i-1", "o-x", "c-sym"]);
}

#[test]
fn limit_zero_and_limit_above_match_count() {
    let tree = TempTree::new("limits");
    let entries = vec![
        entry(0, MentionKind::File, "file:///a", "a"),
        entry(1, MentionKind::File, "file:///b", "b"),
        entry(2, MentionKind::File, "file:///c", "c"),
    ];
    let (source, _handle) = ScriptedSource::new("fixture", vec![entries]);
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    let empty = engine
        .query(tree.root(), "", &options_with_limit(Some(0)))
        .expect("K = 0 succeeds");
    assert!(empty.entries.is_empty());

    let all = engine
        .query(tree.root(), "", &options_with_limit(Some(10)))
        .expect("K > M succeeds");
    assert_eq!(all.entries.len(), 3);

    let one = engine
        .query(tree.root(), "", &options_with_limit(Some(1)))
        .expect("K = 1 succeeds");
    assert_eq!(one.entries.len(), 1);
    assert_eq!(one.entries[0].display, "a");
}

#[test]
fn engine_query_composes_with_the_shared_file_source() {
    let tree = TempTree::new("file-compose");
    tree.git_dir();
    tree.file(".gitignore", "ignored.txt\n");
    tree.file("ignored.txt", "");
    tree.file("sub/b.rs", "");
    tree.file("sp ace.txt", "");

    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(FileMentionSource::new()), true);

    let result = engine
        .query(tree.root(), "b.rs", &QueryOptions::default())
        .expect("file source query succeeds");
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].display, "sub/b.rs");
    // TUI-compatible URI construction survives the engine path unchanged.
    let expected = legacy_file_uri(&tree.root().canonicalize().unwrap().join("sub/b.rs"));
    assert_eq!(result.entries[0].uri, expected);
}

#[test]
fn result_rows_are_owned_data_not_cache_aliases() {
    let tree = TempTree::new("owned-rows");
    let (source, _handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    let mut result = engine
        .query(tree.root(), "a", &QueryOptions::default())
        .expect("query succeeds");
    result.entries[0].display.push_str(" (edited)");
    let again = engine
        .query(tree.root(), "a", &QueryOptions::default())
        .expect("second query succeeds");
    assert_eq!(again.entries[0].display, "a");
}
