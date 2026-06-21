//! T-I3: signal arriving on terminal-status task is recorded as late-arrival,
//! not passed to proposer.

use serde_json::json;
use spur_core::handlers::WorkerCallContext;
use spur_core::mcp::signals::report_signal;
use spur_core::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_core::plan::labels;
use spur_core::plan::signals::{self, WorkerSignal};
use uuid::Uuid;

mod common;

/// T-I3: a signal arriving on a task that beads reports as closed must be
/// recorded as a late-arrival, never passed to the proposer.
#[tokio::test]
async fn report_signal_on_closed_task_records_late_arrival() {
    let (_dir, pm) = common::temp_beads_pm().await;
    let task_id = common::create_task(&pm, "Late signal closed task").await;
    common::close_task(&pm, &task_id).await;
    assert_eq!(
        pm.get_issue(&task_id)
            .await
            .expect("get_issue should succeed")
            .status,
        pm.closed_status(),
        "task status should be closed after close"
    );

    let signal = common::scope_drift_signal(Uuid::new_v4());
    let signal_id = signal.signal_id().to_string();

    let result = report_signal(
        &pm,
        common::pro_feature_gate().as_ref(),
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

    let comments = common::comment_texts(&pm, &task_id).await;
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

    let labels = common::issue_labels(&pm, &task_id).await;
    assert!(
        labels.contains(&labels::SIGNAL_LATE_ARRIVAL.to_string()),
        "late-arrival path should carry the late-arrival label; got labels: {labels:?}"
    );
    assert!(
        !labels.contains(&labels::signal_kind(signal.kind_label())),
        "late-arrival path must not carry the signal kind label; got labels: {labels:?}"
    );
}
