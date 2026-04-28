use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::{SessionInfo, SpurEvent, SpurEventBody};
use spur_tui::app::App;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn session(id: &str) -> SessionInfo {
    SessionInfo::new(
        std::sync::Arc::<str>::from(id),
        std::path::PathBuf::from("/tmp"),
    )
    .title(format!("session {id}"))
}

fn seeded_picker_app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session_metadata.json");
    let mut app = App::new_with_metadata_path_in_picker_for_test(path);
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::SessionsListed {
            agent: "codex".into(),
            sessions: vec![session("session-1")],
        }),
    );
    (dir, app)
}

#[test]
fn x_triggers_archive() {
    let (_dir, mut app) = seeded_picker_app();

    app.handle_crossterm_event_for_test(key('x'));

    assert_eq!(
        app.metadata_store_for_test()
            .entry("session-1")
            .map(|entry| entry.archived),
        Some(true)
    );
    assert_eq!(
        app.transient_hint_for_test().map(|hint| hint.text.as_str()),
        Some("Archived 'session-1' — press u to undo")
    );
}

#[test]
fn d_triggers_archive_with_deprecation_toast() {
    let (_dir, mut app) = seeded_picker_app();

    app.handle_crossterm_event_for_test(key('d'));

    assert_eq!(
        app.metadata_store_for_test()
            .entry("session-1")
            .map(|entry| entry.archived),
        Some(true)
    );
    assert_eq!(
        app.transient_hint_for_test().map(|hint| hint.text.as_str()),
        Some("d → archive renamed to x")
    );
}
