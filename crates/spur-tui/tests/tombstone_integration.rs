// Integration tests for tombstone behavior at the App level.
// Additional tests are added in later destructive-undo tasks.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use spur_core::ReviewDecision;
use spur_tui::action::{Action, IssueAction, ViewId};
use spur_tui::app::App;
use spur_tui::components::input_bar::{EditMode, VimMode};
use spur_tui::components::tombstone::{Tombstone, TombstoneKind};
use spur_tui::test_support::process_action;

#[test]
fn tombstone_slots_field_accessible_via_tick() {
    let mut app = spur_tui::app::App::new_for_tests();
    app.tick();
}

fn reversible_tombstone(view: ViewId) -> Tombstone {
    let now = Instant::now();
    Tombstone {
        view,
        kind: TombstoneKind::Reversible {
            inverse: Action::ToggleSessionArchive {
                session_id: "s1".into(),
                via_legacy_key: false,
            },
        },
        label: "Archived 'foo'".into(),
        created_at: now,
        expires_at: now + Duration::from_secs(60),
    }
}

fn issue_summary(id: &str, status: &str) -> spur_pm::IssueSummary {
    spur_pm::IssueSummary {
        id: id.into(),
        source: spur_pm::PmSource::Beads,
        title: "Fix bug".into(),
        status: status.into(),
        labels: vec![],
        url: String::new(),
        priority: None,
        issue_type: None,
        assignee: None,
        description: None,
    }
}

#[test]
fn undo_with_no_tombstone_flashes_nothing_to_undo() {
    let mut app = App::new_for_tests();
    process_action(&mut app, Action::NavigateTo(ViewId::SessionPicker));

    app.handle_undo_for_test();

    assert!(
        app.transient_hint_text()
            .unwrap_or("")
            .contains("nothing to undo"),
        "expected 'nothing to undo' hint"
    );
}

#[test]
fn undo_reversible_tombstone_dispatches_inverse() {
    let mut app = App::new_for_tests();
    app.tombstones_for_test()
        .install(reversible_tombstone(ViewId::SessionPicker));
    process_action(&mut app, Action::NavigateTo(ViewId::SessionPicker));

    app.handle_undo_for_test();

    assert!(!app.tombstones_for_test().has(&ViewId::SessionPicker));
    assert!(matches!(
        app.last_action_for_test(),
        Some(Action::ToggleSessionArchive {
            session_id,
            via_legacy_key: false,
        }) if session_id == "s1"
    ));
    assert!(
        app.transient_hint_text().unwrap_or("").contains("Undid"),
        "expected undo confirmation"
    );
}

#[test]
fn undo_queued_remote_cancels_without_dispatch() {
    let mut app = App::new_for_tests();
    let now = Instant::now();
    app.tombstones_for_test().install(Tombstone {
        view: ViewId::Dashboard,
        kind: TombstoneKind::QueuedRemote {
            pending: Action::SubmitReviewDispatch {
                executor_id: "exec-1".into(),
                attempt_n: 1,
                decision: ReviewDecision::Approve,
            },
        },
        label: "Approve".into(),
        created_at: now,
        expires_at: now + Duration::from_secs(3),
    });

    app.handle_undo_for_test();

    assert!(!app.tombstones_for_test().has(&ViewId::Dashboard));
    assert!(!matches!(
        app.last_action_for_test(),
        Some(Action::SubmitReviewDispatch { .. })
    ));
    assert!(
        app.transient_hint_text()
            .unwrap_or("")
            .contains("Cancelled"),
        "expected cancel confirmation"
    );
}

#[test]
fn undo_in_compose_mode_does_not_consume_tombstone() {
    let mut app = App::new_for_tests();
    app.tombstones_for_test()
        .install(reversible_tombstone(ViewId::Dashboard));
    app.dashboard_mut_for_test().handle_paste("draft");
    app.set_edit_mode_for_test(EditMode::Vim(VimMode::Insert));

    app.handle_undo_for_test();

    assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
}

#[test]
fn undo_blocked_by_picker_open_does_not_consume() {
    let mut app = App::new_for_tests();
    app.tombstones_for_test()
        .install(reversible_tombstone(ViewId::Dashboard));
    app.open_dashboard_slash_picker_for_test();

    app.handle_undo_for_test();

    assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
    assert!(
        app.transient_hint_text().is_none(),
        "picker-owned undo key should not flash a tombstone hint"
    );
}

#[test]
fn undo_blocked_by_help_overlay_flashes_close_hint() {
    let mut app = App::new_for_tests();
    app.tombstones_for_test()
        .install(reversible_tombstone(ViewId::Dashboard));
    process_action(&mut app, Action::ShowHelp);

    app.handle_crossterm_event(Event::Key(KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::NONE,
    )));

    assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
    assert!(app.is_help_visible_for_test());
    assert!(
        app.transient_hint_text()
            .unwrap_or("")
            .contains("close help"),
        "expected close-help hint"
    );
}

