//! Durability regression: writes that complete before a hard process exit
//! must survive across re-open. Targets claude-code review concern B1.1
//! (whether fsqlite's TRUNCATE checkpoint actually fsync's main DB pages
//! before the SPUR Drop unlinks the WAL sidecar).
//!
//! The subprocess test simulates a process crash via `SIGKILL` after Drop
//! has run. If unlink-after-checkpoint loses data because the main DB
//! wasn't fsync'd, the parent re-open will see no issue rows.

use beads_rust::model::{Issue, IssueType, Priority, Status};
use chrono::Utc;
use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use std::path::PathBuf;

fn make_issue(id: &str) -> Issue {
    let now = Utc::now();
    Issue {
        id: id.into(),
        title: id.into(),
        description: None,
        status: Status::Open,
        priority: Priority::MEDIUM,
        issue_type: IssueType::Task,
        created_at: now,
        updated_at: now,
        assignee: None,
        owner: None,
        estimated_minutes: None,
        due_at: None,
        defer_until: None,
        external_ref: None,
        ephemeral: false,
        content_hash: None,
        design: None,
        acceptance_criteria: None,
        notes: None,
        created_by: None,
        closed_at: None,
        close_reason: None,
        closed_by_session: None,
        source_system: None,
        source_repo: None,
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
    }
}

/// Subprocess entry point: opens the adapter at `$BEADS_DIR`, writes a
/// fixed set of issues, then aborts with `SIGKILL` immediately after
/// `adapter.write` returns. SIGKILL guarantees no graceful Tokio
/// shutdown — only data that the OS has accepted from us survives.
#[tokio::main(flavor = "current_thread")]
async fn run_writer_subprocess() {
    let beads_dir = PathBuf::from(std::env::var("BEADS_DIR").expect("BEADS_DIR set by parent"));
    let adapter = BeadsCrateAdapter::open(&beads_dir, AdapterConfig::default())
        .await
        .expect("adapter opens");
    for i in 0..5 {
        let id = format!("bd-durab-{i}");
        adapter
            .write(move |s| {
                s.create_issue(&make_issue(&id), "durability-test")
                    .map_err(anyhow::Error::from)
            })
            .await
            .expect("write succeeds");
    }
    // SIGKILL self — no Drop on Tokio runtime, no async unwind.
    // Per-write Drop on SqliteStorage already ran inside spawn_blocking
    // and (per fsqlite-wal sync_db semantics) the main DB pages are on
    // stable storage. Anything the OS hasn't accepted is lost.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
    unreachable!("SIGKILL must terminate before this point");
}

#[test]
fn writes_survive_sigkill_after_drop_checkpoint() {
    if std::env::var("BEADS_DURABILITY_SUBPROC").is_ok() {
        // Subprocess mode.
        run_writer_subprocess();
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let beads_dir = dir.path().to_path_buf();

    let exe = std::env::current_exe().expect("test exe path");
    let status = std::process::Command::new(&exe)
        .args([
            "writes_survive_sigkill_after_drop_checkpoint",
            "--exact",
            "--nocapture",
        ])
        .env("BEADS_DURABILITY_SUBPROC", "1")
        .env("BEADS_DIR", &beads_dir)
        .status()
        .expect("subprocess spawns");
    // SIGKILL → status.code() is None on Unix.
    assert!(
        !status.success(),
        "subprocess was supposed to SIGKILL itself, but exited with {status:?}",
    );

    // Reopen as parent — verify all 5 issues persisted across the kill.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let count = rt.block_on(async {
        let adapter = BeadsCrateAdapter::open(&beads_dir, AdapterConfig::default())
            .await
            .expect("re-open adapter");
        adapter
            .read(|s| {
                s.list_issues(&beads_rust::storage::sqlite::ListFilters::default())
                    .map(|v| v.len())
                    .map_err(anyhow::Error::from)
            })
            .await
            .expect("list_issues read")
    });
    assert_eq!(
        count, 5,
        "expected 5 issues to survive SIGKILL after Drop checkpoint, found {count}"
    );
}
