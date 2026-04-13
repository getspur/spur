//! `NativeAcpConnection` — wraps the official ACP SDK's `ClientSideConnection`
//! to talk native ACP over stdio to a real agent subprocess.
//!
//! # Architecture
//!
//! The official `agent-client-protocol` SDK uses `#[async_trait(?Send)]` for its
//! `Client` trait and `LocalBoxFuture` for its spawn parameter.  This means the
//! SDK's I/O loop is inherently `!Send` and cannot run on a regular Tokio task.
//!
//! We solve this by running the entire SDK connection on a dedicated OS thread
//! with its own single-threaded Tokio runtime + `LocalSet`.  The `NativeAcpConnection`
//! communicates with that thread via `tokio::sync::mpsc` and `oneshot` channels,
//! which *are* `Send`.
//!
//! # Lifecycle mapping
//!
//! | `AgentConnection` method | Behaviour |
//! |---|---|
//! | `initialize()` | Spawn the agent subprocess, create `ClientSideConnection`, run the ACP initialize handshake |
//! | `new_session()` | Send `NewSessionRequest` with cwd + MCP servers to the agent |
//! | `prompt()` | Send `PromptRequest`, bridge `SessionNotification`s from `Client::session_notification()` into the returned stream |
//! | `cancel()` | Send `CancelNotification` via the connection |
//! | `shutdown()` | Drop the connection, kill the child process |
//! | `health()` | Return cached `AgentHealth` |

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;

use tokio::io::AsyncReadExt;

use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use tokio::sync::{mpsc, oneshot};

use agent_client_protocol::{
    Agent, CancelNotification, Client, ClientSideConnection, InitializeRequest,
    InitializeResponse, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    McpServer, NewSessionRequest, NewSessionResponse, PromptRequest,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionNotification, WriteTextFileRequest, WriteTextFileResponse,
    CreateTerminalRequest, CreateTerminalResponse,
    KillTerminalRequest, KillTerminalResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse,
    TerminalOutputRequest, TerminalOutputResponse,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse,
    TerminalExitStatus, TerminalId,
};

use crate::connection::AgentConnection;
use crate::types::AgentHealth;

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
}

/// Compute the path where the ACP subprocess's stderr should be written.
/// Uses `.spur/logs/<agent>-<timestamp>-acp.log` relative to CWD.
fn build_acp_log_path(agent_name: &str) -> std::path::PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::path::PathBuf::from(".spur/logs").join(format!("{agent_name}-{ts}-acp.log"))
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
        Self {
            agent_name: agent_name.into(),
            command: command.into(),
            extra_args,
            cmd_tx: None,
            thread_handle: None,
            health_status: AgentHealth::Unknown,
            permission_tx,
        }
    }
}

#[async_trait]
impl AgentConnection for NativeAcpConnection {
    // ─── initialize ─────────────────────────────────────────────────────

    async fn initialize(
        &mut self,
        request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
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
        let handle = std::thread::Builder::new()
            .name(format!("acp-{}", agent_name))
            .spawn(move || {
                acp_thread_main(thread_agent_name, command, extra_args, cmd_rx, permission_tx);
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
            anyhow::anyhow!(
                "NativeAcpConnection '{}': not initialized",
                self.agent_name
            )
        })?;

        let mut request = NewSessionRequest::new(&cwd);
        request.mcp_servers = mcp_servers;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx.send(AcpCommand::NewSession {
            request,
            reply: reply_tx,
        }).map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died",
                self.agent_name
            )
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
            anyhow::anyhow!(
                "NativeAcpConnection '{}': not initialized",
                self.agent_name
            )
        })?;

        let session_id = request.session_id.clone();

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "NativeAcpConnection: sending prompt"
        );

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx.send(AcpCommand::Prompt {
            request,
            reply: reply_tx,
        }).map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died",
                self.agent_name
            )
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
            anyhow::anyhow!(
                "NativeAcpConnection '{}': not initialized",
                self.agent_name
            )
        })?;

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "NativeAcpConnection: cancelling session"
        );

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx.send(AcpCommand::Cancel {
            session_id: session_id.to_string(),
            reply: reply_tx,
        }).map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died",
                self.agent_name
            )
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

    // ─── load_session ────────────────────────────────────────────────────

    async fn load_session(
        &mut self,
        request: LoadSessionRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        let cmd_tx = self.cmd_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': not initialized",
                self.agent_name
            )
        })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx.send(AcpCommand::LoadSession {
            request,
            reply: reply_tx,
        }).map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died",
                self.agent_name
            )
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
            anyhow::anyhow!(
                "NativeAcpConnection '{}': not initialized",
                self.agent_name
            )
        })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        cmd_tx.send(AcpCommand::ListSessions {
            request,
            reply: reply_tx,
        }).map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died",
                self.agent_name
            )
        })?;

        reply_rx.await.map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': ACP thread died during list_sessions",
                self.agent_name
            )
        })?
    }
}

