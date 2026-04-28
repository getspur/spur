//! Tests for `new_session_with_bypass` — the helper that wraps
//! `AgentConnection::new_session` with an optional post-session
//! `set_session_mode` call driven by the agent's config.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::{
    InitializeRequest, InitializeResponse, LoadSessionRequest, McpServer, NewSessionResponse,
    PromptRequest, SessionId, SessionMode, SessionModeId, SessionModeState, SessionNotification,
    SetSessionModeRequest, SetSessionModeResponse,
};
use async_trait::async_trait;
use futures::Stream;
use spur_acp::config::AgentConfig;
use spur_acp::connection::AgentConnection;
use spur_acp::types::{AgentHealth, AgentKind, AgentRole, CostTier, TransportKind};
use spur_core::skip_perm::{load_session_with_bypass, new_session_with_bypass};

#[derive(Default)]
struct MockConn {
    /// Records every method call in order, as
    /// `("new_session", "<cwd>")` or `("set_session_mode", "<mode>")`.
    calls: Arc<Mutex<Vec<(String, String)>>>,
    /// If set, `set_session_mode` returns this error instead of Ok.
    fail_set_session_mode: bool,
    advertised_modes: Option<Vec<SessionModeId>>,
}

#[async_trait]
impl AgentConnection for MockConn {
    async fn initialize(&mut self, _r: InitializeRequest) -> anyhow::Result<InitializeResponse> {
        unimplemented!()
    }

    async fn new_session(
        &mut self,
        cwd: PathBuf,
        _mcp: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        self.calls
            .lock()
            .unwrap()
            .push(("new_session".into(), cwd.display().to_string()));
        let mut response = NewSessionResponse::new(SessionId::new("mock-session".to_string()));
        if let Some(modes) = self.advertised_modes.clone() {
            response = response.modes(mode_state(modes));
        }
        Ok(response)
    }

    async fn prompt(
        &mut self,
        _r: PromptRequest,
    ) -> anyhow::Result<std::pin::Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        unimplemented!()
    }

    async fn cancel(&mut self, _s: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn shutdown(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn health(&self) -> AgentHealth {
        AgentHealth::Ready
    }

    fn advertised_session_modes(&self, _session_id: &SessionId) -> Option<Vec<SessionModeId>> {
        self.advertised_modes.clone()
    }

    async fn set_session_mode(
        &mut self,
        req: SetSessionModeRequest,
    ) -> anyhow::Result<SetSessionModeResponse> {
        // mode_id is a SessionModeId — access its inner Arc<str>.
        self.calls
            .lock()
            .unwrap()
            .push(("set_session_mode".into(), req.mode_id.0.to_string()));
        if self.fail_set_session_mode {
            Err(anyhow::anyhow!("mock rejects mode"))
        } else {
            Ok(SetSessionModeResponse::new())
        }
    }

    async fn load_session(
        &mut self,
        req: LoadSessionRequest,
    ) -> anyhow::Result<std::pin::Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        self.calls
            .lock()
            .unwrap()
            .push(("load_session".into(), req.session_id.0.to_string()));
        Ok(Box::pin(futures::stream::empty()))
    }
}

fn mode_state(mode_ids: Vec<SessionModeId>) -> SessionModeState {
    let current = mode_ids
        .first()
        .cloned()
        .unwrap_or_else(|| SessionModeId::new("default"));
    SessionModeState::new(
        current,
        mode_ids
            .into_iter()
            .map(|id| {
                let name = id.0.to_string();
                SessionMode::new(id, name)
            })
            .collect(),
    )
}

fn conn_with_modes(modes: &[&str]) -> MockConn {
    MockConn {
        advertised_modes: Some(modes.iter().map(|mode| SessionModeId::new(*mode)).collect()),
        ..Default::default()
    }
}

