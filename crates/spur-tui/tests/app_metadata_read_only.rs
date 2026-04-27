use ratatui::{backend::TestBackend, Terminal};
use spur_tui::session_metadata::{current_metadata_version, SessionMetadataStore};

fn rendered_buffer_text(terminal: &Terminal<TestBackend>) -> String {
    let buf = terminal.backend().buffer();
    let mut rendered = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            rendered.push_str(buf[(x, y)].symbol());
        }
        rendered.push('\n');
    }
    rendered
}

#[test]
fn persist_metadata_read_only_refusal_surfaces_visible_warning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let future = current_metadata_version() + 1;
    std::fs::write(&path, format!(r#"{{"version":{future},"sessions":{{}}}}"#)).unwrap();
    let store = SessionMetadataStore::load(&path);
    assert!(store.is_read_only());

    let mut app = spur_tui::test_support::new_app();
    app.set_metadata_store_for_test(store);

    assert!(!app.persist_metadata_for_test("renamed session"));
    let warning = app
        .user_warning_for_test()
        .expect("read-only refusal must record a user-visible warning");
    assert!(warning.contains("Read-only mode"));
    assert!(warning.contains("renamed session not saved"));

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let rendered = rendered_buffer_text(&terminal);

    assert!(
        rendered.contains("Read-only mode"),
        "warning must render on screen, rendered:\n{rendered}"
    );
    assert!(
        rendered.contains("renamed session not saved"),
        "warning context must render on screen, rendered:\n{rendered}"
    );
}
