use std::sync::Arc;
use std::time::Duration;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_adapters_write_read_loops_complete_under_deadline() {
    let dir = TempDir::new().unwrap();
    let cfg = AdapterConfig {
        lock_timeout_ms: 30_000,
        ..AdapterConfig::default()
    };
    let adapter_a = Arc::new(
        BeadsCrateAdapter::open(dir.path(), cfg.clone())
            .await
            .expect("adapter A opens"),
    );
    let adapter_b = Arc::new(
        BeadsCrateAdapter::open(dir.path(), cfg)
            .await
            .expect("adapter B opens"),
    );

    let run = |adapter: Arc<BeadsCrateAdapter>, prefix: &'static str| async move {
        for i in 0..20 {
            let id = format!("bd-cohabit-{prefix}-{i}");
            let title = format!("{prefix} {i}");
            adapter
                .write(move |s| {
                    s.create_issue(&make_issue(id, title), "test")
                        .map_err(anyhow::Error::from)
                })
                .await?;

            let count = adapter.read(|s| Ok(s.count_issues()?)).await?;
            assert!(count > 0, "read should observe the shared database");
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(run(adapter_a, "a"), run(adapter_b, "b"))
    })
    .await
    .expect("cohabiting adapters should not wedge")
    .expect("cohabiting write/read loops should succeed");
}
