//! sud-m2a cache contract tests: cache identity
//! (`canonical_root + source_key + profile_fingerprint + source_token`),
//! 600s TTL on an injected clock, failed-build retention, monotonic
//! generations, and explicit invalidation (2026-08-27 mentions spec §4).

mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{entry, legacy_file_uri, ScriptedSource, TempTree};
use spur_mentions::{
    Clock, FilesystemProfile, ManualClock, MentionEngine, MentionError, MentionKind, QueryOptions,
    SystemClock, DEFAULT_SNAPSHOT_TTL,
};

fn query(engine: &mut MentionEngine, root: &std::path::Path) -> spur_mentions::QueryResult {
    // Empty query keeps every row (rank 0): the cache tests assert on
    // snapshot *content*, not on ranking.
    engine
        .query(root, "", &QueryOptions::default())
        .expect("scripted query succeeds")
}

#[test]
fn default_snapshot_ttl_is_600_seconds() {
    assert_eq!(DEFAULT_SNAPSHOT_TTL, Duration::from_secs(600));
}

#[test]
fn manual_clock_controls_virtual_time() {
    let manual = ManualClock::new();
    let first = manual.now();
    manual.advance(Duration::from_secs(599));
    let second = manual.now();
    assert!(second > first);
    assert!(second.duration_since(first) >= Duration::from_secs(599));
}

#[test]
fn system_clock_is_monotonic() {
    let clock = SystemClock;
    let first = clock.now();
    let second = clock.now();
    assert!(second >= first);
}

#[test]
fn same_key_reuses_snapshot_within_ttl() {
    let tree = TempTree::new("cache-hit");
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///old", "old")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    let first = query(&mut engine, tree.root());
    let second = query(&mut engine, tree.root());

    assert_eq!(first.entries, second.entries);
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);
}

#[test]
fn ttl_boundary_is_exclusive() {
    let tree = TempTree::new("ttl-boundary");
    let manual = Arc::new(ManualClock::new());
    let clock: Arc<dyn Clock> = Arc::<ManualClock>::clone(&manual);
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::clone(&clock));
    engine.register_source(Box::new(source), true);

    query(&mut engine, tree.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);

    // Exactly 600s old is still fresh: today's TUI rebuilds only when
    // `built_at.elapsed() > ttl` (strictly greater).
    manual.advance(DEFAULT_SNAPSHOT_TTL);
    query(&mut engine, tree.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);

    // One nanosecond past the boundary forces a rebuild.
    manual.advance(Duration::from_nanos(1));
    query(&mut engine, tree.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);
}

#[test]
fn distinct_canonical_roots_have_distinct_cache_keys() {
    let tree_a = TempTree::new("root-a");
    let tree_b = TempTree::new("root-b");
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    query(&mut engine, tree_a.root());
    query(&mut engine, tree_b.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);

    // Root A is still cached: revisiting it must not rebuild.
    query(&mut engine, tree_a.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);
}

#[test]
#[cfg(unix)]
fn symlinked_root_shares_the_canonical_entry() {
    let tree = TempTree::new("root-symlink");
    tree.file("a.rs", "");
    let alias = TempTree::new("root-symlink-alias");
    std::os::unix::fs::symlink(tree.root(), alias.root().join("link")).expect("root symlink");

    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    query(&mut engine, tree.root());
    // Querying through the symlink canonicalizes to the same root, so this
    // is a cache hit, not a second build.
    query(&mut engine, &alias.root().join("link"));
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);
}

#[test]
fn profile_fingerprint_partitions_the_cache() {
    let tree = TempTree::new("profile-key");
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    let tui = QueryOptions {
        filesystem_profile: FilesystemProfile::tui(),
        ..QueryOptions::default()
    };
    let notebook = QueryOptions {
        filesystem_profile: FilesystemProfile::notebook_compat(),
        ..QueryOptions::default()
    };

    engine.query(tree.root(), "x", &tui).expect("tui query");
    engine
        .query(tree.root(), "x", &notebook)
        .expect("notebook query");
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);

    // The first profile's snapshot is still cached under its own key.
    engine.query(tree.root(), "x", &tui).expect("tui re-query");
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);
}

#[test]
fn source_token_change_forces_rebuild() {
    let tree = TempTree::new("token-key");
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    query(&mut engine, tree.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);

    // Same token: hit.
    query(&mut engine, tree.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);

    // New token (e.g. a code-graph artifact moved): the cache key changes.
    handle.token.store(7, Ordering::Relaxed);
    query(&mut engine, tree.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);
}

#[test]
fn failed_rebuild_retains_previous_snapshot_for_optional_source() {
    let tree = TempTree::new("retention-optional");
    let manual = Arc::new(ManualClock::new());
    let clock: Arc<dyn Clock> = Arc::<ManualClock>::clone(&manual);
    let (source, handle) = ScriptedSource::new(
        "aux",
        vec![
            vec![entry(0, MentionKind::File, "file:///old", "old")],
            vec![entry(0, MentionKind::File, "file:///new", "new")],
        ],
    );
    let mut engine = MentionEngine::new(Arc::clone(&clock));
    engine.register_source(Box::new(source), false);

    let first = query(&mut engine, tree.root());
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].uri, "file:///old");

    // Expire the snapshot, then make the rebuild fail. The query must keep
    // serving the previous complete snapshot instead of flattening to an
    // empty picker, and report the failure as a diagnostic.
    manual.advance(DEFAULT_SNAPSHOT_TTL + Duration::from_secs(1));
    handle.fail.store(true, Ordering::Relaxed);
    let degraded = query(&mut engine, tree.root());
    assert_eq!(degraded.entries.len(), 1);
    assert_eq!(degraded.entries[0].uri, "file:///old");
    assert_eq!(degraded.diagnostics.len(), 1);
    assert_eq!(degraded.diagnostics[0].source_key, "aux");
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);

    // A later successful rebuild publishes the new generation.
    handle.fail.store(false, Ordering::Relaxed);
    let recovered = query(&mut engine, tree.root());
    assert!(recovered.diagnostics.is_empty());
    assert_eq!(recovered.entries.len(), 1);
    assert_eq!(recovered.entries[0].uri, "file:///new");
}

