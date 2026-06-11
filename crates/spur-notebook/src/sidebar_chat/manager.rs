use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, ListSessionsRequest, LoadSessionRequest, PromptRequest,
    ProtocolVersion, SessionId, SessionInfo, SessionNotification, SessionUpdate, TextContent,
};
use futures::{Stream, StreamExt};
use spur_acp::connection::AgentConnection;
use spur_acp::types::{PermissionRequest, PermissionResponse};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

use super::types::{AppScope, ChatEvent, PermissionOptionView};

/// Owns sidebar chat ACP sessions keyed by notebook/app scope.
pub struct SidebarChat {
    conn: Arc<Mutex<dyn AgentConnection>>,
    initialized: Mutex<bool>,
    sessions: Mutex<HashMap<String, SessionId>>,
    pending_permissions: Mutex<HashMap<String, oneshot::Sender<PermissionResponse>>>,
}

impl SidebarChat {
    pub fn new(conn: Arc<Mutex<dyn AgentConnection>>) -> Self {
        Self {
            conn,
            initialized: Mutex::new(false),
            sessions: Mutex::new(HashMap::new()),
            pending_permissions: Mutex::new(HashMap::new()),
        }
    }

    pub async fn ensure_session(&self, scope: &AppScope) -> anyhow::Result<SessionId> {
        if let Some(session_id) = self.sessions.lock().await.get(&scope.app_key).cloned() {
            return Ok(session_id);
        }

        let mut conn = self.conn.lock().await;
        self.initialize_locked(&mut *conn).await?;

        if let Some(session_id) = self.sessions.lock().await.get(&scope.app_key).cloned() {
            return Ok(session_id);
        }

        let response = conn
            .new_session(scope.cwd.clone(), scope.mcp_servers.clone())
            .await?;
        let session_id = response.session_id;

        self.sessions
            .lock()
            .await
            .insert(scope.app_key.clone(), session_id.clone());
        Ok(session_id)
    }

    pub async fn load_session(
        &self,
        scope: &AppScope,
        session_id: SessionId,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        let mut conn = self.conn.lock().await;
        self.initialize_locked(&mut *conn).await?;

        let request = LoadSessionRequest::new(session_id.clone(), scope.cwd.clone())
            .mcp_servers(scope.mcp_servers.clone());
        let stream = conn.load_session(request).await?;

        self.sessions
            .lock()
            .await
            .insert(scope.app_key.clone(), session_id);
        Ok(stream)
    }

    pub async fn list_sessions(&self, scope: &AppScope) -> anyhow::Result<Vec<SessionInfo>> {
        let mut conn = self.conn.lock().await;
        self.initialize_locked(&mut *conn).await?;

        let response = conn
            .list_sessions(ListSessionsRequest::new().cwd(scope.cwd.clone()))
            .await?;
        Ok(response.sessions)
    }

    pub async fn pending_permission_count(&self) -> usize {
        self.pending_permissions.lock().await.len()
    }

