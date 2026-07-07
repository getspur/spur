use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use spur_tui::action::ViewId;
use spur_tui::components::status_bar::{StatusBar, StatusBarProps};
use spur_tui::git_info::GitInfo;

fn render_to_text(width: u16, git_info: Option<&GitInfo>) -> String {
    let backend = TestBackend::new(width, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, width, 1);
            StatusBar::render(
                frame,
                area,
                StatusBarProps {
                    view: &ViewId::Dashboard,
                    theme: spur_tui::theme::fallback_theme(),
                    tombstone: None,
                    running: 0,
                    pending_review: 0,
                    total_cost: 0.0,
                    elapsed: "0s",
                    current_mode: None,
                    current_model_label: None,
                    current_effort_label: None,
                    usage_supported: false,
                    context_used: None,
                    context_size: None,
                    stream_in_flight: false,
                    esc_consumed_by_composer: false,
                    notebook_ready: false,
                    issue_count: 0,
                    alert_summary: None,
                    flag_summary: None,
                    git_info,
                    view_hint_override: None,
                },
            );
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    buffer.content.iter().map(|c| c.symbol()).collect()
}

fn sample() -> GitInfo {
    GitInfo {
        repo_name: "myrepo".to_string(),
        branch: Some("main".to_string()),
        short_hash: Some("abc1234".to_string()),
    }
}

#[test]
fn status_bar_renders_repo_branch_and_hash_in_full_mode() {
    let info = sample();
    let text = render_to_text(200, Some(&info));
    assert!(
        text.contains("myrepo main@abc1234"),
        "expected full git segment in status bar, got: {text}"
    );
}

#[test]
fn status_bar_compact_mode_drops_repo_name() {
    let info = sample();
    let text = render_to_text(80, Some(&info));
    assert!(
        text.contains("main@abc1234"),
        "expected compact git segment, got: {text}"
    );
    assert!(
        !text.contains("myrepo"),
        "expected repo name omitted in compact mode, got: {text}"
    );
}

#[test]
fn status_bar_omits_git_segment_when_none() {
    let text = render_to_text(200, None);
    assert!(
        !text.contains('@'),
        "expected no git segment when git_info is None, got: {text}"
    );
}

#[test]
fn status_bar_renders_detached_head_marker() {
    let info = GitInfo {
        repo_name: "myrepo".to_string(),
        branch: None,
        short_hash: Some("abc1234".to_string()),
    };
    let text = render_to_text(200, Some(&info));
    assert!(
        text.contains("myrepo detached@abc1234"),
        "expected detached marker, got: {text}"
    );
}

#[test]
fn status_bar_truncates_long_branch_names() {
    let info = GitInfo {
        repo_name: "myrepo".to_string(),
        branch: Some("feature/very-long-branch-name-that-overflows".to_string()),
        short_hash: Some("abc1234".to_string()),
    };
    let text = render_to_text(200, Some(&info));
    assert!(
        !text.contains("feature/very-long-branch-name-that-overflows"),
        "expected long branch name truncated, got: {text}"
    );
    assert!(text.contains('…'), "expected ellipsis, got: {text}");
}
