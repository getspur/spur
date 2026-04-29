use agent_client_protocol::schema::{
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
    let runtime = BotRuntime::new(store).unwrap();
    (runtime, handle, user_rx)
}

fn ready_event(session: &str, acp_session_id: &str, brain: &str) -> spur_acp::SpurEvent {
    spur_acp::SpurEvent::now(spur_acp::SpurEventBody::AgentSessionReady {
        session: spur_acp::SessionId(session.into()),
        acp_session_id: acp_session_id.into(),
        brain: brain.into(),
        resumed: true,
        cancel_mode: spur_acp::CancelMode::AcpSoft,
        fs_unsafe: false,
        caps: None,
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
async fn unknown_topic_plain_text_auto_registers_and_starts_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".spur/bot/state.json");
    let store = BotStateStore::new(path.clone());
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
    let mut runtime = BotRuntime::new(store).unwrap();

    let renders = runtime
        .handle_chat_text(&handle, 42, Some(777), "hello from a manual topic")
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

    let persisted = BotStateStore::new(path).load().unwrap();
    let record = persisted
        .threads
        .get(&777)
        .expect("unknown topic should be lazily registered and persisted");
    assert_eq!(record.topic_name, "Topic 777");
    assert!(!record.archived);
    assert!(matches!(record.binding_state, BindingState::Unbound));
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

    assert!(flushed.is_empty()); // flush_pending returns empty vec; side effect is the send
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
    assert!(matches!(
        renders.as_slice(),
        [RuntimeRender::WorkingStatus { .. }]
    ));
}

#[tokio::test]
async fn unknown_topic_resume_auto_registers_and_enters_restore_pending() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".spur/bot/state.json");
    let store = BotStateStore::new(path.clone());
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
    let mut runtime = BotRuntime::new(store).unwrap();

    let renders = runtime
        .handle_chat_text(&handle, 42, Some(888), "/resume acp-rebound")
        .await
        .unwrap();

    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp-rebound"
    ));
    assert!(matches!(
        renders.as_slice(),
        [RuntimeRender::WorkingStatus { .. }]
    ));

    let persisted = BotStateStore::new(path).load().unwrap();
    let record = persisted
        .threads
        .get(&888)
        .expect("unknown topic should be lazily registered for /resume");
    assert_eq!(record.topic_name, "Topic 888");
    assert!(!record.archived);
    assert!(matches!(
        record.binding_state,
        BindingState::RestorePending {
            ref acp_session_id,
            ref brain,
        } if acp_session_id == "acp-rebound" && brain == "kimi"
    ));
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

// ── Blocking-correctness regression coverage ─────────────────────────────────

#[tokio::test]
async fn new_topic_record_is_persisted_before_first_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".spur/bot/state.json");
    let store = BotStateStore::new(path.clone());
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
    let mut runtime = BotRuntime::new(store).unwrap();

    // /new in the lobby returns CreateTopic and persists the seq bump.
    let renders = runtime
        .handle_chat_text(&handle, 42, None, "/new")
        .await
        .unwrap();
    assert!(matches!(
        renders.as_slice(),
        [RuntimeRender::CreateTopic { .. }]
    ));

    // The transport layer then calls ensure_topic_record; that call MUST persist
    // so the Unbound thread survives a restart before the operator sends the
    // first message.
    runtime
        .ensure_topic_record(42, 500, "Session 1".into())
        .unwrap();

    let reloaded = BotStateStore::new(path).load().unwrap();
    let record = reloaded
        .threads
        .get(&500)
        .expect("new topic must be persisted before first message");
    assert_eq!(record.topic_name, "Session 1");
    assert!(!record.archived);
    assert!(matches!(record.binding_state, BindingState::Unbound));
}