    pub async fn turn(
        &self,
        scope: &AppScope,
        prompt: &str,
        tx: mpsc::UnboundedSender<ChatEvent>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let session_id = self.ensure_session(scope).await?;
        let prompt_request = PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(prompt.to_string()))],
        );

        let mut notif_rx = {
            let conn = self.conn.lock().await;
            conn.subscribe_session_notifications()
        };
        let mut prompt_stream = {
            let mut conn = self.conn.lock().await;
            conn.prompt(prompt_request).await?
        };
        let mut stream_done = false;
        let mut grace_deadline: Option<tokio::time::Instant> = None;

        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    let mut conn = self.conn.lock().await;
                    let _ = conn.cancel(session_id.0.as_ref()).await;
                    break;
                }

                maybe_notification = prompt_stream.next(), if !stream_done => {
                    match maybe_notification {
                        Some(notification) => {
                            if notification.session_id == session_id {
                                self.send_notification(notification, &tx);
                            }
                        }
                        None => {
                            stream_done = true;
                            if notif_rx.is_some() {
                                grace_deadline = Some(
                                    tokio::time::Instant::now() + Duration::from_millis(100),
                                );
                            } else {
                                break;
                            }
                        }
                    }
                }

                bcast_outcome = poll_bcast(&mut notif_rx), if notif_rx.is_some() => {
                    match bcast_outcome {
                        BcastOutcome::Notification(notification) => {
                            if notification.session_id == session_id {
                                self.send_notification(*notification, &tx);
                            }
                        }
                        BcastOutcome::Lagged => {}
                        BcastOutcome::Closed => {
                            notif_rx = None;
                            if stream_done {
                                break;
                            }
                        }
                    }
                }

                _ = async {
                    match grace_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => futures::future::pending().await,
                    }
                }, if grace_deadline.is_some() => {
                    break;
                }
            }
        }

        let _ = tx.send(ChatEvent::Done);
        Ok(())
    }

    pub async fn cancel(&self, session_id: &SessionId) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().await;
        conn.cancel(session_id.0.as_ref()).await
    }

    pub fn forward_notification(&self, notification: SessionNotification) -> Option<ChatEvent> {
        match notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
                ContentBlock::Text(text) => Some(ChatEvent::MessageChunk { text: text.text }),
                _ => None,
            },
            SessionUpdate::UsageUpdate(usage) => Some(ChatEvent::Usage {
                input: Some(usage.used),
                output: Some(usage.size),
            }),
            _ => None,
        }
    }

    fn send_notification(
        &self,
        notification: SessionNotification,
        tx: &mpsc::UnboundedSender<ChatEvent>,
    ) {
        if let Some(event) = self.forward_notification(notification) {
            let _ = tx.send(event);
        }
    }

    pub async fn handle_permission_request(
        &self,
        request: PermissionRequest,
        tx: &mpsc::UnboundedSender<ChatEvent>,
    ) -> anyhow::Result<()> {
        let id = request.args.tool_call.tool_call_id.to_string();
        let title = request
            .args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Permission requested".to_string());
        let options = request
            .args
            .options
            .iter()
            .map(|option| PermissionOptionView {
                id: option.option_id.to_string(),
                label: option.name.clone(),
            })
            .collect();

        self.pending_permissions
            .lock()
            .await
            .insert(id.clone(), request.reply_tx);

        tx.send(ChatEvent::PermissionRequest { id, title, options })
            .map_err(|_| anyhow::anyhow!("permission event receiver dropped"))
    }

    pub async fn respond_permission(
        &self,
        id: &str,
        option_id: Option<String>,
    ) -> anyhow::Result<()> {
        let Some(reply_tx) = self.pending_permissions.lock().await.remove(id) else {
            anyhow::bail!("permission request not found: {id}");
        };

        if let Some(option_id) = option_id {
            reply_tx
                .send(PermissionResponse { option_id })
                .map_err(|_| anyhow::anyhow!("permission request responder dropped"))?;
        }
        Ok(())
    }

    async fn initialize_locked(&self, conn: &mut dyn AgentConnection) -> anyhow::Result<()> {
        let mut initialized = self.initialized.lock().await;
        if *initialized {
            return Ok(());
        }

        conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
            .await?;
        *initialized = true;
        Ok(())
    }
}

enum BcastOutcome {
    Notification(Box<SessionNotification>),
    Lagged,
    Closed,
}

async fn poll_bcast(rx: &mut Option<broadcast::Receiver<SessionNotification>>) -> BcastOutcome {
    match rx.as_mut() {
        Some(receiver) => match receiver.recv().await {
            Ok(notification) => BcastOutcome::Notification(Box::new(notification)),
            Err(RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "sidebar chat notification broadcast lagged");
                BcastOutcome::Lagged
            }
            Err(RecvError::Closed) => BcastOutcome::Closed,
        },
        None => futures::future::pending().await,
    }
}

#[cfg(test)]
mod turn {
    use super::*;
    use agent_client_protocol::schema::{
        ContentChunk, InitializeResponse, ListSessionsResponse, McpServer, NewSessionResponse,
        PermissionOption, PermissionOptionKind, RequestPermissionRequest, ToolCallUpdate,
        ToolCallUpdateFields,
    };
    use async_trait::async_trait;
    use futures::stream;
    use spur_acp::types::AgentHealth;
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[derive(Debug, Default)]
    struct FakeState {
        initialize_protocols: Vec<ProtocolVersion>,
        new_sessions: Vec<(PathBuf, Vec<McpServer>)>,
        list_requests: Vec<ListSessionsRequest>,
        load_requests: Vec<LoadSessionRequest>,
        prompt_requests: Vec<PromptRequest>,
        prompt_chunks: Vec<String>,
        broadcast_chunks: Vec<String>,
        prompt_never_finishes: bool,
        cancel_sessions: Vec<String>,
        next_session: usize,
        notification_tx: Option<broadcast::Sender<SessionNotification>>,
    }

