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

use agent_client_protocol::{
    LoadSessionRequest, McpServer, NewSessionResponse, SessionId, SessionNotification,
    SetSessionModeRequest,
};
use futures::Stream;
use spur_acp::config::AgentConfig;
use spur_acp::connection::AgentConnection;

/// Apply `set_session_mode` on `session_id` when `cfg.skip_permissions`
/// is true and a mode is configured. Non-fatal: errors are logged at
/// `warn!` and swallowed — L2 auto-approve is the fallback, so a
/// non-honoring agent still bypasses permissions.
///
/// `phase` is a short label appearing in the log record so fresh and
/// resumed sessions are distinguishable in production traces.
async fn apply_bypass_session_mode(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    session_id: SessionId,
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
    apply_bypass_session_mode(conn, cfg, resp.session_id.clone(), "new_session").await;
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
    apply_bypass_session_mode(conn, cfg, session_id, "load_session").await;
    Ok(stream)
}
