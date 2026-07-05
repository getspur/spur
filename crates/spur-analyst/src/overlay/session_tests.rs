use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;

use super::*;

fn rebuild_key(head_oid: &str, dirty_byte: u8) -> OverlayRebuildKey {
    let mut dirty = BTreeMap::new();
    dirty.insert(PathBuf::from("src/lib.rs"), [dirty_byte; 20]);
    OverlayRebuildKey::from(head_oid, &dirty)
}

#[tokio::test]
async fn merge_session_reuses_cached_delta_for_same_dirty_key() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let coordinator = OverlaySessionCoordinator::new();
    let worktree = tempdir.path().join("repo");
    let base_db = tempdir.path().join("analyst.duckdb");
    let delta_dir = tempdir.path().join("delta-one");
    let key = rebuild_key("head-a", 1);
    let builds = AtomicUsize::new(0);

    let first = coordinator
        .get_or_build_session(
            worktree.clone(),
            key.clone(),
            base_db.clone(),
            Some("base-hash".to_owned()),
            |_| {
                builds.fetch_add(1, Ordering::SeqCst);
                let delta_dir = delta_dir.clone();
                async move { Ok(delta_dir) }
            },
        )
        .await;
    let second = coordinator
        .get_or_build_session(worktree, key, base_db, Some("base-hash".to_owned()), |_| {
            builds.fetch_add(1, Ordering::SeqCst);
            let delta_dir = delta_dir.clone();
            async move { Ok(delta_dir) }
        })
        .await;

    assert_eq!(
        builds.load(Ordering::SeqCst),
        1,
        "same dirty key should reuse the retained merge session"
    );
    assert!(Arc::ptr_eq(&first, &second));
    assert!(first.delta_applied());
    assert_eq!(first.algo_as_of(), Some("base-hash"));
}

#[tokio::test]
async fn merge_session_rebuilds_after_dirty_key_changes() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let coordinator = OverlaySessionCoordinator::new();
    let worktree = tempdir.path().join("repo");
    let base_db = tempdir.path().join("analyst.duckdb");
    let builds = AtomicUsize::new(0);

    let first = coordinator
        .get_or_build_session(
            worktree.clone(),
            rebuild_key("head-a", 1),
            base_db.clone(),
            Some("base-hash".to_owned()),
            |_| {
                let attempt = builds.fetch_add(1, Ordering::SeqCst);
                let delta_dir = tempdir.path().join(format!("delta-{attempt}"));
                async move { Ok(delta_dir) }
            },
        )
        .await;
    let second = coordinator
        .get_or_build_session(
            worktree,
            rebuild_key("head-a", 2),
            base_db,
            Some("base-hash".to_owned()),
            |_| {
                let attempt = builds.fetch_add(1, Ordering::SeqCst);
                let delta_dir = tempdir.path().join(format!("delta-{attempt}"));
                async move { Ok(delta_dir) }
            },
        )
        .await;

    assert_eq!(
        builds.load(Ordering::SeqCst),
        2,
        "changed dirty key should build a new merge session"
    );
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(second.delta_applied());
}

#[tokio::test]
async fn persistent_delta_failures_escalate_to_clean_rediff_attempt() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let coordinator = OverlaySessionCoordinator::new();
    let worktree = tempdir.path().join("repo");
    let base_db = tempdir.path().join("analyst.duckdb");
    let delta_dir = tempdir.path().join("delta");
    let key = rebuild_key("head-a", 1);
    let modes = Arc::new(Mutex::new(Vec::new()));

    for attempt in 1..=DELTA_FAILURE_ESCALATION_THRESHOLD {
        let modes = Arc::clone(&modes);
        let session = coordinator
            .get_or_build_session(
                worktree.clone(),
                key.clone(),
                base_db.clone(),
                Some("base-hash".to_owned()),
                move |mode| {
                    modes.lock().expect("modes").push(mode);
                    async move { Err(anyhow!("forced delta failure")) }
                },
            )
            .await;
        assert!(
            !session.delta_applied(),
            "attempt {attempt} should degrade to a base-only session"
        );
    }

    let modes_for_success = Arc::clone(&modes);
    let recovered = coordinator
        .get_or_build_session(
            worktree,
            key,
            base_db,
            Some("base-hash".to_owned()),
            move |mode| {
                modes_for_success.lock().expect("modes").push(mode);
                let delta_dir = delta_dir.clone();
                async move { Ok(delta_dir) }
            },
        )
        .await;

    assert!(recovered.delta_applied());
    let modes = modes.lock().expect("modes");
    assert_eq!(
        &modes[..DELTA_FAILURE_ESCALATION_THRESHOLD as usize],
        vec![OverlayBuildMode::IncrementalDelta; DELTA_FAILURE_ESCALATION_THRESHOLD as usize]
            .as_slice()
    );
    assert_eq!(
        modes[DELTA_FAILURE_ESCALATION_THRESHOLD as usize],
        OverlayBuildMode::CleanRediff,
        "the call after the threshold should force a clean re-diff attempt"
    );
}
