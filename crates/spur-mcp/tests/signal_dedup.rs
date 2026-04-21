//! T-F7: duplicate worker signals with the same `signal_id` are applied once.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels;
use spur_mcp::plan::proposers::{ScopeDriftSplitProposer, TrivialScorer};
use spur_mcp::plan::signal_watcher::SignalWatcher;
use spur_mcp::plan::signals::{self, WorkerSignal};
use spur_pm::PmService;
use tempfile::TempDir;
use uuid::Uuid;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn sqlite_available() -> bool {
    Command::new("sqlite3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        ))
    }
}

fn run_sqlite(repo: &Path, sql: &str) -> Result<(), String> {
    let db = repo.join(".beads").join("beads.db");
    let output = Command::new("sqlite3")
        .arg(&db)
        .arg(sql)
        .current_dir(repo)
        .output()
        .expect("sqlite3 invocation failed");
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "sqlite3 {:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            db, output.status
        ))
    }
}

fn br_id(raw: &str) -> String {
    raw.trim().trim_matches('"').to_string()
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(
            None,  // no github_repo
            true,  // beads_enabled
            false, // github_enabled
            repo, None, // closed_status default
        )
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init"),
    )
}

fn scope_drift_signal(signal_id: Uuid) -> WorkerSignal {
    WorkerSignal::ScopeDrift {
        signal_id,
        severity: 0.82,
        reason: "auth refactor pulls in 4 new subsystems".into(),
        estimated_subtasks: Some(3),
    }
}

fn audit_sentinels(comments: &[spur_pm::Comment]) -> Vec<AuditSentinelKind> {
    comments
        .iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect()
}

#[tokio::test]
async fn duplicate_signal_comments_with_same_signal_id_commit_once() {
    if !br_available() {
        eprintln!(
            "skipping duplicate_signal_comments_with_same_signal_id_commit_once: `br` not on PATH"
        );
        return;
    }
    if !sqlite_available() {
        eprintln!(
            "skipping duplicate_signal_comments_with_same_signal_id_commit_once: `sqlite3` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let task_id = br_id(
        &run_br(
            dir.path(),
            &["create", "Signal watcher task", "--silent", "-t", "task"],
        )
        .expect("br create failed"),
    );
    run_sqlite(
        dir.path(),
        &format!(
            "update issues set status = 'awaiting_review', updated_at = CURRENT_TIMESTAMP where id = '{task_id}';"
        ),
    )
    .expect("sqlite3 status update must succeed");

    let pm = beads_pm(dir.path()).await;
    pm.update_issue(
        &task_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::signal_kind("scope-drift")],
            ..Default::default()
        },
    )
    .await
    .expect("signal label update must succeed");

    let signal = scope_drift_signal(Uuid::new_v4());
    let signal_comment = signals::encode_comment(&signal);
    let advanced = pm.advanced().expect("advanced beads surface");
    advanced
        .add_comment(&task_id, &signal_comment)
        .await
        .expect("first signal comment");
    advanced
        .add_comment(&task_id, &signal_comment)
        .await
        .expect("second signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ScopeDriftSplitProposer::default(),
        TrivialScorer,
    );
    watcher
        .tick_once()
        .await
        .expect("signal watcher tick must succeed");

    let comments = advanced
        .list_comments(&task_id)
        .await
        .expect("list comments after signal watcher tick");
    let audits = audit_sentinels(&comments);
    let mutation_commit_count = audits
        .iter()
        .filter(|sentinel| matches!(sentinel, AuditSentinelKind::MutationCommit { .. }))
        .count();

    assert_eq!(
        mutation_commit_count, 1,
        "duplicate signal_id must produce exactly one MutationCommit sentinel; audits={audits:?}"
    );
}
