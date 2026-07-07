use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use std::sync::Arc;

use spur_acp::{
    AcpSessionId, AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion,
    SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionId, SpurAgentCaps,
    SpurEvent, SpurEventBody,
};
use spur_tui::action::{Action, ViewId};
use spur_tui::components::status_bar::{StatusBar, StatusBarProps};
use spur_tui::views::{session_detail::SessionDetailView, View};

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
                    theme: spur_tui::theme::fallback_theme(),
                    tombstone: None,
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
                    notebook_ready: false,
                    issue_count: 0,
                    alert_summary: None,
                    flag_summary: None,
                    git_info: None,
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

fn effort_option(current: &str) -> SessionConfigOption {
    SessionConfigOption::select(
        SessionConfigId::new("reasoning_effort"),
        "Reasoning effort",
        current.to_string(),
        vec![
            SessionConfigSelectOption::new("medium", "Medium"),
            SessionConfigSelectOption::new("high", "High"),
        ],
    )
}

fn caps_with_effort(current: &str) -> Arc<SpurAgentCaps> {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(AcpSessionId::new("caps-session"));
    new.config_options = Some(vec![effort_option(current)]);
    Arc::new(SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp))
}

fn render_session_detail(view: &mut SessionDetailView) -> String {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);

    // Wide enough that the full metrics group (~100 cols) and the SessionDetail
    // hint text (~100 cols) both fit without the status bar falling back to
    // compact mode (see StatusBar::render's per-view hint reserve).
    let backend = TestBackend::new(220, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);

    terminal
        .draw(|frame| view.render(frame, frame.area(), &ctx))
        .unwrap();

    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

fn new_session_detail_view() -> (tempfile::TempDir, SessionDetailView) {
    let tmp = tempfile::tempdir().unwrap();
    let view = SessionDetailView::new(
        SessionId::new(),
        "codex".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("codex"),
        Vec::new(),
    );
    (tmp, view)
}

fn rendered_status_bar_line(output: &str) -> &str {
    output.lines().last().unwrap_or_default()
}

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn submit_text(view: &mut SessionDetailView, text: &str) -> Option<Action> {
    view.input_bar_mut_for_test()
        .set_text(text.to_string(), text.len());
    view.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    )
}

#[test]
fn session_detail_status_bar_uses_frozen_caps_effort_before_live_options() {
    let (_tmp, mut view) = new_session_detail_view();
    view.set_spur_agent_caps(Some(caps_with_effort("medium")));

    let rendered = render_session_detail(&mut view);
    let status = rendered_status_bar_line(&rendered);

    assert!(
        status.contains("Medium"),
        "fresh sessions should render effort from frozen caps before live config options arrive: {status}"
    );
}

#[test]
fn session_detail_effort_submit_updates_status_before_live_confirmation() {
    let (_tmp, mut view) = new_session_detail_view();
    let caps = caps_with_effort("medium");
    view.set_spur_agent_caps(Some(caps.clone()));
    view.apply_advertised_commands(Some(caps.as_ref()), &[]);

    let action = submit_text(&mut view, "/effort high").expect("effort submit action");
    match action {
        Action::SetSessionConfigOption { config_id, value } => {
            assert_eq!(config_id, "reasoning_effort");
            assert_eq!(value, "high");
        }
        other => panic!("expected SetSessionConfigOption, got {other:?}"),
    }

    let rendered = render_session_detail(&mut view);
    let status = rendered_status_bar_line(&rendered);

    assert!(
        status.contains("high"),
        "status bar should reflect the optimistic /effort value before live confirmation: {status}"
    );
    assert!(
        !status.contains("Medium"),
        "optimistic /effort value should replace the frozen caps label before confirmation: {status}"
    );
}

