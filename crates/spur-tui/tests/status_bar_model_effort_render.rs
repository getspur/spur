use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use std::sync::Arc;

use spur_acp::{
    AcpSessionId, AgentCapabilities, AgentKind, InitializeResponse, NewSessionResponse,
    ProtocolVersion, SessionCapabilities, SessionCloseCapabilities, SessionConfigId,
    SessionConfigOption, SessionConfigSelectOption, SessionDeleteCapabilities, SessionId,
    SessionListCapabilities, SessionResumeCapabilities, SpurAgentCaps, SpurEvent, SpurEventBody,
};
use spur_tui::action::ViewId;
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
                    session_lifecycle_caps: None,
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

fn caps_with_lifecycle(resume: bool, delete: bool, list: bool, close: bool) -> Arc<SpurAgentCaps> {
    let mut session_caps = SessionCapabilities::new();
    if resume {
        session_caps = session_caps.resume(SessionResumeCapabilities::new());
    }
    if delete {
        session_caps = session_caps.delete(SessionDeleteCapabilities::new());
    }
    if list {
        session_caps = session_caps.list(SessionListCapabilities::new());
    }
    if close {
        session_caps = session_caps.close(SessionCloseCapabilities::new());
    }

    let mut init = InitializeResponse::new(ProtocolVersion::LATEST);
    init.agent_capabilities = AgentCapabilities::new().session_capabilities(session_caps);
    let new = NewSessionResponse::new(AcpSessionId::new("caps-session"));
    Arc::new(SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp))
}

fn render_session_detail(view: &mut SessionDetailView) -> String {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);

    let backend = TestBackend::new(160, 20);
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

#[test]
fn session_detail_lifecycle_indicator_absent_without_advertised_caps() {
    let (_tmp, mut view) = new_session_detail_view();

    let without_caps = render_session_detail(&mut view);
    let without_caps_status = rendered_status_bar_line(&without_caps);
    assert!(
        !without_caps_status.contains("session:"),
        "session lifecycle indicator must be hidden when caps are absent: {without_caps_status}"
    );

    view.set_spur_agent_caps(Some(caps_with_lifecycle(false, false, false, false)));
    let without_lifecycle = render_session_detail(&mut view);
    let without_lifecycle_status = rendered_status_bar_line(&without_lifecycle);
    assert!(
        !without_lifecycle_status.contains("session:"),
        "session lifecycle indicator must be hidden when no lifecycle caps are advertised: {without_lifecycle_status}"
    );
}

#[test]
fn session_detail_lifecycle_indicator_lists_advertised_caps_only() {
    let (_tmp, mut view) = new_session_detail_view();
    view.set_spur_agent_caps(Some(caps_with_lifecycle(true, false, false, true)));

    let rendered = render_session_detail(&mut view);
    let status = rendered_status_bar_line(&rendered);

    assert!(
        status.contains("session: resume close"),
        "status bar should list advertised session lifecycle caps: {status}"
    );
    assert!(
        !status.contains("delete") && !status.contains("list"),
        "status bar should not list unadvertised lifecycle caps: {status}"
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

#[test]
fn sixty_columns_use_compact_model_effort_usage_form() {
    let line = render_status(
        60,
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
fn one_hundred_sixty_columns_keep_full_model_effort_usage_form() {
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
        "160-column status bar should keep the full model/effort/usage group: {line}"
    );
}
