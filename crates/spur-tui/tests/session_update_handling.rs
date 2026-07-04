//! Confirms unknown SessionUpdate variants don't crash the TUI and that the
//! three new read-only variants mutate session state as expected.

use spur_acp::AcpSessionId;
use spur_acp::{
    domain::events::BrainRetireReason, AvailableCommandsUpdate, CurrentModeUpdate,
    SessionNotification, SessionUpdate, UsageUpdate,
};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn nid() -> AcpSessionId {
    AcpSessionId::new("test")
}

fn submit_new_session_message(app: &mut spur_tui::app::App, text: &str) {
    submit_text(app, text);
}

fn submit_text(app: &mut spur_tui::app::App, text: &str) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    for ch in text.chars() {
        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

fn brain_spawned(session: &spur_acp::SessionId) -> spur_acp::SpurEvent {
    spur_acp::SpurEvent::now(spur_acp::SpurEventBody::BrainSpawned {
        agent: "agent".to_string(),
        session: session.clone(),
    })
}

fn prompt_dispatched(session: &spur_acp::SessionId, turn_kind: &str) -> spur_acp::SpurEvent {
    spur_acp::SpurEvent::now(spur_acp::SpurEventBody::PromptDispatched {
        session: session.clone(),
        turn_kind: turn_kind.to_string(),
        continuations_count: usize::from(turn_kind == "merged"),
    })
}

fn brain_retired_user_clear(session: &spur_acp::SessionId) -> spur_acp::SpurEvent {
    spur_acp::SpurEvent::now(spur_acp::SpurEventBody::BrainRetired {
        session: session.clone(),
        reason: BrainRetireReason::UserClear,
    })
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
    use spur_acp::AvailableCommand;
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
fn usage_update_stores_agent_reported_cost_when_present() {
    let usage: UsageUpdate = serde_json::from_value(serde_json::json!({
        "used": 42,
        "size": 200_000,
        "cost": {
            "amount": 1.35,
            "currency": "USD"
        }
    }))
    .expect("valid usage update with cost");
    let update = SessionUpdate::UsageUpdate(usage);
    let notif = SessionNotification::new(nid(), update);
    let mut s = spur_tui::test_support::new_session_state();
    spur_tui::test_support::apply_notification(&mut s, &notif);

    assert_eq!(s.context_used, Some(42));
    assert_eq!(s.context_size, Some(200_000));
    assert_eq!(
        s.agent_reported_cost,
        Some((1.35, "USD".to_string())),
        "agent-reported cost must be stored separately from SPUR's estimate"
    );
}

#[test]
fn usage_update_without_cost_leaves_agent_reported_cost_absent() {
    let update = SessionUpdate::UsageUpdate(UsageUpdate::new(42u64, 200_000u64));
    let notif = SessionNotification::new(nid(), update);
    let mut s = spur_tui::test_support::new_session_state();
    spur_tui::test_support::apply_notification(&mut s, &notif);

    assert_eq!(s.agent_reported_cost, None);
}

/// Documents the residual ad-hoc race; current state-machine ordering prevents it in production.
#[test]
fn unrelated_brain_then_prompt_dispatched_drains_to_that_view() {
    use spur_acp::SessionId;

    let (mut app, _rx) = spur_tui::test_support::app_with_user_input_tx();
    submit_new_session_message(&mut app, "hello");

    let sid = SessionId("unrelated".to_string());
    spur_tui::test_support::push_event(&mut app, brain_spawned(&sid));
    spur_tui::test_support::push_event(&mut app, prompt_dispatched(&sid, "user_only"));

    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    assert_eq!(detail.trace_snapshot_for_test(), vec!["hello".to_string()]);
}

#[test]
fn merged_turn_kind_first_message_appears_in_trace() {
    use spur_acp::SessionId;

    let (mut app, _rx) = spur_tui::test_support::app_with_user_input_tx();
    submit_new_session_message(&mut app, "hello");

    let sid = SessionId("merged-session".to_string());
    spur_tui::test_support::push_event(&mut app, brain_spawned(&sid));
    spur_tui::test_support::push_event(&mut app, prompt_dispatched(&sid, "merged"));

    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    let trace = detail.trace_snapshot_for_test();
    assert_eq!(trace.first().map(String::as_str), Some("hello"));
}

#[test]
fn continuation_only_preserves_pending() {
    use spur_acp::SessionId;

    let (mut app, _rx) = spur_tui::test_support::app_with_user_input_tx();
    submit_new_session_message(&mut app, "hello");

    let sid = SessionId("continuation-session".to_string());
    spur_tui::test_support::push_event(&mut app, brain_spawned(&sid));
    spur_tui::test_support::push_event(&mut app, prompt_dispatched(&sid, "continuation_only"));

    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    assert_eq!(
        spur_tui::test_support::pending_first_user_message(&app),
        Some("hello")
    );
    let trace = detail.trace_snapshot_for_test();
    assert_eq!(
        trace.first().map(String::as_str),
        Some("▸ Brain resuming with 0 worker results"),
        "continuation-only should render a resume note, not the pending user message: {trace:?}"
    );
}

#[test]
fn new_session_pending_cleared_on_clear() {
    use spur_tui::action::Action;

    let (mut app, _rx) = spur_tui::test_support::app_with_user_input_tx();
    submit_new_session_message(&mut app, "hello");
    assert!(spur_tui::test_support::pending_first_user_message(&app).is_some());

    spur_tui::test_support::process_action(&mut app, Action::ClearSession);

    assert_eq!(
        spur_tui::test_support::pending_first_user_message(&app),
        None,
        "ClearSession must drop pending first message"
    );
}

#[test]
fn existing_send_message_does_not_set_pending() {
    let (mut app, _rx) = spur_tui::test_support::app_with_user_input_tx();
    let sid = spur_acp::SessionId("existing".to_string());
    spur_tui::test_support::push_event(&mut app, brain_spawned(&sid));

    submit_new_session_message(&mut app, "hi");

    assert_eq!(
        spur_tui::test_support::pending_first_user_message(&app),
        None
    );
    let detail = spur_tui::test_support::session_detail(&app).unwrap();
    assert_eq!(detail.trace_entry_count(), 1);
    let snap = detail.trace_snapshot_for_test();
    assert!(snap[0].contains("hi"));
}

#[test]
fn post_clear_submission_lands_in_new_session_trace() {
    use spur_tui::action::Action;

    let (mut app, _rx) = spur_tui::test_support::app_with_user_input_tx();

    let sid_a = spur_acp::SessionId("existing-a".to_string());
    spur_tui::test_support::push_event(&mut app, brain_spawned(&sid_a));

    spur_tui::test_support::process_action(&mut app, Action::ClearSession);
    spur_tui::test_support::push_event(&mut app, brain_retired_user_clear(&sid_a));
    assert!(spur_tui::test_support::session_detail(&app)
        .expect("has cleared detail")
        .is_cleared());

    submit_new_session_message(&mut app, "refactor auth");
    assert_eq!(
        spur_tui::test_support::pending_first_user_message(&app),
        Some("refactor auth")
    );

    let sid_b = spur_acp::SessionId("new-b".to_string());
    spur_tui::test_support::push_event(&mut app, brain_spawned(&sid_b));
    spur_tui::test_support::push_event(&mut app, prompt_dispatched(&sid_b, "user_only"));

    let detail = spur_tui::test_support::session_detail(&app).unwrap();
    assert_eq!(detail.session_id().0, "new-b");
    let snap = detail.trace_snapshot_for_test();
    assert!(
        snap.iter().any(|s| s.contains("refactor auth")),
        "snap: {:?}",
        snap
    );
}

#[test]
fn brain_connect_failed_clears_pending() {
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};

    let (mut app, _rx) = spur_tui::test_support::app_with_user_input_tx();
    submit_new_session_message(&mut app, "hello");
    assert_eq!(
        spur_tui::test_support::pending_first_user_message(&app),
        Some("hello")
    );
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::BrainConnectFailed {
            brain: "agent".to_string(),
            reason: "dial failed".to_string(),
        }),
    );

    let sid = SessionId("after-failure".to_string());
    spur_tui::test_support::push_event(&mut app, brain_spawned(&sid));
    spur_tui::test_support::push_event(&mut app, prompt_dispatched(&sid, "user_only"));

    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    assert_eq!(
        detail.trace_entry_count(),
        0,
        "connect-fail pending text leaked into a later session"
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
        additional_directories: vec![],
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
        additional_directories: vec![],
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

#[test]
fn vendor_exec_on_cleared_view_returns_none() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_tui::views::View;

    let sid = SessionId("kiro-cleared-vendor".to_string());

    let kiro_cfg = std::sync::Arc::new(spur_acp::AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        additional_directories: vec![],
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
    view.handle_spur_event(
        &SpurEvent::now(SpurEventBody::AgentExtNotification {
            session: sid,
            method: spur_acp::ext::KIRO_COMMANDS_AVAILABLE.to_string(),
            params,
        }),
        &test_ctx(),
    );

    view.reset_for_clear();
    assert!(view.is_cleared());

    view.input_bar_mut_for_test()
        .set_text("/context".to_string(), 8);

    let action = view.handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &test_ctx(),
    );

    assert!(
        action.is_none(),
        "vendor-exec on a cleared SessionDetailView must be suppressed; got {action:?}"
    );
}