    #[derive(Clone, Default)]
    struct FakeConn {
        state: Arc<StdMutex<FakeState>>,
    }

    #[async_trait]
    impl AgentConnection for FakeConn {
        async fn initialize(
            &mut self,
            request: InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            self.state
                .lock()
                .unwrap()
                .initialize_protocols
                .push(request.protocol_version);
            Ok(InitializeResponse::new(ProtocolVersion::LATEST))
        }

        async fn new_session(
            &mut self,
            cwd: PathBuf,
            mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<NewSessionResponse> {
            let mut state = self.state.lock().unwrap();
            state.new_sessions.push((cwd, mcp_servers));
            state.next_session += 1;
            Ok(NewSessionResponse::new(SessionId::new(format!(
                "session-{}",
                state.next_session
            ))))
        }

        async fn prompt(
            &mut self,
            request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
            let mut state = self.state.lock().unwrap();
            state.prompt_requests.push(request.clone());

            if let Some(tx) = state.notification_tx.as_ref() {
                for chunk in &state.broadcast_chunks {
                    let _ = tx.send(message_notification(request.session_id.clone(), chunk));
                }
                return Ok(Box::pin(stream::empty()));
            }

            if state.prompt_never_finishes {
                return Ok(Box::pin(stream::pending()));
            }

            let notifications = state
                .prompt_chunks
                .clone()
                .into_iter()
                .map(move |chunk| message_notification(request.session_id.clone(), chunk));
            Ok(Box::pin(stream::iter(notifications)))
        }

        async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
            self.state
                .lock()
                .unwrap()
                .cancel_sessions
                .push(session_id.to_string());
            Ok(())
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }

        async fn load_session(
            &mut self,
            request: LoadSessionRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
            self.state.lock().unwrap().load_requests.push(request);
            Ok(Box::pin(stream::empty()))
        }

        async fn list_sessions(
            &mut self,
            request: ListSessionsRequest,
        ) -> anyhow::Result<ListSessionsResponse> {
            self.state
                .lock()
                .unwrap()
                .list_requests
                .push(request.clone());
            Ok(ListSessionsResponse::new(vec![SessionInfo::new(
                "scope-session",
                request.cwd.unwrap_or_else(|| PathBuf::from("/unscoped")),
            )]))
        }

        fn subscribe_session_notifications(
            &self,
        ) -> Option<broadcast::Receiver<SessionNotification>> {
            self.state
                .lock()
                .unwrap()
                .notification_tx
                .as_ref()
                .map(|tx| tx.subscribe())
        }
    }

    fn scope(app_key: &str, cwd: &str) -> AppScope {
        AppScope {
            cwd: PathBuf::from(cwd),
            mcp_servers: Vec::new(),
            skill: None,
            app_key: app_key.to_string(),
            label: app_key.to_string(),
        }
    }

    fn chat_with_fake() -> (SidebarChat, Arc<StdMutex<FakeState>>) {
        let conn = FakeConn::default();
        let state = conn.state.clone();
        let chat = SidebarChat::new(Arc::new(Mutex::new(conn)));
        (chat, state)
    }

