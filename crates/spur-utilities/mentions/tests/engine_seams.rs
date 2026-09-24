//! sud-m2b seam contract tests: the per-source resolve/publish/invalidate
//! surface the TUI facade composes on (`spur-mentions`, decoupling spec
//! §4.1 "delegating neutral work to the engine"). The fused
//! `MentionEngine::query` path is covered by `engine_query.rs`; these tests
//! pin the seams a frontend with deferred builds and frontend-specific
//! ranking needs, including the raw-root build contract that keeps
//! TUI-derived file URIs byte-identical.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{entry, ScriptedSource, SourceHandle, TempTree};
use spur_mentions::{
    MentionEngine, MentionKind, MentionSource, QueryOptions, SnapshotResolution, SourceBuildError,
    SourceContext, SourceSnapshot, SystemClock,
};

fn options() -> QueryOptions {
    QueryOptions::new()
}

/// Source that records the exact `root` each `build` received.
struct RootRecordingSource {
    handle: SourceHandle,
    roots: Arc<Mutex<Vec<PathBuf>>>,
}

impl RootRecordingSource {
    fn new() -> (Self, Arc<Mutex<Vec<PathBuf>>>, SourceHandle) {
        let handle = SourceHandle::default();
        let roots = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                handle: handle.clone(),
                roots: Arc::clone(&roots),
            },
            roots,
            handle,
        )
    }
}

impl MentionSource for RootRecordingSource {
    fn key(&self) -> &'static str {
        "recording"
    }

    fn source_token(&self) -> u64 {
        self.handle.token.load(Ordering::Relaxed)
    }

    fn build(
        &mut self,
        root: &Path,
        _context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError> {
        self.handle.builds.fetch_add(1, Ordering::Relaxed);
        self.roots
            .lock()
            .expect("roots lock")
            .push(root.to_path_buf());
        Ok(SourceSnapshot::new(
            vec![entry(0, MentionKind::File, "file:///ws/a", "a")],
            0,
        ))
    }
}

#[test]
fn resolve_snapshot_builds_then_serves_fresh_within_ttl() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(Arc::clone(&clock) as _);
    let (source, handle) = ScriptedSource::new(
        "s",
        vec![vec![entry(0, MentionKind::File, "file:///ws/a", "a")]],
    );
    engine.register_source(Box::new(source), false);

    let root = Path::new("/ws");
    let SnapshotResolution::Built(built) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("first resolve must build");
    };
    assert_eq!(built.generation, 1);
    assert_eq!(built.entries.len(), 1);

    let SnapshotResolution::Fresh(fresh) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("second resolve within TTL must be a fresh hit");
    };
    assert_eq!(fresh.generation, built.generation);
    assert!(Arc::ptr_eq(&built, &fresh), "fresh hit serves the same Arc");
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);

    clock.advance(Duration::from_secs(601));
    let SnapshotResolution::Built(rebuilt) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("resolve past TTL must rebuild");
    };
    assert_eq!(rebuilt.generation, 2, "generations stay monotonic");
    assert_eq!(handle.builds.load(Ordering::Relaxed), 2);
}

#[test]
fn retained_snapshot_keeps_expired_data_only_for_the_same_root_profile_and_token() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(clock.clone());
    let (source, handle) = ScriptedSource::new("s", vec![vec![]]);
    engine.register_source(Box::new(source), false);
    let root = Path::new("/retained-root");
    let SnapshotResolution::Built(built) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("first resolve must build");
    };
    clock.advance(engine.cache_ttl() + Duration::from_nanos(1));
    assert!(engine.cached_snapshot(root, "s", &options()).is_none());
    let retained = engine.retained_snapshot(root, "s", &options()).unwrap();
    assert!(Arc::ptr_eq(&built, &retained));
    assert!(engine
        .retained_snapshot(Path::new("/other-root"), "s", &options())
        .is_none());
    let mut other_profile = options();
    other_profile.filesystem_profile = spur_mentions::FilesystemProfile::notebook_compat();
    assert!(engine
        .retained_snapshot(root, "s", &other_profile)
        .is_none());
    handle.token.store(1, Ordering::Relaxed);
    assert!(engine.retained_snapshot(root, "s", &options()).is_none());
    handle.token.store(0, Ordering::Relaxed);
    engine.invalidate_source("s");
    assert!(engine.retained_snapshot(root, "s", &options()).is_none());
}

