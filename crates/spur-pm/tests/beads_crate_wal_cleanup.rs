use beads_rust::model::{Issue, IssueType, Priority, Status};
use chrono::Utc;
use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use tempfile::TempDir;

fn make_issue(id: impl Into<String>, title: impl Into<String>) -> Issue {
    let now = Utc::now();
    Issue {
        id: id.into(),
        title: title.into(),
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

#[tokio::test(flavor = "multi_thread")]
async fn write_returns_with_checkpointed_wal_sidecar() {
    let dir = TempDir::new().unwrap();
    let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
        .await
        .expect("adapter opens");

    adapter
        .write(|s| {
            s.create_issue(&make_issue("bd-wal-cleanup", "WAL cleanup"), "test")
                .map_err(anyhow::Error::from)
        })
        .await
        .expect("write succeeds");

    let db_path = dir.path().join("beads.db");
    let wal_path = std::path::PathBuf::from(format!("{}-wal", db_path.to_string_lossy()));
    let wal_len = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);

    assert!(
        wal_len <= 4096,
        "expected write() to drain WAL sidecar to <= 4 KiB, got {wal_len} bytes at {}",
        wal_path.display()
    );
}
