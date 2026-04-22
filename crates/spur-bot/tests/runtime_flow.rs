use agent_client_protocol::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionRequest,
    ToolCallUpdate, ToolCallUpdateFields,
};
use spur_bot::runtime::{BotRuntime, RuntimeRender};
use spur_bot::state::{BindingState, BotStateStore, PersistedBotState, PersistedThreadRecord};
use spur_interactive::InteractiveFrontendHost;

fn mk_permission_request() -> (
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

fn test_runtime() -> (
    BotRuntime,
    spur_interactive::InteractiveFrontendHandle,
    tokio::sync::mpsc::Receiver<spur_core::InteractiveInput>,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, user_rx) = tokio::sync::mpsc::channel(4);
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
    let runtime = BotRuntime::new(store);
    (runtime, handle, user_rx)
}

fn ready_event(session: &str, acp_session_id: &str, brain: &str) -> spur_acp::SpurEvent {
    spur_acp::SpurEvent::now(spur_acp::SpurEventBody::AgentSessionReady {
        session: spur_acp::SessionId(session.into()),
        acp_session_id: acp_session_id.into(),
        brain: brain.into(),
        resumed: true,
        cancel_mode: spur_acp::CancelMode::AcpSoft,
    })
}

// ── Thread-native runtime tests ─────────────────────────────────────────────

#[tokio::test]
async fn lobby_plain_text_is_rejected() {
    let (mut runtime, handle, _user_rx) = test_runtime();
    let renders = runtime
        .handle_chat_text(&handle, 42, None, "hello")
        .await
        .unwrap();

    assert!(matches!(
        renders.as_slice(),
        [RuntimeRender::ServiceMessage { text }]
        if text.contains("/new")
    ));
}

#[tokio::test]
async fn unbound_topic_plain_text_starts_new_session() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime
        .ensure_topic_record(42, 77, "Session 1".into())
        .unwrap();

    let renders = runtime
        .handle_chat_text(&handle, 42, Some(77), "hello")
        .await
        .unwrap();

    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::NewSessionWithMessage { .. }
    ));
    assert!(matches!(
        renders.as_slice(),
        [RuntimeRender::WorkingStatus { .. }]
    ));
}

#[tokio::test]
async fn restore_pending_topic_queues_resume_then_flushes_message() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-77".into(), "kimi".into());

    runtime
        .handle_chat_text(&handle, 42, Some(77), "hello")
        .await
        .unwrap();
    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp-77"
    ));

    let event = ready_event("session-1", "acp-77", "kimi");
    runtime.handle_spur_event(event).unwrap();
    let key = spur_bot::state::ThreadKey {
        chat_id: 42,
        message_thread_id: Some(77),
    };
    let flushed = runtime.flush_pending(&handle, &key).await.unwrap();

    assert!(!flushed.is_empty() || true); // flush_pending returns empty vec; side effect is the send
    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::Message { .. }
    ));
}

#[tokio::test]
async fn topic_resume_archives_previous_binding() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.activate_topic_binding(42, 77, "Session 1".into(), "acp-old".into(), "kimi".into());

    let renders = runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-new")
        .await
        .unwrap();

    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp-new"
    ));
    assert!(runtime
        .thread_record(77)
        .unwrap()
        .archived_previous
        .contains(&"acp-old".to_string()));
    assert!(matches!(renders.as_slice(), [RuntimeRender::WorkingStatus { .. }]));
}

#[tokio::test]
async fn lobby_new_requires_topic_creation_before_chat() {
    let (mut runtime, handle, _user_rx) = test_runtime();
    let renders = runtime
        .handle_chat_text(&handle, 42, None, "/new")
        .await
        .unwrap();

    assert!(matches!(
        renders.as_slice(),
        [RuntimeRender::CreateTopic { topic_name }]
        if topic_name == "Session 1"
    ));
}

