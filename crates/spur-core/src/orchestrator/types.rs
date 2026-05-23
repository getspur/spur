use std::sync::Arc;

use agent_client_protocol::schema::{InitializeResponse, ProtocolVersion, SessionConfigOption};
use spur_acp::connection::AgentConnection;
use spur_acp::session_lock::SessionAttachGuard;
use spur_acp::{SessionId, SessionInfoCache, SpurAgentCaps};
use spur_mcp::McpCallbackServer;
use tokio::task::JoinHandle;
use tokio_util::task::AbortOnDropHandle;

/// Options for `spur run`.
pub struct RunOpts {
    /// Override brain agent name.
    pub brain: Option<String>,
    /// Issue reference (e.g., "github:owner/repo#42").
    pub issue: Option<String>,
    /// Run in background (detached).
    pub background: bool,
}

/// Result of a completed run.
pub struct RunResult {
    pub session_id: SessionId,
    pub success: bool,
    pub pr_url: Option<String>,
    pub total_cost_usd: f64,
}

/// Holds the active brain transport along with metadata that must
/// share its lifetime. Future fields (e.g. SessionAttachGuard) are
/// added here so they cannot accidentally outlive the connection.
pub struct ActiveConnection {
    pub transport: Box<dyn AgentConnection>,
    pub brain_name: String,
    /// `None` only when no ACP session has been attached yet or when attached
    /// under DegradedNoLock (NFS/sshfs).
    pub(crate) attach_guard: Option<SessionAttachGuard>,
    /// True when this attachment is unprotected (multi-window unsafe).
    pub(crate) fs_unsafe: bool,
    /// Captured at `initialize`. Held alongside the transport so the
    /// orchestrator can build `SpurAgentCaps` once `session/new` (or
    /// `session/load`) returns the per-session state. Spec §6.1.
    pub(crate) init_response: InitializeResponse,
}

/// Holds the state of an active brain session.
pub struct BrainSession {
    pub connection: Box<dyn AgentConnection>,
    pub acp_session_id: String,
    pub spur_session_id: SessionId,
    pub notebook_socket_nonce: String,
    pub brain_name: String,
    pub delegation_handle: JoinHandle<()>,
    /// Phase 5: hold the server itself so retirement can invoke
    /// `mark_retiring` / `cancel_in_flight_workers` / `shutdown`.
    pub mcp_server: Option<Arc<McpCallbackServer>>,
    /// Abort-on-drop guard returned by `McpCallbackServer::start`.
    /// Awaited during retirement after the server has been shut down or
    /// force-aborted so the background watcher task does not linger.
    pub mcp_guard: Option<AbortOnDropHandle<()>>,
    /// Task that drains the connection's session-notification broadcast
    /// and republishes each item onto the `SpurEvent` bus. `None` for
    /// transports that return `None` from `subscribe_session_notifications`
    /// (stdio, cli_wrap, stream_json). Must be aborted whenever the
    /// session is retired — otherwise a pump subscribed against the
    /// reused connection keeps emitting events tagged with this
    /// (now-stale) `spur_session_id`.
    pub notification_pump_handle: Option<JoinHandle<()>>,
    /// Holds the attach lock while the transport lives on this active session.
    /// Moves back to `ActiveConnection` when the transport is cached.
    pub(crate) attach_guard: Option<SessionAttachGuard>,
    /// Mirrors `ActiveConnection.fs_unsafe` for the active transport.
    pub(crate) fs_unsafe: bool,
    /// Wall-clock instant this session was created. Used by
    /// `retire_active_brain` to record session duration in the cost
    /// ledger on close-out.
    pub started_at: std::time::Instant,
    /// Latest `config_options` advertised by the agent. Populated from
    /// `NewSessionResponse.config_options` on session creation; refreshed
    /// by `SetSessionConfigOption` responses (Task 2.14) and by
    /// `session/update.ConfigOptionUpdate` notifications (v2 plan).
    pub config_options: Vec<SessionConfigOption>,
    /// Frozen-per-session capability cache (M8.A). Populated AFTER both
    /// `initialize` and `session/new` complete, since the `set_*` gates
    /// derive from `NewSessionResponse` payload state. Wrapped in `Arc`
    /// so UI consumers can clone cheaply.
    pub spur_agent_caps: Option<Arc<SpurAgentCaps>>,
    /// Last-known `SessionInfoUpdate` payload (M9 hoist, F-3-1). Lives
    /// on the orchestrator entry — not the transient
    /// `SessionDetailView` — so the cached `title` and `updated_at`
    /// survive the view's destruction on navigation away from the
    /// session detail screen. `None` until the agent emits its first
    /// `SessionInfoUpdate` notification.
    pub session_info: Option<SessionInfoCache>,
    /// Captured `InitializeResponse` retained on the session entry so
    /// it can flow back to `ActiveConnection` when the brain is
    /// retired (and reused later for a fresh `new_session` without
    /// re-running `initialize`).
    pub(crate) init_response: InitializeResponse,
}

impl BrainSession {
    /// Test-only constructor that fills the private `attach_guard`,
    /// `fs_unsafe`, and `init_response` fields with sensible defaults so
    /// integration tests in sibling crates can construct a
    /// `BrainSession` without re-implementing the full session-create
    /// pipeline. Hidden from rustdoc; not part of the stable API.
    #[doc(hidden)]
    pub fn for_test(
        connection: Box<dyn AgentConnection>,
        acp_session_id: impl Into<String>,
        spur_session_id: SessionId,
        brain_name: impl Into<String>,
    ) -> Self {
        Self {
            connection,
            acp_session_id: acp_session_id.into(),
            spur_session_id,
            notebook_socket_nonce: String::new(),
            brain_name: brain_name.into(),
            delegation_handle: tokio::spawn(async {}),
            mcp_server: None,
            mcp_guard: None,
            notification_pump_handle: None,
            attach_guard: None,
            fs_unsafe: false,
            started_at: std::time::Instant::now(),
            config_options: Vec::new(),
            spur_agent_caps: None,
            session_info: None,
            init_response: InitializeResponse::new(ProtocolVersion::LATEST),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadBrainSessionError {
    #[error("session {acp_id} is already attached")]
    AlreadyAttached {
        acp_id: String,
        holder: spur_acp::session_lock::HolderInfo,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ReconnectError {
    #[error("session already attached")]
    AlreadyAttached {
        acp_id: String,
        holder: spur_acp::session_lock::HolderInfo,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(any(test, feature = "fault-injection"))]
#[derive(Default, Debug, Clone)]
pub struct FaultInjectionHooks {
    pub panic_after_overlay_apply: Option<String>,
}

#[cfg(not(any(test, feature = "fault-injection")))]
#[derive(Default, Debug, Clone)]
pub struct FaultInjectionHooks {
    _private: (),
}

impl FaultInjectionHooks {
    #[inline]
    pub(super) fn maybe_panic_after_overlay_apply(&self) {
        #[cfg(any(test, feature = "fault-injection"))]
        if let Some(message) = &self.panic_after_overlay_apply {
            panic!("fault injection: {message}");
        }
    }
}
