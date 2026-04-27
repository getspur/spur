use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

fn rendered_buffer_rows(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buf = terminal.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            let mut row = String::new();
            for x in 0..buf.area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            row
        })
        .collect()
}

fn render_app_rows(app: &mut spur_tui::app::App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    rendered_buffer_rows(&terminal)
}

fn future_metadata_store() -> SessionMetadataStore {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    write_future_metadata(&path);
    SessionMetadataStore::load(&path)
}

fn write_future_metadata(path: &std::path::Path) {
    let future = current_metadata_version() + 1;
    std::fs::write(path, format!(r#"{{"version":{future},"sessions":{{}}}}"#)).unwrap();
}

#[test]
fn persist_metadata_read_only_refusal_surfaces_visible_warning() {
    let store = future_metadata_store();
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

#[test]
fn warning_banner_reserves_row_above_view() {
    let mut app_without_warning = spur_tui::app::App::new(None, true);
    let rows_without_warning = render_app_rows(&mut app_without_warning, 80, 24);
    let first_view_row = rows_without_warning
        .iter()
        .position(|row| !row.trim().is_empty())
        .expect("session picker should render visible content");

    let mut app_with_warning = spur_tui::app::App::new(None, true);
    app_with_warning.set_metadata_store_for_test(future_metadata_store());
    assert!(!app_with_warning.persist_metadata_for_test("renamed session"));
    let rows_with_warning = render_app_rows(&mut app_with_warning, 80, 24);

    assert!(
        rows_with_warning[0].contains("Read-only mode"),
        "banner should occupy the reserved top row, row 0: {:?}",
        rows_with_warning[0]
    );
    assert_eq!(
        rows_with_warning[first_view_row + 1],
        rows_without_warning[first_view_row],
        "first visible view row should shift to the row immediately below its normal position"
    );
    assert_ne!(
        rows_with_warning[first_view_row], rows_without_warning[first_view_row],
        "view content should not remain in its original row when the banner is visible"
    );
}

#[test]
fn esc_dismisses_read_only_warning_without_clearing_read_only_state() {
    let mut app = spur_tui::test_support::new_app();
    app.set_metadata_store_for_test(future_metadata_store());

    assert!(!app.persist_metadata_for_test("renamed session"));
    assert!(app.user_warning_for_test().is_some());

    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(
        app.user_warning_for_test().is_none(),
        "Esc should dismiss the visual warning"
    );
    let rendered = render_app_rows(&mut app, 80, 24).join("\n");
    assert!(
        !rendered.contains("Read-only mode"),
        "banner should not render after Esc dismiss, rendered:\n{rendered}"
    );

    assert!(
        !app.persist_metadata_for_test("renamed session"),
        "dismissal must not clear metadata store read-only mode"
    );
    assert!(
        app.user_warning_for_test().is_some(),
        "next refused persist should surface a fresh warning"
    );
}

#[test]
fn long_warning_uses_explicit_ellipsis_at_narrow_width() {
    let mut app = spur_tui::test_support::new_app();
    app.set_metadata_store_for_test(future_metadata_store());

    assert!(!app.persist_metadata_for_test(
        "renamed session with an intentionally long context for narrow terminal polish"
    ));

    let rows = render_app_rows(&mut app, 60, 24);
    let banner = rows[0].trim_end();
    assert!(
        banner.ends_with("..."),
        "long warning should ellipsize explicitly instead of silent clipping: {banner:?}"
    );
}

#[test]
fn future_metadata_shows_read_only_warning_on_first_paint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session_metadata.json");
    write_future_metadata(&path);

    let mut app = spur_tui::app::App::new_with_metadata_path_for_test(path);

    let warning = app
        .user_warning_for_test()
        .expect("future metadata should surface a startup warning");
    assert!(warning.contains("Read-only mode"));
    assert!(warning.contains("Edits this session WILL NOT be persisted"));
    assert!(warning.contains("(Esc to dismiss)"));

    let rendered = render_app_rows(&mut app, 160, 24).join("\n");
    assert!(
        rendered.contains("Edits this session WILL NOT be persisted"),
        "first paint should render the startup warning before any persist attempt:\n{rendered}"
    );
}