// ─── Dedicated ACP thread ───────────────────────────────────────────────────

/// Entry point for the dedicated thread that owns the `!Send` ACP connection.
///
/// This function creates its own single-threaded Tokio runtime + `LocalSet`
/// and runs the SDK's I/O loop alongside a command handler that processes
/// requests from the main thread.
fn acp_thread_main(
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    mut cmd_rx: mpsc::UnboundedReceiver<AcpCommand>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
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
        let stderr_cfg = match std::fs::OpenOptions::new()
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
                    "NativeAcpConnection: failed to open stderr log; falling back to inherit",
                );
                std::process::Stdio::inherit()
            }
        };

        let child_result = tokio::process::Command::new(&command)
            .args(&extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr_cfg)
            .spawn();

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

        // Wrap tokio AsyncRead/Write into futures AsyncRead/Write using compat.
        let stdout_compat = tokio_util::compat::TokioAsyncReadCompatExt::compat(child_stdout);
        let stdin_compat = tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(child_stdin);

        // Create the notification channel for bridging session updates.
        // We use a shared sender so the SpurAcpClient can clone it per-prompt.
        let (notification_tx, _initial_rx) = mpsc::unbounded_channel::<SessionNotification>();

        // Hold the current notification sender in an Rc<RefCell> so we can swap
        // it per-prompt call.
        let notification_tx = std::rc::Rc::new(std::cell::RefCell::new(notification_tx));
        let notification_tx_for_client = notification_tx.clone();

        // Build the SpurAcpClient that handles callbacks from the agent.
        // We use a wrapper that reads the current notification_tx from the RefCell.
        let spur_client = SpurAcpClientDynamic {
            notification_tx: notification_tx_for_client,
            cwd: std::rc::Rc::new(std::cell::RefCell::new(PathBuf::from("."))),
            permission_tx,
            terminals: std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())),
        };
        let cwd_ref = spur_client.cwd.clone();
        let terminals_ref = spur_client.terminals.clone();

        // Create the ClientSideConnection.
        let (connection, io_future) = ClientSideConnection::new(
            spur_client,
            stdin_compat,
            stdout_compat,
            |fut| {
                tokio::task::spawn_local(fut);
            },
        );

        // Spawn the I/O future on the local set.
        let agent_name_io = agent_name.clone();
        tokio::task::spawn_local(async move {
            if let Err(e) = io_future.await {
                tracing::warn!(
                    agent = %agent_name_io,
                    "NativeAcpConnection: I/O loop ended with error: {e}"
                );
            }
        });

        // Send the initialize request.
        let init_result = connection.initialize(init_request).await;
        match init_result {
            Ok(response) => {
                let _ = init_reply.send(Ok(response));
            }
            Err(e) => {
                let _ = init_reply.send(Err(anyhow::anyhow!(
                    "NativeAcpConnection '{}': initialize failed: {e}",
                    agent_name
                )));
                return;
            }
        }

        // Now process commands in a loop.
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                AcpCommand::Initialize { reply, .. } => {
                    let _ = reply.send(Err(anyhow::anyhow!(
                        "NativeAcpConnection '{}': already initialized",
                        agent_name
                    )));
                }
                AcpCommand::NewSession { request, reply } => {
                    // Update the cwd for filesystem operations.
                    *cwd_ref.borrow_mut() = request.cwd.clone();
                    let result = connection.new_session(request).await;
                    let _ = reply.send(result.map_err(|e| {
                        anyhow::anyhow!("NativeAcpConnection '{}': new_session failed: {e}", agent_name)
                    }));
                }
                AcpCommand::Prompt { request, reply } => {
                    // Create a fresh notification channel for this prompt.
                    let (tx, rx) = mpsc::unbounded_channel::<SessionNotification>();
                    *notification_tx.borrow_mut() = tx;

                    // Send the receiver back immediately so the caller can
                    // start consuming notifications.
                    let _ = reply.send(Ok(rx));

                    // Now call prompt — this blocks until the turn completes.
                    // During this time, session_notification() calls will
                    // forward to the channel above.
                    let agent_name_prompt = agent_name.clone();
                    let session_id_for_probe = request.session_id.clone();
                    let _prompt_result = connection.prompt(request).await;
                    match &_prompt_result {
                        Ok(_) => {
                            tracing::debug!(
                                agent = %agent_name_prompt,
                                "NativeAcpConnection: prompt completed successfully"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent = %agent_name_prompt,
                                "NativeAcpConnection: prompt failed: {e}"
                            );
                        }
                    }

                    // When prompt() returns, the notification channel sender
                    // gets replaced on the next prompt call (or dropped on
                    // shutdown), which will close the stream for the consumer.
                    // We explicitly drop the current sender to signal completion.
                    tracing::debug!(
                        streaming_probe = true,
                        site = "B_dead_tx_swap",
                        which = "prompt_end",
                        agent = %agent_name_prompt,
                        session = %session_id_for_probe,
                        "notification_tx -> dead_tx (prompt returned)"
                    );
                    let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
                    *notification_tx.borrow_mut() = dead_tx;
                }
                AcpCommand::Cancel { session_id, reply } => {
                    let cancel = CancelNotification::new(session_id);
                    let result = connection.cancel(cancel).await;
                    let _ = reply.send(result.map_err(|e| {
                        anyhow::anyhow!("NativeAcpConnection '{}': cancel failed: {e}", agent_name)
                    }));
                }
                AcpCommand::Shutdown { reply } => {
                    tracing::debug!(agent = %agent_name, "NativeAcpConnection: ACP thread received shutdown");
                    // Kill all spawned terminals.
                    for (id, terminal) in terminals_ref.borrow().iter() {
                        if terminal.exit_rx.borrow().is_none() {
                            tracing::debug!(terminal = %id, "Killing terminal on shutdown");
                            let _ = std::process::Command::new("kill")
                                .arg("-9")
                                .arg(terminal.pid.to_string())
                                .status();
                        }
                    }
                    // Kill the child process.
                    let _ = child.kill().await;
                    let _ = reply.send(Ok(()));
                    break;
                }
                AcpCommand::LoadSession { request, reply } => {
                    // Update cwd to match the loaded session's cwd.
                    *cwd_ref.borrow_mut() = request.cwd.clone();

                    // Create a fresh notification channel for the load_session history stream.
                    let (tx, rx) = mpsc::unbounded_channel::<SessionNotification>();
                    *notification_tx.borrow_mut() = tx;

                    // Send the receiver back immediately.
                    let _ = reply.send(Ok(rx));

                    // Call load_session — this delivers historical notifications via the
                    // Client::session_notification callback while it runs.
                    let agent_name_load = agent_name.clone();
                    let session_id_for_probe = request.session_id.clone();
                    let _load_result = connection.load_session(request).await;
                    match &_load_result {
                        Ok(_) => {
                            tracing::debug!(
                                agent = %agent_name_load,
                                "NativeAcpConnection: load_session completed"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                agent = %agent_name_load,
                                "NativeAcpConnection: load_session failed: {e}"
                            );
                        }
                    }

                    // Signal stream completion.
                    tracing::debug!(
                        streaming_probe = true,
                        site = "B_dead_tx_swap",
                        which = "load_session_end",
                        agent = %agent_name_load,
                        session = %session_id_for_probe,
                        "notification_tx -> dead_tx (load_session returned)"
                    );
                    let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
                    *notification_tx.borrow_mut() = dead_tx;
                }
                AcpCommand::ListSessions { request, reply } => {
                    let result = connection.list_sessions(request).await;
                    let _ = reply.send(result.map_err(|e| {
                        anyhow::anyhow!("NativeAcpConnection '{}': list_sessions failed: {e}", agent_name)
                    }));
                }
            }
        }

        tracing::debug!(agent = %agent_name, "NativeAcpConnection: ACP thread exiting");
    });
}

