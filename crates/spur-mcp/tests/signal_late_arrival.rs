//! T-I3: signal arriving on terminal-status task is recorded as late-arrival,
//! not passed to proposer.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels;
use spur_mcp::plan::signals::{self, WorkerSignal};
use spur_mcp::server::handle_report_signal;
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;
use uuid::Uuid;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sqlite_available() -> bool {
    Command::new("sqlite3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
}

fn run_sqlite(repo: &Path, sql: &str) -> Result<(), String> {
    let db = repo.join(".beads").join("beads.db");
    let out = Command::new("sqlite3")
        .arg(&db)
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
            "sqlite3 {:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            db, out.status
        ))
    }
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

async fn create_task(pm: &PmService, title: &str) -> String {
    pm.create_issue(IssueCreate {
        title: title.to_string(),
        issue_type: Some("task".into()),
        ..Default::default()
    })
    .await
    .expect("create_issue must succeed")
}

fn comment_texts(repo: &Path, issue_id: &str) -> Vec<String> {
    let raw = run_br(repo, &["comments", "list", issue_id]).expect("br comments list failed");
    let items: serde_json::Value =
        serde_json::from_str(&raw).expect("br comments list output must be valid JSON");
    items
        .as_array()
        .expect("comments list must be a JSON array")
        .iter()
        .filter_map(|item| item.get("text").and_then(|text| text.as_str()))
        .map(String::from)
        .collect()
}

fn issue_labels(repo: &Path, issue_id: &str) -> Vec<String> {
    let raw = run_br(repo, &["show", issue_id]).expect("br show failed");
    let items: serde_json::Value =
        serde_json::from_str(&raw).expect("br show output must be valid JSON");
    items[0]["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| label.as_str().map(String::from))
        .collect()
}

fn transition_issue(repo: &Path, issue_id: &str, status: &str) -> String {
    match run_br(repo, &["update", issue_id, "--status", status]) {
        Ok(_) => format!("br update --status {status}"),
        Err(_) => {
            run_sqlite(
                repo,
                &format!(
                    "update issues set status = '{status}', updated_at = CURRENT_TIMESTAMP where id = '{issue_id}';"
                ),
            )
            .expect("sqlite3 status update must succeed");
            format!("sqlite status override to {status}")
        }
    }
}

fn scope_drift_signal(signal_id: Uuid) -> WorkerSignal {
    WorkerSignal::ScopeDrift {
        signal_id,
        severity: 0.82,
        reason: "auth refactor pulls in 4 new subsystems".into(),
        estimated_subtasks: Some(3),
    }
}

async fn assert_late_arrival_for_status(status: &str) {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    if !sqlite_available() {
        eprintln!("skipping: `sqlite3` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = create_task(&pm, &format!("Late signal task ({status})")).await;
    let transition_mode = transition_issue(dir.path(), &task_id, status);
    assert_eq!(
        pm.get_issue(&task_id)
            .await
            .expect("get_issue should succeed")
            .status,
        status,
        "task status should be {status} after {transition_mode}"
    );

    let signal = scope_drift_signal(Uuid::new_v4());
    let signal_id = signal.signal_id().to_string();

    let result = handle_report_signal(
        Arc::clone(&pm),
        json!({
            "task_id": task_id.clone(),
            "signal": signal.clone(),
        }),
    )
    .await
    .expect("handle_report_signal must succeed");

    assert_eq!(
        result,
        json!({
            "recorded": true,
            "signal_id": signal_id.clone(),
            "late": true,
        })
    );

    let comments = comment_texts(dir.path(), &task_id);
    let signal_comments: Vec<&String> = comments
        .iter()
        .filter(|text| text.trim_start().starts_with(signals::SENTINEL_PREFIX))
        .collect();
    assert!(
        signal_comments.is_empty(),
        "late-arrival path must not emit worker signal sentinels for {status}; got: {signal_comments:?}"
    );

    let audit_comments: Vec<&String> = comments
        .iter()
        .filter(|text| {
            text.trim_start()
                .starts_with(audit_sentinel::SENTINEL_PREFIX)
        })
        .collect();
    assert_eq!(
        audit_comments.len(),
        1,
        "late-arrival path should record exactly one audit sentinel for {status}"
    );

    let parsed_audits: Vec<AuditSentinelKind> = audit_comments
        .iter()
        .map(|text| {
            audit_sentinel::parse_comment(text)
                .expect("audit sentinel prefix must parse")
                .expect("audit sentinel JSON must be valid")
        })
        .collect();
    assert_eq!(
        parsed_audits,
        vec![AuditSentinelKind::LateSignal {
            signal_id: signal_id.clone(),
            terminal_status: status.to_string(),
        }]
    );

    let parsed_signals: Vec<WorkerSignal> = comments
        .iter()
        .filter_map(|text| signals::parse_comment(text).and_then(|parsed| parsed.ok()))
        .collect();
    assert!(
        parsed_signals.is_empty(),
        "late-arrival path must not parse any worker signal sentinels for {status}; got: {parsed_signals:?}"
    );

    let labels = issue_labels(dir.path(), &task_id);
    assert!(
        labels.contains(&labels::SIGNAL_LATE_ARRIVAL.to_string()),
        "late-arrival path should carry the late-arrival label for {status}; got labels: {labels:?}"
    );
    assert!(
        !labels.contains(&labels::signal_kind(signal.kind_label())),
        "late-arrival path must not carry the signal kind label for {status}; got labels: {labels:?}"
    );
}

macro_rules! late_arrival_case {
    ($name:ident, $status:literal) => {
        #[tokio::test]
        async fn $name() {
            assert_late_arrival_for_status($status).await;
        }
    };
}

late_arrival_case!(
    report_signal_on_approved_task_records_late_arrival,
    "approved"
);
late_arrival_case!(report_signal_on_failed_task_records_late_arrival, "failed");
late_arrival_case!(
    report_signal_on_cancelled_task_records_late_arrival,
    "cancelled"
);
late_arrival_case!(
    report_signal_on_superseded_task_records_late_arrival,
    "superseded"
);
