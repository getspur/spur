//! Wave A — PR-2 cache-freshness regression test.
//!
//! When the agent emits a mid-session
//! `session/update.ConfigOptionUpdate(payload)` notification, spur must
//! refresh `SessionDetailView::session_config_options` and rebuild the
//! advertised `/model` (and `/effort`) entries in the `CommandRegistry`.
//!
//! Today (pre-PR-2) the catch-all `_ => trace!(...)` arm in
//! `app::apply_session_update` swallows the notification, leaving loaded
//! sessions with `config_options: Vec::new()` and any picker empty.

use spur_acp::AcpSessionId;
use spur_acp::{
    AgentKind, ConfigOptionUpdate, SessionConfigId, SessionConfigOption, SessionConfigSelectOption,
    SessionId, SessionNotification, SessionUpdate, SpurAgentCaps,
};
use spur_tui::commands::CommandSource;
use spur_tui::views::session_detail::SessionDetailView;

fn select_option(config_id: &str, current: &str, choices: &[(&str, &str)]) -> SessionConfigOption {
    let select_choices: Vec<SessionConfigSelectOption> = choices
        .iter()
        .map(|(id, name)| SessionConfigSelectOption::new((*id).to_string(), (*name).to_string()))
        .collect();
    SessionConfigOption::select(
        SessionConfigId::new(config_id.to_string()),
        "label".to_string(),
        current.to_string(),
        select_choices,
    )
}

#[test]
fn config_option_update_refreshes_advertised_entries_and_cache() {
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

    // Pre-condition: empty cache (loaded-session shape).
    assert!(view.session_config_options_for_test().is_empty());

    let model = select_option(
        "model",
        "gpt-5.5",
        &[("gpt-5.5", "GPT-5.5"), ("gpt-5.4", "GPT-5.4")],
    );
    let payload = ConfigOptionUpdate::new(vec![model.clone()]);
    let update = SessionUpdate::ConfigOptionUpdate(payload);
    let notif = SessionNotification::new(AcpSessionId::new("test"), update);

    spur_tui::test_support::apply_notification(&mut view, &notif);

    let model_entries: Vec<_> = view
        .command_registry()
        .list()
        .into_iter()
        .filter(|e| e.name == "model")
        .collect();
    assert_eq!(
        model_entries.len(),
        1,
        "expected one /model entry after ConfigOptionUpdate refresh"
    );
    assert!(matches!(
        model_entries[0].source,
        CommandSource::Advertised { ref handle } if handle == "codex"
    ));
    assert!(model_entries[0].arg_picker_spec.is_some());

    assert_eq!(
        view.session_config_options_for_test().len(),
        1,
        "view's session_config_options cache must reflect the update"
    );
}

/// Grok re-advertises `configOptions` mid-session with re-scoped choices
/// (e.g. after a model switch, `reasoning_effort` loses the model-incompatible
/// `xhigh`). When the view holds a caps snapshot (fresh-session shape), the
/// frozen `caps.config_options` must also be refreshed — otherwise the
/// synthesized `/model` and `/effort` entries keep stale `current:` hints and
/// caps-derived choice lists.
#[test]
fn config_option_update_refreshes_caps_frozen_snapshot_and_entry_hints() {
    let tmp = tempfile::tempdir().unwrap();
    let session = SessionId::new();
    let mut view = SessionDetailView::new(
        session.clone(),
        "grok".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("grok"),
        Vec::new(),
    );

    // session/new snapshot: union effort choices incl. xhigh, model grok-4.6.
    let init = spur_acp::InitializeResponse::new(spur_acp::ProtocolVersion::LATEST);
    let mut new = spur_acp::NewSessionResponse::new(spur_acp::AcpSessionId::new("acp-session"));
    new.config_options = Some(vec![
        select_option(
            "model",
            "grok-4.6",
            &[("grok-4.6", "Grok 4.6"), ("grok-4.5", "Grok 4.5")],
        ),
        select_option(
            "reasoning_effort",
            "high",
            &[
                ("xhigh", "Extra High Effort"),
                ("high", "High Effort"),
                ("medium", "Medium Effort"),
            ],
        ),
    ]);
    view.set_spur_agent_caps(Some(std::sync::Arc::new(SpurAgentCaps::new(
        &init,
        &new,
        AgentKind::Grok,
    ))));

    // Agent re-advertisement after switching to grok-4.5: efforts narrowed.
    let re_advertised = vec![
        select_option(
            "model",
            "grok-4.5",
            &[("grok-4.6", "Grok 4.6"), ("grok-4.5", "Grok 4.5")],
        ),
        select_option(
            "reasoning_effort",
            "high",
            &[("high", "High Effort"), ("medium", "Medium Effort")],
        ),
    ];
    let notif = SessionNotification::new(
        AcpSessionId::new("test"),
        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(re_advertised)),
    );
    spur_tui::test_support::apply_notification(&mut view, &notif);

    let hint_for = |name: &str| {
        view.command_registry()
            .list()
            .into_iter()
            .find(|e| e.name == name)
            .and_then(|e| e.hint.clone())
    };

    assert_eq!(
        hint_for("model").as_deref(),
        Some("current: grok-4.5"),
        "/model entry hint must reflect the re-advertised current model, not the frozen session/new snapshot"
    );
    assert_eq!(
        hint_for("effort").as_deref(),
        Some("current: high"),
        "/effort entry hint must reflect the re-advertised current effort"
    );
    assert_eq!(
        view.session_config_options_for_test()
            .iter()
            .find(|o| o.id.0.as_ref() == "reasoning_effort")
            .map(|o| spur_acp::extract_choices(o).len()),
        Some(2),
        "narrowed effort choices must replace the union snapshot in the view cache"
    );
}