#[test]
fn resolve_snapshot_passes_root_verbatim_and_shares_identity_across_spellings() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(Arc::clone(&clock) as _);
    let (source, roots, _handle) = RootRecordingSource::new();
    engine.register_source(Box::new(source), false);

    let tree = TempTree::new("seams-raw-root");
    let raw = tree.root().join("nested"); // exists; identity uses canonical form
    std::fs::create_dir_all(&raw).expect("nested dir");

    let SnapshotResolution::Built(_) = engine.resolve_snapshot(&raw, "recording", &options())
    else {
        panic!("first resolve must build");
    };
    let recorded = roots.lock().expect("roots lock").clone();
    assert_eq!(
        recorded,
        vec![raw.clone()],
        "build must receive the root exactly as passed, never canonicalized"
    );

    // Same directory via an equivalent spelling: shared cache identity, no
    // second build, and the fresh hit serves the first snapshot.
    let dotted = raw.join(".");
    let SnapshotResolution::Fresh(_) = engine.resolve_snapshot(&dotted, "recording", &options())
    else {
        panic!("equivalent spelling must hit the same snapshot");
    };
    assert_eq!(
        roots.lock().expect("roots lock").len(),
        1,
        "identity is shared across spellings, so no rebuild happens"
    );
}

#[test]
fn resolve_snapshot_retains_previous_snapshot_on_optional_failure() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(Arc::clone(&clock) as _);
    let (source, handle) = ScriptedSource::new(
        "s",
        vec![vec![entry(0, MentionKind::File, "file:///ws/a", "a")]],
    );
    engine.register_source(Box::new(source), false);
    let root = Path::new("/ws");

    let SnapshotResolution::Built(first) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("first resolve must build");
    };

    // Fail past the TTL: the retained snapshot keeps serving, generations
    // do not advance, and the failed build never replaces it.
    clock.advance(Duration::from_secs(601));
    handle.fail.store(true, Ordering::Relaxed);
    let SnapshotResolution::Retained(retained) = engine.resolve_snapshot(root, "s", &options())
    else {
        panic!("failed rebuild must degrade to Retained");
    };
    assert!(Arc::ptr_eq(&first, &retained));

    // Recovery rebuilds with the next generation.
    handle.fail.store(false, Ordering::Relaxed);
    let SnapshotResolution::Built(rebuilt) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("recovered resolve must rebuild");
    };
    assert_eq!(rebuilt.generation, first.generation + 1);
}

#[test]
fn resolve_snapshot_reports_failure_when_nothing_is_retained() {
    let mut engine = MentionEngine::new(Arc::new(SystemClock) as _);
    let (source, handle) = ScriptedSource::new("s", vec![Vec::new()]);
    engine.register_source(Box::new(source), false);
    handle.fail.store(true, Ordering::Relaxed);

    match engine.resolve_snapshot(Path::new("/ws"), "s", &options()) {
        SnapshotResolution::Failed(failure) => {
            assert_eq!(failure.source_key, "s");
            assert_eq!(failure.message, "scripted build failure");
        }
        other => panic!("expected Failed, got {other:?}"),
    }

    // Unknown keys degrade the same way instead of panicking.
    match engine.resolve_snapshot(Path::new("/ws"), "missing", &options()) {
        SnapshotResolution::Failed(failure) => assert_eq!(failure.source_key, "missing"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn publish_snapshot_stamps_monotonic_generations_and_feeds_the_cache() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(Arc::clone(&clock) as _);
    let (source, _handle) = ScriptedSource::new("s", vec![Vec::new()]);
    engine.register_source(Box::new(source), false);
    let root = Path::new("/ws");

    let SnapshotResolution::Built(built) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("first resolve must build");
    };
    assert_eq!(built.generation, 1);

    // A deferred (off-engine) build publishes externally.
    let external = SourceSnapshot::new(vec![entry(0, MentionKind::File, "file:///ws/b", "b")], 0);
    let published = engine
        .publish_snapshot(root, "s", &options(), external)
        .expect("registered source publishes");
    assert_eq!(
        published.generation,
        built.generation + 1,
        "publish stamps the next generation"
    );

    // The published snapshot is now the fresh cache content.
    let fresh = engine
        .cached_snapshot(root, "s", &options())
        .expect("published snapshot is fresh");
    assert!(Arc::ptr_eq(&published, &fresh));
    assert_eq!(fresh.entries.len(), 1);
    assert_eq!(fresh.entries[0].uri, "file:///ws/b");

    let SnapshotResolution::Fresh(hit) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("resolve after publish must hit the cache");
    };
    assert!(Arc::ptr_eq(&published, &hit));

    assert!(
        engine
            .publish_snapshot(
                root,
                "missing",
                &options(),
                SourceSnapshot::new(Vec::new(), 0)
            )
            .is_none(),
        "unknown keys cannot publish"
    );
}

