//! Wave C.3: integration coverage for the SessionDetailView →
//! CommandRegistry::available_commands_for_session wiring.
//!
//! When a session has gemini-style caps (no `set_config_option`,
//! no `set_model`), the slash-command popup must omit /model and
//! /effort. Per F-3, a session whose caps are absent (None) preserves
//! the unfiltered list — resumed sessions must keep all pickers visible
//! until M9 wires LoadSessionResponse.

use std::sync::Arc;

use agent_client_protocol::schema::{
    InitializeResponse, NewSessionResponse, ProtocolVersion, SessionId,
};
use spur_acp::{AgentKind, SpurAgentCaps};
use spur_tui::commands::advertised::AdvertisedSource;
use spur_tui::commands::CommandRegistry;
use spur_tui::views::session_detail::SessionDetailView;

const HANDLE: &str = "codex";

fn gemini_style_caps() -> Arc<SpurAgentCaps> {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    // Synthetic gemini-style: neither models nor config_options.
    let new = NewSessionResponse::new(SessionId::new("gemini-sid"));
    Arc::new(SpurAgentCaps::new(&init, &new, AgentKind::Generic))
}

fn registry_with_model_and_effort() -> CommandRegistry {
    use spur_acp::{SessionConfigId, SessionConfigOption, SessionConfigSelectOption};

    let model_opt = SessionConfigOption::select(
        SessionConfigId::new("model".to_string()),
        "model label".to_string(),
        "gpt-5".to_string(),
        vec![SessionConfigSelectOption::new(
            "gpt-5".to_string(),
            "GPT-5".to_string(),
        )],
    );
    let effort_opt = SessionConfigOption::select(
        SessionConfigId::new("reasoning_effort".to_string()),
        "effort label".to_string(),
        "medium".to_string(),
        vec![SessionConfigSelectOption::new(
            "medium".to_string(),
            "Medium".to_string(),
        )],
    );
    let entries = AdvertisedSource::entries(HANDLE, &[model_opt, effort_opt]);
    let mut registry = CommandRegistry::new();
    registry.set_advertised_commands(HANDLE, entries);
    registry
}

#[test]
fn session_detail_with_gemini_caps_omits_model_and_effort_from_slash_popup() {
    let mut view = SessionDetailView::new_for_palette_test(registry_with_model_and_effort());
    view.set_spur_agent_caps(Some(gemini_style_caps()));

    let names: Vec<String> = view
        .available_slash_commands()
        .into_iter()
        .map(|e| e.name)
        .collect();

    assert!(
        !names.iter().any(|n| n == "model"),
        "/model must be hidden on a gemini-style session (caps lack set_model AND set_config_option); got names={names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "effort"),
        "/effort must be hidden on a gemini-style session; got names={names:?}"
    );
}

/// F-3 invariant: caps=None preserves the unfiltered popup. Mirrors the
/// behavior expected on resumed sessions before M9 wires
/// LoadSessionResponse into SpurAgentCaps construction.
#[test]
fn session_detail_with_none_caps_keeps_full_popup_list() {
    let view = SessionDetailView::new_for_palette_test(registry_with_model_and_effort());
    // Default constructor leaves spur_agent_caps = None.
    let names: Vec<String> = view
        .available_slash_commands()
        .into_iter()
        .map(|e| e.name)
        .collect();

    assert!(
        names.iter().any(|n| n == "model"),
        "caps=None must be permissive — /model must remain in the popup; got names={names:?}"
    );
    assert!(
        names.iter().any(|n| n == "effort"),
        "caps=None must be permissive — /effort must remain in the popup; got names={names:?}"
    );
}
