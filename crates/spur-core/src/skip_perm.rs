//! `new_session_with_bypass` — wraps `AgentConnection::new_session` with
//! the optional `set_session_mode("bypassPermissions")` call that L1b of
//! the skip-permissions design requires.
//!
//! Keeps `AgentConfig`-aware logic out of `spur-acp` (which must stay
//! agent-agnostic). Callers in `orchestrator.rs` use this instead of
//! `conn.new_session(...)` whenever they have an `AgentConfig` in scope.

use std::path::PathBuf;
use std::pin::Pin;

use agent_client_protocol::{
    LoadSessionRequest, McpServer, NewSessionResponse, SessionNotification, SetSessionModeRequest,
};
use futures::Stream;
use spur_acp::config::AgentConfig;
use spur_acp::connection::AgentConnection;

/// Call `conn.new_session(cwd, mcp)`. If `cfg.skip_permissions` is true
/// and `cfg.skip_permissions_session_mode` is set, then additionally
/// invoke `conn.set_session_mode(...)` with that mode on the freshly
/// created session id.
///
/// Errors from `new_session` propagate. Errors from `set_session_mode`
/// are logged at `warn!` and swallowed — L2 auto-approve is the
/// fallback, so a non-honoring agent still bypasses permissions.
pub async fn new_session_with_bypass(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> anyhow::Result<NewSessionResponse> {
    let resp = conn.new_session(cwd, mcp_servers).await?;

    if cfg.skip_permissions {
        if let Some(mode) = cfg.skip_permissions_session_mode.as_deref() {
            // SessionModeId's From<&str> requires 'static; convert via
            // String so a runtime-provided mode name compiles.
            let req = SetSessionModeRequest::new(resp.session_id.clone(), mode.to_string());
            if let Err(e) = conn.set_session_mode(req).await {
                tracing::warn!(
                    agent = %cfg.name,
                    session_id = %resp.session_id.0,
                    mode_id = %mode,
                    error = %e,
                    "skip_permissions: set_session_mode failed; \
                     relying on L2 auto-approve"
                );
            } else {
                tracing::debug!(
                    agent = %cfg.name,
                    session_id = %resp.session_id.0,
                    mode_id = %mode,
                    "skip_permissions: set_session_mode applied"
                );
            }
        }
    }

    Ok(resp)
}

/// Call `conn.load_session(request)`. If `cfg.skip_permissions` is true
/// and `cfg.skip_permissions_session_mode` is set, then additionally
/// invoke `conn.set_session_mode(...)` with that mode on the loaded
/// session id.
///
/// Mirror of `new_session_with_bypass` for the brain-resume path.
/// Without this, resumed sessions would run in the default mode and
/// rely on L2 auto-approve as the only bypass — functionally correct
/// but logs diverge from fresh sessions.
///
/// Errors from `load_session` propagate. Errors from `set_session_mode`
/// are logged at `warn!` and swallowed — L2 auto-approve is the
/// fallback.
pub async fn load_session_with_bypass(
    conn: &mut dyn AgentConnection,
    cfg: &AgentConfig,
    acp_session_id: String,
    cwd: PathBuf,
    mcp_servers: Vec<McpServer>,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
    let session_id = agent_client_protocol::SessionId::new(acp_session_id.clone());
    let request = LoadSessionRequest::new(session_id.clone(), cwd).mcp_servers(mcp_servers);
    let stream = conn.load_session(request).await?;

    if cfg.skip_permissions {
        if let Some(mode) = cfg.skip_permissions_session_mode.as_deref() {
            let req = SetSessionModeRequest::new(session_id, mode.to_string());
            if let Err(e) = conn.set_session_mode(req).await {
                tracing::warn!(
                    agent = %cfg.name,
                    session_id = %acp_session_id,
                    mode_id = %mode,
                    error = %e,
                    "skip_permissions (load_session): set_session_mode failed; \
                     relying on L2 auto-approve"
                );
            } else {
                tracing::debug!(
                    agent = %cfg.name,
                    session_id = %acp_session_id,
                    mode_id = %mode,
                    "skip_permissions (load_session): set_session_mode applied"
                );
            }
        }
    }

    Ok(stream)
}
