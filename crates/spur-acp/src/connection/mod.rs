//! The `AgentConnection` trait defines how SPUR talks to any ACP-compatible agent.
//!
//! All parameters and return types use official ACP SDK types from
//! `agent_client_protocol`. Implementations bridge from whatever their
//! underlying transport is (native ACP stdio, HTTP adapter, CLI wrapper, etc.)
//! into this unified async interface.
//!
//! Unlike the SDK's own `Client` trait (which is `#[async_trait(?Send)]`),
//! `AgentConnection` is `Send + Sync` so it can be held in an `Arc` and shared
//! across Tokio tasks.

pub mod child_stderr_bridge;

pub mod cli_wrap_adapter;
pub use cli_wrap_adapter::CliWrapAdapter;

pub mod native;
pub use native::NativeAcpConnection;

pub mod stdio_adapter;
pub use stdio_adapter::StdioAdapter;

pub mod stream_json_adapter;
pub use stream_json_adapter::StreamJsonAdapter;

pub use tokio::sync::broadcast;

use std::path::PathBuf;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use agent_client_protocol::schema::{
    AuthenticateRequest, AuthenticateResponse, InitializeRequest, InitializeResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, McpServer, ModelId,
    NewSessionResponse, PromptRequest, SessionId, SessionNotification,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, SetSessionModeRequest,
    SetSessionModeResponse,
};

use crate::error::AcpError;
use crate::spur_agent_caps::SpurAgentCaps;
use crate::types::AgentHealth;

