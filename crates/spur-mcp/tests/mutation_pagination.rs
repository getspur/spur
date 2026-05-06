//! Performance-regression fixture for `PmService::list_issues`.
//!
//! `list_issues` once silently truncated at 10,000 rows; any future change that
//! reintroduces a similar cap (default limit, pagination off-by-one, query plan
//! change) must fail this test. The fixture seeds 10,050 issues, places a
//! `boundary_downstream` past the former 10k cap, runs `apply_mutation(SplitTask)`,
//! and asserts the boundary downstream gets rewired off the parent.
//!
//! This is a *correctness regression guard*, not a throughput benchmark — no
//! wall-clock or memory budgets are asserted. Timing-floor assertions were
//! considered and rejected: perf budgets in unit-style integration tests
//! tend to flake more than they catch.

use std::path::Path;
use std::process::Command;

use serde_json::json;
use spur_mcp::plan::mutation::{MutationBatch, PlanMutationOp, TaskDraft};
use uuid::Uuid;

mod common;

const FILLER_COUNT: usize = 10_050;

fn br_available() -> bool {
    common::beads::br_available()
}

fn sqlite_available() -> bool {
    Command::new("sqlite3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
}

fn run_sql(repo: &Path, sql: &str) -> Result<(), String> {
    let db = repo.join(".beads/beads.db");
    let out = Command::new("sqlite3")
        .arg("-cmd")
        .arg(".timeout 2000")
        .arg(db)
        .arg(sql)
        .current_dir(repo)
        .output()
        .expect("sqlite3 invocation failed");
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        Err(format!(
            "sqlite3 failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
}

fn br_id(raw: &str) -> String {
    raw.trim().trim_matches('"').to_string()
}

fn task_draft(title: &str, description: &str) -> TaskDraft {
    serde_json::from_value(json!({
        "title": title,
        "description": description,
        "assignee": null,
        "priority": null
    }))
    .expect("TaskDraft JSON must deserialize")
}

fn mutation_batch(
    mutation_id: Uuid,
    trigger_signal_id: Option<Uuid>,
    trigger_task_id: String,
    ops: Vec<PlanMutationOp>,
) -> MutationBatch {
    serde_json::from_value(json!({
        "mutation_id": mutation_id,
        "trigger_signal_id": trigger_signal_id,
        "trigger_task_id": trigger_task_id,
        "ops": ops
    }))
    .expect("MutationBatch JSON must deserialize")
}

fn seed_filler_issues(repo: &Path, count: usize) -> Result<(), String> {
    run_sql(
        repo,
        &format!(
            "WITH RECURSIVE seq(n) AS (
                 SELECT 1
                 UNION ALL
                 SELECT n + 1 FROM seq WHERE n < {count}
             )
             INSERT INTO issues(
                 id, title, created_at, updated_at, created_by, source_repo
             )
             SELECT
                 printf('seed-%05d', n),
                 printf('Seed %05d', n),
                 datetime('2090-01-01 00:00:00', printf('+%d seconds', n)),
                 datetime('2090-01-01 00:00:00', printf('+%d seconds', n)),
                 'mutation-pagination-test',
                 '.'
             FROM seq;"
        ),
    )
}

fn set_issue_timestamp(repo: &Path, issue_id: &str, timestamp: &str) -> Result<(), String> {
    run_sql(
        repo,
        &format!(
            "UPDATE issues
             SET created_at = '{timestamp}', updated_at = '{timestamp}'
             WHERE id = '{issue_id}';"
        ),
    )
}

mod perf_regressions {
    use super::{
        br_available, br_id, common, mutation_batch, run_br, seed_filler_issues,
        set_issue_timestamp, sqlite_available, task_draft, FILLER_COUNT,
    };
    use spur_mcp::plan::mutation::{DepRewirePolicy, PlanMutationOp};
    use spur_mcp::plan::mutation_executor::apply_mutation;
    use spur_pm::{IssueFilter, PmService};
    use std::sync::Arc;
    use tempfile::TempDir;
    use uuid::Uuid;

    #[tokio::test]
    #[ignore = "heavy: bulk-inserts 10k+ issues to guard the old list_issues cap; run with `cargo test -- --ignored`"]
    async fn mutation_scans_paginate_past_10k_issues() {
        assert!(
            br_available(),
            "this test requires `br` on PATH; run with `cargo test -- --ignored`"
        );
        assert!(
            sqlite_available(),
            "this test requires `sqlite3` on PATH; run with `cargo test -- --ignored`"
        );

        let dir = TempDir::new().expect("tempdir");
        run_br(dir.path(), &["init"]).expect("br init failed");

        let parent = br_id(
            &run_br(dir.path(), &["create", "Parent", "--silent", "-t", "task"])
                .expect("create parent"),
        );
        let boundary_downstream = br_id(
            &run_br(
                dir.path(),
                &["create", "Boundary Downstream", "--silent", "-t", "task"],
            )
            .expect("create boundary downstream"),
        );

        seed_filler_issues(dir.path(), FILLER_COUNT).expect("seed filler issues");

        let head_downstream = br_id(
            &run_br(
                dir.path(),
                &["create", "Head Downstream", "--silent", "-t", "task"],
            )
            .expect("create head downstream"),
        );
        set_issue_timestamp(dir.path(), &head_downstream, "2091-01-01 00:00:00")
            .expect("promote head downstream to newest");

        run_br(dir.path(), &["dep", "add", &boundary_downstream, &parent])
            .expect("seed boundary dep");
        run_br(dir.path(), &["dep", "add", &head_downstream, &parent]).expect("seed head dep");

        let pm = Arc::new(
            PmService::try_new(None, true, false, dir.path(), None)
                .await
                .expect("PmService::try_new failed")
                .expect("expected beads-backed PmService"),
        );

        let first_ten_thousand = pm
            .list_issues(IssueFilter {
                limit: Some(10_000),
                ..Default::default()
            })
            .await
            .expect("list first 10k issues");
        assert!(
            first_ten_thousand
                .iter()
                .any(|issue| issue.id == head_downstream),
            "newest downstream should be inside the first 10k page"
        );
        assert!(
            !first_ten_thousand
                .iter()
                .any(|issue| issue.id == boundary_downstream),
            "boundary downstream must sit beyond the former 10k truncation point"
        );

        let widened_scan = pm
            .list_issues(IssueFilter {
                limit: Some(10_200),
                ..Default::default()
            })
            .await
            .expect("list widened issue window");
        assert!(
            widened_scan
                .iter()
                .any(|issue| issue.id == boundary_downstream),
            "widened scan must include the boundary downstream so the fixture proves the 10k split"
        );

        let batch = mutation_batch(
            Uuid::new_v4(),
            Some(Uuid::new_v4()),
            parent.clone(),
            vec![PlanMutationOp::SplitTask {
                parent: parent.clone(),
                children: vec![
                    task_draft("Child A", "First split child"),
                    task_draft("Child B", "Second split child"),
                ],
                dep_rewire: DepRewirePolicy::Barrier,
            }],
        );

        let child_ids = apply_mutation(
            pm.clone(),
            common::server_builder::pro_feature_gate(),
            &batch,
        )
        .await
        .expect("apply_mutation should succeed");
        assert_eq!(child_ids.len(), 2, "expected two split children");

        for downstream_id in [&head_downstream, &boundary_downstream] {
            let downstream = pm
                .get_issue(downstream_id)
                .await
                .expect("load downstream after mutation");
            assert!(
                !downstream.blocked_by.iter().any(|dep| dep == &parent),
                "downstream {downstream_id} must no longer depend on parent; blocked_by={:?}",
                downstream.blocked_by
            );
            for child_id in &child_ids {
                assert!(
                    downstream.blocked_by.iter().any(|dep| dep == child_id),
                    "downstream {downstream_id} must depend on child {child_id}; blocked_by={:?}",
                    downstream.blocked_by
                );
            }
        }
    }
}