// ─── SpurAcpClientDynamic ───────────────────────────────────────────────────

struct TerminalState {
    output: std::rc::Rc<std::cell::RefCell<String>>,
    truncated: std::rc::Rc<Cell<bool>>,
    exit_rx: tokio::sync::watch::Receiver<Option<TerminalExitStatus>>,
    pid: u32,
}

/// A variant of `SpurAcpClient` that reads the notification sender and cwd
/// from `Rc<RefCell<_>>` so they can be swapped per-prompt.
///
/// This is necessary because `ClientSideConnection::new` takes ownership of
/// the client, but we need to change the notification destination for each
/// prompt call.
struct SpurAcpClientDynamic {
    notification_tx: std::rc::Rc<std::cell::RefCell<mpsc::UnboundedSender<SessionNotification>>>,
    cwd: std::rc::Rc<std::cell::RefCell<PathBuf>>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
    terminals: std::rc::Rc<std::cell::RefCell<HashMap<String, TerminalState>>>,
}

#[async_trait::async_trait(?Send)]
impl Client for SpurAcpClientDynamic {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> agent_client_protocol::Result<RequestPermissionResponse> {
        let Some(ref perm_tx) = self.permission_tx else {
            return auto_approve(&args);
        };

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
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
                let option_id = agent_client_protocol::PermissionOptionId::new(response.option_id);
                tracing::debug!(option = %option_id, "NativeAcpConnection: permission responded");
                Ok(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(option_id),
                    ),
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