#[tokio::test]
async fn review_callback_becomes_stale_after_topic_rebind() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.activate_topic_binding(42, 77, "Topic A".into(), "acp-old".into(), "kimi".into());

    // Spawn an executor in the live session so the review prompt gets routed.
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::ExecutorSpawned {
                id: "exec-1".into(),
                parent_id: None,
                session_id: spur_acp::SessionId("spur_acp-old".into()),
                agent: "kimi".into(),
                role: spur_acp::Role::Executor,
                task_spec: String::new(),
            },
        ))
        .unwrap();
    let (_key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::ExecutorReviewRequested {
                id: "exec-1".into(),
                attempt_n: 1,
                kind: spur_acp::ReviewKind::Completion,
                payload: spur_acp::ReviewPayload {
                    summary: "Review needed".into(),
                    diff_summary: None,
                    pr_url: None,
                    error: None,
                    delegation_plan: None,
                    chosen_matches_dispatched: None,
                    peer_influence: None,
                },
            },
        ))
        .unwrap();
    let token = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::ReviewPrompt { buttons, .. } => Some(buttons[0].token.clone()),
            _ => None,
        })
        .unwrap();

    // Rebind the same topic to a different ACP session.
    runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-new")
        .await
        .unwrap();
    let _ = user_rx.recv().await; // drain ResumeSession

    // Old button must now be stale even though the ThreadKey still matches.
    let key = spur_bot::state::ThreadKey {
        chat_id: 42,
        message_thread_id: Some(77),
    };
    let renders = runtime
        .handle_callback(&handle, &key, "cbq-late", &token)
        .await
        .unwrap();
    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::AnswerCallback { text, .. } if text.contains("expired")
    )));
}

#[tokio::test]
async fn review_callback_becomes_stale_after_topic_archived_with_preserved_acp_id() {
    // Keep review_rx alive so that a regression (non-stale path) would surface
    // as a failed stale-assertion rather than a closed-channel panic.
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, mut user_rx) = tokio::sync::mpsc::channel(4);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(4);
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
    let mut runtime = BotRuntime::new(store).unwrap();
    // Topic A owns acp-shared and has a live session spur_acp-shared.
    runtime.activate_topic_binding(42, 77, "Topic A".into(), "acp-shared".into(), "kimi".into());
    // Topic B will take over acp-shared via /resume, forcing Topic A to archive.
    runtime
        .ensure_topic_record(42, 88, "Topic B".into())
        .unwrap();

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::ExecutorSpawned {
                id: "exec-1".into(),
                parent_id: None,
                session_id: spur_acp::SessionId("spur_acp-shared".into()),
                agent: "kimi".into(),
                role: spur_acp::Role::Executor,
                task_spec: String::new(),
            },
        ))
        .unwrap();
    let (_key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::ExecutorReviewRequested {
                id: "exec-1".into(),
                attempt_n: 1,
                kind: spur_acp::ReviewKind::Completion,
                payload: spur_acp::ReviewPayload {
                    summary: "Review needed".into(),
                    diff_summary: None,
                    pr_url: None,
                    error: None,
                    delegation_plan: None,
                    chosen_matches_dispatched: None,
                    peer_influence: None,
                },
            },
        ))
        .unwrap();
    let token = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::ReviewPrompt { buttons, .. } => Some(buttons[0].token.clone()),
            _ => None,
        })
        .unwrap();

    // Topic B takes over acp-shared; Topic A becomes ArchivedDetached while its
    // record still retains `acp_session_id = "acp-shared"` for /sessions history.
    runtime
        .handle_chat_text(&handle, 42, Some(88), "/resume acp-shared")
        .await
        .unwrap();
    let _ = user_rx.recv().await; // drain ResumeSession

    let a = runtime.thread_record(77).expect("topic A");
    assert!(matches!(a.binding, BindingState::ArchivedDetached));
    assert_eq!(
        a.acp_session_id.as_deref(),
        Some("acp-shared"),
        "archived topic must retain its acp_session_id for history",
    );

    let key = spur_bot::state::ThreadKey {
        chat_id: 42,
        message_thread_id: Some(77),
    };
    let renders = runtime
        .handle_callback(&handle, &key, "cbq-late", &token)
        .await
        .unwrap();
    assert!(
        renders.iter().any(|item| matches!(
            item,
            RuntimeRender::AnswerCallback { text, .. } if text.contains("expired")
        )),
        "old button in archived topic must go stale; got: {:?}",
        renders
    );
}