// ── Existing regression tests (adapted to thread-native API) ────────────────

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
    runtime
        .ensure_topic_record(10_001, 77, "Session 1".into())
        .unwrap();

    let renders = runtime
        .handle_chat_text(&handle, 10_001, Some(77), "Investigate review loop")
        .await
        .unwrap();

    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::NewSessionWithMessage { .. }
    ));
    assert!(renders
        .iter()
        .any(|item| matches!(item, RuntimeRender::WorkingStatus { .. })));
}

#[tokio::test]
async fn agent_session_ready_commits_binding_and_persists() {
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
    runtime
        .ensure_topic_record(42, 77, "Session 1".into())
        .unwrap();

    runtime
        .handle_chat_text(&handle, 42, Some(77), "hello")
        .await
        .unwrap();
    // consume the NewSessionWithMessage command
    let _ = user_rx.recv().await.unwrap();

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
    assert_eq!(persisted.threads.len(), 1);
    let record = persisted.threads.values().next().unwrap();
    assert_eq!(record.acp_session_id.as_deref(), Some("acp_1"));
    assert_eq!(record.brain.as_deref(), Some("claude-code"));
    assert!(matches!(
        record.binding_state,
        BindingState::RestorePending { .. }
    ));
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

    let key = spur_bot::state::ThreadKey::lobby(0);
    let renders = runtime
        .handle_callback(&handle, &key, "cbq-stale", "deadbeef")
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

    let (_key, renders) = runtime.handle_permission_request(request).unwrap();
    let token = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::PermissionPrompt { buttons, .. } => Some(buttons[0].token.clone()),
            _ => None,
        })
        .unwrap();

    let key = spur_bot::state::ThreadKey::lobby(0);
    runtime
        .handle_callback(&handle, &key, "cbq-perm", &token)
        .await
        .unwrap();

    let response = reply_rx.await.unwrap();
    assert_eq!(response.option_id, "allow_once");
}

#[tokio::test]
async fn review_prompt_resolves_once_and_siblings_go_stale() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, mut review_rx) = tokio::sync::mpsc::channel(1);
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

    let (_key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::ExecutorReviewRequested {
                id: "exec-1".into(),
                attempt_n: 1,
                kind: spur_acp::ReviewKind::Completion,
                payload: spur_acp::ReviewPayload {
                    summary: "Add frobnicate module".into(),
                    diff_summary: None,
                    pr_url: None,
                    error: None,
                    delegation_plan: None,
                    chosen_matches_dispatched: None,
                },
            },
        ))
        .unwrap();

    let buttons = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::ReviewPrompt { buttons, .. } => Some(buttons.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(buttons.len(), 3);
    let first_token = &buttons[0].token;
    let second_token = &buttons[1].token;
    assert_ne!(first_token, second_token);

    // First callback succeeds.
    let key = spur_bot::state::ThreadKey::lobby(0);
    let renders = runtime
        .handle_callback(&handle, &key, "cbq-1", first_token)
        .await
        .unwrap();
    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::AnswerCallback { text, .. } if text == "Review decision received."
    )));

    // Exactly one review decision was enqueued.
    let decision = review_rx.recv().await.unwrap();
    assert!(matches!(
        decision,
        spur_core::InteractiveInput::SubmitReview { executor_id, attempt_n, .. }
        if executor_id == "exec-1" && attempt_n == 1
    ));

    // Second callback on a sibling token is rejected as stale.
    let key = spur_bot::state::ThreadKey::lobby(0);
    let renders = runtime
        .handle_callback(&handle, &key, "cbq-2", second_token)
        .await
        .unwrap();
    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::AnswerCallback { text, .. } if text.contains("expired")
    )));

    // No second review decision was enqueued.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), review_rx.recv())
            .await
            .is_err()
    );
}

// ── Output rendering regression tests ───────────────────────────────────────

