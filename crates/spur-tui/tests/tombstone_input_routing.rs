use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use spur_acp::{SessionInfo, SpurEvent, SpurEventBody};
use spur_license::{FeatureGateError, FeatureKey, Plan, Tier};
use spur_tui::action::{Action, ViewId};
use spur_tui::app::App;
use spur_tui::components::input_bar::EditMode;
use spur_tui::components::tombstone::{Tombstone, TombstoneKind};
use spur_tui::test_support::{process_action, push_event};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn dispatch_key(app: &mut App, key: KeyEvent) {
    app.handle_crossterm_event(Event::Key(key));
}

fn session(id: &str, title: &str) -> SessionInfo {
    SessionInfo::new(
        std::sync::Arc::<str>::from(id),
        std::path::PathBuf::from("/tmp"),
    )
    .title(title.to_string())
}

fn reversible_tombstone(view: ViewId) -> Tombstone {
    let now = Instant::now();
    Tombstone {
        view,
        kind: TombstoneKind::Reversible {
            inverse: Action::ToggleSessionPin {
                session_id: "session-1".into(),
            },
        },
        label: "Pinned 'session-1'".into(),
        created_at: now,
        expires_at: now + Duration::from_secs(60),
    }
}

fn install_tombstone(app: &mut App, view: ViewId) {
    app.tombstones_for_test()
        .install(reversible_tombstone(view.clone()));
    assert!(app.tombstones_for_test().has(&view));
}

fn seeded_picker_app() -> App {
    let mut app = App::new_for_tests();
    process_action(&mut app, Action::RequestSessions);
    push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::SessionsListed {
            agent: "codex".into(),
            sessions: vec![session("session-1", "alpha"), session("session-2", "beta")],
        }),
    );
    assert_eq!(app.current_view_for_test(), &ViewId::SessionPicker);
    app
}

fn denied_feature() -> FeatureGateError {
    FeatureGateError::Denied {
        key: FeatureKey::CLI_CORE_EXEC,
        tier: Tier::Community,
    }
}

#[test]
fn palette_open_u_types_into_query() {
    let mut app = App::new_for_tests();
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, ctrl('k'));
    dispatch_key(&mut app, key('u'));

    assert_eq!(app.palette_state_for_test().query(), "u");
    assert!(app.is_palette_visible());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn colon_palette_open_u_types_into_query() {
    let mut app = App::new_for_tests();
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, key(':'));
    dispatch_key(&mut app, key('u'));

    assert_eq!(app.palette_state_for_test().query(), "u");
    assert!(app.is_palette_visible());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn session_picker_rename_u_extends_buffer() {
    let mut app = seeded_picker_app();
    let view = ViewId::SessionPicker;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, key('R'));
    let before = app
        .session_picker_for_test()
        .and_then(|picker| picker.rename_buffer_for_test())
        .expect("rename should be active")
        .to_string();

    dispatch_key(&mut app, key('u'));

    let after = app
        .session_picker_for_test()
        .and_then(|picker| picker.rename_buffer_for_test())
        .expect("rename should remain active");
    assert_eq!(after, format!("{before}u"));
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn session_picker_search_focused_u_extends_filter() {
    let mut app = seeded_picker_app();
    let view = ViewId::SessionPicker;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, key('/'));
    dispatch_key(&mut app, key('u'));

    let filter = app
        .session_picker_for_test()
        .expect("picker should be active")
        .filter();
    assert_eq!(filter, "u");
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn session_picker_confirm_switch_swallows_u() {
    let mut app = seeded_picker_app();
    let view = ViewId::SessionPicker;
    install_tombstone(&mut app, view.clone());
    app.set_session_picker_current_session_has_draft_for_test(Some("session-1".into()));

    dispatch_key(&mut app, key('n'));
    assert!(app
        .session_picker_for_test()
        .expect("picker should be active")
        .is_confirm_switch_visible());

    dispatch_key(&mut app, key('u'));

    assert!(app
        .session_picker_for_test()
        .expect("picker should be active")
        .is_confirm_switch_visible());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn collision_modal_swallows_u() {
    let mut app = App::new_for_tests();
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());
    app.set_collision_modal_for_test("session-1", spur_acp::session_lock::HolderInfo::default());

    dispatch_key(&mut app, key('u'));

    assert!(app.is_collision_modal_visible_for_test());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn upgrade_modal_swallows_u() {
    let mut app = App::new_for_tests();
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());
    app.set_upgrade_modal_for_test(denied_feature(), Some(Plan::Pro));

    dispatch_key(&mut app, key('u'));

    assert!(app.is_upgrade_modal_visible_for_test());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn add_comment_modal_swallows_u() {
    let mut app = App::new_for_tests();
    let view = ViewId::IssueBrowser;
    install_tombstone(&mut app, view.clone());
    app.open_issue_browser_add_comment_modal_for_test("bd-1");

    dispatch_key(&mut app, key('u'));

    let body = app
        .issue_browser_for_test()
        .and_then(|v| v.add_comment_modal_body_for_test())
        .expect("add_comment_modal should remain open");
    assert_eq!(body, "u");
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn help_open_u_flashes_close_help_to_undo() {
    let mut app = App::new_for_tests();
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());
    process_action(&mut app, Action::ShowHelp);

    dispatch_key(&mut app, key('u'));

    assert!(app
        .transient_hint_text()
        .unwrap_or("")
        .contains("close help to undo"));
    assert!(app.is_help_visible_for_test());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn residual_undo_happy_path_via_event_dispatch() {
    let mut app = App::new_for_tests();
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, key('u'));

    assert!(!app.tombstones_for_test().has(&view));
    assert!(matches!(
        app.last_action_for_test(),
        Some(Action::ToggleSessionPin { session_id }) if session_id == "session-1"
    ));
}

#[test]
fn quit_confirm_swallows_u() {
    let mut app = App::new_for_tests();
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, ctrl('c'));
    assert!(app.is_quit_confirm_visible_for_test());

    dispatch_key(&mut app, key('u'));

    assert!(app.is_quit_confirm_visible_for_test());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn emacs_ctrl_z_in_palette_swallowed_silently() {
    let mut app = App::new_for_tests();
    app.set_edit_mode_for_test(EditMode::Emacs);
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, ctrl('k'));
    dispatch_key(&mut app, ctrl('z'));

    assert!(app.is_palette_visible());
    assert!(app.tombstones_for_test().has(&view));
}

#[test]
fn emacs_ctrl_z_residual_undoes_in_emacs_mode() {
    let mut app = App::new_for_tests();
    app.set_edit_mode_for_test(EditMode::Emacs);
    let view = ViewId::Dashboard;
    install_tombstone(&mut app, view.clone());

    dispatch_key(&mut app, ctrl('z'));

    assert!(!app.tombstones_for_test().has(&view));
    assert!(matches!(
        app.last_action_for_test(),
        Some(Action::ToggleSessionPin { session_id }) if session_id == "session-1"
    ));
}
