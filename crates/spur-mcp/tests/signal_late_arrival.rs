//! T-I3: signal arriving on terminal-status task is recorded as late-arrival,
//! not passed to proposer.

use std::path::Path;
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
    common::beads::br_available()
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
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

/// T-I3: a signal arriving on a task that beads reports as closed must be
/// recorded as a late-arrival, never passed to the proposer.
///
/// Beads persists a compressed status vocabulary — SPUR's nine-state
/// PlanTaskStatus terminals (Approved, Failed, Cancelled, Superseded) all
/// project to `closed` in the backend. The prior 4-way parameterization over
/// SPUR-vocab strings exercised dead code (beads never emits those strings
/// via `br show`). This single test uses the production flow (`br close`)
/// and verifies the invariant holds end-to-end.
#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn report_signal_on_closed_task_records_late_arrival() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = beads_pm(dir.path()).await;
    let task_id = create_task(&pm, "Late signal closed task").await;
    run_br(dir.path(), &["close", &task_id]).expect("br close failed");
    assert_eq!(
        pm.get_issue(&task_id)
            .await
            .expect("get_issue should succeed")
            .status,
        pm.closed_status(),
        "task status should be closed after `br close`"
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
    let signal_comments: Vec<&String> = comments
        .iter()
        .filter(|text| text.trim_start().starts_with(signals::SENTINEL_PREFIX))
        .collect();
    assert!(
        signal_comments.is_empty(),
        "late-arrival path must not emit worker signal sentinels; got: {signal_comments:?}"
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
        "late-arrival path should record exactly one audit sentinel"
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
            terminal_status: pm.closed_status().to_string(),
        }]
    );

    let parsed_signals: Vec<WorkerSignal> = comments
        .iter()
        .filter_map(|text| signals::parse_comment(text).and_then(|parsed| parsed.ok()))
        .collect();
    assert!(
        parsed_signals.is_empty(),
        "late-arrival path must not parse any worker signal sentinels; got: {parsed_signals:?}"
    );

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