#[test]
fn required_source_failure_is_typed_and_retains_the_cache() {
    let tree = TempTree::new("retention-required");
    let manual = Arc::new(ManualClock::new());
    let clock: Arc<dyn Clock> = Arc::<ManualClock>::clone(&manual);
    let (source, handle) = ScriptedSource::new(
        "main",
        vec![
            vec![entry(0, MentionKind::File, "file:///old", "old")],
            vec![entry(0, MentionKind::File, "file:///new", "new")],
        ],
    );
    let mut engine = MentionEngine::new(Arc::clone(&clock));
    engine.register_source(Box::new(source), true);

    let first = query(&mut engine, tree.root());
    assert_eq!(first.entries[0].uri, "file:///old");
    let generation_before = engine
        .snapshot_generation("main")
        .expect("generation stamped after first build");

    // Expire, then fail: required sources fail the query with a typed error.
    manual.advance(DEFAULT_SNAPSHOT_TTL + Duration::from_secs(1));
    handle.fail.store(true, Ordering::Relaxed);
    let error = engine
        .query(tree.root(), "x", &QueryOptions::default())
        .expect_err("required source failure fails the query");
    assert!(matches!(
        error,
        MentionError::SourceBuild(failure) if failure.source_key == "main"
    ));

    // Retention: the failed build never replaced the valid snapshot, and
    // the generation counter never went backwards.
    assert_eq!(engine.snapshot_generation("main"), Some(generation_before));

    handle.fail.store(false, Ordering::Relaxed);
    let recovered = query(&mut engine, tree.root());
    assert_eq!(recovered.entries[0].uri, "file:///new");
    assert!(
        engine.snapshot_generation("main").unwrap_or(0) > generation_before,
        "recovery publishes a strictly newer generation"
    );
}

#[test]
fn snapshot_generations_are_monotonic_across_rebuilds() {
    let tree = TempTree::new("generations");
    let manual = Arc::new(ManualClock::new());
    let clock: Arc<dyn Clock> = Arc::<ManualClock>::clone(&manual);
    let (source, _handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::clone(&clock));
    engine.register_source(Box::new(source), true);

    assert_eq!(engine.snapshot_generation("fixture"), None);
    query(&mut engine, tree.root());
    let first = engine.snapshot_generation("fixture").expect("gen 1");

    manual.advance(DEFAULT_SNAPSHOT_TTL + Duration::from_secs(1));
    query(&mut engine, tree.root());
    let second = engine.snapshot_generation("fixture").expect("gen 2");
    assert!(second > first);

    // Generation continues across profile switches for the same source.
    let notebook = QueryOptions {
        filesystem_profile: FilesystemProfile::notebook_compat(),
        ..QueryOptions::default()
    };
    engine.query(tree.root(), "x", &notebook).expect("query");
    let third = engine.snapshot_generation("fixture").expect("gen 3");
    assert!(third > second);
}

#[test]
fn clear_cache_forces_rebuild_but_keeps_generations_monotonic() {
    let tree = TempTree::new("clear");
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    query(&mut engine, tree.root());
    let before = engine.snapshot_generation("fixture").expect("gen");

    engine.clear_cache();
    query(&mut engine, tree.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);
    assert!(engine.snapshot_generation("fixture").unwrap_or(0) > before);
}

#[test]
fn invalidate_root_drops_only_that_root() {
    let tree_a = TempTree::new("invalidate-a");
    let tree_b = TempTree::new("invalidate-b");
    let (source, handle) = ScriptedSource::new(
        "fixture",
        vec![vec![entry(0, MentionKind::File, "file:///a", "a")]],
    );
    let mut engine = MentionEngine::new(Arc::new(ManualClock::new()));
    engine.register_source(Box::new(source), true);

    query(&mut engine, tree_a.root());
    query(&mut engine, tree_b.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);

    engine.invalidate_root(tree_a.root());

    // A rebuilds; B is untouched.
    query(&mut engine, tree_a.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 3);
    query(&mut engine, tree_b.root());
    assert_eq!(handle.builds.load(Ordering::Relaxed), 3);
}

#[test]
fn engine_is_send_and_shareable_behind_arc_mutex() {
    fn assert_send<T: Send>() {}
    assert_send::<MentionEngine>();
    fn share<T: Send>(_lock: Arc<Mutex<T>>) {}
    share(Arc::new(Mutex::new(MentionEngine::new(Arc::new(
        ManualClock::new(),
    )))));
}

#[test]
fn cache_ttl_is_configurable() {
    let engine = MentionEngine::with_ttl(Arc::new(ManualClock::new()), Duration::from_secs(1));
    assert_eq!(engine.cache_ttl(), Duration::from_secs(1));
    assert_eq!(
        MentionEngine::new(Arc::new(ManualClock::new())).cache_ttl(),
        DEFAULT_SNAPSHOT_TTL
    );
}

#[test]
fn retained_legacy_uri_stays_unencoded_for_tui_parity() {
    // Pins the URI encoder the TUI-compatible profile must keep: raw
    // `file://{abs}` formatting, no percent-encoding.
    let tree = TempTree::new("uri-encoder");
    let path = tree.file("sp ace.txt", "");
    assert_eq!(legacy_file_uri(&path), format!("file://{}", path.display()));
}
