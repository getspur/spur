//! Confirms unknown SessionUpdate variants don't crash the TUI and that the
//! three new read-only variants mutate session state as expected.

use agent_client_protocol::SessionId as AcpSessionId;
use spur_acp::{
    AvailableCommandsUpdate, CurrentModeUpdate, SessionNotification, SessionUpdate, UsageUpdate,
};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

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

#[test]
fn new_session_with_message_does_not_leak_into_later_sessions() {
    // Regression for BUG-1: after Task 15's NewSessionWithMessage plumbing,
    // a typed dashboard message must NOT get replayed into an unrelated
    // session that happens to spawn later. The `pending_user_messages`
    // buffer has been removed, so the only cross-session-replay vector
    // is gone — this test asserts a fresh BrainSpawned produces a trace
    // with zero user entries.
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};

    let mut app = spur_tui::test_support::new_app();
    let sid = SessionId("unrelated".to_string());
    let ev = SpurEvent::now(SpurEventBody::BrainSpawned {
        agent: "agent".to_string(),
        session: sid.clone(),
    });
    spur_tui::test_support::push_event(&mut app, ev);

    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    assert_eq!(
        detail.trace_entry_count(),
        0,
        "buffered text leaked into unrelated session"
    );
}

#[test]
fn agent_notification_replay_path_stays_eager_and_does_not_require_history_chunks() {
    use spur_acp::{
        ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, SpurEvent,
        SpurEventBody, TextContent,
    };

    let mut app = spur_tui::test_support::new_app();
    let sid = SessionId("resume-live".into());
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "claude".into(),
            session: sid.clone(),
        }),
    );

    let notif = SessionNotification::new(
        sid.0.clone(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "live replay text",
        )))),
    );
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: sid.clone(),
            notification: Box::new(notif),
        }),
    );

    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    let entries = detail.react_trace().entries();
    assert!(
        entries.iter().any(|entry| {
            matches!(
                &entry.kind,
                spur_tui::components::react_trace::TraceKind::AgentMessage { agent }
                    if agent == "claude"
            ) && entry
                .markdown
                .as_ref()
                .is_some_and(|stream| stream.raw_text().contains("live replay text"))
        }),
        "live replay path must still work without SessionHistoryChunk; entries={entries:?}"
    );
}

#[test]
fn kiro_available_notification_populates_registry() {
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_tui::views::View;

    let sid = SessionId("kiro-test-session".to_string());

    let kiro_cfg = std::sync::Arc::new(spur_acp::AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        kind: spur_acp::types::AgentKind::Generic,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: spur_acp::CommandsConfig {
            dispatch: spur_acp::DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".to_string()),
            args_template: spur_acp::ArgsTemplateKind::RawRest,
            ingest: vec![spur_acp::IngestBinding {
                method: spur_acp::ext::KIRO_COMMANDS_AVAILABLE.to_string(),
                parser: spur_acp::IngestParserKind::JsonPathList,
                path: "availableCommands".to_string(),
                item_schema: spur_acp::ItemSchemaKind::AcpAvailableCommand,
            }],
            response: vec![],
            static_commands: vec![],
        },
        permissions: Default::default(),
        skip_permissions: false,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: None,
        delegation: Default::default(),
    });

    let mut view = spur_tui::views::session_detail::SessionDetailView::new(
        sid.clone(),
        "kiro".to_string(),
        "brain".to_string(),
        std::path::PathBuf::from("."),
        kiro_cfg,
        Vec::new(),
    );

    let params = serde_json::json!({
        "sessionId": sid.0,
        "availableCommands": [
            { "name": "context", "description": "manage context" }
        ]
    });
    let ev = SpurEvent::now(SpurEventBody::AgentExtNotification {
        session: sid,
        method: spur_acp::ext::KIRO_COMMANDS_AVAILABLE.to_string(),
        params,
    });
    view.handle_spur_event(&ev, &test_ctx());

    let entries = view.command_registry().list();
    assert!(
        entries.iter().any(|e| e.name == "context"
            && matches!(
                &e.source,
                spur_tui::commands::CommandSource::Agent { handle } if handle == "kiro"
            )),
        "context not populated as kiro agent command: {:?}",
        entries
            .iter()
            .map(|e| (e.name.clone(), e.source.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn kiro_execute_response_renders_as_system_note() {
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_tui::views::View;

    let sid = SessionId("kiro-exec-session".to_string());

    let kiro_cfg = std::sync::Arc::new(spur_acp::AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        kind: spur_acp::types::AgentKind::Generic,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: spur_acp::CommandsConfig {
            dispatch: spur_acp::DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".to_string()),
            args_template: spur_acp::ArgsTemplateKind::RawRest,
            ingest: vec![],
            response: vec![spur_acp::ResponseBinding {
                method: "_kiro.dev/commands/execute/response".to_string(),
                render: spur_acp::ResponseRenderKind::SystemNote,
            }],
            static_commands: vec![],
        },
        permissions: Default::default(),
        skip_permissions: false,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: None,
        delegation: Default::default(),
    });

    let mut view = spur_tui::views::session_detail::SessionDetailView::new(
        sid.clone(),
        "kiro".to_string(),
        "brain".to_string(),
        std::path::PathBuf::from("."),
        kiro_cfg,
        Vec::new(),
    );

    let ev = SpurEvent::now(SpurEventBody::AgentExtNotification {
        session: sid,
        method: "_kiro.dev/commands/execute/response".to_string(),
        params: serde_json::json!({"stdout": "ok"}),
    });
    view.handle_spur_event(&ev, &test_ctx());

    let last_trace = view.trace_snapshot_for_test();
    assert!(
        last_trace
            .iter()
            .any(|t| t.contains("kiro") && t.contains("response")),
        "expected a kiro-tagged response system note; got {last_trace:?}"
    );
}
