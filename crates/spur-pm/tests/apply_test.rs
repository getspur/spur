//! T-4 integration cases for `apply_remote_delta` (spec §10).
//!
//! Drives the ingest module with a hand-rolled in-memory `MockSync`
//! and a tempdir-backed `BeadsCrateAdapter`. Mirrors PR-3 acceptance
//! criteria: all 11 cases from §10 plus A-9 single-store invariant.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use fs2::FileExt;
use spur_pm::adapter::IssueTracker;
use spur_pm::advanced::BeadsAdvanced;
use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm::ingest::{apply_remote_delta, watermark, IngestOptions};
use spur_pm::sync::{
    ExternalPmSync, FetchOneOutcome, LocalMutation, PushOutcome, RemoteComment, RemoteConflict,
    RemoteDelta, RemoteKind, RemoteNode, RemoteRef, RemoteState, SyncResult, SyncWatermark,
};
use spur_pm::types::{IssueCreate, IssueUpdate};
use tempfile::TempDir;

// ─── Helpers ──────────────────────────────────────────────────────────

fn ts(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().unwrap()
}

struct MockSync {
    repo: String,
    delta: RemoteDelta,
}

impl MockSync {
    fn new(repo: &str, delta: RemoteDelta) -> Self {
        Self {
            repo: repo.to_string(),
            delta,
        }
    }
}

#[async_trait]
impl ExternalPmSync for MockSync {
    fn source_system(&self) -> &'static str {
        "github"
    }
    fn source_repo(&self) -> &str {
        &self.repo
    }
    async fn fetch_changes_since(&self, _since: Option<DateTime<Utc>>) -> SyncResult<RemoteDelta> {
        Ok(self.delta.clone())
    }
    async fn fetch_one(
        &self,
        _remote_id: &str,
        _if_none_match: Option<&str>,
    ) -> SyncResult<FetchOneOutcome> {
        Ok(FetchOneOutcome::Gone)
    }
    async fn push_mutations(&self, _diff: Vec<LocalMutation>) -> SyncResult<Vec<PushOutcome>> {
        Ok(Vec::new())
    }
    async fn detect_conflicts(
        &self,
        _watermarks: &[SyncWatermark],
    ) -> SyncResult<Vec<RemoteConflict>> {
        Ok(Vec::new())
    }
}

fn issue_node(remote_id: &str, number: u64, title: &str, updated_at: i64) -> RemoteNode {
    RemoteNode {
        remote_id: remote_id.to_string(),
        remote_number: Some(number),
        kind: RemoteKind::Issue,
        title: title.to_string(),
        body: format!("body for {title}"),
        state: RemoteState::Open,
        labels: vec![],
        assignees: vec![],
        created_at: ts(updated_at),
        updated_at: ts(updated_at),
        html_url: format!("https://github.com/o/r/issues/{number}"),
        etag: None,
        dep_hints: vec![],
        comments: vec![],
        raw: serde_json::Value::Null,
    }
}

fn test_opts() -> IngestOptions {
    IngestOptions {
        // Short lock timeout for the concurrent-runs case so the
        // suite stays fast (well under any test-runner deadline).
        lock_timeout_ms: Some(200),
        ..IngestOptions::default()
    }
}

async fn open_adapter(dir: &Path) -> BeadsCrateAdapter {
    let beads_dir = dir.join(".beads");
    std::fs::create_dir_all(&beads_dir).unwrap();
    BeadsCrateAdapter::open(&beads_dir, AdapterConfig::default())
        .await
        .unwrap()
}

async fn count_sync_sentinels(beads: &BeadsCrateAdapter, beads_id: &str) -> usize {
    beads
        .list_comments(beads_id)
        .await
        .unwrap()
        .iter()
        .filter(|c| c.body.starts_with("spur-sync v1"))
        .count()
}

async fn count_import_markers(beads: &BeadsCrateAdapter, beads_id: &str) -> usize {
    let comments = beads.list_comments(beads_id).await.unwrap();
    comments
        .iter()
        .filter(|c| {
            c.body
                .lines()
                .next()
                .is_some_and(|l| l.starts_with("<!-- spur-import gh:") && l.contains(" -->"))
        })
        .count()
}