    async fn session_notification(
        &self,
        args: SessionNotification,
    ) -> agent_client_protocol::Result<()> {
        let variant = session_update_variant_name(&args.update);
        let text_len = match &args.update {
            agent_client_protocol::SessionUpdate::AgentMessageChunk(c)
            | agent_client_protocol::SessionUpdate::AgentThoughtChunk(c)
            | agent_client_protocol::SessionUpdate::UserMessageChunk(c) => {
                content_chunk_text_len(c)
            }
            _ => 0,
        };
        let session = args.session_id.to_string();
        let send_result = self.notification_tx.borrow().send(args);
        let send_result_str = if send_result.is_ok() { "ok" } else { "err" };
        tracing::debug!(
            streaming_probe = true,
            site = "A_session_notification",
            variant = variant,
            text_len = text_len,
            session = %session,
            send_result = send_result_str,
            "ACP session_notification"
        );
        Ok(())
    }

    async fn read_text_file(
        &self,
        args: ReadTextFileRequest,
    ) -> agent_client_protocol::Result<ReadTextFileResponse> {
        let cwd = self.cwd.borrow().clone();
        let path = if args.path.is_absolute() {
            args.path.clone()
        } else {
            cwd.join(&args.path)
        };

        tracing::debug!(
            path = %path.display(),
            "NativeAcpConnection: reading text file"
        );

        let content = std::fs::read_to_string(&path).map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("Failed to read {}: {e}", path.display()))
        })?;

        let content = match (args.line, args.limit) {
            (Some(start_line), Some(limit)) => {
                let start = (start_line.saturating_sub(1)) as usize;
                content
                    .lines()
                    .skip(start)
                    .take(limit as usize)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            (Some(start_line), None) => {
                let start = (start_line.saturating_sub(1)) as usize;
                content
                    .lines()
                    .skip(start)
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            (None, Some(limit)) => content
                .lines()
                .take(limit as usize)
                .collect::<Vec<_>>()
                .join("\n"),
            (None, None) => content,
        };

        Ok(ReadTextFileResponse::new(content))
    }

    async fn write_text_file(
        &self,
        args: WriteTextFileRequest,
    ) -> agent_client_protocol::Result<WriteTextFileResponse> {
        let cwd = self.cwd.borrow().clone();
        let path = if args.path.is_absolute() {
            args.path.clone()
        } else {
            cwd.join(&args.path)
        };

        tracing::debug!(
            path = %path.display(),
            content_len = args.content.len(),
            "NativeAcpConnection: writing text file"
        );

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                agent_client_protocol::Error::internal_error()
                    .data(format!("Failed to create directories for {}: {e}", path.display()))
            })?;
        }

        std::fs::write(&path, &args.content).map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("Failed to write {}: {e}", path.display()))
        })?;

        Ok(WriteTextFileResponse::new())
    }

    async fn create_terminal(
        &self,
        args: CreateTerminalRequest,
    ) -> agent_client_protocol::Result<CreateTerminalResponse> {
        let cwd = args.cwd.clone().unwrap_or_else(|| self.cwd.borrow().clone());
        let byte_limit = args.output_byte_limit.or(Some(10 * 1024 * 1024));

        let mut cmd = tokio::process::Command::new(&args.command);
        cmd.args(&args.args)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for env_var in &args.env {
            cmd.env(&env_var.name, &env_var.value);
        }

        let mut child = cmd.spawn().map_err(|e| {
            agent_client_protocol::Error::internal_error()
                .data(format!("Failed to spawn '{}': {e}", args.command))
        })?;
        let pid = child.id().ok_or_else(|| {
            agent_client_protocol::Error::internal_error().data("Failed to get process ID")
        })?;
        let child_stdout = child.stdout.take().ok_or_else(|| {
            agent_client_protocol::Error::internal_error().data("Failed to capture stdout")
        })?;
        let child_stderr = child.stderr.take().ok_or_else(|| {
            agent_client_protocol::Error::internal_error().data("Failed to capture stderr")
        })?;

        let output = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let truncated = std::rc::Rc::new(std::cell::Cell::new(false));
        let (exit_tx, exit_rx) = tokio::sync::watch::channel(None);

        tokio::task::spawn_local(terminal_reader(
            child_stdout, child_stderr, child,
            output.clone(), truncated.clone(), byte_limit, exit_tx,
        ));

        let terminal_id = TerminalId::new(uuid::Uuid::new_v4().to_string());
        let id_string = terminal_id.to_string();
        tracing::debug!(terminal = %id_string, command = %args.command, pid = pid, "Terminal created");
        self.terminals.borrow_mut().insert(id_string, TerminalState { output, truncated, exit_rx, pid });
        Ok(CreateTerminalResponse::new(terminal_id))
    }

    async fn terminal_output(
        &self,
        args: TerminalOutputRequest,
    ) -> agent_client_protocol::Result<TerminalOutputResponse> {
        let key = args.terminal_id.to_string();
        let map = self.terminals.borrow();
        let terminal = map.get(&key).ok_or_else(|| {
            agent_client_protocol::Error::invalid_params().data(format!("Terminal '{}' not found", key))
        })?;
        let output = terminal.output.borrow().clone();
        let truncated = terminal.truncated.get();
        let exit_status = terminal.exit_rx.borrow().clone();
        Ok(TerminalOutputResponse::new(output, truncated).exit_status(exit_status))
    }

    async fn wait_for_terminal_exit(
        &self,
        args: WaitForTerminalExitRequest,
    ) -> agent_client_protocol::Result<WaitForTerminalExitResponse> {
        let key = args.terminal_id.to_string();
        let mut exit_rx = {
            let map = self.terminals.borrow();
            let terminal = map.get(&key).ok_or_else(|| {
                agent_client_protocol::Error::invalid_params().data(format!("Terminal '{}' not found", key))
            })?;
            terminal.exit_rx.clone()
        };
        if let Some(status) = exit_rx.borrow().clone() {
            return Ok(WaitForTerminalExitResponse::new(status));
        }
        loop {
            match exit_rx.changed().await {
                Ok(()) => {
                    if let Some(status) = exit_rx.borrow().clone() {
                        return Ok(WaitForTerminalExitResponse::new(status));
                    }
                }
                Err(_) => {
                    let status = exit_rx.borrow().clone().unwrap_or_else(TerminalExitStatus::new);
                    return Ok(WaitForTerminalExitResponse::new(status));
                }
            }
        }
    }

    async fn kill_terminal(
        &self,
        args: KillTerminalRequest,
    ) -> agent_client_protocol::Result<KillTerminalResponse> {
        let key = args.terminal_id.to_string();
        let (pid, is_running) = {
            let map = self.terminals.borrow();
            let terminal = map.get(&key).ok_or_else(|| {
                agent_client_protocol::Error::invalid_params().data(format!("Terminal '{}' not found", key))
            })?;
            let is_running = terminal.exit_rx.borrow().is_none();
            (terminal.pid, is_running)
        };
        if is_running {
            tracing::debug!(terminal = %key, pid = pid, "Killing terminal");
            let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
        Ok(KillTerminalResponse::new())
    }

    async fn release_terminal(
        &self,
        args: ReleaseTerminalRequest,
    ) -> agent_client_protocol::Result<ReleaseTerminalResponse> {
        let key = args.terminal_id.to_string();
        let pid_to_kill = {
            let map = self.terminals.borrow();
            if let Some(terminal) = map.get(&key) {
                if terminal.exit_rx.borrow().is_none() { Some(terminal.pid) } else { None }
            } else { None }
        };
        if let Some(pid) = pid_to_kill {
            tracing::debug!(terminal = %key, pid = pid, "Killing terminal on release");
            let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
        self.terminals.borrow_mut().remove(&key);
        Ok(ReleaseTerminalResponse::new())
    }
}

// ─── Diagnostic helpers (streaming probes) ──────────────────────────────────

/// Short static name for each SessionUpdate discriminant.
/// Used by diagnostic logging only; keep lowercase snake_case.
fn session_update_variant_name(u: &agent_client_protocol::SessionUpdate) -> &'static str {
    use agent_client_protocol::SessionUpdate::*;
    match u {
        AgentThoughtChunk(_) => "agent_thought_chunk",
        AgentMessageChunk(_) => "agent_message_chunk",
        UserMessageChunk(_) => "user_message_chunk",
        ToolCall(_) => "tool_call",
        ToolCallUpdate(_) => "tool_call_update",
        Plan(_) => "plan",
        AvailableCommandsUpdate(_) => "available_commands_update",
        CurrentModeUpdate(_) => "current_mode_update",
        _ => "other",
    }
}