#[tokio::test]
async fn permission_callback_becomes_stale_after_topic_archived_with_preserved_acp_id() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.activate_topic_binding(42, 77, "Topic A".into(), "acp-shared".into(), "kimi".into());
    runtime
        .ensure_topic_record(42, 88, "Topic B".into())
        .unwrap();

    // Permission request whose session_id matches Topic A's live session,
    // so the prompt is routed into Topic A.
    let tool_call = ToolCallUpdate::new("tool-1", ToolCallUpdateFields::new());
    let args = RequestPermissionRequest::new(
        "spur_acp-shared",
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
    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    let request = spur_acp::types::PermissionRequest { args, reply_tx };

    let (_key, renders) = runtime.handle_permission_request(request).unwrap();
    let token = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::PermissionPrompt { buttons, .. } => Some(buttons[0].token.clone()),
            _ => None,
        })
        .unwrap();

    // /resume acp-shared on Topic B archives Topic A but keeps its acp_session_id.
    runtime
        .handle_chat_text(&handle, 42, Some(88), "/resume acp-shared")
        .await
        .unwrap();
    let _ = user_rx.recv().await;

    let key = spur_bot::state::ThreadKey {
        chat_id: 42,
        message_thread_id: Some(77),
    };
    let renders = runtime
        .handle_callback(&handle, &key, "cbq-late", &token)
        .await
        .unwrap();
    assert!(
        renders.iter().any(|item| matches!(
            item,
            RuntimeRender::AnswerCallback { text, .. } if text.contains("expired")
        )),
        "permission button in archived topic must go stale; got: {:?}",
        renders
    );
}

#[tokio::test]
async fn resume_detaches_other_topic_owning_same_session() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.activate_topic_binding(42, 77, "Topic A".into(), "acp-shared".into(), "kimi".into());
    runtime
        .ensure_topic_record(42, 88, "Topic B".into())
        .unwrap();

    runtime
        .handle_chat_text(&handle, 42, Some(88), "/resume acp-shared")
        .await
        .unwrap();
    let _ = user_rx.recv().await;

    let a = runtime.thread_record(77).expect("topic A");
    assert!(a.archived, "topic A must be archived after collision");
    assert!(matches!(a.binding, BindingState::ArchivedDetached));
    assert!(a.live_session.is_none());

    let b = runtime.thread_record(88).expect("topic B");
    assert!(matches!(
        b.binding,
        BindingState::RestorePending { ref acp_session_id, .. } if acp_session_id == "acp-shared"
    ));
}

#[tokio::test]
async fn same_topic_does_not_start_two_fresh_sessions_before_ready() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime
        .ensure_topic_record(42, 77, "Topic A".into())
        .unwrap();

    // First plain text in Unbound: NewSessionWithMessage.
    runtime
        .handle_chat_text(&handle, 42, Some(77), "hello 1")
        .await
        .unwrap();
    let first = user_rx.recv().await.unwrap();
    assert!(
        matches!(
            first,
            spur_core::InteractiveInput::NewSessionWithMessage { .. }
        ),
        "first plain text must kick off NewSessionWithMessage; got {:?}",
        first
    );

    // Second plain text BEFORE AgentSessionReady returns must not emit a
    // second NewSessionWithMessage. The latest message must be queued
    // instead so the topic still binds exactly once.
    let renders = runtime
        .handle_chat_text(&handle, 42, Some(77), "hello 2")
        .await
        .unwrap();
    assert!(
        renders
            .iter()
            .any(|r| matches!(r, RuntimeRender::WorkingStatus { .. })),
        "second plain text must render a working status, got {:?}",
        renders
    );
    assert!(
        user_rx.try_recv().is_err(),
        "same topic must not emit a second NewSessionWithMessage before the first ready"
    );

    // When the fresh ready finally arrives, the queued message flushes.
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_a".into()),
                acp_session_id: "acp-a".into(),
                brain: "kimi".into(),
                resumed: false,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    let key = spur_bot::state::ThreadKey {
        chat_id: 42,
        message_thread_id: Some(77),
    };
    runtime.flush_pending(&handle, &key).await.unwrap();

    let queued = user_rx.recv().await.unwrap();
    assert!(
        matches!(&queued, spur_core::InteractiveInput::Message { .. }),
        "queued message must flush as a regular Message after bind; got {:?}",
        queued
    );

    let record = runtime.thread_record(77).unwrap();
    assert!(
        matches!(&record.binding, BindingState::Active { acp_session_id, .. } if acp_session_id == "acp-a"),
        "topic must be Active with acp-a; got {:?}",
        record.acp_session_id
    );
}