async fn count_audit_disconnected(beads: &BeadsCrateAdapter, beads_id: &str) -> usize {
    beads
        .list_comments(beads_id)
        .await
        .unwrap()
        .iter()
        .filter(|c| c.body.starts_with("spur-audit v1") && c.body.contains("kind: disconnected"))
        .count()
}

/// Latest spur-sync v1 sentinel's `state:` field.
async fn latest_link_state(beads: &BeadsCrateAdapter, beads_id: &str) -> Option<String> {
    let comments = beads.list_comments(beads_id).await.unwrap();
    let crate_comments: Vec<_> = comments
        .into_iter()
        .map(|c| spur_pm::advanced::Comment {
            id: c.id,
            body: c.body,
            actor: c.actor,
            created_at: c.created_at,
        })
        .collect();
    watermark::latest_sync_sentinel(&crate_comments).map(|s| s.state.as_str().to_string())
}

/// A-9 single-store invariant. Returns Err if anything beyond the
/// allowlist appears in `.beads/`.
fn assert_single_store_invariant(beads_dir: &Path) {
    let allowed: HashSet<&str> = [
        "beads.db",
        "beads.db-wal",
        "beads.db-shm",
        "beads.db-journal",
        ".write.lock",
        ".spur-ingest.lock",
        ".spur-poll-cursor",
        ".br_history",
        ".br_history.lock",
        "issues.jsonl",
        ".br.lock",
        ".init.lock",
    ]
    .into_iter()
    .collect();
    for entry in std::fs::read_dir(beads_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        // Tolerate beads_rust internal temp/probe files (their exact
        // names move between versions); we only fail on names that
        // *look* like sidecar databases or per-source caches.
        if name_str.ends_with(".db") || name_str.ends_with(".sqlite") {
            assert!(
                allowed.contains(name_str.as_str()),
                "A-9 violation: unexpected db-like file {name_str:?} in .beads/",
            );
        }
        if name_str.contains("external_links") || name_str.contains("ingest_cache") {
            panic!("A-9 violation: forbidden sidecar file {name_str:?} in .beads/");
        }
    }
}

// ─── Case 1: Fresh repo — N issues land with sentinels ───────────────

#[tokio::test(flavor = "multi_thread")]
async fn case01_fresh_repo_creates_issues_with_sentinels() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;
    let delta = RemoteDelta {
        nodes: vec![
            issue_node("I_node_1", 1, "First", 100),
            issue_node("I_node_2", 2, "Second", 110),
            issue_node("I_node_3", 3, "Third", 120),
        ],
        deletions: vec![],
        watermark: ts(120),
    };
    let mock = MockSync::new("o/r", delta.clone());

    let report = apply_remote_delta(&beads, &mock, delta, &test_opts())
        .await
        .unwrap();
    assert_eq!(report.fetched_remote_nodes, 3);
    assert!(!report.dry_run);
    assert_eq!(report.ingested, 3);
    assert_eq!(report.updated, 0);
    assert_eq!(report.unchanged, 0);
    assert!(report.conflicts.is_empty());

    // Each issue should now have exactly one spur-sync v1 sentinel.
    for node_id in ["I_node_1", "I_node_2", "I_node_3"] {
        let issue = beads
            .find_by_external_ref(&format!("github:{node_id}"))
            .await
            .unwrap()
            .expect("issue created");
        assert_eq!(count_sync_sentinels(&beads, &issue.id).await, 1);
        assert_eq!(
            latest_link_state(&beads, &issue.id).await.as_deref(),
            Some("active")
        );
    }

    assert_single_store_invariant(&tmp.path().join(".beads"));
}

// ─── Case 2: Re-ingest unchanged — cheap-path short-circuit ──────────

