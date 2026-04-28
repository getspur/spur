use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::action::ViewId;
use spur_tui::components::status_bar::{StatusBar, StatusBarProps};

fn render_status(
    width: u16,
    model: Option<&str>,
    effort: Option<&str>,
    usage_supported: bool,
    context_used: Option<u64>,
    context_size: Option<u64>,
) -> String {
    let backend = TestBackend::new(width, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    let view = ViewId::SessionDetail(spur_acp::SessionId("session".to_string()));

    terminal
        .draw(|frame| {
            StatusBar::render(
                frame,
                Rect::new(0, 0, width, 1),
                StatusBarProps {
                    view: &view,
                    running: 0,
                    pending_review: 0,
                    total_cost: 0.0,
                    elapsed: "0s",
                    current_mode: Some("default"),
                    current_model_label: model,
                    current_effort_label: effort,
                    usage_supported,
                    context_used,
                    context_size,
                    stream_in_flight: false,
                    esc_consumed_by_composer: false,
                    issue_count: 0,
                    alert_summary: None,
                    license_badge: None,
                    flag_summary: None,
                    view_hint_override: None,
                },
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    (0..buffer.area.width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect::<String>()
}

#[test]
fn codex_status_bar_renders_model_effort_and_usage() {
    let line = render_status(
        160,
        Some("GPT-5 Codex"),
        Some("Medium"),
        true,
        Some(47),
        Some(100),
    );

    assert!(
        line.contains("[default] GPT-5 Codex · Medium · ctx 47%"),
        "status bar missing model/effort/usage group: {line}"
    );
}

#[test]
fn claude_code_status_bar_hides_effort_and_unsupported_usage() {
    let line = render_status(160, Some("Sonnet 4.5"), None, false, None, None);

    assert!(
        line.contains("Sonnet 4.5"),
        "status bar missing model label: {line}"
    );
    assert!(
        !line.contains("ctx"),
        "usage segment must be hidden when unsupported: {line}"
    );
}

#[test]
fn supported_usage_without_update_renders_placeholder() {
    let line = render_status(160, Some("GPT-5 Codex"), Some("Medium"), true, None, None);

    assert!(
        line.contains("ctx --%"),
        "usage-supported sessions should show a placeholder before updates: {line}"
    );
}
