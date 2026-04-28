//! `new_session_with_bypass` / `load_session_with_bypass` — wrap the
//! ACP `new_session` / `load_session` calls with the optional
//! `set_session_mode("bypassPermissions")` call that L1b of the
//! skip-permissions design requires.
//!
//! Keeps `AgentConfig`-aware logic out of `spur-acp` (which must stay
//! agent-agnostic). Callers in `orchestrator.rs` use these helpers
//! whenever they have an `AgentConfig` in scope.

use std::path::PathBuf;
use std::pin::Pin;

use agent_client_protocol::schema::{
    LoadSessionRequest, McpServer, NewSessionResponse, SessionId, SessionModeId, SessionModeState,
    SessionNotification, SetSessionModeRequest,
};
use futures::Stream;
use spur_acp::config::AgentConfig;
use spur_acp::connection::AgentConnection;

/// Apply `set_session_mode` on `session_id` when the agent's effective
/// permissions request bypass (`cfg.effective_permissions().skip == true`)
/// and a mode is configured. Non-fatal: errors are logged at
/// `warn!` and swallowed — L2 auto-approve is the fallback, so a
/// non-honoring agent still bypasses permissions.
///
/// `phase` is a short label appearing in the log record so fresh and
/// resumed sessions are distinguishable in production traces.
async fn apply_bypass_session_mode(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    session_id: SessionId,
    advertised_modes: Option<Vec<SessionModeId>>,
    phase: &'static str,
) {
    let perms = cfg.effective_permissions();
    if !perms.skip {
        return;
    }
    let Some(mode) = perms.session_mode.as_deref() else {
        return;
    };

    let sid_for_log = session_id.0.to_string();
    let Some(advertised_modes) = advertised_modes else {
        tracing::debug!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            phase,
            "skip_permissions: set_session_mode skipped; no advertised session modes"
        );
        return;
    };
    if !advertised_modes
        .iter()
        .any(|advertised| advertised.0.as_ref() == mode)
    {
        tracing::debug!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            phase,
            advertised_modes = ?advertised_modes,
            "skip_permissions: set_session_mode skipped; mode not advertised"
        );
        return;
    }

    let req = SetSessionModeRequest::new(session_id, mode.to_string());

    if let Err(e) = conn.set_session_mode(req).await {
        tracing::warn!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            phase,
            error = %e,
            "skip_permissions: set_session_mode failed; relying on L2 auto-approve"
        );
    } else {
        tracing::debug!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            phase,
            "skip_permissions: set_session_mode applied"
        );
    }
}

