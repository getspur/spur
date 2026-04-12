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

pub mod cli_wrap_adapter;
pub use cli_wrap_adapter::CliWrapAdapter;

use std::path::PathBuf;
use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use agent_client_protocol::{
    InitializeRequest, InitializeResponse, McpServer, NewSessionResponse, PromptRequest,
    SessionNotification,
};

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
    /// The returned stream yields `SessionNotification` items as the agent
    /// processes the prompt. Implementations must bridge from their underlying
    /// update mechanism (ACP callbacks, raw I/O, etc.) into this stream.
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
}
