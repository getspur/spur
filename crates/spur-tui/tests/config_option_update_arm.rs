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
    ConfigOptionUpdate, SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionId,
    SessionNotification, SessionUpdate,
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
