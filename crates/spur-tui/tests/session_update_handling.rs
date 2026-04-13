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
    assert_eq!(s.available_commands.len(), 1);
    assert_eq!(s.available_commands[0].name, "compact");
}

#[test]
fn available_commands_update_preserves_hint() {
    use spur_acp::{
        AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate,
        SessionUpdate, UnstructuredCommandInput,
    };

    let mut view = spur_tui::test_support::new_session_state();

    let cmd = AvailableCommand::new("compact", "compact history").input(
        AvailableCommandInput::Unstructured(UnstructuredCommandInput::new("[threshold]")),
    );
    let update = SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![cmd]));

    spur_tui::app::apply_session_update(&mut view, &update);

    let got = view.available_commands();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "compact");
    assert_eq!(got[0].description, "compact history");
    match got[0].input.as_ref() {
        Some(AvailableCommandInput::Unstructured(u)) => {
            assert_eq!(u.hint, "[threshold]");
        }
        other => panic!("expected Unstructured hint, got {:?}", other),
    }
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
