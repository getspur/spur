//! T-F5: happy path. T-I3: late-arrival gate.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::handlers::{report_signal, WorkerCallContext};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels;
use spur_mcp::plan::signals::{self, WorkerSignal};
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;
use uuid::Uuid;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
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

fn scope_drift_signal(signal_id: Uuid) -> WorkerSignal {
    WorkerSignal::ScopeDrift {
        signal_id,
        severity: 0.82,
        reason: "auth refactor pulls in 4 new subsystems".into(),
        estimated_subtasks: Some(3),
    }
}

#[tokio::test]
async fn report_signal_on_open_task_records_all_artifacts() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = create_task(&pm, "Open signal task").await;
    let signal = scope_drift_signal(Uuid::new_v4());
    let signal_id = signal.signal_id().to_string();

    let result = report_signal(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        &WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: "test-session".into(),
        },
        json!({
            "task_id": task_id.clone(),
            "signal": signal.clone(),
        }),
    )
    .await
    .expect("report_signal must succeed");

    assert_eq!(
        result,
        json!({
            "recorded": true,
            "signal_id": signal_id.clone(),
            "late": false,
        })
    );

    let comments = comment_texts(dir.path(), &task_id);
    let parsed_signals: Vec<WorkerSignal> = comments
        .iter()
        .filter_map(|text| signals::parse_comment(text).and_then(|parsed| parsed.ok()))
        .collect();
    assert_eq!(
        parsed_signals,
        vec![signal.clone()],
        "open task should record exactly one worker signal sentinel"
    );

    let parsed_audits: Vec<AuditSentinelKind> = comments
        .iter()
        .filter_map(|text| audit_sentinel::parse_comment(text).and_then(|parsed| parsed.ok()))
        .collect();
    assert_eq!(
        parsed_audits.len(),
        1,
        "open task should record exactly one audit sentinel"
    );
    assert!(parsed_audits.iter().any(|kind| {
        matches!(
            kind,
            AuditSentinelKind::Signal {
                signal_id: found_id,
                delegation_id,
                kind,
                severity,
                reason,
            } if found_id == &signal_id
                && delegation_id.is_empty()
                && kind == "scope-drift"
                && (*severity - 0.82).abs() < 1e-6
                && reason == "auth refactor pulls in 4 new subsystems"
        )
    }));

    let labels = issue_labels(dir.path(), &task_id);
    assert!(
        labels.contains(&labels::signal_kind(signal.kind_label())),
        "open task should carry the signal kind label; got labels: {labels:?}"
    );
    assert!(
        !labels.contains(&labels::SIGNAL_LATE_ARRIVAL.to_string()),
        "open task must not carry the late-arrival label; got labels: {labels:?}"
    );
}

#[tokio::test]
async fn report_signal_on_terminal_task_records_late_arrival() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = create_task(&pm, "Terminal signal task").await;
    // Beads vocabulary reality: all SPUR terminals project to `closed`.
    // Close via `br close` (production flow) instead of injecting a SPUR-vocab
    // string via sqlite — the handler now gates on `status == closed_status()`.
    run_br(dir.path(), &["close", &task_id]).expect("br close failed");
    assert_eq!(
        pm.get_issue(&task_id)
            .await
            .expect("get_issue should succeed")
            .status,
        pm.closed_status()
    );

    let signal = scope_drift_signal(Uuid::new_v4());
    let signal_id = signal.signal_id().to_string();

    let result = report_signal(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        &WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: "test-session".into(),
        },
        json!({
            "task_id": task_id.clone(),
            "signal": signal.clone(),
        }),
    )
    .await
    .expect("report_signal must succeed");

    assert_eq!(
        result,
        json!({
            "recorded": true,
            "signal_id": signal_id.clone(),
            "late": true,
        })
    );

    let comments = comment_texts(dir.path(), &task_id);
    let parsed_signals: Vec<WorkerSignal> = comments
        .iter()
        .filter_map(|text| signals::parse_comment(text).and_then(|parsed| parsed.ok()))
        .collect();
    assert!(
        parsed_signals.is_empty(),
        "late-arrival path must not emit a worker signal sentinel; got: {parsed_signals:?}"
    );

    let parsed_audits: Vec<AuditSentinelKind> = comments
        .iter()
        .filter_map(|text| audit_sentinel::parse_comment(text).and_then(|parsed| parsed.ok()))
        .collect();
    assert_eq!(
        parsed_audits.len(),
        1,
        "late-arrival path should record exactly one audit sentinel"
    );
    let expected_terminal = pm.closed_status().to_string();
    assert!(parsed_audits.iter().any(|kind| {
        matches!(
            kind,
            AuditSentinelKind::LateSignal {
                signal_id: found_id,
                terminal_status,
            } if found_id == &signal_id && terminal_status == &expected_terminal
        )
    }));

    let labels = issue_labels(dir.path(), &task_id);
    assert!(
        labels.contains(&labels::SIGNAL_LATE_ARRIVAL.to_string()),
        "late-arrival path should carry the late-arrival label; got labels: {labels:?}"
    );
    assert!(
        !labels.contains(&labels::signal_kind(signal.kind_label())),
        "late-arrival path must not carry the signal kind label; got labels: {labels:?}"
    );
}

#[tokio::test]
async fn report_signal_threads_worker_call_context() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = create_task(&pm, "Context thread task").await;
    let signal = scope_drift_signal(Uuid::new_v4());
    let signal_id = signal.signal_id().to_string();
    let expected_delegation_id = "del-test-42";

    let result = report_signal(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        &WorkerCallContext {
            delegation_id: expected_delegation_id.into(),
            brain_session_id: "test-session".into(),
        },
        json!({
            "task_id": task_id.clone(),
            "signal": signal.clone(),
        }),
    )
    .await
    .expect("report_signal must succeed");

    assert_eq!(
        result,
        json!({
            "recorded": true,
            "signal_id": signal_id.clone(),
            "late": false,
        })
    );

    let comments = comment_texts(dir.path(), &task_id);
    let parsed_audits: Vec<AuditSentinelKind> = comments
        .iter()
        .filter_map(|text| audit_sentinel::parse_comment(text).and_then(|parsed| parsed.ok()))
        .collect();
    assert_eq!(
        parsed_audits.len(),
        1,
        "task should record exactly one audit sentinel"
    );
    assert!(parsed_audits.iter().any(|kind| {
        matches!(
            kind,
            AuditSentinelKind::Signal {
                signal_id: found_id,
                delegation_id,
                kind,
                severity,
                reason,
            } if found_id == &signal_id
                && delegation_id == expected_delegation_id
                && kind == "scope-drift"
                && (*severity - 0.82).abs() < 1e-6
                && reason == "auth refactor pulls in 4 new subsystems"
        )
    }));
}