#[test]
fn tombstone_installs_on_archive_with_60s_window() {
    let mut app = App::new_for_tests();
    let before = Instant::now();
    process_action(
        &mut app,
        Action::ToggleSessionArchive {
            session_id: "s1".into(),
            via_legacy_key: false,
        },
    );
    let ts = app.tombstones_for_test().peek(&ViewId::SessionPicker);
    assert!(ts.is_some(), "tombstone must be installed");
    let ts = ts.unwrap();
    assert!(matches!(ts.kind, TombstoneKind::Reversible { .. }));
    let elapsed = ts.expires_at.saturating_duration_since(before);
    assert!(
        elapsed >= Duration::from_secs(59) && elapsed <= Duration::from_secs(61),
        "expires_at must be ~60s from now, got {elapsed:?}"
    );
}

#[test]
fn tombstone_archive_undo_restores_via_inverse() {
    let mut app = App::new_for_tests();
    process_action(
        &mut app,
        Action::ToggleSessionArchive {
            session_id: "s1".into(),
            via_legacy_key: false,
        },
    );
    process_action(&mut app, Action::NavigateTo(ViewId::SessionPicker));
    app.handle_undo_for_test();
    assert!(!app.tombstones_for_test().has(&ViewId::SessionPicker));
    assert!(matches!(
        app.last_action_for_test(),
        Some(Action::ToggleSessionArchive { .. })
    ));
}

#[test]
fn tombstone_installs_on_pin_with_60s_window() {
    let mut app = App::new_for_tests();
    process_action(
        &mut app,
        Action::ToggleSessionPin {
            session_id: "s2".into(),
        },
    );
    let ts = app.tombstones_for_test().peek(&ViewId::SessionPicker);
    assert!(ts.is_some());
    assert!(matches!(ts.unwrap().kind, TombstoneKind::Reversible { .. }));
}

#[test]
fn tombstone_pin_undo_restores_via_inverse() {
    let mut app = App::new_for_tests();
    process_action(
        &mut app,
        Action::ToggleSessionPin {
            session_id: "s2".into(),
        },
    );
    process_action(&mut app, Action::NavigateTo(ViewId::SessionPicker));
    app.handle_undo_for_test();
    assert!(!app.tombstones_for_test().has(&ViewId::SessionPicker));
    assert!(matches!(
        app.last_action_for_test(),
        Some(Action::ToggleSessionPin { .. })
    ));
}

#[test]
fn tombstone_installs_on_rename_with_original_title_as_inverse() {
    let mut app = App::new_for_tests();
    process_action(
        &mut app,
        Action::RenameSession {
            session_id: "s3".into(),
            new_title: "New Name".into(),
            original_title: "Old Name".into(),
        },
    );
    let ts = app.tombstones_for_test().peek(&ViewId::SessionPicker);
    assert!(ts.is_some());
    let ts = ts.unwrap();
    match &ts.kind {
        TombstoneKind::Reversible { inverse } => {
            assert!(
                matches!(
                    inverse,
                    Action::RenameSession { new_title, .. } if new_title == "Old Name"
                ),
                "inverse should restore old title"
            );
        }
        _ => panic!("expected Reversible tombstone"),
    }
}

#[test]
fn rename_undo_restores_original_title() {
    let mut app = App::new_for_tests();
    process_action(
        &mut app,
        Action::RenameSession {
            session_id: "s3".into(),
            new_title: "New".into(),
            original_title: "Old".into(),
        },
    );
    process_action(&mut app, Action::NavigateTo(ViewId::SessionPicker));
    app.handle_undo_for_test();
    assert!(!app.tombstones_for_test().has(&ViewId::SessionPicker));
    assert!(matches!(
        app.last_action_for_test(),
        Some(Action::RenameSession { new_title, .. }) if new_title == "Old"
    ));
}

#[test]
fn tombstone_installs_on_issue_status_update_with_previous_status() {
    let mut app = App::new_for_tests();
    app.set_tracked_issues_for_test(vec![issue_summary("ISSUE-1", "open")]);
    process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    process_action(
        &mut app,
        Action::Issue(IssueAction::UpdateStatus {
            id: "ISSUE-1".into(),
            status: "closed".into(),
            via_legacy_key: false,
        }),
    );

    let ts = app.tombstones_for_test().peek(&ViewId::IssueBrowser);
    assert!(ts.is_some(), "tombstone must be installed");
    match &ts.unwrap().kind {
        TombstoneKind::Reversible { inverse } => {
            assert!(matches!(
                inverse,
                Action::Issue(IssueAction::UpdateStatus { status, .. }) if status == "open"
            ));
        }
        _ => panic!("expected Reversible"),
    }
}

