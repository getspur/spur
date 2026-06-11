use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use agent_client_protocol::schema::{
    InitializeRequest, ListSessionsRequest, LoadSessionRequest, ProtocolVersion, SessionId,
    SessionInfo, SessionNotification,
};
use futures::Stream;
use spur_acp::connection::AgentConnection;
use spur_acp::types::PermissionResponse;
use tokio::sync::{oneshot, Mutex};

use super::types::AppScope;

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

#[cfg(test)]
mod lifecycle {
    use super::*;
    use agent_client_protocol::schema::{
        InitializeResponse, ListSessionsResponse, McpServer, NewSessionResponse, PromptRequest,
    };
    use async_trait::async_trait;
    use futures::stream;
    use spur_acp::types::AgentHealth;
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;

    #[derive(Debug, Default)]
    struct FakeState {
        initialize_protocols: Vec<ProtocolVersion>,
        new_sessions: Vec<(PathBuf, Vec<McpServer>)>,
        list_requests: Vec<ListSessionsRequest>,
        load_requests: Vec<LoadSessionRequest>,
        next_session: usize,
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
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
            Ok(Box::pin(stream::empty()))
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
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
}