/// Call `conn.new_session(cwd, mcp)`, then apply the L1b bypass mode
/// via [`apply_bypass_session_mode`] if the agent config requests it.
///
/// Errors from `new_session` propagate. Errors from `set_session_mode`
/// are non-fatal (see [`apply_bypass_session_mode`]).
pub async fn new_session_with_bypass(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> anyhow::Result<NewSessionResponse> {
    let resp = conn.new_session(cwd, mcp_servers).await?;
    let advertised_modes = advertised_mode_ids(resp.modes.as_ref());
    apply_bypass_session_mode(
        conn,
        cfg,
        resp.session_id.clone(),
        advertised_modes,
        "new_session",
    )
    .await;
    Ok(resp)
}

/// Call `conn.load_session(request)`, then apply the L1b bypass mode
/// via [`apply_bypass_session_mode`] if the agent config requests it.
///
/// Mirror of [`new_session_with_bypass`] for the brain-resume path.
/// Without this, resumed sessions would run in the default mode and
/// rely on L2 auto-approve as the only bypass — functionally correct
/// but logs diverge from fresh sessions.
pub async fn load_session_with_bypass(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    acp_session_id: String,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
    let session_id = SessionId::new(acp_session_id);
    let request = LoadSessionRequest::new(session_id.clone(), cwd).mcp_servers(mcp_servers);
    let stream = conn.load_session(request).await?;
    let advertised_modes = conn.advertised_session_modes(&session_id);
    apply_bypass_session_mode(conn, cfg, session_id, advertised_modes, "load_session").await;
    Ok(stream)
}

fn advertised_mode_ids(modes: Option<&SessionModeState>) -> Option<Vec<SessionModeId>> {
    modes.map(|modes| {
        modes
            .available_modes
            .iter()
            .map(|mode| mode.id.clone())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use agent_client_protocol::schema::{
        InitializeRequest, InitializeResponse, PromptRequest, SessionMode, SessionModeId,
        SessionModeState, SetSessionModeResponse,
    };
    use async_trait::async_trait;
    use spur_acp::types::{AgentHealth, AgentKind, AgentRole, CostTier, TransportKind};

    use super::*;

    #[derive(Default)]
    struct MockConn {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        new_session_modes: Option<SessionModeState>,
    }

    #[async_trait]
    impl AgentConnection for MockConn {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            unimplemented!()
        }

        async fn new_session(
            &mut self,
            cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<NewSessionResponse> {
            self.calls
                .lock()
                .unwrap()
                .push(("new_session".into(), cwd.display().to_string()));
            let mut response = NewSessionResponse::new(SessionId::new("mock-session"));
            if let Some(modes) = self.new_session_modes.clone() {
                response = response.modes(modes);
            }
            Ok(response)
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
            unimplemented!()
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
            unimplemented!()
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }

        async fn set_session_mode(
            &mut self,
            request: SetSessionModeRequest,
        ) -> anyhow::Result<SetSessionModeResponse> {
            self.calls
                .lock()
                .unwrap()
                .push(("set_session_mode".into(), request.mode_id.0.to_string()));
            Ok(SetSessionModeResponse::new())
        }
    }

    fn cfg(mode: &str) -> AgentConfig {
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
            skip_permissions: true,
            skip_permissions_args: vec![],
            skip_permissions_session_mode: Some(mode.into()),
            delegation: Default::default(),
        }
    }

    fn modes(ids: &[&str]) -> SessionModeState {
        let current = ids.first().copied().unwrap_or("default");
        SessionModeState::new(
            SessionModeId::new(current),
            ids.iter()
                .map(|id| SessionMode::new(SessionModeId::new(*id), *id))
                .collect(),
        )
    }

    #[tokio::test]
    async fn skips_set_session_mode_when_requested_mode_not_advertised() {
        let mut conn = MockConn {
            calls: Arc::default(),
            new_session_modes: Some(modes(&["default"])),
        };
        let calls = conn.calls.clone();

        new_session_with_bypass(
            &mut conn,
            &cfg("bypassPermissions"),
            PathBuf::from("/cwd"),
            vec![],
        )
        .await
        .expect("new_session should succeed");

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded, vec![("new_session".into(), "/cwd".into())]);
    }

    #[tokio::test]
    async fn skips_set_session_mode_when_no_advertisement() {
        // Agent did not advertise any session modes (NewSessionResponse.modes
        // is None). Conservative gate must skip dispatch — sending a mode the
        // agent never advertised produces a -32602 Invalid params error.
        let mut conn = MockConn {
            calls: Arc::default(),
            new_session_modes: None,
        };
        let calls = conn.calls.clone();

        new_session_with_bypass(
            &mut conn,
            &cfg("bypassPermissions"),
            PathBuf::from("/cwd"),
            vec![],
        )
        .await
        .expect("new_session should succeed");

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded, vec![("new_session".into(), "/cwd".into())]);
    }

    #[tokio::test]
    async fn applies_set_session_mode_when_requested_mode_advertised() {
        let mut conn = MockConn {
            calls: Arc::default(),
            new_session_modes: Some(modes(&["default", "bypassPermissions"])),
        };
        let calls = conn.calls.clone();

        new_session_with_bypass(
            &mut conn,
            &cfg("bypassPermissions"),
            PathBuf::from("/cwd"),
            vec![],
        )
        .await
        .expect("new_session should succeed");

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                ("new_session".into(), "/cwd".into()),
                ("set_session_mode".into(), "bypassPermissions".into()),
            ]
        );
    }
}
