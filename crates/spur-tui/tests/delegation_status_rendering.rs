//! Regression tests for delegation-status rendering in the dashboard activity
//! log and session-detail react trace.
//!
//! These tests cover the three variants that previously fell through to a
//! generic placeholder arm: Rejected, Modified, and TimedOut.

use spur_acp::{DelegationStatus, SessionId, SpurEvent, SpurEventBody, TimeoutFallback};
use spur_tui::views::dashboard::DashboardView;
use spur_tui::views::View;

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn delegation_completed(status: DelegationStatus) -> SpurEvent {
    SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: SessionId("aabbccdd1234".to_string()),
        status,
    })
}

#[test]
fn dashboard_worker_report_progress_renders_message_and_percent() {
    let mut dash = DashboardView::new();
    dash.handle_spur_event(
        &SpurEvent::now(SpurEventBody::WorkerReportProgress {
            delegation_id: "deleg-progress-123456".to_string(),
            message: "Red test committed; implementing display".to_string(),
            percent: Some(45.0),
        }),
        &test_ctx(),
    );

    let entries = dash.activity_log().entries();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert!(
        entry
            .message
            .contains("Red test committed; implementing display"),
        "progress log must include the report message, got: {:?}",
        entry.message,
    );
    assert!(
        entry.message.contains("45%"),
        "progress log must include the percent, got: {:?}",
        entry.message,
    );
    assert_eq!(entry.kind, spur_tui::components::LogEntryKind::Info);
}

#[test]
fn dashboard_modified_status_renders_as_complete_not_failed() {
    let mut dash = DashboardView::new();
    dash.handle_spur_event(
        &delegation_completed(DelegationStatus::Modified {
            reviewer_note: "fix the typo in README".to_string(),
        }),
        &test_ctx(),
    );

    let entries = dash.activity_log().entries();
    assert_eq!(entries.len(), 1, "one log entry expected");
    let entry = &entries[0];
    assert!(
        entry.message.contains("modified"),
        "Modified status must say 'modified', got: {:?}",
        entry.message,
    );
    assert!(
        entry.message.contains("fix the typo in README"),
        "Modified status must include reviewer note, got: {:?}",
        entry.message,
    );
    // Modified is approval-with-note — must use Complete kind, not Error.
    assert_eq!(
        entry.kind,
        spur_tui::components::LogEntryKind::Complete,
        "Modified must render as LogEntryKind::Complete, not Error"
    );
}

#[test]
fn dashboard_rejected_status_renders_with_reason() {
    let mut dash = DashboardView::new();
    dash.handle_spur_event(
        &delegation_completed(DelegationStatus::Rejected {
            reason: "scope too large".to_string(),
        }),
        &test_ctx(),
    );

    let entries = dash.activity_log().entries();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert!(
        entry.message.contains("rejected"),
        "Rejected status must say 'rejected', got: {:?}",
        entry.message,
    );
    assert!(
        entry.message.contains("scope too large"),
        "Rejected status must include the reason, got: {:?}",
        entry.message,
    );
    assert_eq!(
        entry.kind,
        spur_tui::components::LogEntryKind::Error,
        "Rejected must render as LogEntryKind::Error"
    );
}

#[test]
fn dashboard_timed_out_status_renders_with_wait_duration() {
    let mut dash = DashboardView::new();
    dash.handle_spur_event(
        &delegation_completed(DelegationStatus::TimedOut {
            waited_for: std::time::Duration::from_secs(1800),
            fallback: TimeoutFallback::Abandon,
        }),
        &test_ctx(),
    );

    let entries = dash.activity_log().entries();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert!(
        entry.message.contains("1800"),
        "TimedOut status must include wait seconds, got: {:?}",
        entry.message,
    );
    assert!(
        entry.message.contains("timed out") || entry.message.contains("timeout"),
        "TimedOut status must mention timeout, got: {:?}",
        entry.message,
    );
    assert_eq!(
        entry.kind,
        spur_tui::components::LogEntryKind::Error,
        "TimedOut must render as LogEntryKind::Error"
    );
}
