use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};

use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, ListSessionsRequest,
    ListSessionsResponse, LoadSessionRequest, McpServer, NewSessionResponse, PermissionOption,
    PermissionOptionKind, PromptRequest, ProtocolVersion, RequestPermissionRequest, SessionId,
    SessionInfo, SessionNotification, SessionUpdate, TextContent, ToolCallUpdate,
    ToolCallUpdateFields,
};
use async_trait::async_trait;
use futures::{stream, Stream};
use jute::chat_commands::chat_permission_respond;
use jute::sidebar_chat::manager::SidebarChat;
use jute::sidebar_chat::types::{AppScope, ChatEvent};
use spur_acp::connection::AgentConnection;
use spur_acp::types::{AgentHealth, PermissionRequest};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct FakeState {
    prompt_requests: Vec<PromptRequest>,
    prompt_chunks: Vec<String>,
    broadcast_chunks: Vec<String>,
    next_session: usize,
    notification_tx: Option<broadcast::Sender<SessionNotification>>,
}

#[derive(Clone, Default)]
struct FakeConn {
    state: Arc<StdMutex<FakeState>>,
}

#[async_trait]
impl AgentConnection for FakeConn {
    async fn initialize(&mut self, _: InitializeRequest) -> anyhow::Result<InitializeResponse> {
        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    async fn new_session(
        &mut self,
        _: PathBuf,
        _: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        let mut state = self.state.lock().unwrap();
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

        let notifications = state
            .prompt_chunks
            .clone()
            .into_iter()
            .map(move |chunk| message_notification(request.session_id.clone(), chunk));
        Ok(Box::pin(stream::iter(notifications)))
    }

    async fn cancel(&mut self, _: &str) -> anyhow::Result<()> {
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
        _: LoadSessionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        Ok(Box::pin(stream::empty()))
    }

    async fn list_sessions(
        &mut self,
        request: ListSessionsRequest,
    ) -> anyhow::Result<ListSessionsResponse> {
        Ok(ListSessionsResponse::new(vec![SessionInfo::new(
            "session-1",
            request
                .cwd
                .unwrap_or_else(|| PathBuf::from("/tmp/notebook")),
        )]))
    }

    fn subscribe_session_notifications(&self) -> Option<broadcast::Receiver<SessionNotification>> {
        self.state
            .lock()
            .unwrap()
            .notification_tx
            .as_ref()
            .map(|tx| tx.subscribe())
    }
}

fn chat_with_fake() -> (SidebarChat, Arc<StdMutex<FakeState>>) {
    let conn = FakeConn::default();
    let state = Arc::clone(&conn.state);
    let chat = SidebarChat::new(Arc::new(Mutex::new(conn)));
    (chat, state)
}

fn scope() -> AppScope {
    AppScope {
        cwd: PathBuf::from("/tmp/notebook"),
        mcp_servers: Vec::new(),
        skill: None,
        app_key: "notebook".to_string(),
        label: "Notebook".to_string(),
    }
}

fn message_notification(session_id: SessionId, text: impl Into<String>) -> SessionNotification {
    let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text.into())));
    SessionNotification::new(session_id, SessionUpdate::AgentMessageChunk(chunk))
}

async fn collect_until_done(mut rx: mpsc::UnboundedReceiver<ChatEvent>) -> Vec<ChatEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(2), async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            let done = matches!(event, ChatEvent::Done);
            events.push(event);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("chat turn should finish")
}

#[tokio::test]
async fn stream_backed_fake_connection_emits_ordered_chunks_then_done() {
    let (chat, state) = chat_with_fake();
    state.lock().unwrap().prompt_chunks = vec!["Hello, ".into(), "stream".into()];
    let (tx, rx) = mpsc::unbounded_channel();

    chat.turn(&scope(), "say hi", tx, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        collect_until_done(rx).await,
        vec![
            ChatEvent::MessageChunk {
                text: "Hello, ".into()
            },
            ChatEvent::MessageChunk {
                text: "stream".into()
            },
            ChatEvent::Done,
        ]
    );
}

#[tokio::test]
async fn broadcast_backed_fake_connection_with_empty_prompt_stream_emits_chunks_then_done() {
    let (chat, state) = chat_with_fake();
    let (notification_tx, _) = broadcast::channel(8);
    {
        let mut state = state.lock().unwrap();
        state.notification_tx = Some(notification_tx);
        state.broadcast_chunks = vec!["Hello, ".into(), "broadcast".into()];
    }
    let (tx, rx) = mpsc::unbounded_channel();

    chat.turn(&scope(), "say hi", tx, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        collect_until_done(rx).await,
        vec![
            ChatEvent::MessageChunk {
                text: "Hello, ".into()
            },
            ChatEvent::MessageChunk {
                text: "broadcast".into()
            },
            ChatEvent::Done,
        ]
    );
}

#[tokio::test]
async fn permission_response_resolves_pending_manager_request() {
    let _command = chat_permission_respond;
    let (chat, _) = chat_with_fake();
    let (reply_tx, reply_rx) = oneshot::channel();
    let request = PermissionRequest {
        args: RequestPermissionRequest::new(
            "session-1",
            ToolCallUpdate::new(
                "perm-1",
                ToolCallUpdateFields::new().title("Allow command?".to_string()),
            ),
            vec![PermissionOption::new(
                "allow",
                "Allow",
                PermissionOptionKind::AllowOnce,
            )],
        ),
        reply_tx,
    };
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    chat.handle_permission_request(request, &event_tx)
        .await
        .unwrap();
    assert!(matches!(
        event_rx.recv().await.unwrap(),
        ChatEvent::PermissionRequest { id, .. } if id == "perm-1"
    ));

    chat.respond_permission("perm-1", Some("allow".to_string()))
        .await
        .unwrap();

    assert_eq!(reply_rx.await.unwrap().option_id, "allow");
    assert_eq!(chat.pending_permission_count().await, 0);
}
