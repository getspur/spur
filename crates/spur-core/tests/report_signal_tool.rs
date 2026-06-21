//! T-F5: happy path. T-I3: late-arrival gate.
#![allow(clippy::await_holding_lock)]

use std::sync::{Mutex, MutexGuard, OnceLock};

use serde_json::json;
use spur_core::handlers::WorkerCallContext;
use spur_core::mcp::signals::report_signal;
use spur_core::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_core::plan::labels;
use spur_core::plan::signals::{self, WorkerSignal};
use uuid::Uuid;

mod common;

static REPORT_SIGNAL_PM_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn report_signal_pm_lock() -> MutexGuard<'static, ()> {
    REPORT_SIGNAL_PM_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("report_signal PM lock poisoned")
}

#[tokio::test]
async fn report_signal_on_open_task_records_all_artifacts() {
    let _lock = report_signal_pm_lock();
    let (_dir, pm) = common::temp_beads_pm().await;
    let task_id = common::create_task(&pm, "Open signal task").await;
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
            "late": false,
        })
    );

    let comments = common::comment_texts(&pm, &task_id).await;
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

    let labels = common::issue_labels(&pm, &task_id).await;
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
    let _lock = report_signal_pm_lock();
    let (_dir, pm) = common::temp_beads_pm().await;
    let task_id = common::create_task(&pm, "Terminal signal task").await;
    common::close_task(&pm, &task_id).await;
    assert_eq!(
        pm.get_issue(&task_id)
            .await
            .expect("get_issue should succeed")
            .status,
        pm.closed_status()
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

#[tokio::test]
async fn report_signal_threads_worker_call_context() {
    let _lock = report_signal_pm_lock();
    let (_dir, pm) = common::temp_beads_pm().await;
    let task_id = common::create_task(&pm, "Context thread task").await;
    let signal = common::scope_drift_signal(Uuid::new_v4());
    let signal_id = signal.signal_id().to_string();
    let expected_delegation_id = "del-test-42";

    let result = report_signal(
        &pm,
        common::pro_feature_gate().as_ref(),
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

    let comments = common::comment_texts(&pm, &task_id).await;
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