#[test]
fn tombstone_issue_undo_restores_previous_status() {
    let mut app = App::new_for_tests();
    app.set_tracked_issues_for_test(vec![issue_summary("ISSUE-1", "open")]);
    process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    process_action(
        &mut app,
        Action::Issue(IssueAction::UpdateStatus {
            id: "ISSUE-1".into(),
            status: "closed".into(),
            via_legacy_key: false,
        }),
    );

    app.handle_undo_for_test();

    assert!(!app.tombstones_for_test().has(&ViewId::IssueBrowser));
    assert!(matches!(
        app.last_action_for_test(),
        Some(Action::Issue(IssueAction::UpdateStatus { status, .. })) if status == "open"
    ));
}

#[test]
fn tombstone_skipped_when_issue_not_in_tracked_issues() {
    // Regression test: previously the UpdateStatus arm guessed `previous_status = "open"`
    // when the issue wasn't found in tracked_issues, which silently corrupted undo.
    // Now: skip tombstone install entirely so undo is "nothing to undo" rather than wrong.
    let mut app = App::new_for_tests();
    process_action(&mut app, Action::NavigateTo(ViewId::IssueBrowser));
    // Empty tracked_issues — issue "ghost-1" is NOT registered.
    process_action(
        &mut app,
        Action::Issue(IssueAction::UpdateStatus {
            id: "ghost-1".into(),
            status: "closed".into(),
            via_legacy_key: false,
        }),
    );

    // No tombstone installed because previous_status couldn't be captured.
    assert!(
        !app.tombstones_for_test().has(&ViewId::IssueBrowser),
        "tombstone must NOT be installed when issue is missing from tracked_issues"
    );
}

#[test]
fn tombstone_remote_queue_installs_and_does_not_dispatch_immediately() {
    let mut app = App::new_for_tests();
    app.add_pending_review_for_test("exec-1", 1);
    process_action(
        &mut app,
        Action::SubmitReview {
            executor_id: "exec-1".into(),
            attempt_n: 1,
            decision: ReviewDecision::Approve,
        },
    );

    let ts = app.tombstones_for_test().peek(&ViewId::Dashboard);
    assert!(ts.is_some(), "tombstone must be installed");
    match &ts.unwrap().kind {
        TombstoneKind::QueuedRemote {
            pending: Action::SubmitReviewDispatch { .. },
        } => {}
        other => panic!("expected QueuedRemote SubmitReviewDispatch tombstone, got {other:?}"),
    }
    assert!(
        !app.user_input_sent_for_test(),
        "SubmitReview must not dispatch during queue window"
    );
}

#[test]
fn tombstone_remote_queue_cancel_via_undo() {
    let mut app = App::new_for_tests();
    app.add_pending_review_for_test("exec-1", 1);
    process_action(
        &mut app,
        Action::SubmitReview {
            executor_id: "exec-1".into(),
            attempt_n: 1,
            decision: ReviewDecision::Approve,
        },
    );

    app.handle_undo_for_test();

    assert!(!app.tombstones_for_test().has(&ViewId::Dashboard));
    assert!(!app.user_input_sent_for_test());
    assert!(
        app.transient_hint_text()
            .unwrap_or("")
            .contains("Cancelled"),
        "expected cancel confirmation"
    );
}

#[test]
fn tombstone_remote_queue_displaced_by_next_review_dispatches_first_immediately() {
    let mut app = App::new_for_tests();
    app.add_pending_review_for_test("exec-1", 1);
    app.add_pending_review_for_test("exec-2", 1);
    process_action(
        &mut app,
        Action::SubmitReview {
            executor_id: "exec-1".into(),
            attempt_n: 1,
            decision: ReviewDecision::Approve,
        },
    );
    process_action(
        &mut app,
        Action::SubmitReview {
            executor_id: "exec-2".into(),
            attempt_n: 1,
            decision: ReviewDecision::Reject {
                reason: "needs changes".into(),
            },
        },
    );

    assert!(app.user_input_sent_for_test_with_executor("exec-1"));
    assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
}

#[test]
fn tombstone_panic_esc_cancels_queued_without_dispatch() {
    let mut app = App::new_for_tests();
    app.add_pending_review_for_test("exec-1", 1);
    process_action(
        &mut app,
        Action::SubmitReview {
            executor_id: "exec-1".into(),
            attempt_n: 1,
            decision: ReviewDecision::Approve,
        },
    );
    assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
    process_action(&mut app, Action::PanicReset);
    // Tombstone cleared.
    assert!(!app.tombstones_for_test().has(&ViewId::Dashboard));
    // SubmitReview was NOT dispatched (no displaced-flush).
    assert!(!app.user_input_sent_for_test());
}

#[test]
fn tombstone_panic_esc_cancels_reversible_too() {
    let mut app = App::new_for_tests();
    process_action(
        &mut app,
        Action::ToggleSessionArchive {
            session_id: "s1".into(),
            via_legacy_key: false,
        },
    );
    assert!(app.tombstones_for_test().has(&ViewId::SessionPicker));
    process_action(&mut app, Action::PanicReset);
    // Reversible tombstone is also cleared (already-committed; just removes undo).
    assert!(!app.tombstones_for_test().has(&ViewId::SessionPicker));
}
