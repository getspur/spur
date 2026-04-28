use std::time::Duration;

use ratatui::{backend::TestBackend, Terminal};
use spur_tui::app::App;

fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
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
fn transient_hint_is_none_initially() {
    let app = App::new(None, false);

    assert!(app.transient_hint_for_test().is_none());
}

#[test]
fn flash_hint_short_sets_hint() {
    let mut app = App::new(None, false);

    app.flash_hint_short_for_test("hello");

    assert_eq!(
        app.transient_hint_for_test().map(|h| h.text.as_str()),
        Some("hello")
    );
}

#[test]
fn transient_hint_dismissed_after_tick_past_expiry() {
    let mut app = App::new(None, false);

    app.flash_hint_for_test("bye", Duration::ZERO);
    app.tick_transient_hint_for_test(std::time::Instant::now() + Duration::from_secs(10));

    assert!(app.transient_hint_for_test().is_none());
}

#[test]
fn transient_hint_overrides_status_bar_hint() {
    let mut app = App::new(None, false);
    app.flash_hint_for_test("temporary hint", Duration::from_secs(2));

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();

    let rendered = rendered_text(&terminal);
    assert!(
        rendered.contains("temporary hint"),
        "status bar should render transient hint:\n{rendered}"
    );
}
