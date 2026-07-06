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

use futures::Stream;
use spur_acp::config::AgentConfig;
use spur_acp::connection::{AcpSessionModeSnapshot, AgentConnection};
use spur_acp::{
    AcpError, AcpSessionId, LoadSessionRequest, LoadSessionResponse, McpServer, NewSessionResponse,
    ResumeSessionRequest, ResumeSessionResponse, SessionModeId, SessionNotification,
    SetSessionModeRequest,
};

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
    session_id: AcpSessionId,
    mode_snapshot: Option<AcpSessionModeSnapshot>,
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
    let advertised_modes = mode_snapshot
        .as_ref()
        .map(|snapshot| snapshot.available_modes.clone());
    let acp_current_mode = mode_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.current_mode_id.as_ref())
        .map(|mode| mode.0.to_string());
    let Some(advertised_modes) = advertised_modes else {
        tracing::debug!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            requested_mode = %mode,
            advertised_modes = ?Option::<Vec<SessionModeId>>::None,
            acp_current_mode = ?acp_current_mode,
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
            requested_mode = %mode,
            phase,
            advertised_modes = ?advertised_modes,
            acp_current_mode = ?acp_current_mode,
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
            requested_mode = %mode,
            phase,
            advertised_modes = ?advertised_modes,
            acp_current_mode = ?acp_current_mode,
            error = %e,
            "skip_permissions: set_session_mode failed; relying on L2 auto-approve"
        );
    } else {
        tracing::debug!(
            agent = %cfg.name,
            session_id = %sid_for_log,
            mode_id = %mode,
            requested_mode = %mode,
            phase,
            advertised_modes = ?advertised_modes,
            acp_current_mode = ?acp_current_mode,
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
    let session_id = resp.session_id.clone();
    let mode_snapshot = conn.session_mode_snapshot(&session_id);
    apply_bypass_session_mode(conn, cfg, session_id, mode_snapshot, "new_session").await;
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
) -> anyhow::Result<(
    LoadSessionResponse,
    Pin<Box<dyn Stream<Item = SessionNotification> + Send>>,
)> {
    let session_id = AcpSessionId::new(acp_session_id);
    let request = LoadSessionRequest::new(session_id.clone(), cwd).mcp_servers(mcp_servers);
    let (response, stream) = conn.load_session(request).await?;
    let mode_snapshot = conn.session_mode_snapshot(&session_id);
    apply_bypass_session_mode(conn, cfg, session_id, mode_snapshot, "load_session").await;
    Ok((response, stream))
}

/// Call `conn.resume_session(request)`, then best-effort apply the L1b bypass
/// mode if this connection already knows the session's advertised modes.
pub async fn resume_session_with_bypass(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    acp_session_id: String,
    cwd: PathBuf,
) -> Result<ResumeSessionResponse, AcpError> {
    let session_id = AcpSessionId::new(acp_session_id);
    let request = ResumeSessionRequest::new(session_id.clone(), cwd);
    let response = conn.resume_session(request).await?;
    let mode_snapshot = conn.session_mode_snapshot(&session_id);
    apply_bypass_session_mode(conn, cfg, session_id, mode_snapshot, "resume_session").await;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use spur_acp::types::AgentHealth;
    use spur_acp::{
        AcpSessionId, InitializeRequest, InitializeResponse, PromptRequest, SessionModeId,
        SetSessionModeResponse,
    };

    use super::*;

    #[derive(Default)]
    struct MockConn {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        advertised_modes: Option<Vec<SessionModeId>>,
    }

    #[async_trait]
    impl AgentConnection for MockConn {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            panic!("MockConn::initialize must not be called")
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
            Ok(NewSessionResponse::new(spur_acp::AcpSessionId::new(
                "mock-session",
            )))
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
            panic!("MockConn::prompt must not be called")
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
            panic!("MockConn::cancel must not be called")
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }

        fn advertised_session_modes(
            &self,
            session_id: &AcpSessionId,
        ) -> Option<Vec<SessionModeId>> {
            self.calls
                .lock()
                .unwrap()
                .push(("advertised_session_modes".into(), session_id.0.to_string()));
            self.advertised_modes.clone()
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

    fn cfg(skip: bool, mode: Option<&str>) -> AgentConfig {
        let mut cfg = AgentConfig::with_defaults("mock");
        cfg.skip_permissions = skip;
        cfg.skip_permissions_session_mode = mode.map(str::to_owned);
        cfg
    }

    #[tokio::test]
    async fn skips_set_session_mode_when_requested_mode_not_advertised() {
        let mut conn = MockConn {
            calls: Arc::default(),
            advertised_modes: Some(vec![SessionModeId::new("default")]),
        };
        let calls = conn.calls.clone();

        new_session_with_bypass(
            &mut conn,
            &cfg(true, Some("bypassPermissions")),
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
                ("advertised_session_modes".into(), "mock-session".into()),
            ]
        );
    }

    #[tokio::test]
    async fn skips_set_session_mode_when_no_advertisement() {
        let mut conn = MockConn {
            calls: Arc::default(),
            advertised_modes: None,
        };
        let calls = conn.calls.clone();

        new_session_with_bypass(
            &mut conn,
            &cfg(true, Some("bypassPermissions")),
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
                ("advertised_session_modes".into(), "mock-session".into()),
            ]
        );
    }

    #[tokio::test]
    async fn skips_set_session_mode_when_skip_permissions_false() {
        let mut conn = MockConn {
            calls: Arc::default(),
            advertised_modes: Some(vec![SessionModeId::new("bypassPermissions")]),
        };
        let calls = conn.calls.clone();

        new_session_with_bypass(
            &mut conn,
            &cfg(false, Some("bypassPermissions")),
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
                ("advertised_session_modes".into(), "mock-session".into()),
            ]
        );
    }

    #[tokio::test]
    async fn applies_set_session_mode_when_requested_mode_advertised() {
        let mut conn = MockConn {
            calls: Arc::default(),
            advertised_modes: Some(vec![
                SessionModeId::new("default"),
                SessionModeId::new("bypassPermissions"),
            ]),
        };
        let calls = conn.calls.clone();

        new_session_with_bypass(
            &mut conn,
            &cfg(true, Some("bypassPermissions")),
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
                ("advertised_session_modes".into(), "mock-session".into()),
                ("set_session_mode".into(), "bypassPermissions".into()),
            ]
        );
    }
}