#[tokio::test]
async fn stale_fresh_ready_does_not_reactivate_rebound_topic() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime
        .ensure_topic_record(42, 77, "Topic A".into())
        .unwrap();

    // Topic 77 kicks off a fresh session.
    runtime
        .handle_chat_text(&handle, 42, Some(77), "hello")
        .await
        .unwrap();
    let _ = user_rx.recv().await.unwrap();

    // Before the fresh AgentSessionReady returns, the same topic is rebound
    // via /resume to an existing ACP session.
    runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-existing")
        .await
        .unwrap();
    let _ = user_rx.recv().await.unwrap();

    // A late AgentSessionReady { resumed: false } arrives from the in-flight
    // new-session call. It must not reactivate topic 77 because the topic
    // was evicted from the pending-new guard on /resume.
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_fresh".into()),
                acp_session_id: "acp-fresh".into(),
                brain: "kimi".into(),
                resumed: false,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    let record = runtime.thread_record(77).expect("topic 77 present");
    assert!(
        matches!(
            &record.binding,
            BindingState::RestorePending { acp_session_id, .. } if acp_session_id == "acp-existing"
        ),
        "stale fresh ready must not overwrite the /resume target; binding was {:?}",
        record.binding
    );
    assert_eq!(
        record.acp_session_id.as_deref(),
        Some("acp-existing"),
        "stale fresh ready must not overwrite the /resume acp_session_id"
    );
    assert!(
        record.live_session.is_none(),
        "topic 77 must not gain a live session from the stale fresh ready"
    );
}

#[tokio::test]
async fn same_topic_resume_supersession_keeps_new_binding_when_old_ready_arrives_late() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-X".into(), "kimi".into());

    runtime
        .handle_chat_text(&handle, 42, Some(77), "hello while restoring")
        .await
        .unwrap();
    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp-X"
    ));

    runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-Y")
        .await
        .unwrap();
    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp-Y"
    ));

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_Y".into()),
                acp_session_id: "acp-Y".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_X".into()),
                acp_session_id: "acp-X".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    let record = runtime.thread_record(77).expect("topic 77 present");
    assert!(
        matches!(
            &record.binding,
            BindingState::Active { acp_session_id, .. } if acp_session_id == "acp-Y"
        ),
        "late ready for X must not overwrite the newer /resume Y binding; binding was {:?}",
        record.binding
    );
}

#[tokio::test]
async fn same_topic_resume_supersession_ignores_old_ready_until_new_ready_arrives() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-X".into(), "kimi".into());

    runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-Y")
        .await
        .unwrap();
    let _ = user_rx.recv().await.unwrap();

    let (key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_X".into()),
                acp_session_id: "acp-X".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    assert!(
        key.is_none(),
        "stale resumed-ready for X must not route anywhere while /resume Y is pending"
    );
    assert!(
        renders.is_empty(),
        "stale resumed-ready for X must not render while /resume Y is pending"
    );

    let record = runtime.thread_record(77).expect("topic 77 present");
    assert!(
        matches!(
            &record.binding,
            BindingState::RestorePending { acp_session_id, .. } if acp_session_id == "acp-Y"
        ),
        "old ready for X must not activate the topic while /resume Y is pending; binding was {:?}",
        record.binding
    );
    assert!(record.live_session.is_none());
}

#[tokio::test]
async fn late_resumed_ready_without_pending_target_is_ignored() {
    let (mut runtime, handle, _user_rx) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-77".into(), "kimi".into());
    let _ = runtime
        .handle_chat_text(&handle, 42, None, "/sessions")
        .await
        .unwrap();

    let (key, renders) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur-late".into()),
                acp_session_id: "acp-late".into(),
                brain: "kimi".into(),
                resumed: true,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    assert!(
        key.is_none(),
        "late resumed-ready with no pending target must not route anywhere"
    );
    assert!(
        renders.is_empty(),
        "late resumed-ready with no pending target must not render"
    );

    let record = runtime.thread_record(77).expect("topic 77 present");
    assert!(
        matches!(
            &record.binding,
            BindingState::RestorePending { acp_session_id, .. } if acp_session_id == "acp-77"
        ),
        "late unrelated resumed-ready must not mutate existing restore state; binding was {:?}",
        record.binding
    );
    assert!(record.live_session.is_none());
}

#[tokio::test]
async fn fresh_ready_replaces_existing_live_route_for_same_topic() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime
        .ensure_topic_record(42, 77, "Session 1".into())
        .unwrap();

    runtime
        .handle_chat_text(&handle, 42, Some(77), "hello")
        .await
        .unwrap();
    let _ = user_rx.recv().await.unwrap();

    runtime.activate_topic_binding(42, 77, "Session 1".into(), "acp-X".into(), "kimi".into());

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_Y".into()),
                acp_session_id: "acp-Y".into(),
                brain: "kimi".into(),
                resumed: false,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    let (old_key, _) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::TurnComplete {
                session: spur_acp::SessionId("spur_acp-X".into()),
            },
        ))
        .unwrap();
    assert!(
        old_key.is_none(),
        "stale session route for acp-X must be removed after the fresh ready binds spur_Y"
    );

    let (new_key, _) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::TurnComplete {
                session: spur_acp::SessionId("spur_Y".into()),
            },
        ))
        .unwrap();
    assert_eq!(
        new_key,
        Some(spur_bot::state::ThreadKey {
            chat_id: 42,
            message_thread_id: Some(77),
        }),
        "the committed fresh session spur_Y must still route to topic 77"
    );
}