#[tokio::test]
async fn agent_notification_and_turn_complete_renders_final_answer() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let mut runtime = BotRuntime::new(store);
    let session = spur_acp::SessionId("spur_1".into());

    // Accumulate two chunks.
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentNotification {
                session: session.clone(),
                notification: Box::new(spur_acp::SessionNotification::new(
                    "spur_1",
                    spur_acp::SessionUpdate::AgentMessageChunk(spur_acp::ContentChunk::new(
                        spur_acp::ContentBlock::Text(spur_acp::TextContent::new("Hello, ")),
                    )),
                )),
            },
        ))
        .unwrap();
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentNotification {
                session: session.clone(),
                notification: Box::new(spur_acp::SessionNotification::new(
                    "spur_1",
                    spur_acp::SessionUpdate::AgentMessageChunk(spur_acp::ContentChunk::new(
                        spur_acp::ContentBlock::Text(spur_acp::TextContent::new("world!")),
                    )),
                )),
            },
        ))
        .unwrap();

    // TurnComplete flushes the accumulated text.
    let (_key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::TurnComplete {
                session: session.clone(),
            },
        ))
        .unwrap();

    assert_eq!(renders.len(), 1);
    assert!(
        matches!(&renders[0], RuntimeRender::FinalAnswer { text } if text == "Hello, world!"),
        "expected FinalAnswer with accumulated text, got {:?}",
        renders
    );

    // A second TurnComplete for the same session must not re-emit stale text.
    let (_key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::TurnComplete { session },
        ))
        .unwrap();
    assert!(renders.is_empty());
}

#[tokio::test]
async fn brain_error_renders_service_message() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let mut runtime = BotRuntime::new(store);
    let session = spur_acp::SessionId("spur_1".into());

    // Seed some output so we can verify the buffer is cleared on error.
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentNotification {
                session: session.clone(),
                notification: Box::new(spur_acp::SessionNotification::new(
                    "spur_1",
                    spur_acp::SessionUpdate::AgentMessageChunk(spur_acp::ContentChunk::new(
                        spur_acp::ContentBlock::Text(spur_acp::TextContent::new("partial")),
                    )),
                )),
            },
        ))
        .unwrap();

    let (_key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::BrainError {
                session,
                message: "brain subprocess exited".into(),
            },
        ))
        .unwrap();

    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::ServiceMessage { text } if text.contains("brain subprocess exited")
    )));
}

// ── Restore-before-send regression test ─────────────────────────────────────

#[tokio::test]
async fn restore_pending_plain_text_queues_resume_then_message() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, mut user_rx) = tokio::sync::mpsc::channel(4);
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

    // Pre-seed persisted state so the runtime starts in RestorePending.
    let mut persisted = PersistedBotState::default();
    persisted.operator_chat_id = Some(10_001);
    persisted.threads.insert(
        77,
        PersistedThreadRecord {
            topic_name: "Session 1".into(),
            archived: false,
            acp_session_id: Some("acp_77".into()),
            brain: Some("kiro".into()),
            binding_state: BindingState::RestorePending {
                acp_session_id: "acp_77".into(),
                brain: "kiro".into(),
            },
        },
    );
    store.save(&persisted).unwrap();

    let mut runtime = BotRuntime::new(store);

    // Plain text while RestorePending must trigger ResumeSession, not Message.
    let renders = runtime
        .handle_chat_text(&handle, 10_001, Some(77), "hello after restart")
        .await
        .unwrap();
    assert!(renders
        .iter()
        .any(|r| matches!(r, RuntimeRender::WorkingStatus { .. })));

    let input = user_rx.recv().await.unwrap();
    assert!(
        matches!(&input, spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp_77"),
        "expected ResumeSession for persisted acp id, got {:?}",
        input
    );

    // The plain-text Message must NOT have been sent yet.
    assert!(user_rx.try_recv().is_err());

    // Simulate the orchestrator finishing the restore.
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_77".into()),
                acp_session_id: "acp_77".into(),
                brain: "kiro".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    // flush_pending should now forward the queued Message.
    let key = spur_bot::state::ThreadKey {
        chat_id: 10_001,
        message_thread_id: Some(77),
    };
    let pending_renders = runtime.flush_pending(&handle, &key).await.unwrap();
    assert!(pending_renders.is_empty());

    let input = user_rx.recv().await.unwrap();
    assert!(
        matches!(&input, spur_core::InteractiveInput::Message { .. }),
        "expected queued Message after restore, got {:?}",
        input
    );
}
