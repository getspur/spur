// kill_on_drop audit (bd-arch.WTA Phase 0a, 2026-04-26):
// - pre-audit line 204 is the killpg helper. The helper itself does not wait; its
//   graceful-shutdown escalation caller pairs it with child.wait()/child.kill(),
//   but the Drop safety-net caller cannot reap. Phase 0a follow-up: keep this
//   distinction explicit when adding kill_on_drop(true).
// - pre-audit lines 884, 1340, and 1367 are terminal SIGKILL fallbacks. The terminal
//   Child is owned by terminal_reader, which always reaches child.wait().await
//   after stdout/stderr close, so these explicit kills are paired with reaping.
// Second SIGKILL races after kill_on_drop are benign on POSIX (ESRCH/no-op).
//! `NativeAcpConnection` — drives an ACP agent subprocess over stdio using the
//! official SDK's builder/handler API (`Client.builder()…connect_with`).
//!
//! # Architecture
//!
//! Spur's high-level orchestrator runs on a multi-threaded Tokio runtime, so its
//! channels and tasks are required to be `Send`. The ACP SDK builder, on the
//! other hand, registers handler callbacks that themselves must be `Send`, but
//! the `connect_with` "command-loop" closure is allowed to be `!Send`. We keep
//! the dedicated-OS-thread + `LocalSet` shape from the previous SDK version —
//! it gives us a single-threaded execution surface for the loop's bookkeeping
//! (e.g. small `Rc<RefCell<…>>` reply slots) without needing `Send` everywhere.
//!
//! Send-safe state (cwd, terminal map) is held in `Arc<Mutex<…>>` so handlers
//! can clone it cheaply.
//!
//! # Lifecycle mapping
//!
//! | `AgentConnection` method | Behaviour |
//! |---|---|
//! | `initialize()` | Spawn the agent subprocess, build the SDK connection, send `initialize` |
//! | `new_session()` | Send `NewSessionRequest` with cwd + MCP servers to the agent |
//! | `prompt()` | Send `PromptRequest`; `SessionNotification`s flow out via the connection-scoped broadcast |
//! | `cancel()` | Send `CancelNotification` via the connection |
//! | `shutdown()` | Close stdin (drop the SDK connection), SIGTERM the process group, then SIGKILL if needed |
//! | `health()` | Return cached `AgentHealth` |

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;

use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use tokio::sync::{mpsc, oneshot};

use agent_client_protocol::schema::{
    AuthenticateRequest, AuthenticateResponse, CancelNotification, ClientCapabilities,
    ContentBlock, ContentChunk, CreateTerminalRequest, CreateTerminalResponse, ExtRequest,
    ExtResponse, FileSystemCapabilities, InitializeRequest, InitializeResponse,
    KillTerminalRequest, KillTerminalResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, McpServer, ModelId, NewSessionRequest, NewSessionResponse,
    PermissionOptionId, PermissionOptionKind, PromptRequest, ReadTextFileRequest,
    ReadTextFileResponse, ReleaseTerminalRequest, ReleaseTerminalResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionConfigId, SessionConfigValueId, SessionId, SessionModeId,
    SessionModeState, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
    SetSessionModelRequest, SetSessionModelResponse, TerminalExitStatus, TerminalId,
    TerminalOutputRequest, TerminalOutputResponse, WaitForTerminalExitRequest,
    WaitForTerminalExitResponse, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};

use crate::config::LogConfig;
use crate::connection::child_stderr_bridge::ChildStderrBridge;
use crate::connection::{AgentConnection, ExtNotificationPayload};
use crate::error::AcpError;
use crate::spur_agent_caps::SpurAgentCaps;
use crate::types::AgentHealth;

/// Spur's canonical `ClientCapabilities` literal advertised at every
/// `initialize` call. Spec §6.2.
///
/// Declares:
/// - `fs.{read_text_file, write_text_file}` — spur honors `fs/*` requests.
/// - `terminal = true` — spur honors all `terminal/*` RPCs.
/// - `_meta.terminal_output = true` — vendor extension that unlocks
///   codex's tool-call meta tunneling (consumed in M9).
pub fn spur_client_capabilities() -> ClientCapabilities {
    let mut meta = serde_json::Map::new();
    meta.insert("terminal_output".to_string(), serde_json::Value::Bool(true));

    ClientCapabilities::new()
        .fs(FileSystemCapabilities::new()
            .read_text_file(true)
            .write_text_file(true))
        .terminal(true)
        .meta(meta)
}

#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_native_worker_for_test(
    command: &str,
    args: &[&str],
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

// ─── Commands sent to the dedicated ACP thread ──────────────────────────────

/// Commands sent from the main (Send) world to the dedicated !Send ACP thread.
enum AcpCommand {
    Initialize {
        request: InitializeRequest,
        reply: oneshot::Sender<anyhow::Result<InitializeResponse>>,
    },
    NewSession {
        request: NewSessionRequest,
        reply: oneshot::Sender<anyhow::Result<NewSessionResponse>>,
    },
    Prompt {
        request: PromptRequest,
        /// We send back a receiver that will yield SessionNotifications as
        /// they arrive via the Client callback, plus the final PromptResponse.
        reply: oneshot::Sender<anyhow::Result<mpsc::UnboundedReceiver<SessionNotification>>>,
    },
    Cancel {
        session_id: String,
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    Shutdown {
        reply: oneshot::Sender<anyhow::Result<()>>,
    },
    LoadSession {
        request: LoadSessionRequest,
        reply: oneshot::Sender<anyhow::Result<mpsc::UnboundedReceiver<SessionNotification>>>,
    },
    ListSessions {
        request: ListSessionsRequest,
        reply: oneshot::Sender<anyhow::Result<ListSessionsResponse>>,
    },
    SetSessionMode {
        request: SetSessionModeRequest,
        reply: oneshot::Sender<anyhow::Result<SetSessionModeResponse>>,
    },
    SetSessionModel {
        request: SetSessionModelRequest,
        reply: oneshot::Sender<anyhow::Result<SetSessionModelResponse>>,
    },
    SetSessionConfigOption {
        request: SetSessionConfigOptionRequest,
        reply: oneshot::Sender<anyhow::Result<SetSessionConfigOptionResponse>>,
    },
    Authenticate {
        request: AuthenticateRequest,
        reply: oneshot::Sender<anyhow::Result<AuthenticateResponse>>,
    },
    ExtMethod {
        request: ExtRequest,
        reply: oneshot::Sender<anyhow::Result<ExtResponse>>,
    },
}

// ─── NativeAcpConnection ────────────────────────────────────────────────────

/// A native ACP connection that wraps the official SDK's `ClientSideConnection`.
///
/// This is the "real" ACP implementation that spawns an agent subprocess and
/// communicates via the Agent Client Protocol over stdio.
///
/// Because the SDK requires `!Send` futures, the actual SDK connection lives on
/// a dedicated thread.  This struct is `Send + Sync` and communicates with that
/// thread via channels.
pub struct NativeAcpConnection {
    /// Human-readable agent name.
    agent_name: String,
    /// Binary to invoke.
    command: String,
    /// Extra arguments passed to the binary on startup.
    extra_args: Vec<String>,
    /// Channel to send commands to the dedicated ACP thread.
    cmd_tx: Option<mpsc::UnboundedSender<AcpCommand>>,
    /// Join handle for the dedicated thread.
    thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Cached health status.
    health_status: AgentHealth,
    /// Optional sender for interactive permission requests (forwarded to the TUI).
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
    /// Receiver for vendor-extension notifications. Filled at construction,
    /// taken once by the orchestrator via `take_ext_notification_rx`.
    ext_notification_rx: Option<mpsc::UnboundedReceiver<ExtNotificationPayload>>,
    /// Paired sender for `ext_notification_rx`, cloned into the ACP thread.
    ext_notification_tx: mpsc::UnboundedSender<ExtNotificationPayload>,
    /// Connection-scoped broadcast of session notifications. Cloned into
    /// `SpurAcpClientDynamic` (via `acp_thread_main`); subscribers obtained
    /// via `subscribe_session_notifications` live for the connection's
    /// whole lifetime — no per-turn channel swap, no grace window, no
    /// dead_tx. Capacity 1024 absorbs bursty history replay from
    /// `load_session`. Task 4 rewires `session_notification` onto this;
    /// today it's only plumbed.
    session_notif_tx: tokio::sync::broadcast::Sender<SessionNotification>,
    /// Last advertised session modes keyed by ACP session id. Populated from
    /// `NewSessionResponse` / `LoadSessionResponse` so policy code can gate
    /// `session/set_mode` without probing unsupported modes.
    advertised_modes: Arc<Mutex<HashMap<String, Vec<SessionModeId>>>>,
    /// Process-group id of the spawned child (equal to its pid because we spawn
    /// with `process_group(0)`). Populated by the ACP thread after spawn, read
    /// by the graceful shutdown path and the `Drop` safety net to kill the
    /// entire descendant tree via `killpg`.
    child_pgid: Arc<Mutex<Option<i32>>>,
    /// Repo root used to resolve `.spur/pgids/<pgid>.toml` for the orphan-
    /// reaping registry. Defaults to `PathBuf::from(".")` so production
    /// callers (which run with cwd at the repo root) need no extra wiring.
    repo_root: PathBuf,
    /// Per-connection log configuration. Defaults to `LogConfig::default()`,
    /// which has `child_stderr_pipe: true` (the new file-rotate-backed
    /// stderr bridge is on by default). Tests and orchestrator wiring may
    /// override via [`Self::set_log_config`].
    log_config: LogConfig,
}

/// Compute the path where the ACP subprocess's stderr should be written.
/// Uses `.spur/logs/<agent>-<unix_ts>-<pid>-acp.log` relative to CWD.
///
/// The file is truncated when opened and the child process appends to it
/// for its lifetime — so one file per child-process spawn. Including PID
/// avoids collisions when multiple agents start in the same second.
fn build_acp_log_path(agent_name: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let pid = std::process::id();
    std::path::PathBuf::from(".spur/logs").join(format!("{agent_name}-{ts}-{pid}-acp.log"))
}

/// State-gated dispatch decision for `set_session_model`. Spec §6.3.
///
/// The decision is made *once* by reading `SpurAgentCaps` — never by
/// probing the agent at runtime. Codex (which advertises both `models`
/// AND a `model` config option) takes the dedicated `Direct` path; an
/// agent that only advertises config options takes the fallback; an
/// agent that advertises neither yields `Unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetSessionModelDispatch {
    /// `caps.supports_set_model()` — dispatch `SetSessionModelRequest`.
    Direct,
    /// `caps.supports_set_config_option()` only — fall back to
    /// `set_session_config_option` with `config_id = "model"`.
    FallbackConfigOption,
    /// Neither capability is advertised → `AcpError::CapabilityMissing`.
    Unsupported,
}

