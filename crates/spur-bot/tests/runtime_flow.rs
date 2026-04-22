use agent_client_protocol::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionRequest,
    ToolCallUpdate, ToolCallUpdateFields,
};
use spur_bot::runtime::{BotRuntime, RuntimeRender};
use spur_bot::state::BotStateStore;
use spur_interactive::InteractiveFrontendHost;

fn mk_permission_request(
) -> (
    spur_acp::types::PermissionRequest,
    tokio::sync::oneshot::Receiver<spur_acp::types::PermissionResponse>,
) {
    let tool_call = ToolCallUpdate::new("tool-1", ToolCallUpdateFields::new());
    let args = RequestPermissionRequest::new(
        "session-1",
        tool_call,
        vec![
            PermissionOption::new(
                PermissionOptionId::new("allow_once"),
                "Allow Once",
                PermissionOptionKind::AllowOnce,
            ),
            PermissionOption::new(
                PermissionOptionId::new("deny"),
                "Deny",
                PermissionOptionKind::RejectOnce,
            ),
        ],
    );
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    (
        spur_acp::types::PermissionRequest { args, reply_tx },
        reply_rx,
    )
}

#[tokio::test]
async fn first_plain_message_starts_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, mut user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();
    let mut runtime = BotRuntime::new(store);

    let renders = runtime
        .handle_chat_text(&handle, 10_001, "Investigate review loop")
        .await
        .unwrap();

    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::NewSessionWithMessage { .. }
    ));
    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::WorkingStatus { .. }
    )));
}

#[tokio::test]
async fn agent_session_ready_commits_binding_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let mut runtime = BotRuntime::new(store);

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_1".into()),
                acp_session_id: "acp_1".into(),
                brain: "claude-code".into(),
                resumed: false,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    let persisted = runtime.state_store().load().unwrap();
    assert_eq!(persisted.current_acp_session_id.as_deref(), Some("acp_1"));
    assert_eq!(persisted.current_brain.as_deref(), Some("claude-code"));
}

#[tokio::test]
async fn stale_callback_is_reported_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();
    let mut runtime = BotRuntime::new(store);

    let renders = runtime
        .handle_callback(&handle, "cbq-stale", "deadbeef")
        .await
        .unwrap();
    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::AnswerCallback {
            text,
            ..
        } if text.contains("expired")
    )));
}

#[tokio::test]
async fn permission_callback_returns_exact_option_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();
    let mut runtime = BotRuntime::new(store);
    let (request, reply_rx) = mk_permission_request();

    let renders = runtime.handle_permission_request(request).unwrap();
    let token = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::PermissionPrompt { buttons, .. } => {
                Some(buttons[0].token.clone())
            }
            _ => None,
        })
        .unwrap();

    runtime
        .handle_callback(&handle, "cbq-perm", &token)
        .await
        .unwrap();

    let response = reply_rx.await.unwrap();
    assert_eq!(response.option_id, "allow_once");
}
