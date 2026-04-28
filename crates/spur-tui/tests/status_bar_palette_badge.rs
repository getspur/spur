use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::action::ViewId;
use spur_tui::components::status_bar::{StatusBar, StatusBarProps};

fn render_status(width: u16) -> String {
    let backend = TestBackend::new(width, 1);
    let mut term = Terminal::new(backend).unwrap();
    let view = ViewId::Dashboard;
    term.draw(|f| {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height: 1,
        };
        let props = StatusBarProps {
            view: &view,
            tombstone: None,
            running: 0,
            pending_review: 0,
            total_cost: 0.0,
            elapsed: "",
            current_mode: None,
            current_model_label: None,
            current_effort_label: None,
            usage_supported: false,
            context_used: None,
            context_size: None,
            stream_in_flight: false,
            esc_consumed_by_composer: false,
            issue_count: 0,
            alert_summary: None,
            license_badge: None,
            flag_summary: None,
            view_hint_override: None,
        };
        StatusBar::render(f, area, props);
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.width)
        .map(|x| buf[(x, 0)].symbol().to_string())
        .collect::<String>()
}

#[test]
fn status_bar_shows_ctrl_k_go_badge() {
    let line = render_status(120);
    assert!(
        line.contains("Ctrl+K"),
        "status bar missing Ctrl+K badge: {line}"
    );
    assert!(line.contains("go"), "badge missing 'go' label: {line}");
}
