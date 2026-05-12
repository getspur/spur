//! R-4 acceptance check (spec §13): watermark scans must complete
//! in <500ms wall time per 1k issues × ~50 comments each on an
//! M-series Mac in SQLite WAL mode.
//!
//! Runs `#[ignore]` by default; locally invoke with
//! `cargo test -p spur-pm --test watermark_scan_perf --release -- \
//!     --ignored --nocapture`.

use std::time::Instant;

use chrono::Utc;
use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm::ingest::watermark;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn r4_watermark_scan_under_500ms_per_1k_issues() {
    let tmp = TempDir::new().unwrap();
    let beads_dir = tmp.path().join(".beads");
    std::fs::create_dir_all(&beads_dir).unwrap();
    let beads = BeadsCrateAdapter::open(&beads_dir, AdapterConfig::default())
        .await
        .unwrap();

    const ISSUES: usize = 1_000;
    const COMMENTS_PER_ISSUE: usize = 50;

    let setup = Instant::now();
    // Seed directly via storage (avoids generate_id's contention on
    // rapid bd-id allocation and keeps the setup phase honest about
    // measuring read-path cost only).
    let ids: Vec<String> = beads
        .write(move |s| -> anyhow::Result<_> {
            let now = Utc::now();
            let mut ids = Vec::with_capacity(ISSUES);
            for i in 0..ISSUES {
                let id = format!("bd-perf-{i:04}");
                let issue = beads_rust::model::Issue {
                    id: id.clone(),
                    title: format!("Perf {i}"),
                    description: Some(format!("body {i}")),
                    status: beads_rust::model::Status::Open,
                    priority: beads_rust::model::Priority::default(),
                    issue_type: beads_rust::model::IssueType::Task,
                    created_at: now,
                    updated_at: now,
                    assignee: None,
                    owner: None,
                    estimated_minutes: None,
                    due_at: None,
                    defer_until: None,
                    external_ref: Some(format!("github:I_kwDO_{i}")),
                    ephemeral: false,
                    content_hash: None,
                    design: None,
                    acceptance_criteria: None,
                    notes: None,
                    created_by: Some("perf".into()),
                    closed_at: None,
                    close_reason: None,
                    closed_by_session: None,
                    source_system: Some("github".into()),
                    source_repo: Some("o/r".into()),
                    deleted_at: None,
                    deleted_by: None,
                    delete_reason: None,
                    original_type: None,
                    compaction_level: None,
                    compacted_at: None,
                    compacted_at_commit: None,
                    original_size: None,
                    sender: None,
                    pinned: false,
                    is_template: false,
                    labels: Vec::new(),
                    dependencies: Vec::new(),
                    comments: Vec::new(),
                };
                s.create_issue(&issue, "perf")?;
                ids.push(id);
            }
            for (i, id) in ids.iter().enumerate() {
                let sentinel = watermark::format_sync_sentinel(&watermark::SyncSentinel {
                    source_system: "github".into(),
                    remote_id: format!("I_kwDO_{i}"),
                    remote_number: Some(i as u64),
                    remote_etag: None,
                    remote_updated_at: now,
                    last_synced_at: now,
                    last_synced_remote_updated_at: now,
                    state: watermark::LinkState::Active,
                });
                s.add_comment(id, "perf", &sentinel)?;
                for j in 0..(COMMENTS_PER_ISSUE - 1) {
                    s.add_comment(id, "perf", &format!("filler {j}"))?;
                }
            }
            Ok(ids)
        })
        .await
        .unwrap();
    eprintln!("setup took {:?}", setup.elapsed());

    // Measure: scan every issue's comments and pick the latest
    // `spur-sync v1` sentinel — the hot read path inside the
    // cheap-path preview.
    let scan = Instant::now();
    let beads_dir_for_scan = beads_dir.clone();
    let ids_for_scan = ids.clone();
    let scan_ms = tokio::task::spawn_blocking(move || -> u128 {
        let storage = beads_rust::storage::sqlite::SqliteStorage::open_with_timeout(
            &beads_dir_for_scan.join("beads.db"),
            Some(5000),
        )
        .unwrap();
        let inner = Instant::now();
        for id in &ids_for_scan {
            let comments = storage.get_comments(id).unwrap();
            let mapped: Vec<_> = comments
                .into_iter()
                .map(|c| spur_pm::advanced::Comment {
                    id: c.id.to_string(),
                    body: c.body,
                    actor: c.author,
                    created_at: c.created_at,
                })
                .collect();
            let _ = watermark::latest_sync_sentinel(&mapped);
        }
        inner.elapsed().as_millis()
    })
    .await
    .unwrap();
    let elapsed = scan.elapsed();
    eprintln!(
        "R-4: 1000 issues × {COMMENTS_PER_ISSUE} comments — scan {scan_ms}ms (incl. open: {elapsed:?})",
    );
    assert!(
        scan_ms < 500,
        "R-4 gate failed: scan took {scan_ms}ms, must be < 500ms",
    );
}