pub(crate) fn decide_set_session_model_dispatch(caps: &SpurAgentCaps) -> SetSessionModelDispatch {
    if caps.supports_set_model() {
        SetSessionModelDispatch::Direct
    } else if caps.supports_set_config_option() {
        SetSessionModelDispatch::FallbackConfigOption
    } else {
        SetSessionModelDispatch::Unsupported
    }
}

fn cache_session_modes(
    advertised_modes: &Arc<Mutex<HashMap<String, Vec<SessionModeId>>>>,
    session_id: &SessionId,
    modes: Option<&SessionModeState>,
) {
    let Some(modes) = modes else {
        return;
    };
    let ids = modes
        .available_modes
        .iter()
        .map(|mode| mode.id.clone())
        .collect();
    if let Ok(mut guard) = advertised_modes.lock() {
        guard.insert(session_id.0.to_string(), ids);
    }
}

fn busy_in_flight_error(agent_name: &str, in_flight: &str) -> anyhow::Error {
    anyhow::anyhow!("NativeAcpConnection '{agent_name}': busy ({in_flight} in flight)")
}

/// Reply to every non-Cancel/non-Shutdown command variant with a busy error.
/// Used inside the in-flight `Prompt` and `LoadSession` select! loops to
/// reject commands the orchestrator should not be issuing while a request is
/// pending. The match is exhaustive over `AcpCommand` so a future variant
/// cannot silently sneak past the busy guard.
fn reject_busy_command(cmd: AcpCommand, agent_name: &str, in_flight: &str) {
    let err = || busy_in_flight_error(agent_name, in_flight);
    match cmd {
        AcpCommand::Initialize { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::NewSession { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::Prompt { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::Cancel { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::Shutdown { reply } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::LoadSession { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::ListSessions { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::SetSessionMode { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::SetSessionModel { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::SetSessionConfigOption { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::Authenticate { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
        AcpCommand::ExtMethod { reply, .. } => {
            let _ = reply.send(Err(err()));
        }
    }
}

/// Send `session/cancel` as a JSON-RPC notification (one-way) and reply to
/// the originating `AcpCommand::Cancel`. `cx.send_notification` is synchronous
/// and does not wait for an agent response, so this is safe to call while a
/// `Prompt` or `LoadSession` request future is in flight on the same `cx`.
fn dispatch_cancel(
    cx: &ConnectionTo<Agent>,
    session_id: String,
    reply: oneshot::Sender<anyhow::Result<()>>,
    agent_name: &str,
) {
    let cancel = CancelNotification::new(session_id);
    let result = cx.send_notification(cancel);
    let _ = reply
        .send(result.map_err(|e| {
            anyhow::anyhow!("NativeAcpConnection '{agent_name}': cancel failed: {e}")
        }));
}

impl NativeAcpConnection {
    /// Create a new native ACP connection.
    ///
    /// `command` is the agent binary (e.g. "claude", "codex").
    /// `extra_args` are passed to the binary at spawn time.
    pub fn new(
        agent_name: impl Into<String>,
        command: impl Into<String>,
        extra_args: Vec<String>,
        permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
    ) -> Self {
        let (ext_tx, ext_rx) = mpsc::unbounded_channel::<ExtNotificationPayload>();
        // Capacity 4096 per the broadcast-sizing invariant (anchor 3ff4e86):
        // bursty history replay from `load_session` can produce O(hundreds)
        // of notifications in rapid succession, and the floor was established
        // empirically under 20 workers × 80 evt/s load.
        let (session_notif_tx, _) = tokio::sync::broadcast::channel(4096);
        Self {
            agent_name: agent_name.into(),
            command: command.into(),
            extra_args,
            cmd_tx: None,
            thread_handle: None,
            health_status: AgentHealth::Unknown,
            permission_tx,
            ext_notification_rx: Some(ext_rx),
            ext_notification_tx: ext_tx,
            session_notif_tx,
            advertised_modes: Arc::new(Mutex::new(HashMap::new())),
            child_pgid: Arc::new(Mutex::new(None)),
            repo_root: PathBuf::from("."),
            log_config: LogConfig::default(),
        }
    }

    /// Override the directory used to resolve `.spur/pgids/`. Production
    /// callers run with cwd at the repo root so the default is correct;
    /// tests use this to redirect the registry into a tempdir.
    pub fn set_repo_root(&mut self, root: PathBuf) {
        self.repo_root = root;
    }

    /// Override the log configuration used by the spawn site (controls the
    /// child-stderr capture mode + per-child rotation caps). Default is
    /// `LogConfig::default()`, which enables the file-rotate-backed bridge.
    pub fn set_log_config(&mut self, log_config: LogConfig) {
        self.log_config = log_config;
    }
}

/// Send `signal` (e.g. `"TERM"`, `"KILL"`) to the process group `pgid` via the
/// `kill(1)` CLI. Mirrors the existing terminal-cleanup pattern in this file;
/// keeps us off of a `libc` dependency.
///
/// stdout/stderr are redirected to `/dev/null` so benign races (ESRCH on a
/// already-reaped group, EPERM on a recycled pgid) don't leak to the user's
/// terminal after TUI teardown.
fn killpg(pgid: i32, signal: &str) {
    let _ = std::process::Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(format!("-{pgid}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

impl Drop for NativeAcpConnection {
    fn drop(&mut self) {
        // Safety net for panics, runtime teardown, or `tokio::task::abort()`
        // paths that skipped `shutdown()`. Synchronous SIGKILL of the process
        // group catches the agent subprocess AND its descendants (claude-
        // agent-acp spawns `node`, which can spawn further children).
        //
        // Skip when `shutdown()` has already run — it takes `cmd_tx`, so a
        // `None` here means graceful teardown succeeded and the pgid is
        // already gone. Re-killing a reaped (possibly recycled) pgid is how
        // we leaked `kill: -NNNN: Operation not permitted` to the terminal.
        if self.cmd_tx.is_none() {
            return;
        }
        if let Ok(guard) = self.child_pgid.lock() {
            if let Some(pgid) = *guard {
                killpg(pgid, "KILL");
                let registry = crate::orphan_registry::PgidRegistry::new(
                    self.repo_root.join(".spur").join("pgids"),
                );
                let _ = registry.delete(pgid);
            }
        }
    }
}

#[async_trait]
impl AgentConnection for NativeAcpConnection {
    // ─── initialize ─────────────────────────────────────────────────────

    async fn initialize(
        &mut self,
        mut request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        // Override the caller-supplied client_capabilities with spur's
        // canonical literal so every InitializeRequest carries the
        // explicit fs / terminal / meta.terminal_output advertisement.
        // Callers today all pass InitializeRequest::new(ProtocolVersion::LATEST)
        // (which yields ClientCapabilities::default()) — spec §6.2 requires
        // we replace those defaults with the explicit gate.
        request.client_capabilities = spur_client_capabilities();

        let agent_name = self.agent_name.clone();
        let command = self.command.clone();
        let extra_args = self.extra_args.clone();

        // Create the command channel.
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<AcpCommand>();

        tracing::info!(
            agent = %agent_name,
            command = %command,
            "NativeAcpConnection: spawning agent subprocess and ACP thread"
        );

        // Spawn the dedicated thread that will own the !Send SDK connection.
        let thread_agent_name = agent_name.clone();
        let permission_tx = self.permission_tx.clone();
        let ext_tx = self.ext_notification_tx.clone();
        let session_notif_tx_for_thread = self.session_notif_tx.clone();
        let advertised_modes = self.advertised_modes.clone();
        let child_pgid = self.child_pgid.clone();
        let repo_root = self.repo_root.clone();
        let log_config = self.log_config.clone();
        let handle = std::thread::Builder::new()
            .name(format!("acp-{}", agent_name))
            .spawn(move || {
                acp_thread_main(
                    thread_agent_name,
                    command,
                    extra_args,
                    cmd_rx,
                    permission_tx,
                    ext_tx,
                    session_notif_tx_for_thread,
                    advertised_modes,
                    child_pgid,
                    repo_root,
                    log_config,
                );
            })
            .map_err(|e| {
                anyhow::anyhow!(
                    "NativeAcpConnection '{}': failed to spawn ACP thread: {e}",
                    agent_name
                )
            })?;

        self.thread_handle = Some(handle);
        self.cmd_tx = Some(cmd_tx.clone());

        // Send the initialize command and wait for the response.
        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::Initialize {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "NativeAcpConnection '{}': ACP thread died before initialize",
                    self.agent_name
                )
            })?;

        let result = reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during initialize",
                self.agent_name
            )
        })??;

        self.health_status = AgentHealth::Ready;
        tracing::info!(
            agent = %self.agent_name,
            "NativeAcpConnection: initialized successfully"
        );

        Ok(result)
    }

    // ─── new_session ────────────────────────────────────────────────────

    async fn new_session(
        &mut self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        let mut request = NewSessionRequest::new(&cwd);
        request.mcp_servers = mcp_servers;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::NewSession {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        let result = reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during new_session",
                self.agent_name
            )
        })??;

        tracing::debug!(
            agent = %self.agent_name,
            session = %result.session_id,
            "NativeAcpConnection: session created"
        );

        Ok(result)
    }

    // ─── prompt ─────────────────────────────────────────────────────────

    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        let session_id = request.session_id.clone();

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "NativeAcpConnection: sending prompt"
        );

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::Prompt {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        let notification_rx = reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during prompt setup",
                self.agent_name
            )
        })??;

        // Wrap the unbounded receiver as a Stream.
        let stream = unfold(notification_rx, |mut rx| async move {
            rx.recv().await.map(|notif| (notif, rx))
        });

        Ok(Box::pin(stream))
    }

    // ─── cancel ─────────────────────────────────────────────────────────

    async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "NativeAcpConnection: cancelling session"
        );

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::Cancel {
                session_id: session_id.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during cancel",
                self.agent_name
            )
        })?
    }

    // ─── shutdown ───────────────────────────────────────────────────────

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        tracing::info!(agent = %self.agent_name, "NativeAcpConnection: shutting down");

        if let Some(cmd_tx) = self.cmd_tx.take() {
            let (reply_tx, reply_rx) = oneshot::channel();
            // If the thread is already dead, that's fine — we'll just drop.
            let _ = cmd_tx.send(AcpCommand::Shutdown { reply: reply_tx });
            // Wait for acknowledgement, but don't fail if the thread is gone.
            let _ = reply_rx.await;
        }

        // Wait for the thread to finish.
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }

        self.health_status = AgentHealth::Unknown;
        tracing::info!(agent = %self.agent_name, "NativeAcpConnection: shutdown complete");
        Ok(())
    }

    // ─── health ─────────────────────────────────────────────────────────

    fn health(&self) -> AgentHealth {
        self.health_status.clone()
    }

    fn advertised_session_modes(&self, session_id: &SessionId) -> Option<Vec<SessionModeId>> {
        self.advertised_modes
            .lock()
            .ok()
            .and_then(|modes| modes.get(session_id.0.as_ref()).cloned())
    }

    // ─── load_session ────────────────────────────────────────────────────

    async fn load_session(
        &mut self,
        request: LoadSessionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::LoadSession {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        let notification_rx = reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during load_session setup",
                self.agent_name
            )
        })??;

        let stream = unfold(notification_rx, |mut rx| async move {
            rx.recv().await.map(|notif| (notif, rx))
        });
        Ok(Box::pin(stream))
    }

    // ─── list_sessions ───────────────────────────────────────────────────

    async fn list_sessions(
        &mut self,
        request: ListSessionsRequest,
    ) -> anyhow::Result<ListSessionsResponse> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::ListSessions {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during list_sessions",
                self.agent_name
            )
        })?
    }

    // ─── set_session_mode ────────────────────────────────────────────────

    async fn set_session_mode(
        &mut self,
        request: SetSessionModeRequest,
    ) -> anyhow::Result<SetSessionModeResponse> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::SetSessionMode {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during set_session_mode",
                self.agent_name
            )
        })?
    }

    // ─── set_session_config_option ───────────────────────────────────────

    async fn set_session_config_option(
        &mut self,
        request: SetSessionConfigOptionRequest,
    ) -> anyhow::Result<SetSessionConfigOptionResponse> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::SetSessionConfigOption {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during set_session_config_option",
                self.agent_name
            )
        })?
    }

    // ─── set_session_model ───────────────────────────────────────────────

    /// Issue ACP `session/set_model` with a state-gated fallback to
    /// `session/set_config_option`. Spec §6.3.
    ///
    /// The dispatch decision is read from `caps` once (see
    /// [`decide_set_session_model_dispatch`]); there is no try-and-see
    /// runtime probe. Both `Direct` and `FallbackConfigOption` paths
    /// drop the wire response — the orchestrator refreshes its cached
    /// `current_model` from the next agent-emitted notification.
    async fn set_session_model(
        &mut self,
        sid: SessionId,
        model_id: ModelId,
        caps: &SpurAgentCaps,
    ) -> Result<(), AcpError> {
        match decide_set_session_model_dispatch(caps) {
            SetSessionModelDispatch::Direct => {
                let request = SetSessionModelRequest::new(sid, model_id);
                let agent_name = self.agent_name.clone();
                let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
                    AcpError::Transport(anyhow::anyhow!(
                        "NativeAcpConnection '{agent_name}': not initialized"
                    ))
                })?;
                let (reply_tx, reply_rx) = oneshot::channel();
                cmd_tx
                    .send(AcpCommand::SetSessionModel {
                        request,
                        reply: reply_tx,
                    })
                    .map_err(|_| {
                        AcpError::Transport(anyhow::anyhow!(
                            "NativeAcpConnection '{agent_name}': ACP thread died"
                        ))
                    })?;
                reply_rx
                    .await
                    .map_err(|_| {
                        AcpError::Transport(anyhow::anyhow!(
                            "NativeAcpConnection '{agent_name}': ACP thread died during set_session_model"
                        ))
                    })?
                    .map_err(AcpError::Transport)?;
                Ok(())
            }
            SetSessionModelDispatch::FallbackConfigOption => {
                let request = SetSessionConfigOptionRequest::new(
                    sid,
                    SessionConfigId::new(Arc::<str>::from("model")),
                    SessionConfigValueId::new(model_id.0),
                );
                self.set_session_config_option(request)
                    .await
                    .map(|_| ())
                    .map_err(AcpError::Transport)
            }
            SetSessionModelDispatch::Unsupported => Err(AcpError::CapabilityMissing("set_model")),
        }
    }

    // ─── authenticate ────────────────────────────────────────────────────

    async fn authenticate(
        &mut self,
        request: AuthenticateRequest,
    ) -> anyhow::Result<AuthenticateResponse> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::Authenticate {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during authenticate",
                self.agent_name
            )
        })?
    }

    // ─── call_ext ────────────────────────────────────────────────────────

    async fn call_ext(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
        })?;

        // The ACP SDK re-prepends `_` when serializing `ExtRequest::method`
        // to the wire, so strip a single leading `_` here if present.
        let sdk_method = method.strip_prefix('_').unwrap_or(method).to_string();

        let raw: Box<serde_json::value::RawValue> = serde_json::value::to_raw_value(&params)?;
        let raw_arc: std::sync::Arc<serde_json::value::RawValue> = raw.into();
        let request = ExtRequest::new(sdk_method, raw_arc);

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx
            .send(AcpCommand::ExtMethod {
                request,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!("NativeAcpConnection '{}': ACP thread died", self.agent_name)
            })?;

        let response: ExtResponse = reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during ext_method",
                self.agent_name
            )
        })??;

        let value: serde_json::Value = serde_json::from_str(response.0.get())?;
        Ok(value)
    }

    fn take_ext_notification_rx(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<ExtNotificationPayload>> {
        self.ext_notification_rx.take()
    }

    fn subscribe_session_notifications(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<SessionNotification>> {
        Some(self.session_notif_tx.subscribe())
    }
}

// ─── Dedicated ACP thread ───────────────────────────────────────────────────

/// Entry point for the dedicated thread that owns the `!Send` ACP connection.
///
/// This function creates its own single-threaded Tokio runtime + `LocalSet`
/// and runs the SDK's I/O loop alongside a command handler that processes
/// requests from the main thread.
#[allow(clippy::too_many_arguments)]
fn acp_thread_main(
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<AcpCommand>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
    ext_notification_tx: mpsc::UnboundedSender<ExtNotificationPayload>,
    session_notif_tx: tokio::sync::broadcast::Sender<SessionNotification>,
    advertised_modes: Arc<Mutex<HashMap<String, Vec<SessionModeId>>>>,
    child_pgid: Arc<Mutex<Option<i32>>>,
    repo_root: PathBuf,
    log_config: LogConfig,
) {
    // Build a single-threaded runtime for this thread.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(
                agent = %agent_name,
                "NativeAcpConnection: failed to create tokio runtime: {e}"
            );
            return;
        }
    };

    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        // Wait for the Initialize command — it tells us when to spawn the process.
        let Some(first_cmd) = cmd_rx.recv().await else {
            tracing::warn!(agent = %agent_name, "NativeAcpConnection: command channel closed before initialize");
            return;
        };

        let (init_request, init_reply) = match first_cmd {
            AcpCommand::Initialize { request, reply } => (request, reply),
            _ => {
                tracing::error!(agent = %agent_name, "NativeAcpConnection: first command must be Initialize");
                return;
            }
        };

        // Spawn the agent subprocess.
        let log_path = build_acp_log_path(&agent_name);
        if let Some(parent) = log_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    agent = %agent_name,
                    path = %parent.display(),
                    error = %e,
                    "NativeAcpConnection: failed to create log directory; falling back to inherit",
                );
            }
        }
        tracing::info!(
            agent = %agent_name,
            log_path = %log_path.display(),
            "NativeAcpConnection: capturing child stderr to log file"
        );
        let stderr_cfg = if log_config.child_stderr_pipe {
            // New default: spur owns the writer, child stderr flows through a
            // bounded byte-chunk reader into a per-child file-rotate writer.
            // See `connection/child_stderr_bridge.rs`.
            std::process::Stdio::piped()
        } else {
            // Legacy fall-back: child holds the FD directly. No rotation.
            match std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_path)
            {
                Ok(f) => std::process::Stdio::from(f),
                Err(e) => {
                    tracing::warn!(
                        agent = %agent_name,
                        path = %log_path.display(),
                        error = %e,
                        "NativeAcpConnection: child_stderr_pipe disabled but log open failed; using inherit",
                    );
                    std::process::Stdio::inherit()
                }
            }
        };

        let mut cmd = tokio::process::Command::new(&command);
        cmd.args(&extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr_cfg)
            .kill_on_drop(true);
        // Put the child (and its descendants, e.g. the `node` tree beneath
        // `claude-agent-acp`) in its own process group so shutdown can reap
        // the whole tree with `killpg`. Without this, grandchildren orphan
        // to init when spur exits.
        #[cfg(unix)]
        cmd.process_group(0);
        let child_result = cmd.spawn();

        let mut child = match child_result {
            Ok(c) => c,
            Err(e) => {
                let _ = init_reply.send(Err(anyhow::anyhow!(
                    "NativeAcpConnection '{}': failed to spawn '{}': {e}",
                    agent_name,
                    command
                )));
                return;
            }
        };

        // Record the pgid (= child pid under `process_group(0)`) so the
        // `Drop` safety net and the graceful shutdown arm can reach the
        // entire process group.
        if let Some(pid) = child.id() {
            if let Ok(mut guard) = child_pgid.lock() {
                *guard = Some(pid as i32);
            }

            // Persist a registry record so the next-boot sweep can
            // reconcile this pgid even if spur dies before reaping it.
            let registry = crate::orphan_registry::PgidRegistry::new(
                repo_root.join(".spur").join("pgids"),
            );
            let rec = crate::orphan_registry::PgidRecord {
                spur_pid: std::process::id() as i32,
                spur_pid_start_time: crate::process_inspector::starttime_of_self(),
                agent_name: agent_name.clone(),
                cmd: format!("{} {}", command, extra_args.join(" ")),
                pgid: pid as i32,
                pgid_leader_start_time: crate::process_inspector::starttime_of(pid as i32)
                    .unwrap_or(0),
                spawned_at: chrono::Utc::now().timestamp(),
            };
            if let Err(e) = registry.write(&rec) {
                tracing::warn!(
                    error = %e,
                    "orphan_registry write failed; sweep cannot reclaim this child"
                );
            }
        }

        // Start the per-child stderr bridge when piping is enabled. The
        // bridge owns the read side of the child's stderr pipe and writes
        // through `file-rotate` so per-child disk usage stays bounded.
        // The handle is kept in scope until after `child.wait()` returns
        // so we can drain its `non_blocking` worker on shutdown.
        let stderr_bridge: Option<ChildStderrBridge> = if log_config.child_stderr_pipe {
            match child.stderr.take() {
                Some(stderr) => {
                    let log_dir = log_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| PathBuf::from("."));
                    let pid = child.id().unwrap_or(0);
                    match ChildStderrBridge::start(
                        stderr,
                        &log_dir,
                        &agent_name,
                        pid,
                        log_config.child_stderr_max_bytes,
                        log_config.child_stderr_max_files,
                        log_config.child_stderr_buffered_lines_limit,
                    ) {
                        Ok(bridge) => Some(bridge),
                        Err(e) => {
                            tracing::warn!(
                                agent = %agent_name,
                                error = %e,
                                "NativeAcpConnection: failed to start child stderr bridge; \
                                 child stderr will be discarded for this run"
                            );
                            None
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        agent = %agent_name,
                        "NativeAcpConnection: child_stderr_pipe enabled but child.stderr was None"
                    );
                    None
                }
            }
        } else {
            None
        };

        let child_stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                let _ = init_reply.send(Err(anyhow::anyhow!(
                    "NativeAcpConnection '{}': failed to capture stdin",
                    agent_name
                )));
                return;
            }
        };
        let child_stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = init_reply.send(Err(anyhow::anyhow!(
                    "NativeAcpConnection '{}': failed to capture stdout",
                    agent_name
                )));
                return;
            }
        };

        // Wrap tokio AsyncRead/Write into futures AsyncRead/Write using compat,
        // then hand both halves to the SDK's `ByteStreams` transport.
        let stdout_compat = tokio_util::compat::TokioAsyncReadCompatExt::compat(child_stdout);
        let stdin_compat = tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(child_stdin);
        let transport = ByteStreams::new(stdin_compat, stdout_compat);

        // Send-safe state shared between handler closures (which carry a
        // `+ Send` bound in the 0.11.1 API). Builder handler closures clone
        // these Arcs into their own captures.
        let cwd: Arc<Mutex<PathBuf>> = Arc::new(Mutex::new(PathBuf::from(".")));
        let terminals: Arc<Mutex<HashMap<String, TerminalState>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // !Send slots used to ferry oneshot replies between the connect_with
        // closure (which is allowed to be !Send) and the post-connection
        // cleanup phase. `init_reply_slot` lets us surface a fatal error to
        // the caller even if `connect_with` ends before initialize completes;
        // `shutdown_reply_slot` lets us ack `AcpCommand::Shutdown` only AFTER
        // the child has been reaped.
        let init_reply_slot: std::rc::Rc<
            std::cell::RefCell<Option<oneshot::Sender<anyhow::Result<InitializeResponse>>>>,
        > = std::rc::Rc::new(std::cell::RefCell::new(Some(init_reply)));
        let shutdown_reply_slot: std::rc::Rc<
            std::cell::RefCell<Option<oneshot::Sender<anyhow::Result<()>>>>,
        > = std::rc::Rc::new(std::cell::RefCell::new(None));

        let connect_result: Result<(), agent_client_protocol::Error> = {
            // Per-handler clones. Each handler closure is `async move`, so
            // it owns its captures; we hand it a fresh clone of every Arc /
            // sender it needs.
            let perm_tx_h = permission_tx.clone();
            let session_notif_tx_h = session_notif_tx.clone();
            let ext_notification_tx_h = ext_notification_tx.clone();

            let cwd_read = cwd.clone();
            let cwd_write = cwd.clone();
            let cwd_create_term = cwd.clone();

            let terminals_create = terminals.clone();
            let terminals_output = terminals.clone();
            let terminals_wait = terminals.clone();
            let terminals_kill = terminals.clone();
            let terminals_release = terminals.clone();

            // Captures for the connect_with main_fn (the command loop).
            let cwd_loop = cwd.clone();
            let agent_name_loop = agent_name.clone();
            let init_reply_slot_loop = init_reply_slot.clone();
            let shutdown_reply_slot_loop = shutdown_reply_slot.clone();

            Client
                .builder()
                .name(format!("spur-acp-{}", agent_name))
                // ── session/request_permission ────────────────────────────
                .on_receive_request(
                    async move |req: RequestPermissionRequest, responder, cx| {
                        cx.spawn({
                            let permission_tx = perm_tx_h.clone();
                            async move {
                                let outcome = handle_request_permission(req, permission_tx).await;
                                responder.respond_with_result(outcome)
                            }
                        })?;
                        Ok(())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── fs/read_text_file ─────────────────────────────────────
                .on_receive_request(
                    async move |req: ReadTextFileRequest, responder, _cx| {
                        let cwd_now = cwd_read.lock().unwrap().clone();
                        let path = if req.path.is_absolute() {
                            req.path.clone()
                        } else {
                            cwd_now.join(&req.path)
                        };
                        tracing::debug!(
                            path = %path.display(),
                            "NativeAcpConnection: reading text file"
                        );
                        let outcome = std::fs::read_to_string(&path)
                            .map_err(|e| {
                                agent_client_protocol::Error::internal_error()
                                    .data(format!("Failed to read {}: {e}", path.display()))
                            })
                            .map(|content| {
                                let trimmed = match (req.line, req.limit) {
                                    (Some(s), Some(l)) => content
                                        .lines()
                                        .skip((s.saturating_sub(1)) as usize)
                                        .take(l as usize)
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                    (Some(s), None) => content
                                        .lines()
                                        .skip((s.saturating_sub(1)) as usize)
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                    (None, Some(l)) => content
                                        .lines()
                                        .take(l as usize)
                                        .collect::<Vec<_>>()
                                        .join("\n"),
                                    (None, None) => content,
                                };
                                ReadTextFileResponse::new(trimmed)
                            });
                        responder.respond_with_result(outcome)
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── fs/write_text_file ────────────────────────────────────
                .on_receive_request(
                    async move |req: WriteTextFileRequest, responder, _cx| {
                        let cwd_now = cwd_write.lock().unwrap().clone();
                        let path = if req.path.is_absolute() {
                            req.path.clone()
                        } else {
                            cwd_now.join(&req.path)
                        };
                        tracing::debug!(
                            path = %path.display(),
                            content_len = req.content.len(),
                            "NativeAcpConnection: writing text file"
                        );
                        let outcome: agent_client_protocol::Result<WriteTextFileResponse> = (|| {
                            if let Some(parent) = path.parent() {
                                std::fs::create_dir_all(parent).map_err(|e| {
                                    agent_client_protocol::Error::internal_error().data(format!(
                                        "Failed to create directories for {}: {e}",
                                        path.display()
                                    ))
                                })?;
                            }
                            std::fs::write(&path, &req.content).map_err(|e| {
                                agent_client_protocol::Error::internal_error()
                                    .data(format!("Failed to write {}: {e}", path.display()))
                            })?;
                            Ok(WriteTextFileResponse::new())
                        })();
                        responder.respond_with_result(outcome)
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── terminal/create ───────────────────────────────────────
                .on_receive_request(
                    async move |req: CreateTerminalRequest, responder, _cx| {
                        let cwd_now = req
                            .cwd
                            .clone()
                            .unwrap_or_else(|| cwd_create_term.lock().unwrap().clone());
                        let byte_limit = req.output_byte_limit.or(Some(10 * 1024 * 1024));
                        let mut cmd = tokio::process::Command::new(&req.command);
                        cmd.args(&req.args)
                            .current_dir(&cwd_now)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .kill_on_drop(true);
                        #[cfg(unix)]
                        cmd.process_group(0);
                        for env_var in &req.env {
                            cmd.env(&env_var.name, &env_var.value);
                        }
                        let outcome: agent_client_protocol::Result<CreateTerminalResponse> =
                            (|| -> agent_client_protocol::Result<CreateTerminalResponse> {
                                let mut child = cmd.spawn().map_err(|e| {
                                    agent_client_protocol::Error::internal_error().data(
                                        format!("Failed to spawn '{}': {e}", req.command),
                                    )
                                })?;
                                let pid = child.id().ok_or_else(|| {
                                    agent_client_protocol::Error::internal_error()
                                        .data("Failed to get process ID")
                                })?;
                                let child_stdout = child.stdout.take().ok_or_else(|| {
                                    agent_client_protocol::Error::internal_error()
                                        .data("Failed to capture stdout")
                                })?;
                                let child_stderr = child.stderr.take().ok_or_else(|| {
                                    agent_client_protocol::Error::internal_error()
                                        .data("Failed to capture stderr")
                                })?;

                                let output = Arc::new(Mutex::new(String::new()));
                                let truncated = Arc::new(AtomicBool::new(false));
                                let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);

                                // Reader runs on the LocalSet task — its captured
                                // state is all `Send` so it would also satisfy
                                // `tokio::spawn`, but we don't have a multi-threaded
                                // runtime here.
                                tokio::task::spawn_local(terminal_reader(
                                    child_stdout,
                                    child_stderr,
                                    child,
                                    output.clone(),
                                    truncated.clone(),
                                    byte_limit,
                                    exit_tx,
                                ));

                                let terminal_id =
                                    TerminalId::new(uuid::Uuid::new_v4().to_string());
                                let id_string = terminal_id.to_string();
                                tracing::debug!(
                                    terminal = %id_string,
                                    command = %req.command,
                                    pid = pid,
                                    "Terminal created"
                                );
                                terminals_create.lock().unwrap().insert(
                                    id_string,
                                    TerminalState {
                                        output,
                                        truncated,
                                        exit_rx,
                                        pid,
                                    },
                                );
                                Ok(CreateTerminalResponse::new(terminal_id))
                            })();
                        responder.respond_with_result(outcome)
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── terminal/output ───────────────────────────────────────
                .on_receive_request(
                    async move |req: TerminalOutputRequest, responder, _cx| {
                        let key = req.terminal_id.to_string();
                        let outcome: agent_client_protocol::Result<TerminalOutputResponse> = {
                            let map = terminals_output.lock().unwrap();
                            match map.get(&key) {
                                Some(terminal) => {
                                    let output = terminal.output.lock().unwrap().clone();
                                    let truncated = terminal.truncated.load(Ordering::Relaxed);
                                    let exit_status = terminal.exit_rx.borrow().clone();
                                    Ok(TerminalOutputResponse::new(output, truncated)
                                        .exit_status(exit_status))
                                }
                                None => Err(agent_client_protocol::Error::invalid_params()
                                    .data(format!("Terminal '{}' not found", key))),
                            }
                        };
                        responder.respond_with_result(outcome)
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── terminal/wait_for_exit ────────────────────────────────
                .on_receive_request(
                    async move |req: WaitForTerminalExitRequest, responder, cx| {
                        let key = req.terminal_id.to_string();
                        let mut exit_rx = {
                            let map = terminals_wait.lock().unwrap();
                            match map.get(&key) {
                                Some(terminal) => terminal.exit_rx.clone(),
                                None => {
                                    return responder.respond_with_result(Err(
                                        agent_client_protocol::Error::invalid_params().data(
                                            format!("Terminal '{}' not found", key),
                                        ),
                                    ));
                                }
                            }
                        };
                        cx.spawn(async move {
                            if let Some(status) = exit_rx.borrow().clone() {
                                return responder
                                    .respond(WaitForTerminalExitResponse::new(status));
                            }
                            loop {
                                match exit_rx.changed().await {
                                    Ok(()) => {
                                        if let Some(status) = exit_rx.borrow().clone() {
                                            return responder.respond(
                                                WaitForTerminalExitResponse::new(status),
                                            );
                                        }
                                    }
                                    Err(_) => {
                                        let status = exit_rx
                                            .borrow()
                                            .clone()
                                            .unwrap_or_else(TerminalExitStatus::new);
                                        return responder
                                            .respond(WaitForTerminalExitResponse::new(status));
                                    }
                                }
                            }
                        })?;
                        Ok(())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── terminal/kill ─────────────────────────────────────────
                .on_receive_request(
                    async move |req: KillTerminalRequest, responder, _cx| {
                        let key = req.terminal_id.to_string();
                        let result: Option<(u32, bool)> = {
                            let map = terminals_kill.lock().unwrap();
                            map.get(&key)
                                .map(|t| (t.pid, t.exit_rx.borrow().is_none()))
                        };
                        match result {
                            None => responder.respond_with_result(Err(
                                agent_client_protocol::Error::invalid_params()
                                    .data(format!("Terminal '{}' not found", key)),
                            )),
                            Some((pid, is_running)) => {
                                if is_running {
                                    tracing::debug!(
                                        terminal = %key,
                                        pid = pid,
                                        "Killing terminal"
                                    );
                                    let _ = std::process::Command::new("kill")
                                        .arg("-9")
                                        .arg(pid.to_string())
                                        .status();
                                }
                                responder.respond(KillTerminalResponse::new())
                            }
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── terminal/release ──────────────────────────────────────
                .on_receive_request(
                    async move |req: ReleaseTerminalRequest, responder, _cx| {
                        let key = req.terminal_id.to_string();
                        let pid_to_kill: Option<u32> = {
                            let map = terminals_release.lock().unwrap();
                            map.get(&key).and_then(|t| {
                                if t.exit_rx.borrow().is_none() {
                                    Some(t.pid)
                                } else {
                                    None
                                }
                            })
                        };
                        if let Some(pid) = pid_to_kill {
                            tracing::debug!(
                                terminal = %key,
                                pid = pid,
                                "Killing terminal on release"
                            );
                            let _ = std::process::Command::new("kill")
                                .arg("-9")
                                .arg(pid.to_string())
                                .status();
                        }
                        terminals_release.lock().unwrap().remove(&key);
                        responder.respond(ReleaseTerminalResponse::new())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                // ── notifications: session/update + extension ────────────
                .on_receive_notification(
                    async move |notif: agent_client_protocol::AgentNotification, _cx| {
                        match notif {
                            agent_client_protocol::AgentNotification::SessionNotification(args) => {
                                let variant = session_update_variant_name(&args.update);
                                let text_len = match &args.update {
                                    SessionUpdate::AgentMessageChunk(c)
                                    | SessionUpdate::AgentThoughtChunk(c)
                                    | SessionUpdate::UserMessageChunk(c) => {
                                        content_chunk_text_len(c)
                                    }
                                    _ => 0,
                                };
                                let session = args.session_id.to_string();
                                // `broadcast::Sender::send` returns `Err(SendError)` only when every
                                // receiver has been dropped. The orchestrator pre-subscribes before
                                // calling `new_session` / `load_session` (see `create_brain_session`
                                // and `load_brain_session` in `spur-core/src/orchestrator.rs`) and
                                // holds the receiver for the lifetime of the BrainSession — so
                                // `Err` here indicates the connection is tearing down and we can
                                // safely ignore it. If this starts producing `err` in logs under
                                // normal operation, the pre-subscribe ordering has regressed.
                                let send_result = session_notif_tx_h.send(args);
                                let send_result_str =
                                    if send_result.is_ok() { "ok" } else { "err" };
                                tracing::debug!(
                                    streaming_probe = true,
                                    site = "A_session_notification",
                                    variant = variant,
                                    text_len = text_len,
                                    session = %session,
                                    send_result = send_result_str,
                                    "ACP session_notification (broadcast)"
                                );
                            }
                            agent_client_protocol::AgentNotification::ExtNotification(args) => {
                                // The SDK already stripped the leading `_` from
                                // the wire method, so reattach it when reporting
                                // upward so consumers see the full
                                // `_foo.dev/...` form.
                                let method = format!("_{}", args.method);
                                let params: serde_json::Value =
                                    serde_json::from_str(args.params.get())
                                        .unwrap_or(serde_json::Value::Null);
                                tracing::debug!(
                                    method = %method,
                                    "NativeAcpConnection: ext_notification"
                                );
                                let _ = ext_notification_tx_h.send(ExtNotificationPayload {
                                    method,
                                    params,
                                });
                            }
                            _ => {
                                // `AgentNotification` is `#[non_exhaustive]`;
                                // future variants under unstable features land
                                // here. Drop them silently.
                            }
                        }
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                // ── connect_with: drives the command loop ────────────────
                .connect_with(transport, async move |cx: ConnectionTo<Agent>| -> agent_client_protocol::Result<()> {
                    // 1. Run the ACP initialize handshake and forward the
                    //    response to the caller blocked in `initialize()`.
                    let init_outcome = cx.send_request(init_request).block_task().await;
                    match init_outcome {
                        Ok(response) => {
                            if let Some(reply) =
                                init_reply_slot_loop.borrow_mut().take()
                            {
                                let _ = reply.send(Ok(response));
                            }
                        }
                        Err(e) => {
                            if let Some(reply) =
                                init_reply_slot_loop.borrow_mut().take()
                            {
                                let _ = reply.send(Err(anyhow::anyhow!(
                                    "NativeAcpConnection '{}': initialize failed: {e}",
                                    agent_name_loop
                                )));
                            }
                            return Err(e);
                        }
                    }

                    // 2. Process commands sequentially. Each `block_task().await`
                    //    suspends here while handler callbacks continue to run
                    //    on the dispatch loop.
                    while let Some(cmd) = cmd_rx.recv().await {
                        match cmd {
                            AcpCommand::Initialize { reply, .. } => {
                                let _ = reply.send(Err(anyhow::anyhow!(
                                    "NativeAcpConnection '{}': already initialized",
                                    agent_name_loop
                                )));
                            }
                            AcpCommand::NewSession { request, reply } => {
                                *cwd_loop.lock().unwrap() = request.cwd.clone();
                                let result = cx.send_request(request).block_task().await;
                                if let Ok(response) = &result {
                                    cache_session_modes(
                                        &advertised_modes,
                                        &response.session_id,
                                        response.modes.as_ref(),
                                    );
                                }
                                let _ = reply.send(result.map_err(|e| {
                                    anyhow::anyhow!(
                                        "NativeAcpConnection '{}': new_session failed: {e}",
                                        agent_name_loop
                                    )
                                }));
                            }
                            AcpCommand::Prompt { request, reply } => {
                                // Notifications flow out-of-band via the
                                // `session_notif_tx` broadcast. The `Stream`
                                // returned by `prompt()` is a live but-empty
                                // `UnboundedReceiver`; closing it (drop of
                                // `tx_empty`) signals turn completion to the
                                // caller.
                                let (tx_empty, rx_empty) =
                                    mpsc::unbounded_channel::<SessionNotification>();
                                let _ = reply.send(Ok(rx_empty));
                                let session_id_for_probe = request.session_id.clone();
                                // Multiplex command intake against the in-flight prompt
                                // future so `Cancel` and `Shutdown` can be serviced while
                                // the agent is still streaming. `biased;` polls cmd_rx
                                // first so a queued cancel cannot starve behind heavy
                                // notification flow.
                                let prompt_fut =
                                    cx.send_request(request).block_task();
                                tokio::pin!(prompt_fut);
                                let mut cmd_rx_closed = false;
                                loop {
                                    tokio::select! {
                                        biased;
                                        maybe_cmd = cmd_rx.recv(), if !cmd_rx_closed => {
                                            match maybe_cmd {
                                                Some(AcpCommand::Cancel { session_id, reply }) => {
                                                    dispatch_cancel(&cx, session_id, reply, &agent_name_loop);
                                                }
                                                Some(AcpCommand::Shutdown { reply }) => {
                                                    tracing::debug!(
                                                        agent = %agent_name_loop,
                                                        "NativeAcpConnection: ACP thread received shutdown during in-flight prompt"
                                                    );
                                                    *shutdown_reply_slot_loop.borrow_mut() = Some(reply);
                                                    drop(tx_empty);
                                                    return Ok(());
                                                }
                                                Some(other) => {
                                                    reject_busy_command(other, &agent_name_loop, "prompt");
                                                }
                                                None => {
                                                    cmd_rx_closed = true;
                                                }
                                            }
                                        }
                                        prompt_result = &mut prompt_fut => {
                                            match &prompt_result {
                                                Ok(_) => tracing::debug!(
                                                    agent = %agent_name_loop,
                                                    session = %session_id_for_probe,
                                                    "NativeAcpConnection: prompt completed"
                                                ),
                                                Err(e) => tracing::warn!(
                                                    agent = %agent_name_loop,
                                                    session = %session_id_for_probe,
                                                    "NativeAcpConnection: prompt failed: {e}"
                                                ),
                                            }
                                            drop(tx_empty);
                                            break;
                                        }
                                    }
                                }
                            }
                            AcpCommand::Cancel { session_id, reply } => {
                                dispatch_cancel(&cx, session_id, reply, &agent_name_loop);
                            }
                            AcpCommand::Shutdown { reply } => {
                                tracing::debug!(
                                    agent = %agent_name_loop,
                                    "NativeAcpConnection: ACP thread received shutdown"
                                );
                                // Stash the reply for the post-connection
                                // cleanup phase. Returning here closes the
                                // SDK's writer half — which is the protocol's
                                // graceful-exit contract: the agent sees EOF
                                // on stdin and exits cleanly.
                                *shutdown_reply_slot_loop.borrow_mut() = Some(reply);
                                return Ok(());
                            }
                            AcpCommand::LoadSession { request, reply } => {
                                *cwd_loop.lock().unwrap() = request.cwd.clone();
                                let (tx_empty, rx_empty) =
                                    mpsc::unbounded_channel::<SessionNotification>();
                                let session_id_for_probe = request.session_id.clone();
                                // Multiplex command intake against the in-flight
                                // `session/load` future so cancel/shutdown can be
                                // serviced while history replay is in progress.
                                // Reply is sent AFTER the future resolves (load_session's
                                // contract is reply-with-result, unlike Prompt which
                                // hands the empty stream out up front).
                                let load_session_fut =
                                    cx.send_request(request).block_task();
                                tokio::pin!(load_session_fut);
                                let mut reply_holder = Some(reply);
                                let mut cmd_rx_closed = false;
                                loop {
                                    tokio::select! {
                                        biased;
                                        maybe_cmd = cmd_rx.recv(), if !cmd_rx_closed => {
                                            match maybe_cmd {
                                                Some(AcpCommand::Cancel { session_id, reply }) => {
                                                    dispatch_cancel(&cx, session_id, reply, &agent_name_loop);
                                                }
                                                Some(AcpCommand::Shutdown { reply }) => {
                                                    tracing::debug!(
                                                        agent = %agent_name_loop,
                                                        "NativeAcpConnection: ACP thread received shutdown during in-flight load_session"
                                                    );
                                                    *shutdown_reply_slot_loop.borrow_mut() = Some(reply);
                                                    drop(tx_empty);
                                                    return Ok(());
                                                }
                                                Some(other) => {
                                                    reject_busy_command(other, &agent_name_loop, "load_session");
                                                }
                                                None => {
                                                    cmd_rx_closed = true;
                                                }
                                            }
                                        }
                                        load_result = &mut load_session_fut => {
                                            let reply = reply_holder
                                                .take()
                                                .expect("LoadSession reply consumed only once");
                                            match load_result {
                                                Ok(response) => {
                                                    cache_session_modes(
                                                        &advertised_modes,
                                                        &session_id_for_probe,
                                                        response.modes.as_ref(),
                                                    );
                                                    tracing::debug!(
                                                        agent = %agent_name_loop,
                                                        session = %session_id_for_probe,
                                                        "NativeAcpConnection: load_session completed"
                                                    );
                                                    let _ = reply.send(Ok(rx_empty));
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        agent = %agent_name_loop,
                                                        session = %session_id_for_probe,
                                                        "NativeAcpConnection: load_session failed: {e}"
                                                    );
                                                    let _ = reply.send(Err(anyhow::anyhow!(
                                                        "NativeAcpConnection '{}': load_session failed: {e}",
                                                        agent_name_loop
                                                    )));
                                                }
                                            }
                                            drop(tx_empty);
                                            break;
                                        }
                                    }
                                }
                            }
                            AcpCommand::ListSessions { request, reply } => {
                                let result = cx.send_request(request).block_task().await;
                                let _ = reply.send(result.map_err(|e| {
                                    anyhow::anyhow!(
                                        "NativeAcpConnection '{}': list_sessions failed: {e}",
                                        agent_name_loop
                                    )
                                }));
                            }
                            AcpCommand::SetSessionMode { request, reply } => {
                                let result = cx.send_request(request).block_task().await;
                                let _ = reply.send(result.map_err(|e| {
                                    anyhow::anyhow!(
                                        "NativeAcpConnection '{}': set_session_mode failed: {e}",
                                        agent_name_loop
                                    )
                                }));
                            }
                            AcpCommand::SetSessionModel { request, reply } => {
                                let result = cx.send_request(request).block_task().await;
                                let _ = reply.send(result.map_err(|e| {
                                    anyhow::anyhow!(
                                        "NativeAcpConnection '{}': set_session_model failed: {e}",
                                        agent_name_loop
                                    )
                                }));
                            }
                            AcpCommand::SetSessionConfigOption { request, reply } => {
                                let result = cx.send_request(request).block_task().await;
                                let _ = reply.send(result.map_err(|e| {
                                    anyhow::anyhow!(
                                        "NativeAcpConnection '{}': set_session_config_option failed: {e}",
                                        agent_name_loop
                                    )
                                }));
                            }
                            AcpCommand::Authenticate { request, reply } => {
                                let result = cx.send_request(request).block_task().await;
                                let _ = reply.send(result.map_err(|e| {
                                    anyhow::anyhow!(
                                        "NativeAcpConnection '{}': authenticate failed: {e}",
                                        agent_name_loop
                                    )
                                }));
                            }
                            AcpCommand::ExtMethod { request, reply } => {
                                // The 0.11.1 SDK exposes extension calls only
                                // via the wrapping `ClientRequest` enum (whose
                                // `Response` is `serde_json::Value`). We
                                // re-wrap the response payload back into an
                                // `ExtResponse` so the caller-side translation
                                // in `call_ext` is unchanged.
                                let client_req =
                                    agent_client_protocol::ClientRequest::ExtMethodRequest(
                                        request,
                                    );
                                let result =
                                    cx.send_request(client_req).block_task().await;
                                let mapped: anyhow::Result<ExtResponse> = match result {
                                    Ok(json) => match serde_json::value::to_raw_value(&json)
                                    {
                                        Ok(raw) => Ok(ExtResponse::new(std::sync::Arc::from(
                                            raw,
                                        ))),
                                        Err(e) => Err(anyhow::anyhow!(
                                            "NativeAcpConnection '{}': ext_method response not serializable: {e}",
                                            agent_name_loop
                                        )),
                                    },
                                    Err(e) => Err(anyhow::anyhow!(
                                        "NativeAcpConnection '{}': ext_method failed: {e}",
                                        agent_name_loop
                                    )),
                                };
                                let _ = reply.send(mapped);
                            }
                        }
                    }
                    Ok(())
                })
                .await
        };

        if let Err(e) = &connect_result {
            tracing::warn!(
                agent = %agent_name,
                "NativeAcpConnection: connection ended with error: {e}"
            );
        }

        // If init never produced a response (transport died during the
        // handshake), make sure the caller blocked in `initialize()` sees an
        // error instead of waiting forever on the oneshot.
        if let Some(reply) = init_reply_slot.borrow_mut().take() {
            let err = match &connect_result {
                Err(e) => anyhow::anyhow!(
                    "NativeAcpConnection '{}': connection ended before initialize: {e}",
                    agent_name
                ),
                Ok(()) => anyhow::anyhow!(
                    "NativeAcpConnection '{}': connection closed before initialize",
                    agent_name
                ),
            };
            let _ = reply.send(Err(err));
        }

        // Kill any still-running terminals — both the explicit-shutdown path
        // and the unexpected-disconnect path share this code.
        for (id, terminal) in terminals.lock().unwrap().iter() {
            if terminal.exit_rx.borrow().is_none() {
                tracing::debug!(terminal = %id, "Killing terminal on shutdown");
                let _ = std::process::Command::new("kill")
                    .arg("-9")
                    .arg(terminal.pid.to_string())
                    .status();
            }
        }

        // Stdin has already closed (the SDK writer was dropped when the
        // connection tore down), but ACP+rmcp agents can remain alive on
        // non-stdin event loops. Send SIGTERM immediately via the process
        // group, which catches grandchildren (e.g. the `node` tree under
        // `claude-agent-acp`) that don't watch stdin themselves.
        // Take (not copy) so the Drop safety net at :349-373 sees `None`
        // and does not later signal a recycled PID.
        let pgid = child_pgid.lock().ok().and_then(|mut g| g.take());
        if let Some(pgid) = pgid {
            killpg(pgid, "TERM");
        }
        let graceful = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            child.wait(),
        )
        .await;
        match graceful {
            Ok(Ok(status)) => {
                tracing::debug!(
                    agent = %agent_name,
                    ?status,
                    "NativeAcpConnection: agent exited gracefully after SIGTERM"
                );
            }
            _ => {
                tracing::warn!(
                    agent = %agent_name,
                    "NativeAcpConnection: agent did not exit within 1s of SIGTERM; escalating"
                );
                if let Some(pgid) = pgid {
                    killpg(pgid, "KILL");
                }
                let _ = child.kill().await;
            }
        }
        // The pgid (if any) is now reaped via either branch above; clear
        // its on-disk registry record so the next-boot sweep doesn't trip
        // over a recycled pid.
        if let Some(pgid) = pgid {
            let registry = crate::orphan_registry::PgidRegistry::new(
                repo_root.join(".spur").join("pgids"),
            );
            let _ = registry.delete(pgid);
        }

        // Drain the per-child stderr bridge: child exit closed the pipe so
        // the reader task is at EOF; awaiting the join handle then dropping
        // the WorkerGuard lets `non_blocking` flush remaining chunks.
        if let Some(bridge) = stderr_bridge {
            bridge.shutdown().await;
        }
        // Mark pgid consumed so `Drop` won't re-kill a reaped or recycled
        // group id.
        if let Ok(mut guard) = child_pgid.lock() {
            *guard = None;
        }

        // Send shutdown ack only after the child has been reaped — so the
        // caller sees `Ok(())` truly mean "everything is gone".
        if let Some(reply) = shutdown_reply_slot.borrow_mut().take() {
            let _ = reply.send(Ok(()));
        }

        tracing::debug!(agent = %agent_name, "NativeAcpConnection: ACP thread exiting");
    });
}

/// Permission request handler factored out so the handler closure stays
/// small. Keeps the original 60s timeout + auto-fallback behaviour.
async fn handle_request_permission(
    args: RequestPermissionRequest,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let Some(perm_tx) = permission_tx else {
        return auto_approve(&args);
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let request = crate::types::PermissionRequest {
        args: args.clone(),
        reply_tx,
    };

    if perm_tx.send(request).is_err() {
        tracing::warn!("NativeAcpConnection: permission channel closed, auto-approving");
        return auto_approve(&args);
    }

    tracing::debug!(
        session = %args.session_id,
        "NativeAcpConnection: awaiting interactive permission response"
    );

    match tokio::time::timeout(std::time::Duration::from_secs(60), reply_rx).await {
        Ok(Ok(response)) => {
            let option_id = PermissionOptionId::new(response.option_id);
            tracing::debug!(option = %option_id, "NativeAcpConnection: permission responded");
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
            ))
        }
        Ok(Err(_)) => {
            tracing::debug!("NativeAcpConnection: permission denied (channel dropped)");
            auto_deny(&args)
        }
        Err(_) => {
            tracing::warn!("NativeAcpConnection: permission timed out (60s safety)");
            auto_deny(&args)
        }
    }
}

// ─── Terminal state ─────────────────────────────────────────────────────────

/// Per-terminal handle stored in the connection-scoped `terminals` map.
///
/// The fields are all `Send` so the entire map can sit behind an
/// `Arc<Mutex<…>>` shared by Send-bounded handler closures.
struct TerminalState {
    output: Arc<Mutex<String>>,
    truncated: Arc<AtomicBool>,
    exit_rx: tokio::sync::watch::Receiver<Option<TerminalExitStatus>>,
    pid: u32,
}

// ─── Diagnostic helpers (streaming probes) ──────────────────────────────────

/// Short static name for each SessionUpdate discriminant.
/// Used by diagnostic logging only; keep lowercase snake_case.
fn session_update_variant_name(u: &SessionUpdate) -> &'static str {
    use agent_client_protocol::schema::SessionUpdate::*;
    match u {
        AgentThoughtChunk(_) => "agent_thought_chunk",
        AgentMessageChunk(_) => "agent_message_chunk",
        UserMessageChunk(_) => "user_message_chunk",
        ToolCall(_) => "tool_call",
        ToolCallUpdate(_) => "tool_call_update",
        Plan(_) => "plan",
        AvailableCommandsUpdate(_) => "available_commands_update",
        ConfigOptionUpdate(_) => "config_option_update",
        CurrentModeUpdate(_) => "current_mode_update",
        SessionInfoUpdate(_) => "session_info_update",
        UsageUpdate(_) => "usage_update",
        _ => "other",
    }
}

/// Return the text length of a content chunk, or 0 if non-text.
fn content_chunk_text_len(chunk: &ContentChunk) -> usize {
    match &chunk.content {
        ContentBlock::Text(tc) => tc.text.len(),
        _ => 0,
    }
}

// ─── Permission helpers ─────────────────────────────────────────────────────

fn auto_approve(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    // Prefer an explicitly allow-class option. Falls back to the first
    // option (historical behavior) if no allow-class is present, and to
    // a hardcoded "allow" id if the options list is empty.
    //
    // `PermissionOptionKind` is `#[non_exhaustive]`, so the match below
    // uses a `_` arm to stay forward-compatible with future variants.
    let option_id = args
        .options
        .iter()
        .find(|o| {
            matches!(
                o.kind,
                PermissionOptionKind::AllowAlways | PermissionOptionKind::AllowOnce
            )
        })
        .map(|o| o.option_id.clone())
        .or_else(|| args.options.first().map(|o| o.option_id.clone()))
        .unwrap_or_else(|| PermissionOptionId::new("allow"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}

/// Test-only re-export of the private `auto_approve` helper so
/// integration tests under `tests/` can exercise its selection logic
/// without spawning an agent. Hidden from rustdoc; not a stability
/// surface.
#[doc(hidden)]
pub fn __test_auto_approve(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    auto_approve(args)
}

fn auto_deny(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let option_id = args
        .options
        .last()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| PermissionOptionId::new("deny"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}

// ─── Terminal helpers ────────────────────────────────────────────────────────

fn append_terminal_output(
    output: &Arc<Mutex<String>>,
    truncated: &Arc<AtomicBool>,
    byte_limit: Option<u64>,
    data: &[u8],
) {
    let text = String::from_utf8_lossy(data);
    let mut buf = output.lock().unwrap();
    buf.push_str(&text);
    if let Some(limit) = byte_limit {
        let limit = limit as usize;
        if buf.len() > limit {
            let mut start = buf.len() - limit;
            while !buf.is_char_boundary(start) {
                start += 1;
            }
            *buf = buf[start..].to_string();
            truncated.store(true, Ordering::Relaxed);
        }
    }
}

async fn terminal_reader(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    mut child: tokio::process::Child,
    output: Arc<Mutex<String>>,
    truncated: Arc<AtomicBool>,
    byte_limit: Option<u64>,
    exit_tx: tokio::sync::watch::Sender<Option<TerminalExitStatus>>,
) {
    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;

    loop {
        if stdout_done && stderr_done {
            break;
        }
        tokio::select! {
            result = AsyncReadExt::read(&mut stdout, &mut stdout_buf), if !stdout_done => {
                match result {
                    Ok(0) | Err(_) => stdout_done = true,
                    Ok(n) => append_terminal_output(&output, &truncated, byte_limit, &stdout_buf[..n]),
                }
            }
            result = AsyncReadExt::read(&mut stderr, &mut stderr_buf), if !stderr_done => {
                match result {
                    Ok(0) | Err(_) => stderr_done = true,
                    Ok(n) => append_terminal_output(&output, &truncated, byte_limit, &stderr_buf[..n]),
                }
            }
        }
    }

    let exit_status = match child.wait().await {
        Ok(status) => {
            let mut es = TerminalExitStatus::new();
            if let Some(code) = status.code() {
                es = es.exit_code(code as u32);
            }
            es
        }
        Err(_) => TerminalExitStatus::new(),
    };
    let _ = exit_tx.send(Some(exit_status));
}

#[cfg(test)]
mod client_capabilities_tests {
    use super::*;
    use agent_client_protocol::schema::ProtocolVersion;

    /// Spur must announce the explicit, non-default `ClientCapabilities`
    /// literal at initialize: fs.read/write, terminal=true, and the
    /// `_meta.terminal_output` extension that unlocks codex's tool-call
    /// meta tunnelling. See design spec §6.2.
    #[test]
    fn spur_client_capabilities_advertises_terminal_fs_and_terminal_output_meta() {
        let caps = spur_client_capabilities();

        assert!(caps.terminal, "spur supports terminal/* methods");
        assert!(
            caps.fs.read_text_file,
            "spur supports fs/read_text_file requests"
        );
        assert!(
            caps.fs.write_text_file,
            "spur supports fs/write_text_file requests"
        );

        let meta = caps
            .meta
            .as_ref()
            .expect("client meta must include terminal_output gate");
        let terminal_output = meta
            .get("terminal_output")
            .and_then(serde_json::Value::as_bool)
            .expect("meta.terminal_output must be a bool");
        assert!(
            terminal_output,
            "meta.terminal_output must be true to unlock codex tool-call meta tunneling"
        );
    }

    /// The constructed `InitializeRequest` is what spur actually sends on
    /// the wire. Serialize the full thing and confirm the negotiated
    /// `clientCapabilities` shape includes the gate codex looks for.
    #[test]
    fn initialize_request_payload_contains_explicit_client_capabilities() {
        let caps = spur_client_capabilities();
        let req = InitializeRequest::new(ProtocolVersion::LATEST).client_capabilities(caps);
        let json = serde_json::to_value(&req).expect("InitializeRequest must serialize");

        let cc = json
            .get("clientCapabilities")
            .expect("clientCapabilities must serialize");
        assert_eq!(cc.get("terminal"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(
            cc.get("fs").and_then(|v| v.get("readTextFile")),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            cc.get("fs").and_then(|v| v.get("writeTextFile")),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            cc.get("_meta").and_then(|v| v.get("terminal_output")),
            Some(&serde_json::Value::Bool(true))
        );
    }
}

#[cfg(test)]
mod set_session_model_dispatch_tests {
    use super::{decide_set_session_model_dispatch, SetSessionModelDispatch};
    use crate::connection::AgentConnection;
    use crate::SpurAgentCaps;
    use agent_client_protocol::schema::{
        InitializeResponse, ModelId, ModelInfo, NewSessionResponse, ProtocolVersion,
        SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigSelect,
        SessionConfigSelectOptions, SessionConfigValueId, SessionId, SessionModelState,
    };

    fn caps_from(modify: impl FnOnce(&mut NewSessionResponse)) -> SpurAgentCaps {
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(SessionId::new("test"));
        modify(&mut new);
        SpurAgentCaps::new(&init, &new, crate::AgentKind::CodexAcp)
    }

    fn codex_model_state() -> SessionModelState {
        SessionModelState::new(
            ModelId::new("gpt-5-codex"),
            vec![ModelInfo::new(ModelId::new("gpt-5-codex"), "GPT-5 Codex")],
        )
    }

    #[test]
    fn caps_with_models_some_routes_direct() {
        let caps = caps_from(|n| {
            *n = n.clone().models(codex_model_state());
        });
        assert!(caps.supports_set_model());
        assert!(matches!(
            decide_set_session_model_dispatch(&caps),
            SetSessionModelDispatch::Direct
        ));
    }

    #[test]
    fn caps_with_only_config_options_routes_fallback() {
        let caps = caps_from(|n| {
            n.config_options = Some(vec![SessionConfigOption::new(
                SessionConfigId::new("model"),
                "Model",
                SessionConfigKind::Select(SessionConfigSelect::new(
                    SessionConfigValueId::new("default"),
                    SessionConfigSelectOptions::Ungrouped(vec![]),
                )),
            )]);
        });
        assert!(!caps.supports_set_model());
        assert!(caps.supports_set_config_option());
        assert!(matches!(
            decide_set_session_model_dispatch(&caps),
            SetSessionModelDispatch::FallbackConfigOption
        ));
    }

    #[test]
    fn caps_with_neither_routes_unsupported() {
        let caps = caps_from(|_| {});
        assert!(!caps.supports_set_model());
        assert!(!caps.supports_set_config_option());
        assert!(matches!(
            decide_set_session_model_dispatch(&caps),
            SetSessionModelDispatch::Unsupported
        ));
    }

    #[test]
    fn models_takes_precedence_over_config_options() {
        // Codex case: both populated. Decision must pick Direct, not Fallback.
        let caps = caps_from(|n| {
            *n = n.clone().models(codex_model_state());
            n.config_options = Some(vec![SessionConfigOption::new(
                SessionConfigId::new("model"),
                "Model",
                SessionConfigKind::Select(SessionConfigSelect::new(
                    SessionConfigValueId::new("default"),
                    SessionConfigSelectOptions::Ungrouped(vec![]),
                )),
            )]);
        });
        assert!(matches!(
            decide_set_session_model_dispatch(&caps),
            SetSessionModelDispatch::Direct
        ));
    }

    #[tokio::test]
    async fn set_session_model_returns_capability_missing_when_unsupported() {
        let caps = caps_from(|_| {});
        let mut conn = super::NativeAcpConnection::new(
            "test-agent".to_string(),
            "/bin/false".to_string(),
            vec![],
            None,
        );
        let res = conn
            .set_session_model(SessionId::new("sid"), ModelId::new("m"), &caps)
            .await;
        match res {
            Err(crate::AcpError::CapabilityMissing(name)) => assert_eq!(name, "set_model"),
            other => panic!("expected CapabilityMissing(\"set_model\"), got {other:?}"),
        }
    }
}

// Note: behavioral verification of the SIGTERM-before-wait shutdown ladder
// belongs in integration tests with a real subprocess (e.g. `sleep 30` +
// signal-status capture: SIGTERM yields exit code 143, SIGKILL yields 137).
// A unit-level "regression test" that greps this file's source for symbol
// ordering passes for the wrong reasons (textual reorder, not runtime
// ordering) and silently breaks on refactor — explicitly omitted.

#[cfg(test)]
mod stderr_capture_tests {
    use super::*;

    #[test]
    fn log_path_uses_spur_logs_directory() {
        let path = build_acp_log_path("claude-code-acp");
        let s = path.to_string_lossy();
        assert!(
            s.contains(".spur/logs/"),
            "expected log under .spur/logs/, got {}",
            path.display()
        );
        assert!(
            s.ends_with("-acp.log"),
            "expected -acp.log suffix, got {}",
            path.display()
        );
        assert!(
            s.contains("claude-code-acp"),
            "expected agent name in path, got {}",
            path.display()
        );
        // PID must be embedded so concurrent spawns don't collide.
        let pid = std::process::id().to_string();
        assert!(
            s.contains(&pid),
            "expected process id {pid} in path, got {}",
            path.display()
        );
    }
}
