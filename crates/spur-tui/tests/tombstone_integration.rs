// Integration tests for tombstone behavior at the App level.
// Additional tests are added in later destructive-undo tasks.

use std::time::{Duration, Instant};

use spur_core::ReviewDecision;
use spur_tui::action::{Action, ViewId};
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
            pending: Action::SubmitReview {
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
        Some(Action::SubmitReview { .. })
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

    app.handle_undo_for_test();

    assert!(app.tombstones_for_test().has(&ViewId::Dashboard));
    assert!(app.is_help_visible_for_test());
    assert!(
        app.transient_hint_text()
            .unwrap_or("")
            .contains("close help"),
        "expected close-help hint"
    );
}