#[test]
fn session_detail_live_effort_refresh_clears_pending_override() {
    let (_tmp, mut view) = new_session_detail_view();
    let caps = caps_with_effort("medium");
    view.set_spur_agent_caps(Some(caps.clone()));
    view.apply_advertised_commands(Some(caps.as_ref()), &[]);

    let action = submit_text(&mut view, "/effort high").expect("effort submit action");
    assert!(
        matches!(action, Action::SetSessionConfigOption { .. }),
        "expected effort submit to dispatch set_config_option, got {action:?}"
    );

    view.apply_advertised_commands(Some(caps.as_ref()), &[effort_option("medium")]);
    let refreshed = render_session_detail(&mut view);
    let refreshed_status = rendered_status_bar_line(&refreshed);
    assert!(
        refreshed_status.contains("Medium"),
        "live effort config option should take precedence over pending override: {refreshed_status}"
    );

    view.apply_advertised_commands(Some(caps.as_ref()), &[]);
    let after_empty_refresh = render_session_detail(&mut view);
    let after_empty_status = rendered_status_bar_line(&after_empty_refresh);
    assert!(
        after_empty_status.contains("Medium"),
        "pending effort override should stay cleared after a resolvable live refresh: {after_empty_status}"
    );
    assert!(
        !after_empty_status.contains("high"),
        "cleared pending effort override must not return after options disappear: {after_empty_status}"
    );
}

#[test]
fn session_detail_status_bar_uses_live_effort_after_config_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let session = SessionId::new();
    let mut view = SessionDetailView::new(
        session.clone(),
        "codex".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("codex"),
        Vec::new(),
    );
    view.set_spur_agent_caps(Some(caps_with_effort("medium")));
    view.apply_advertised_commands(
        Some(&caps_with_effort("medium")),
        &[effort_option("medium")],
    );

    let initial = render_session_detail(&mut view);
    assert!(
        initial.contains("Medium"),
        "initial status bar should render frozen initialize effort: {initial}"
    );

    let event = SpurEvent::now(SpurEventBody::CommandRegistryDirty {
        session,
        caps: Some(caps_with_effort("high")),
        config_options: vec![effort_option("high")],
    });
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    view.handle_spur_event(&event, &ctx);

    let refreshed = render_session_detail(&mut view);
    assert!(
        refreshed.contains("High"),
        "status bar effort must follow live config_options after refresh: {refreshed}"
    );
}

#[test]
fn codex_status_bar_renders_model_effort_and_usage() {
    // Wide enough for full metrics (~100 cols) plus the SessionDetail hint
    // text (~100 cols) to both fit without falling back to compact mode.
    let line = render_status(
        220,
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
fn unsupported_usage_flag_does_not_hide_real_live_usage_data() {
    // `usage_supported` is a frozen per-agent-kind prediction captured at
    // session start (see spur-acp's `agent_quirks::usage_emit_default`), not
    // a live protocol capability — ACP has no capability flag for
    // `UsageUpdate` emission. When the live agent sends real usage data
    // despite that prediction saying "unsupported", the gauge must still
    // render it: arrived data is strictly better evidence than a guess.
    let line = render_status(160, Some("Sonnet 4.5"), None, false, Some(47), Some(100));

    assert!(
        line.contains("ctx 47%"),
        "real live usage data must render even when usage_supported predicted false: {line}"
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

#[test]
fn one_hundred_seventy_columns_use_compact_model_effort_usage_form() {
    // Narrow enough that full metrics (~103 cols) + hint (~98 cols) don't fit,
    // but wide enough that compact metrics (~50 cols) + hint do — the band
    // where compacting the metrics actually buys back the hint's space.
    let line = render_status(
        170,
        Some("gpt-5-super-long-model-name"),
        Some("Medium"),
        true,
        Some(47),
        Some(100),
    );

    assert!(
        line.contains("super-long-mo… · ctx 47%"),
        "compact status bar should keep truncated model and usage: {line}"
    );
    assert!(
        !line.contains("Medium"),
        "compact status bar should drop effort: {line}"
    );
}

#[test]
fn one_hundred_columns_keep_full_model_effort_usage_form() {
    let line = render_status(
        100,
        Some("GPT-5 Codex"),
        Some("Medium"),
        true,
        Some(47),
        Some(100),
    );

    assert!(
        line.contains("[default] GPT-5 Codex · Medium · ctx 47%"),
        "100-column status bar should keep the full model/effort/usage group: {line}"
    );
}

#[test]
fn two_hundred_twenty_columns_keep_full_model_effort_usage_form() {
    // Wide enough for full metrics (~100 cols) plus the SessionDetail hint
    // text (~100 cols) to both fit without falling back to compact mode.
    let line = render_status(
        220,
        Some("GPT-5 Codex"),
        Some("Medium"),
        true,
        Some(47),
        Some(100),
    );

    assert!(
        line.contains("[default] GPT-5 Codex · Medium · ctx 47%"),
        "220-column status bar should keep the full model/effort/usage group: {line}"
    );
}