fn cfg(skip: bool, mode: Option<&str>) -> AgentConfig {
    AgentConfig {
        name: "mock".into(),
        command: "mock".into(),
        args: vec![],
        transport: TransportKind::Acp,
        kind: AgentKind::Generic,
        role: AgentRole::Both,
        capabilities: vec![],
        cost_tier: CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        skip_permissions: skip,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: mode.map(String::from),
        delegation: Default::default(),
    }
}

#[tokio::test]
async fn skips_set_session_mode_when_flag_off() {
    let mut conn = MockConn::default();
    let calls = conn.calls.clone();
    let cfg = cfg(false, Some("bypassPermissions"));
    new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded, vec![("new_session".into(), "/cwd".into())]);
}

#[tokio::test]
async fn skips_set_session_mode_when_mode_absent() {
    let mut conn = MockConn::default();
    let calls = conn.calls.clone();
    let cfg = cfg(true, None);
    new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded, vec![("new_session".into(), "/cwd".into())]);
}

#[tokio::test]
async fn calls_set_session_mode_when_bypass_and_mode_present() {
    let mut conn = conn_with_modes(&["bypassPermissions"]);
    let calls = conn.calls.clone();
    let cfg = cfg(true, Some("bypassPermissions"));
    new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            ("new_session".into(), "/cwd".into()),
            ("set_session_mode".into(), "bypassPermissions".into()),
        ]
    );
}

#[tokio::test]
async fn set_session_mode_error_is_non_fatal() {
    let mut conn = MockConn {
        fail_set_session_mode: true,
        advertised_modes: Some(vec![SessionModeId::new("bypassPermissions")]),
        calls: Arc::default(),
    };
    let cfg = cfg(true, Some("bypassPermissions"));
    // Must succeed even though set_session_mode fails — L2 auto-approve
    // is the fallback.
    let resp = new_session_with_bypass(&mut conn, &cfg, PathBuf::from("/cwd"), vec![])
        .await
        .expect("ok despite mode failure");
    assert_eq!(resp.session_id.0.as_ref(), "mock-session");
}

// ─── load_session_with_bypass ────────────────────────────────────────────────

#[tokio::test]
async fn load_session_skips_set_session_mode_when_flag_off() {
    let mut conn = MockConn::default();
    let calls = conn.calls.clone();
    let cfg = cfg(false, Some("bypassPermissions"));
    let _stream = load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-1".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await
    .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![("load_session".into(), "acp-sess-1".into())],
        "set_session_mode must not be called when skip_permissions = false",
    );
}

#[tokio::test]
async fn load_session_skips_set_session_mode_when_mode_absent() {
    let mut conn = MockConn::default();
    let calls = conn.calls.clone();
    let cfg = cfg(true, None);
    let _stream = load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-3".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await
    .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![("load_session".into(), "acp-sess-3".into())],
        "set_session_mode must not be called when no mode configured",
    );
}

#[tokio::test]
async fn load_session_calls_set_session_mode_when_bypass_and_mode_present() {
    let mut conn = conn_with_modes(&["bypassPermissions"]);
    let calls = conn.calls.clone();
    let cfg = cfg(true, Some("bypassPermissions"));
    let _stream = load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-2".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await
    .expect("ok");
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            ("load_session".into(), "acp-sess-2".into()),
            ("set_session_mode".into(), "bypassPermissions".into()),
        ],
    );
}

#[tokio::test]
async fn load_session_set_session_mode_error_is_non_fatal() {
    let mut conn = MockConn {
        fail_set_session_mode: true,
        advertised_modes: Some(vec![SessionModeId::new("bypassPermissions")]),
        calls: Arc::default(),
    };
    let cfg = cfg(true, Some("bypassPermissions"));
    let res = load_session_with_bypass(
        &mut conn,
        &cfg,
        "acp-sess-4".to_string(),
        PathBuf::from("/cwd"),
        vec![],
    )
    .await;
    assert!(
        res.is_ok(),
        "load_session must succeed even if set_session_mode errors"
    );
}