/// Return the text length of a content chunk, or 0 if non-text.
fn content_chunk_text_len(chunk: &agent_client_protocol::ContentChunk) -> usize {
    match &chunk.content {
        agent_client_protocol::ContentBlock::Text(tc) => tc.text.len(),
        _ => 0,
    }
}

// ─── Permission helpers ─────────────────────────────────────────────────────

fn auto_approve(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let option_id = args
        .options
        .first()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("allow"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}

fn auto_deny(
    args: &RequestPermissionRequest,
) -> agent_client_protocol::Result<RequestPermissionResponse> {
    let option_id = args
        .options
        .last()
        .map(|o| o.option_id.clone())
        .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("deny"));
    Ok(RequestPermissionResponse::new(
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
    ))
}

// ─── Terminal helpers ────────────────────────────────────────────────────────

fn append_terminal_output(
    output: &std::rc::Rc<std::cell::RefCell<String>>,
    truncated: &std::rc::Rc<Cell<bool>>,
    byte_limit: Option<u64>,
    data: &[u8],
) {
    let text = String::from_utf8_lossy(data);
    let mut buf = output.borrow_mut();
    buf.push_str(&text);
    if let Some(limit) = byte_limit {
        let limit = limit as usize;
        if buf.len() > limit {
            let mut start = buf.len() - limit;
            while !buf.is_char_boundary(start) {
                start += 1;
            }
            *buf = buf[start..].to_string();
            truncated.set(true);
        }
    }
}

async fn terminal_reader(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    mut child: tokio::process::Child,
    output: std::rc::Rc<std::cell::RefCell<String>>,
    truncated: std::rc::Rc<Cell<bool>>,
    byte_limit: Option<u64>,
    exit_tx: tokio::sync::watch::Sender<Option<TerminalExitStatus>>,
) {
    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];
    let mut stdout_done = false;
    let mut stderr_done = false;

    loop {
        if stdout_done && stderr_done { break; }
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
mod stderr_capture_tests {
    use super::*;

    #[test]
    fn log_path_uses_spur_logs_directory() {
        let path = build_acp_log_path("claude-code-acp");
        assert!(
            path.to_string_lossy().contains(".spur/logs/"),
            "expected log under .spur/logs/, got {}",
            path.display()
        );
        assert!(
            path.to_string_lossy().ends_with("-acp.log"),
            "expected -acp.log suffix, got {}",
            path.display()
        );
        assert!(
            path.to_string_lossy().contains("claude-code-acp"),
            "expected agent name in path, got {}",
            path.display()
        );
    }
}
