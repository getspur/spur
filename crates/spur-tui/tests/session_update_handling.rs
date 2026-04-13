//! Confirms unknown SessionUpdate variants don't crash the TUI and that the
//! three new read-only variants mutate session state as expected.

use agent_client_protocol::SessionId as AcpSessionId;
use spur_acp::{
    AvailableCommandsUpdate, CurrentModeUpdate, SessionNotification, SessionUpdate, UsageUpdate,
};

fn nid() -> AcpSessionId {
    AcpSessionId::new("test")
}

#[test]
fn current_mode_update_sets_mode() {
    let update = SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new("plan"));
    let notif = SessionNotification::new(nid(), update);
    let mut s = spur_tui::test_support::new_session_state();
    spur_tui::test_support::apply_notification(&mut s, &notif);
    assert_eq!(s.current_mode.as_deref(), Some("plan"));
}

#[test]
fn available_commands_update_stores_names() {
    use agent_client_protocol::AvailableCommand;
    let cmds = vec![AvailableCommand::new("compact", "compress context")];
    let update = SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(cmds));
    let notif = SessionNotification::new(nid(), update);
    let mut s = spur_tui::test_support::new_session_state();
    spur_tui::test_support::apply_notification(&mut s, &notif);
    let entries = s.command_registry().list();
    assert!(
        entries.iter().any(|e| e.name == "compact"),
        "compact missing from registry: {:?}",
        entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>()
    );
}

#[test]
fn available_commands_update_preserves_hint() {
    use spur_acp::{
        AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate, SessionUpdate,
        UnstructuredCommandInput,
    };

    let mut view = spur_tui::test_support::new_session_state();

    let cmd = AvailableCommand::new("compact", "compact history").input(
        AvailableCommandInput::Unstructured(UnstructuredCommandInput::new("[threshold]")),
    );
    let update = SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![cmd]));
    let notif = SessionNotification::new(nid(), update);

    spur_tui::test_support::apply_notification(&mut view, &notif);

    let entries = view.command_registry().list();
    let compact = entries
        .iter()
        .find(|e| e.name == "compact")
        .expect("compact present");
    assert_eq!(compact.description, "compact history");
    assert_eq!(compact.hint.as_deref(), Some("[threshold]"));
}

#[test]
fn usage_update_sets_context() {
    let update = SessionUpdate::UsageUpdate(UsageUpdate::new(42u64, 200_000u64));
    let notif = SessionNotification::new(nid(), update);
    let mut s = spur_tui::test_support::new_session_state();
    spur_tui::test_support::apply_notification(&mut s, &notif);
    assert_eq!(s.context_used, Some(42));
    assert_eq!(s.context_size, Some(200_000));
}