#[test]
fn invalidate_source_drops_only_that_source_snapshots() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(Arc::clone(&clock) as _);
    let (source_a, handle_a) = ScriptedSource::new(
        "a",
        vec![vec![entry(0, MentionKind::File, "file:///ws/a", "a")]],
    );
    let (source_b, handle_b) = ScriptedSource::new(
        "b",
        vec![vec![entry(0, MentionKind::File, "file:///ws/b", "b")]],
    );
    engine.register_source(Box::new(source_a), false);
    engine.register_source(Box::new(source_b), false);
    let root = Path::new("/ws");

    for key in ["a", "b"] {
        assert!(matches!(
            engine.resolve_snapshot(root, key, &options()),
            SnapshotResolution::Built(_)
        ));
    }

    engine.invalidate_source("a");
    assert!(
        engine.cached_snapshot(root, "a", &options()).is_none(),
        "invalidated source has no cached snapshot"
    );
    assert!(
        engine.cached_snapshot(root, "b", &options()).is_some(),
        "other sources keep their snapshots"
    );

    let SnapshotResolution::Built(rebuilt) = engine.resolve_snapshot(root, "a", &options()) else {
        panic!("resolve after invalidate rebuilds");
    };
    assert_eq!(rebuilt.generation, 2, "generations survive invalidation");
    assert_eq!(handle_a.builds.load(Ordering::Relaxed), 2);
    assert_eq!(handle_b.builds.load(Ordering::Relaxed), 1);
}

#[test]
fn register_source_replaces_same_key_and_keeps_generation_monotonic() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(Arc::clone(&clock) as _);
    let (first, _first_handle) = ScriptedSource::new(
        "s",
        vec![vec![entry(0, MentionKind::File, "file:///ws/first", "f")]],
    );
    engine.register_source(Box::new(first), false);
    let root = Path::new("/ws");

    let SnapshotResolution::Built(built) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("first resolve must build");
    };
    assert_eq!(built.generation, 1);

    // Replacement under the same key keeps the generation counter. The old
    // snapshot stays cached until invalidated (or token-changed): source
    // swaps invalidate explicitly, as the facade does.
    let (second, second_handle) = ScriptedSource::new(
        "s",
        vec![vec![entry(0, MentionKind::File, "file:///ws/second", "s")]],
    );
    engine.register_source(Box::new(second), false);
    engine.invalidate_source("s");

    let SnapshotResolution::Built(rebuilt) = engine.resolve_snapshot(root, "s", &options()) else {
        panic!("resolve after replacement must rebuild");
    };
    assert_eq!(
        rebuilt.generation,
        built.generation + 1,
        "replacement keeps the key's generation monotonic"
    );
    assert_eq!(rebuilt.entries[0].uri, "file:///ws/second");
    assert_eq!(second_handle.builds.load(Ordering::Relaxed), 1);
}

#[test]
fn cached_snapshot_misses_when_never_built() {
    let mut engine = MentionEngine::new(Arc::new(SystemClock) as _);
    let (source, _handle) = ScriptedSource::new("s", vec![Vec::new()]);
    engine.register_source(Box::new(source), false);
    assert!(engine
        .cached_snapshot(Path::new("/ws"), "s", &options())
        .is_none());
    assert!(engine
        .cached_snapshot(Path::new("/ws"), "unregistered", &options())
        .is_none());
}

#[test]
fn resolve_snapshot_shares_identity_across_root_spellings() {
    let clock = Arc::new(spur_mentions::ManualClock::new());
    let mut engine = MentionEngine::new(Arc::clone(&clock) as _);
    let (source, handle) = ScriptedSource::new("s", vec![Vec::new()]);
    engine.register_source(Box::new(source), false);

    let tree = TempTree::new("seams-identity");
    let raw = tree.root();
    let SnapshotResolution::Built(_) = engine.resolve_snapshot(raw, "s", &options()) else {
        panic!("first resolve must build");
    };

    // Equivalent spelling of the same directory: one shared cache identity.
    let dotted = raw.join(".");
    let SnapshotResolution::Fresh(_) = engine.resolve_snapshot(&dotted, "s", &options()) else {
        panic!("equivalent spelling must hit the same snapshot");
    };
    assert_eq!(handle.builds.load(Ordering::Relaxed), 1);
}

#[test]
fn resolve_snapshot_unbuilt_root_uses_raw_identity() {
    // A root that cannot be canonicalized (does not exist) still resolves:
    // the identity falls back to the path as given, and the build still
    // receives it verbatim.
    let mut engine = MentionEngine::new(Arc::new(SystemClock) as _);
    let (source, _handle) = ScriptedSource::new("s", vec![Vec::new()]);
    engine.register_source(Box::new(source), false);

    let missing = PathBuf::from("/definitely/not/a/real/workspace-root");
    let SnapshotResolution::Built(built) = engine.resolve_snapshot(&missing, "s", &options())
    else {
        panic!("unresolvable roots still build");
    };
    assert!(built.entries.is_empty());
    assert!(
        engine.cached_snapshot(&missing, "s", &options()).is_some(),
        "raw-identity snapshots stay addressable"
    );
}