/// A transport-agnostic connection to a single ACP agent.
///
/// Each implementation (native ACP over stdio, HTTP/SSE adapter, CLI wrapper)
/// is responsible for translating between its own I/O model and this trait's
/// async, streaming interface.
///
/// # Streaming model
///
/// `prompt()` returns a `Stream<Item = SessionNotification>` instead of a
/// single `PromptResponse`. Implementations that talk native ACP receive
/// session updates via the `Client::session_notification()` callback and
/// bridge them into the returned stream. Adapter implementations may produce
/// the stream from whatever raw I/O they use (e.g. parsing line-delimited
/// JSON from a subprocess stdout).
///
/// The final item in the stream is expected to carry a `SessionUpdate` that
/// signals completion (e.g. `SessionUpdate::Completed`).
///
/// # Lifecycle
///
/// 1. `initialize()` -- negotiate protocol version and capabilities.
/// 2. `new_session()` -- create a working session (with cwd + MCP servers).
/// 3. `prompt()` -- send messages and stream back notifications.
/// 4. `cancel()` -- cancel an in-flight prompt for a session.
/// 5. `shutdown()` -- gracefully tear down the connection.
///
/// `health()` may be called at any point (including before `initialize()`).
#[async_trait]
pub trait AgentConnection: Send + Sync {
    /// Negotiate protocol version and exchange capabilities with the agent.
    ///
    /// This must be the first method called on a new connection.
    async fn initialize(
        &mut self,
        request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse>;

    /// Create a new session on the agent.
    ///
    /// Per ACP spec, MCP servers are provided at session creation time (not
    /// during initialize). The `cwd` sets the working directory for the session.
    async fn new_session(
        &mut self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse>;

    /// Send a prompt to the agent and receive a stream of session notifications.
    ///
    /// The returned stream carries `SessionNotification` items as the agent
    /// processes the prompt. Implementations that route notifications through
    /// their underlying update mechanism (raw I/O, line-based protocols)
    /// deliver them here.
    ///
    /// **Hybrid delivery model:** implementations that also override
    /// [`AgentConnection::subscribe_session_notifications`] (currently
    /// `NativeAcpConnection`) publish notifications through a
    /// connection-scoped `broadcast` channel instead and return an **empty**
    /// stream here. The stream still closes when the prompt turn completes,
    /// so it remains useful as a turn-completion signal, but it carries no
    /// notification payload. New callers SHOULD prefer the subscriber:
    /// subscribe *before* issuing the prompt to avoid missing early
    /// notifications. Existing stream-based callers keep working for
    /// transports that do not publish via broadcast (stdio, cli_wrap,
    /// stream_json). See `docs/superpowers/specs/2026-04-14-acp-notification-bus-design.md`.
    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>>;

    /// Cancel an in-flight prompt for the given session.
    ///
    /// This is a best-effort notification; the agent may or may not honor it.
    async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()>;

    /// Gracefully shut down the connection and release any held resources.
    async fn shutdown(&mut self) -> anyhow::Result<()>;

    /// Return the current health status of the agent behind this connection.
    ///
    /// This is a synchronous, non-fallible query -- implementations should
    /// cache the last-known health and return it immediately.
    fn health(&self) -> AgentHealth;

    /// Load an existing session by ID, returning a stream of historical notifications.
    ///
    /// Not all transports support this; the default implementation returns
    /// an error. Like [`prompt`](Self::prompt), implementations that also
    /// override [`subscribe_session_notifications`](Self::subscribe_session_notifications)
    /// publish replayed history through the broadcast and return an empty
    /// stream here — subscribe *before* calling `load_session` to capture
    /// replay items.
    async fn load_session(
        &mut self,
        request: LoadSessionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        let _ = request;
        Err(anyhow::anyhow!(
            "load_session not supported by this transport"
        ))
    }

    /// List all sessions known to the agent.
    ///
    /// Not all transports support this; the default implementation returns an error.
    async fn list_sessions(
        &mut self,
        request: ListSessionsRequest,
    ) -> anyhow::Result<ListSessionsResponse> {
        let _ = request;
        Err(anyhow::anyhow!(
            "list_sessions not supported by this transport"
        ))
    }

    /// Set the current mode of a session (e.g. `"plan"`, `"default"`).
    ///
    /// Not all transports support this; the default implementation returns an error.
    async fn set_session_mode(
        &mut self,
        request: SetSessionModeRequest,
    ) -> anyhow::Result<SetSessionModeResponse> {
        let _ = request;
        Err(anyhow::anyhow!(
            "set_session_mode not supported by this transport"
        ))
    }

    /// Issue ACP `session/set_config_option`. Returns the agent's updated
    /// `Vec<SessionConfigOption>` so callers can refresh their cache.
    ///
    /// Not all transports support this; the default implementation returns an error.
    async fn set_session_config_option(
        &mut self,
        request: SetSessionConfigOptionRequest,
    ) -> anyhow::Result<SetSessionConfigOptionResponse> {
        let _ = request;
        Err(anyhow::anyhow!(
            "set_session_config_option not supported by this transport"
        ))
    }

    /// Issue ACP `session/set_model` (capability-gated, with state-derived
    /// fallback to `set_session_config_option`). Spec §6.3.
    ///
    /// `caps` is read once to choose between the dedicated `set_model`
    /// method, the `set_config_option` fallback, or `CapabilityMissing`.
    /// The default implementation returns `CapabilityMissing` — transports
    /// that talk native ACP (currently `NativeAcpConnection`) override
    /// this with the real dispatch decision.
    async fn set_session_model(
        &mut self,
        sid: SessionId,
        model_id: ModelId,
        caps: &SpurAgentCaps,
    ) -> Result<(), AcpError> {
        let _ = (sid, model_id, caps);
        Err(AcpError::CapabilityMissing("set_model"))
    }

    /// Authenticate with the agent using a previously-advertised auth method.
    ///
    /// Not all transports support this; the default implementation returns an error.
    async fn authenticate(
        &mut self,
        request: AuthenticateRequest,
    ) -> anyhow::Result<AuthenticateResponse> {
        let _ = request;
        Err(anyhow::anyhow!(
            "authenticate not supported by this transport"
        ))
    }

    /// Invoke a vendor-extension method (`_foo.dev/bar/baz`) on the agent.
    ///
    /// `method` is the wire method name including the leading `_` (e.g.
    /// `"_kiro.dev/commands/execute"`). Transports that wrap the ACP SDK's
    /// `ClientSideConnection::ext_method` must strip the leading `_` before
    /// constructing the SDK request (the SDK re-adds it).
    ///
    /// Not all transports support this; the default implementation returns
    /// an error.
    async fn call_ext(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let _ = (method, params);
        Err(anyhow::anyhow!("call_ext not supported by this transport"))
    }

    /// Take ownership of the receiver that drains vendor-extension
    /// notifications pushed by the agent (`_foo.dev/bar/baz`).
    ///
    /// Implementations that route ext notifications through a channel
    /// should return the receiver here exactly once. The default
    /// implementation returns `None`, meaning no ext notifications will
    /// be delivered for this transport.
    fn take_ext_notification_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<ExtNotificationPayload>> {
        None
    }

    /// Subscribe to the connection-scoped broadcast of `SessionNotification`s.
    ///
    /// Implementations that publish notifications through a long-lived
    /// broadcast channel (only `NativeAcpConnection` at time of writing)
    /// return `Some(receiver)` here. The orchestrator spawns a pump task
    /// that converts every published notification into a
    /// `SpurEventBody::AgentNotification` tagged with the brain/worker's
    /// `spur_session_id`.
    ///
    /// Transports that stay on the per-call `Stream` API return `None`
    /// (default) — the orchestrator falls back to draining the stream
    /// handed back by `prompt()` / `load_session()`.
    fn subscribe_session_notifications(&self) -> Option<broadcast::Receiver<SessionNotification>> {
        None
    }
}

/// A vendor-extension notification pulled off the wire.
///
/// `method` is the wire method name including the leading `_` (e.g.
/// `"_kiro.dev/commands/available"`). `params` is the raw JSON payload.
#[derive(Debug, Clone)]
pub struct ExtNotificationPayload {
    pub method: String,
    pub params: serde_json::Value,
}

/// Test-only `AgentConnection` impl that panics on all I/O. Useful for
/// constructing a `BrainSession` in integration tests that exercise
/// orchestrator-side cache plumbing without spawning any subprocess.
///
/// Hidden from rustdoc; not part of the stable API.
#[doc(hidden)]
pub struct TestStubConnection;

#[async_trait]
impl AgentConnection for TestStubConnection {
    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        unimplemented!("TestStubConnection: initialize")
    }
    async fn new_session(
        &mut self,
        _cwd: PathBuf,
        _mcp: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        unimplemented!("TestStubConnection: new_session")
    }
    async fn prompt(
        &mut self,
        _request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
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
}

#[cfg(test)]
mod agent_connection_defaults {
    use super::*;
    use agent_client_protocol::schema::{
        AuthMethodId, AuthenticateRequest, SessionId, SetSessionModeRequest,
    };

    struct NullConn;

    #[async_trait]
    impl AgentConnection for NullConn {
        async fn initialize(
            &mut self,
            _r: InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            unimplemented!()
        }
        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp: Vec<McpServer>,
        ) -> anyhow::Result<NewSessionResponse> {
            unimplemented!()
        }
        async fn prompt(
            &mut self,
            _r: PromptRequest,
        ) -> anyhow::Result<std::pin::Pin<Box<dyn Stream<Item = SessionNotification> + Send>>>
        {
            unimplemented!()
        }
        async fn cancel(&mut self, _s: &str) -> anyhow::Result<()> {
            unimplemented!()
        }
        async fn shutdown(&mut self) -> anyhow::Result<()> {
            unimplemented!()
        }
        fn health(&self) -> crate::types::AgentHealth {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn set_session_mode_default_is_unsupported() {
        let mut c = NullConn;
        let req = SetSessionModeRequest::new(SessionId::new("s".to_string()), "plan");
        let err = c.set_session_mode(req).await.unwrap_err().to_string();
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[tokio::test]
    async fn authenticate_default_is_unsupported() {
        let mut c = NullConn;
        let req = AuthenticateRequest::new(AuthMethodId::new("x"));
        let err = c.authenticate(req).await.unwrap_err().to_string();
        assert!(err.contains("not supported"), "got: {err}");
    }
}