#[tokio::test]
async fn multiple_pending_new_sessions_bind_in_fifo_order() {
    let (mut runtime, handle, mut user_rx) = test_runtime();
    runtime
        .ensure_topic_record(42, 77, "Topic A".into())
        .unwrap();
    runtime
        .ensure_topic_record(42, 88, "Topic B".into())
        .unwrap();

    // Both topics fire NewSessionWithMessage before AgentSessionReady returns.
    runtime
        .handle_chat_text(&handle, 42, Some(77), "hello from A")
        .await
        .unwrap();
    let _ = user_rx.recv().await;
    runtime
        .handle_chat_text(&handle, 42, Some(88), "hello from B")
        .await
        .unwrap();
    let _ = user_rx.recv().await;

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_a".into()),
                acp_session_id: "acp-a".into(),
                brain: "kimi".into(),
                resumed: false,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();
    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_b".into()),
                acp_session_id: "acp-b".into(),
                brain: "kimi".into(),
                resumed: false,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    let a = runtime.thread_record(77).unwrap();
    let b = runtime.thread_record(88).unwrap();
    assert_eq!(
        a.acp_session_id.as_deref(),
        Some("acp-a"),
        "topic A (first to fire) must bind the first Ready event"
    );
    assert_eq!(
        b.acp_session_id.as_deref(),
        Some("acp-b"),
        "topic B (second to fire) must bind the second Ready event"
    );
}

#[tokio::test]
async fn sessions_command_renders_thread_registry_not_raw_acp_ids() {
    let (mut runtime, handle, _user_rx) = test_runtime();
    runtime.activate_topic_binding(42, 77, "Topic A".into(), "acp-a".into(), "kimi".into());
    runtime.restore_topic_binding(42, 88, "Topic B".into(), "acp-b".into(), "claude".into());
    runtime.seed_archived_topic_record(42, 99, "Topic C".into(), "acp-c".into(), "kimi".into());

    let renders = runtime
        .handle_chat_text(&handle, 42, None, "/sessions")
        .await
        .unwrap();
    let text = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::ServiceMessage { text } => Some(text.clone()),
            _ => None,
        })
        .expect("expected a ServiceMessage for /sessions");

    assert!(
        text.contains("Topic A") && text.contains("Topic B") && text.contains("Topic C"),
        "sessions render must list every topic name; got: {text}"
    );
    assert!(
        text.contains("acp-a") && text.contains("acp-b") && text.contains("acp-c"),
        "sessions render must expose ACP ids when present; got: {text}"
    );
    assert!(
        text.to_ascii_lowercase().contains("archived"),
        "sessions render must label archived state; got: {text}"
    );
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
    let mut runtime = BotRuntime::new(store).unwrap();
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
    let mut runtime = BotRuntime::new(store).unwrap();
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
                fs_unsafe: false,
                caps: None,
            },
        ))
        .unwrap();

    let (key, _) = runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::TurnComplete {
                session: spur_acp::SessionId("spur_1".into()),
            },
        ))
        .unwrap();
    assert_eq!(
        key,
        Some(spur_bot::state::ThreadKey {
            chat_id: 42,
            message_thread_id: Some(77),
        }),
        "fresh AgentSessionReady must still install a live route for turn completion"
    );

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
    let mut runtime = BotRuntime::new(store).unwrap();

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
    let mut runtime = BotRuntime::new(store).unwrap();
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
    let mut runtime = BotRuntime::new(store).unwrap();

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
                    peer_influence: None,
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
    let mut runtime = BotRuntime::new(store).unwrap();
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
    let mut runtime = BotRuntime::new(store).unwrap();
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
    let mut persisted = PersistedBotState {
        operator_chat_id: Some(10_001),
        ..PersistedBotState::default()
    };
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

    let mut runtime = BotRuntime::new(store).unwrap();

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
                fs_unsafe: false,
                caps: None,
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