#[tokio::test(flavor = "multi_thread")]
async fn case02_reingest_unchanged_short_circuits() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;
    let delta = RemoteDelta {
        nodes: vec![issue_node("I_node_2", 2, "Stable", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock = MockSync::new("o/r", delta.clone());

    let _ = apply_remote_delta(&beads, &mock, delta.clone(), &test_opts())
        .await
        .unwrap();

    let again = apply_remote_delta(&beads, &mock, delta, &test_opts())
        .await
        .unwrap();
    assert_eq!(again.ingested, 0);
    assert_eq!(again.updated, 0);
    assert_eq!(again.unchanged, 1);

    let issue = beads
        .find_by_external_ref("github:I_node_2")
        .await
        .unwrap()
        .unwrap();
    // Cheap path: no fresh sentinel.
    assert_eq!(count_sync_sentinels(&beads, &issue.id).await, 1);
}

// ─── Case 3: Mutated remote — update + new sentinel appended ─────────

#[tokio::test(flavor = "multi_thread")]
async fn case03_mutated_remote_applies_and_appends_sentinel() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;

    let first = RemoteDelta {
        nodes: vec![issue_node("I_node_3", 3, "Original", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock1 = MockSync::new("o/r", first.clone());
    let _ = apply_remote_delta(&beads, &mock1, first, &test_opts())
        .await
        .unwrap();

    // Second ingest: title changed, updated_at moved forward.
    let mut node2 = issue_node("I_node_3", 3, "Updated title", 200);
    node2.labels = vec!["bug".to_string()];
    let second = RemoteDelta {
        nodes: vec![node2],
        deletions: vec![],
        watermark: ts(200),
    };
    let mock2 = MockSync::new("o/r", second.clone());
    let report = apply_remote_delta(&beads, &mock2, second, &test_opts())
        .await
        .unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(report.ingested, 0);

    let issue_brief = beads
        .find_by_external_ref("github:I_node_3")
        .await
        .unwrap()
        .unwrap();
    let issue = beads.get_issue(&issue_brief.id).await.unwrap();
    assert_eq!(issue.title, "Updated title");
    assert!(
        issue.labels.iter().any(|l| l == "gh:bug"),
        "labels: {:?}",
        issue.labels
    );
    // Two sentinels: initial + after-update.
    assert_eq!(count_sync_sentinels(&beads, &issue.id).await, 2);
}

// ─── Case 4: Deletions — Gone surfaces as disconnected sentinel ──────

#[tokio::test(flavor = "multi_thread")]
async fn case04_deletion_marks_disconnected() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;
    let first = RemoteDelta {
        nodes: vec![issue_node("I_to_delete", 4, "Gone soon", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock1 = MockSync::new("o/r", first.clone());
    let _ = apply_remote_delta(&beads, &mock1, first, &test_opts())
        .await
        .unwrap();

    let second = RemoteDelta {
        nodes: vec![],
        deletions: vec![RemoteRef {
            source_system: "github".into(),
            remote_id: "I_to_delete".into(),
        }],
        watermark: ts(200),
    };
    let mock2 = MockSync::new("o/r", second.clone());
    let report = apply_remote_delta(&beads, &mock2, second, &test_opts())
        .await
        .unwrap();
    assert_eq!(report.deletions.len(), 1);

    let issue = beads
        .find_by_external_ref("github:I_to_delete")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        latest_link_state(&beads, &issue.id).await.as_deref(),
        Some("disconnected")
    );
    assert_eq!(count_audit_disconnected(&beads, &issue.id).await, 1);
}

// ─── Case 5: Disjoint conflict regression (§5.4) ─────────────────────
// Local edits priority, remote adds a comment (bumps updated_at but
// not mapped fields). Field-level detector must NOT flag conflict;
// new remote comment must land.

#[tokio::test(flavor = "multi_thread")]
async fn case05_disjoint_mutations_are_not_a_conflict() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;

    // Seed: first ingest at t=100.
    let first = RemoteDelta {
        nodes: vec![issue_node("I_disjoint", 5, "T", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock1 = MockSync::new("o/r", first.clone());
    let _ = apply_remote_delta(&beads, &mock1, first, &test_opts())
        .await
        .unwrap();

    let issue = beads
        .find_by_external_ref("github:I_disjoint")
        .await
        .unwrap()
        .unwrap();

    // Wait past the sentinel-bump tolerance window so the subsequent
    // local edit registers as a real user mutation rather than an
    // ingest self-bump artifact.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Local-only field change: bump priority. Pushes
    // local.updated_at past wm.last_synced_at + tolerance.
    beads
        .update_issue(
            &issue.id,
            IssueUpdate {
                priority: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Remote-only change: new comment (no mapped-field change).
    let mut node = issue_node("I_disjoint", 5, "T", 200);
    node.comments = vec![RemoteComment {
        remote_id: "IC_xyz".into(),
        author: "alice".into(),
        body: "Looks good".into(),
        created_at: ts(190),
        updated_at: ts(190),
    }];
    let second = RemoteDelta {
        nodes: vec![node],
        deletions: vec![],
        watermark: ts(200),
    };
    let mock2 = MockSync::new("o/r", second.clone());
    let report = apply_remote_delta(&beads, &mock2, second, &test_opts())
        .await
        .unwrap();
    assert!(
        report.conflicts.is_empty(),
        "disjoint local+remote edits must NOT conflict",
    );
    // Comment imported via marker scan.
    assert_eq!(count_import_markers(&beads, &issue.id).await, 1);
    assert!(report.comments_added >= 1);
}

// ─── Case 6: Same-field conflict ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn case06_same_field_conflict_returns_remote_conflict() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;
    let first = RemoteDelta {
        nodes: vec![issue_node("I_both", 6, "Original", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock1 = MockSync::new("o/r", first.clone());
    let _ = apply_remote_delta(&beads, &mock1, first, &test_opts())
        .await
        .unwrap();
    let issue = beads
        .find_by_external_ref("github:I_both")
        .await
        .unwrap()
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Local title edit.
    beads
        .update_issue(
            &issue.id,
            IssueUpdate {
                add_labels: vec!["touched".into()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Force title change locally via direct write.
    let issue_for_write = issue.id.clone();
    beads
        .write(move |s| {
            let u = beads_rust::storage::sqlite::IssueUpdate {
                title: Some("Local renamed".into()),
                ..Default::default()
            };
            s.update_issue(&issue_for_write, &u, "test")?;
            Ok(())
        })
        .await
        .unwrap();

    // Remote title edit.
    let mut node = issue_node("I_both", 6, "Remote renamed", 200);
    node.body = "body for Original".into();
    let second = RemoteDelta {
        nodes: vec![node],
        deletions: vec![],
        watermark: ts(200),
    };
    let mock2 = MockSync::new("o/r", second.clone());
    let report = apply_remote_delta(&beads, &mock2, second, &test_opts())
        .await
        .unwrap();
    assert_eq!(report.conflicts.len(), 1, "expected same-field conflict");
    assert_eq!(report.updated, 0);
    // Issue untouched.
    let after = beads.get_issue(&issue.id).await.unwrap();
    assert_eq!(after.title, "Local renamed");
}

// ─── Case 7: Idempotency — partial batch re-run produces no dups ────

#[tokio::test(flavor = "multi_thread")]
async fn case07_idempotency_under_partial_rerun() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;
    let nodes: Vec<RemoteNode> = (1..=6)
        .map(|i| {
            issue_node(
                &format!("I_idem_{i}"),
                i,
                &format!("Node {i}"),
                100 + i as i64,
            )
        })
        .collect();
    let delta = RemoteDelta {
        nodes: nodes.clone(),
        deletions: vec![],
        watermark: ts(200),
    };

    // First run lands a subset (simulate partial by halving the
    // delta — equivalent to a crash after node 3 because the rest
    // never got processed).
    let half = RemoteDelta {
        nodes: nodes[..3].to_vec(),
        deletions: vec![],
        watermark: ts(150),
    };
    let mock1 = MockSync::new("o/r", half.clone());
    let r1 = apply_remote_delta(&beads, &mock1, half, &test_opts())
        .await
        .unwrap();
    assert_eq!(r1.ingested, 3);

    // Second run with the FULL delta. external_ref UNIQUE means the
    // first 3 short-circuit; the remaining 3 ingest cleanly.
    let mock2 = MockSync::new("o/r", delta.clone());
    let r2 = apply_remote_delta(&beads, &mock2, delta, &test_opts())
        .await
        .unwrap();
    assert_eq!(r2.ingested, 3, "remaining N/2 must land");
    assert_eq!(r2.unchanged, 3, "first N/2 must be a no-op");

    // Idempotency: no duplicate issues.
    for i in 1..=6u64 {
        let id = format!("github:I_idem_{i}");
        let issue = beads.find_by_external_ref(&id).await.unwrap();
        assert!(issue.is_some(), "missing issue for {id}");
        assert_eq!(
            count_sync_sentinels(&beads, &issue.unwrap().id).await,
            1,
            "exactly one sentinel per issue",
        );
    }
}

// ─── Case 8: Concurrent ingest runs — second fails fast ─────────────

#[tokio::test(flavor = "multi_thread")]
async fn case08_concurrent_ingest_fails_fast_with_pid() {
    let tmp = TempDir::new().unwrap();
    let beads_dir = tmp.path().join(".beads");
    std::fs::create_dir_all(&beads_dir).unwrap();
    let beads = BeadsCrateAdapter::open(&beads_dir, AdapterConfig::default())
        .await
        .unwrap();

    // Take the ingest lock manually (simulates a concurrent run that
    // holds the flock for the whole duration of the inner ingest).
    let lock_path = beads_dir.join(".spur-ingest.lock");
    let outer = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    outer.try_lock_exclusive().unwrap();
    // Write a fake PID payload so the error message can quote it.
    {
        use std::io::Write as _;
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&lock_path)
            .unwrap();
        write!(writer, "99999").unwrap();
    }

    let delta = RemoteDelta {
        nodes: vec![issue_node("I_conc_1", 1, "X", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock = MockSync::new("o/r", delta.clone());

    // Short lock timeout (200ms) — test should return quickly.
    let err = apply_remote_delta(&beads, &mock, delta.clone(), &test_opts())
        .await
        .expect_err("contended ingest must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("another ingest run is in progress"),
        "wrong error: {msg}",
    );
    assert!(msg.contains("pid=99999"), "missing pid in: {msg}");

    // Release and retry — second invocation lands cleanly.
    drop(outer);
    // The lock file is left in place; the new run reuses the inode.
    let mock2 = MockSync::new("o/r", delta.clone());
    let r2 = apply_remote_delta(&beads, &mock2, delta, &test_opts())
        .await
        .unwrap();
    assert_eq!(r2.ingested, 1);
}

// ─── Case 9: Comment dedup — same RemoteComment twice = no-op ───────

#[tokio::test(flavor = "multi_thread")]
async fn case09_comment_dedup_via_marker_scan() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;

    let mut node = issue_node("I_cmt", 9, "C", 100);
    node.comments = vec![RemoteComment {
        remote_id: "IC_only".into(),
        author: "alice".into(),
        body: "Hello".into(),
        created_at: ts(95),
        updated_at: ts(95),
    }];
    let delta = RemoteDelta {
        nodes: vec![node.clone()],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock = MockSync::new("o/r", delta.clone());
    let _ = apply_remote_delta(&beads, &mock, delta.clone(), &test_opts())
        .await
        .unwrap();

    // Second run — same delta. Marker scan should skip the import.
    let _ = apply_remote_delta(&beads, &mock, delta, &test_opts())
        .await
        .unwrap();

    let issue = beads
        .find_by_external_ref("github:I_cmt")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count_import_markers(&beads, &issue.id).await, 1);
}

// ─── Case 10: Manual marker removal — "duplicate, never corrupt" ────
//
// The marker is fragile by design (§4.2). If a human strips the
// `<!-- spur-import gh:<id> -->` first line, the dedup scan misses
// the imported comment and the next ingest imports a duplicate.
// Beads must stay internally consistent. This pins the failure
// mode as "duplicate, never corrupt."

#[tokio::test(flavor = "multi_thread")]
async fn case10_manual_marker_removal_duplicates_but_never_corrupts() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;

    // Pre-stage the post-human-edit state: create the issue +
    // seed a comment whose first line lacks the marker (simulates
    // a human having stripped it after the ingest landed).
    let beads_id = beads
        .create_issue(IssueCreate {
            title: "S".into(),
            external_ref: Some("github:I_strip".into()),
            source_system: Some("github".into()),
            source_repo: Some("o/r".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    // The human-edited body — no marker line.
    beads
        .add_comment(
            &beads_id,
            "imported from https://github.com/o/r/issues/10#issuecomment-1\n\nBody line",
        )
        .await
        .unwrap();
    let before_count = beads.list_comments(&beads_id).await.unwrap().len();

    // Now re-ingest. The remote presents the same comment again;
    // the marker scan can't see the stripped one, so a duplicate
    // (marker-bearing) imported comment lands.
    let mut node = issue_node("I_strip", 10, "S", 100);
    node.comments = vec![RemoteComment {
        remote_id: "IC_strip_1".into(),
        author: "alice".into(),
        body: "Body line".into(),
        created_at: ts(95),
        updated_at: ts(95),
    }];
    let delta = RemoteDelta {
        nodes: vec![node],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock = MockSync::new("o/r", delta.clone());
    let r = apply_remote_delta(&beads, &mock, delta, &test_opts())
        .await
        .unwrap();
    assert!(r.conflicts.is_empty());

    // The "duplicate, never corrupt" contract: Beads issue stays
    // intact AND the marker-bearing import has appeared (so the
    // count strictly increased; both copies coexist).
    let after_count = beads.list_comments(&beads_id).await.unwrap().len();
    assert!(after_count > before_count, "duplicate import should land");
    let after = beads.get_issue(&beads_id).await.unwrap();
    assert_eq!(after.title, "S", "Beads issue must remain intact");
    assert_eq!(count_import_markers(&beads, &beads_id).await, 1);
}

// ─── Case 11: Recovery — pre-existing external_ref without sentinel ─

#[tokio::test(flavor = "multi_thread")]
async fn case11_recovery_branch_adopts_existing_local_issue() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;

    // Pre-create a Beads issue with the same external_ref the
    // upcoming RemoteDelta will reference, but no spur-sync v1
    // sentinel attached.
    let existing_id = beads
        .create_issue(IssueCreate {
            title: "Pre-existing".into(),
            external_ref: Some("github:I_recover_1".into()),
            source_system: Some("github".into()),
            source_repo: Some("o/r".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let delta = RemoteDelta {
        nodes: vec![issue_node("I_recover_1", 11, "Pre-existing", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock = MockSync::new("o/r", delta.clone());
    let report = apply_remote_delta(&beads, &mock, delta, &test_opts())
        .await
        .unwrap();
    // No conflict: we adopt the local issue and emit the first sentinel.
    assert!(report.conflicts.is_empty(), "recovery must not conflict");
    // Local issue still exists; same beads_id.
    let still = beads
        .find_by_external_ref("github:I_recover_1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.id, existing_id);
    assert_eq!(count_sync_sentinels(&beads, &existing_id).await, 1);
}

// ─── Single-store invariant (A-9) ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn case12_single_store_invariant_after_run() {
    let tmp = TempDir::new().unwrap();
    let beads = open_adapter(tmp.path()).await;
    let delta = RemoteDelta {
        nodes: vec![issue_node("I_inv_1", 1, "A", 100)],
        deletions: vec![],
        watermark: ts(100),
    };
    let mock = MockSync::new("o/r", delta.clone());
    let _ = apply_remote_delta(&beads, &mock, delta, &test_opts())
        .await
        .unwrap();
    assert_single_store_invariant(&tmp.path().join(".beads"));
}
