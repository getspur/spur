//! End-to-end: a `SpurEventBody::CommandRegistryDirty` event scoped to the
//! view's session id should populate the synthesized `/model` (and `/effort`)
//! entries in the `CommandRegistry` and refresh the cached
//! `session_config_options` snapshot.

use spur_acp::{
    AgentKind, SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionId,
    SpurAgentCaps, SpurEvent, SpurEventBody,
};
use spur_tui::commands::CommandSource;
use spur_tui::views::{session_detail::SessionDetailView, View};

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

fn caps_with_options(options: Vec<SessionConfigOption>) -> std::sync::Arc<SpurAgentCaps> {
    let init = agent_client_protocol::schema::InitializeResponse::new(
        agent_client_protocol::schema::ProtocolVersion::LATEST,
    );
    let mut new = agent_client_protocol::schema::NewSessionResponse::new(
        agent_client_protocol::schema::SessionId::new("acp-session"),
    );
    new.config_options = Some(options);
    std::sync::Arc::new(SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp))
}

#[test]
fn command_registry_dirty_populates_advertised_entry_and_caches_options() {
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

    // Pre-condition: no advertised entries, no cached options.
    assert!(view
        .command_registry()
        .list()
        .iter()
        .all(|e| !matches!(e.source, CommandSource::Advertised { .. })));
    assert!(view.session_config_options_for_test().is_empty());

    let model_opt = select_option(
        "model",
        "gpt-5-codex",
        &[("gpt-5-codex", "GPT-5 Codex"), ("gpt-5", "GPT-5")],
    );
    let event = SpurEvent::now(SpurEventBody::CommandRegistryDirty {
        session: session.clone(),
        caps: Some(caps_with_options(vec![model_opt.clone()])),
        config_options: vec![model_opt.clone()],
    });

    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    view.handle_spur_event(&event, &ctx);

    let model_entries: Vec<_> = view
        .command_registry()
        .list()
        .into_iter()
        .filter(|e| e.name == "model")
        .collect();
    assert_eq!(
        model_entries.len(),
        1,
        "expected one /model entry after CommandRegistryDirty"
    );
    assert!(matches!(
        model_entries[0].source,
        CommandSource::Advertised { ref handle } if handle == "codex"
    ));
    assert!(
        model_entries[0].arg_picker_spec.is_some(),
        "/model entry must carry an arg picker spec"
    );

    assert_eq!(view.session_config_options_for_test().len(), 1);
}

#[test]
fn command_registry_dirty_for_other_session_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let mut view = SessionDetailView::new(
        SessionId::new(),
        "codex".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("codex"),
        Vec::new(),
    );

    let model_opt = select_option("model", "gpt-5", &[("gpt-5", "GPT-5")]);
    let event = SpurEvent::now(SpurEventBody::CommandRegistryDirty {
        session: SessionId::new(),
        caps: Some(caps_with_options(vec![model_opt.clone()])),
        config_options: vec![model_opt],
    });

    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    view.handle_spur_event(&event, &ctx);

    assert!(view.session_config_options_for_test().is_empty());
    assert!(view
        .command_registry()
        .list()
        .iter()
        .all(|e| !matches!(e.source, CommandSource::Advertised { .. })));
}

#[test]
fn command_registry_dirty_with_none_caps_falls_back_to_options_synthesizer() {
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
    view.set_spur_agent_caps(None);

    let model_opt = select_option(
        "model",
        "gpt-5-codex",
        &[("gpt-5-codex", "GPT-5 Codex"), ("gpt-5", "GPT-5")],
    );
    let event = SpurEvent::now(SpurEventBody::CommandRegistryDirty {
        session,
        caps: None,
        config_options: vec![model_opt],
    });

    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    let ctx = spur_tui::test_support::test_view_ctx(&LINEAGE);
    view.handle_spur_event(&event, &ctx);

    let has_model = view
        .command_registry()
        .list()
        .iter()
        .any(|entry| entry.name == "model");
    assert!(has_model, "expected /model entry from options fallback");
}
