use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use spur_tui::action::ViewId;
use spur_tui::components::status_bar::{LicenseBadge, LicenseBadgeTone, StatusBar, StatusBarProps};

#[test]
fn status_bar_renders_flag_summary() {
    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 80, 1);
            StatusBar::render(
                frame,
                area,
                StatusBarProps {
                    view: &ViewId::Dashboard,
                    running: 0,
                    pending_review: 0,
                    total_cost: 0.0,
                    elapsed: "0s",
                    current_mode: None,
                    context_used: None,
                    context_size: None,
                    stream_in_flight: false,
                    esc_consumed_by_composer: false,
                    issue_count: 0,
                    alert_summary: None,
                    license_badge: Some(&LicenseBadge::new("community", LicenseBadgeTone::Neutral)),
                    flag_summary: Some((3, 4)),
                },
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    let text = buffer
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(
        text.contains("F:3/4"),
        "expected flag summary 'F:3/4' in status bar, got: {}",
        text
    );
}

#[test]
fn status_bar_omits_flag_summary_when_none() {
    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 80, 1);
            StatusBar::render(
                frame,
                area,
                StatusBarProps {
                    view: &ViewId::Dashboard,
                    running: 0,
                    pending_review: 0,
                    total_cost: 0.0,
                    elapsed: "0s",
                    current_mode: None,
                    context_used: None,
                    context_size: None,
                    stream_in_flight: false,
                    esc_consumed_by_composer: false,
                    issue_count: 0,
                    alert_summary: None,
                    license_badge: None,
                    flag_summary: None,
                },
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    let text = buffer
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(
        !text.contains("F:"),
        "expected no flag summary when None, got: {}",
        text
    );
}