    fn message_notification(session_id: SessionId, text: impl Into<String>) -> SessionNotification {
        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text.into())));
        SessionNotification::new(session_id, SessionUpdate::AgentMessageChunk(chunk))
    }

    async fn collect_until_done(rx: &mut mpsc::UnboundedReceiver<ChatEvent>) -> Vec<ChatEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            let done = matches!(event, ChatEvent::Done);
            events.push(event);
            if done {
                break;
            }
        }
        events
    }

    #[tokio::test]
    async fn ensure_session_initializes_latest_and_creates_first_app_session() {
        let (chat, state) = chat_with_fake();
        let scope = scope("app-a", "/workspace/app-a");

        let session_id = chat.ensure_session(&scope).await.unwrap();

        assert_eq!(session_id.0.as_ref(), "session-1");
        let state = state.lock().unwrap();
        assert_eq!(state.initialize_protocols, vec![ProtocolVersion::LATEST]);
        assert_eq!(state.new_sessions, vec![(scope.cwd.clone(), Vec::new())]);
    }

    #[tokio::test]
    async fn ensure_session_caches_by_app_key() {
        let (chat, state) = chat_with_fake();
        let first = scope("app-a", "/workspace/app-a");
        let same_key_different_cwd = scope("app-a", "/workspace/renamed");
        let other = scope("app-b", "/workspace/app-b");

        let first_id = chat.ensure_session(&first).await.unwrap();
        let cached_id = chat.ensure_session(&same_key_different_cwd).await.unwrap();
        let other_id = chat.ensure_session(&other).await.unwrap();

        assert_eq!(first_id, cached_id);
        assert_ne!(first_id, other_id);
        let state = state.lock().unwrap();
        assert_eq!(state.initialize_protocols.len(), 1);
        assert_eq!(state.new_sessions.len(), 2);
        assert_eq!(state.new_sessions[0].0, first.cwd);
        assert_eq!(state.new_sessions[1].0, other.cwd);
    }

    #[tokio::test]
    async fn list_sessions_is_cwd_scoped() {
        let (chat, state) = chat_with_fake();
        let scope = scope("app-a", "/workspace/app-a");

        let sessions = chat.list_sessions(&scope).await.unwrap();

        assert_eq!(sessions[0].cwd, scope.cwd);
        let state = state.lock().unwrap();
        assert_eq!(state.list_requests.len(), 1);
        assert_eq!(
            state.list_requests[0].cwd.as_deref(),
            Some(scope.cwd.as_path())
        );
    }

    #[tokio::test]
    async fn load_session_is_cwd_scoped_and_caches_by_app_key() {
        let (chat, state) = chat_with_fake();
        let scope = scope("app-a", "/workspace/app-a");
        let loaded_id = SessionId::new("loaded-session");

        let _stream = chat.load_session(&scope, loaded_id.clone()).await.unwrap();
        let ensured = chat.ensure_session(&scope).await.unwrap();

        assert_eq!(ensured, loaded_id);
        let state = state.lock().unwrap();
        assert_eq!(state.load_requests.len(), 1);
        assert_eq!(state.load_requests[0].session_id, loaded_id);
        assert_eq!(state.load_requests[0].cwd, scope.cwd);
        assert_eq!(state.new_sessions.len(), 0);
    }

    #[tokio::test]
    async fn manager_starts_with_no_pending_permissions() {
        let (chat, _state) = chat_with_fake();

        assert_eq!(chat.pending_permission_count().await, 0);
    }

    #[tokio::test]
    async fn turn_streams_prompt_stream_chunks_into_chat_events() {
        let (chat, state) = chat_with_fake();
        state.lock().unwrap().prompt_chunks = vec!["Hello, ".into(), "world".into()];
        let (tx, mut rx) = mpsc::unbounded_channel();

        chat.turn(
            &scope("app-a", "/workspace/app-a"),
            "say hi",
            tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let events = collect_until_done(&mut rx).await;

        assert_eq!(
            events,
            vec![
                ChatEvent::MessageChunk {
                    text: "Hello, ".into()
                },
                ChatEvent::MessageChunk {
                    text: "world".into()
                },
                ChatEvent::Done
            ]
        );
        let state = state.lock().unwrap();
        assert_eq!(state.prompt_requests.len(), 1);
        assert_eq!(state.prompt_requests[0].session_id.0.as_ref(), "session-1");
    }

    #[tokio::test]
    async fn turn_streams_broadcast_chunks_when_prompt_stream_is_empty() {
        let (chat, state) = chat_with_fake();
        let (tx, _rx) = broadcast::channel(8);
        {
            let mut state = state.lock().unwrap();
            state.notification_tx = Some(tx);
            state.broadcast_chunks = vec!["from ".into(), "broadcast".into()];
        }
        let (tx, mut rx) = mpsc::unbounded_channel();

        chat.turn(
            &scope("app-a", "/workspace/app-a"),
            "say hi",
            tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let events = collect_until_done(&mut rx).await;

        assert_eq!(
            events,
            vec![
                ChatEvent::MessageChunk {
                    text: "from ".into()
                },
                ChatEvent::MessageChunk {
                    text: "broadcast".into()
                },
                ChatEvent::Done
            ]
        );
    }

    #[tokio::test]
    async fn turn_cancellation_calls_connection_cancel_for_session() {
        let (chat, state) = chat_with_fake();
        state.lock().unwrap().prompt_never_finishes = true;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, _rx) = mpsc::unbounded_channel();

        chat.turn(&scope("app-a", "/workspace/app-a"), "say hi", tx, cancel)
            .await
            .unwrap();

        assert_eq!(
            state.lock().unwrap().cancel_sessions,
            vec!["session-1".to_string()]
        );
    }

    #[tokio::test]
    async fn cancel_forwards_session_id_to_connection() {
        let (chat, state) = chat_with_fake();
        let session_id = SessionId::new("sidebar-session");

        chat.cancel(&session_id).await.unwrap();

        assert_eq!(
            state.lock().unwrap().cancel_sessions,
            vec!["sidebar-session".to_string()]
        );
    }

    #[tokio::test]
    async fn handle_permission_request_emits_event_with_option_ids_and_labels() {
        let (chat, _state) = chat_with_fake();
        let (reply_tx, _reply_rx) = oneshot::channel();
        let request = PermissionRequest {
            args: RequestPermissionRequest::new(
                "session-1",
                ToolCallUpdate::new(
                    "perm-1",
                    ToolCallUpdateFields::new().title("Allow file edit?".to_string()),
                ),
                vec![
                    PermissionOption::new("allow", "Allow", PermissionOptionKind::AllowOnce),
                    PermissionOption::new("deny", "Deny", PermissionOptionKind::RejectOnce),
                ],
            ),
            reply_tx,
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        chat.handle_permission_request(request, &tx).await.unwrap();
        let event = rx.recv().await.unwrap();

        assert_eq!(
            event,
            ChatEvent::PermissionRequest {
                id: "perm-1".into(),
                title: "Allow file edit?".into(),
                options: vec![
                    PermissionOptionView {
                        id: "allow".into(),
                        label: "Allow".into()
                    },
                    PermissionOptionView {
                        id: "deny".into(),
                        label: "Deny".into()
                    },
                ],
            }
        );
        assert_eq!(chat.pending_permission_count().await, 1);
    }

    #[tokio::test]
    async fn respond_permission_sends_stored_reply_and_clears_pending() {
        let (chat, _state) = chat_with_fake();
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = PermissionRequest {
            args: RequestPermissionRequest::new(
                "session-1",
                ToolCallUpdate::new("perm-1", ToolCallUpdateFields::new()),
                vec![PermissionOption::new(
                    "allow",
                    "Allow",
                    PermissionOptionKind::AllowOnce,
                )],
            ),
            reply_tx,
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        chat.handle_permission_request(request, &tx).await.unwrap();

        chat.respond_permission("perm-1", Some("allow".into()))
            .await
            .unwrap();

        assert_eq!(reply_rx.await.unwrap().option_id, "allow");
        assert_eq!(chat.pending_permission_count().await, 0);
    }

    #[tokio::test]
    async fn respond_permission_none_drops_stored_reply() {
        let (chat, _state) = chat_with_fake();
        let (reply_tx, reply_rx) = oneshot::channel();
        let request = PermissionRequest {
            args: RequestPermissionRequest::new(
                "session-1",
                ToolCallUpdate::new("perm-1", ToolCallUpdateFields::new()),
                vec![PermissionOption::new(
                    "deny",
                    "Deny",
                    PermissionOptionKind::RejectOnce,
                )],
            ),
            reply_tx,
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        chat.handle_permission_request(request, &tx).await.unwrap();

        chat.respond_permission("perm-1", None).await.unwrap();

        assert!(reply_rx.await.is_err());
        assert_eq!(chat.pending_permission_count().await, 0);
    }
}
