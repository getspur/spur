use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use spur_acp::config::SpurConfig;
use spur_acp::connection::{AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter};
use spur_acp::registry::AgentRegistry;
use spur_acp::types::*;
use spur_acp::{
    DelegationResult, DelegationStatus, LifecycleState, ReviewKind, ReviewPayload, SpurEvent,
    SpurEventBody, TimeoutFallback,
};
use spur_pm::Issue;

use agent_client_protocol::{
    ContentBlock, InitializeRequest, ListSessionsRequest, McpServer, McpServerStdio,
    PromptRequest, ProtocolVersion, SessionInfo, SessionUpdate, SetSessionModeRequest,
    TextContent,
};

use spur_cost::CostTracker;
use spur_mcp::{DelegationChannel, DelegationRequest, McpCallbackServer, WorkerInfo};
use spur_pm::adapter::PmAdapter;
use spur_pm::GitHubAdapter;
use spur_worktree::WorktreeManager;

use crate::review_sink::{ReviewSink, ReviewSinkError};
use crate::lineage::ExecutorId;

// ─── Run options ─────────────────────────────────────────────────────

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

/// Holds the state of an active brain session.
pub struct BrainSession {
    pub connection: Box<dyn AgentConnection>,
    pub acp_session_id: String,
    pub spur_session_id: SessionId,
    pub brain_name: String,
    pub mcp_server: Arc<McpCallbackServer>,
    pub delegation_handle: JoinHandle<()>,
}

/// A user input message from the TUI.
pub enum InteractiveInput {
    Message { blocks: Vec<ContentBlock>, interrupt: bool },
    /// Spawn a fresh brain session and send these blocks as the first prompt
    /// atomically. If a brain is already attached, it is shut down first.
    /// Empty `blocks` means spawn-only with no first prompt.
    NewSessionWithMessage { blocks: Vec<ContentBlock>, interrupt: bool },
    ListSessions,
    ResumeSession { session_id: String },
    /// Request `set_session_mode` on the active brain session. No-op if
    /// there is no active brain session.
    SetSessionMode { mode_id: String },
    /// Invoke an agent vendor-extension RPC on the active brain session.
    /// No-op if there is no active brain session. The method name and params
    /// are chosen by the TUI's config-driven dispatch path — the
    /// orchestrator is agnostic to specific extensions. `sessionId` is
    /// injected into `params` here (the TUI doesn't know ACP session IDs).
    VendorExec {
        session: SessionId,
        method: String,
        params: serde_json::Value,
    },
    /// Submit a human review decision. Routed to the ReviewSink by the
    /// dispatcher task, not handled inline in `run_interactive`.
    SubmitReview {
        executor_id: String,
        attempt_n: u32,
        decision: spur_acp::ReviewDecision,
    },
}

// ─── Orchestrator ────────────────────────────────────────────────────

/// The central orchestrator that drives the brain-worker pipeline.
pub struct Orchestrator {
    pub registry: AgentRegistry,
    pub config: SpurConfig,
    pub worktrees: WorktreeManager,
    pub cost_tracker: Option<CostTracker>,
    pub event_tx: broadcast::Sender<SpurEvent>,
    pub review_sink: ReviewSink,  // Clone type, shares inner Arc<Mutex>
    repo_root: PathBuf,
}

impl Orchestrator {
    /// Create a new orchestrator for the given repo directory.
    pub fn new(repo_root: PathBuf, config: SpurConfig) -> Result<Self> {
        let registry = AgentRegistry::load(config.agents.entries.clone());
        let worktrees = WorktreeManager::new(repo_root.clone());

        // Try to open cost tracker (non-fatal if it fails).
        let cost_tracker = {
            let db_path = shellexpand_tilde(&config.cost.db_path);
            if let Some(parent) = Path::new(&db_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match CostTracker::open(Path::new(&db_path)) {
                Ok(ct) => Some(ct),
                Err(e) => {
                    warn!(error = %e, "Failed to open cost database, cost tracking disabled");
                    None
                }
            }
        };

        let (event_tx, _) = broadcast::channel(256);
        let review_sink = ReviewSink::new();

        Ok(Self {
            registry,
            config,
            worktrees,
            cost_tracker,
            event_tx,
            review_sink,
            repo_root,
        })
    }

    /// Subscribe to orchestrator events (for TUI, logging, etc.).
    pub fn subscribe(&self) -> broadcast::Receiver<SpurEvent> {
        self.event_tx.subscribe()
    }

    /// Classify an error as an auth-required failure.
    ///
    /// The ACP spec reserves error code `-32000` with `authRequired`-shaped
    /// data payloads for this, but in practice the agent_client_protocol
    /// crate surfaces it as a stringly-typed error. Claude Code's wrapper
    /// also prints human-readable prompts. Match on substrings.
    fn is_auth_required_error(e: &anyhow::Error) -> bool {
        let msg = e.to_string().to_lowercase();
        msg.contains("authrequired")
            || msg.contains("auth_required")
            || msg.contains("please run /login")
            || msg.contains("run `/login`")
            || msg.contains("run /login")
    }

    /// Human-readable banner text for auth-required failures.
    fn auth_required_banner() -> String {
        "Claude Code requires authentication. Run `claude /login` in a \
         terminal, then restart this session. Press any key to dismiss."
            .to_string()
    }

    /// Run an ad-hoc task through the brain agent.
    pub async fn run_adhoc(&mut self, task: &str, opts: RunOpts) -> Result<RunResult> {
        let start = Instant::now();
        let session_id = SessionId::new();

        // 1. Resolve brain agent.
        let brain_name = opts
            .brain
            .as_deref()
            .unwrap_or(&self.config.brain.default)
            .to_string();

        let brain_config = self
            .registry
            .get(&brain_name)
            .ok_or_else(|| anyhow!("Brain agent '{}' not found in registry", brain_name))?
            .clone();

        info!(brain = %brain_name, session = %session_id, "Starting ad-hoc run");
        self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        }));

        // 2. Optionally fetch issue context.
        let issue_context = if let Some(ref issue_ref) = opts.issue {
            match self.fetch_issue_context(issue_ref).await {
                Ok(issue) => {
                    self.emit(SpurEvent::now(SpurEventBody::IssueReceived {
                        source: format!("{:?}", issue.source),
                        id: issue.id.clone(),
                    }));
                    Some(issue)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to fetch issue context, proceeding without it");
                    None
                }
            }
        } else {
            None
        };

        // 3. Build brain prompt.
        let prompt_text = self.build_brain_prompt(task, issue_context.as_ref());

        // 4. Start MCP callback server.
        let (mcp_server, delegation_channel) = McpCallbackServer::new(&session_id);
        let mut mcp_server = mcp_server;

        // Populate available workers.
        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .iter()
            .map(|c| WorkerInfo {
                name: c.name.clone(),
                description: c.capabilities.join(", "),
                cost_tier: c.cost_tier,
            })
            .collect();
        mcp_server.set_workers(workers);

        let mcp_endpoint = mcp_server.endpoint();
        let mcp_server = Arc::new(mcp_server);
        let _mcp_handle = mcp_server
            .clone()
            .start()
            .context("Failed to start MCP callback server")?;

        // 5. Log session start.
        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                &brain_name,
                "brain",
                None,
                task,
                self.config.project.as_ref().map(|p| p.name.as_str()),
                opts.issue.as_deref(),
            );
        }

        // 6. Spawn brain agent via AgentConnection.
        let mut connection = self.create_connection(&brain_config, None);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        let _capabilities = connection
            .initialize(init_request)
            .await
            .context("Failed to initialize brain agent")?;

        debug!(
            brain = %brain_name,
            "Brain agent initialized"
        );

        // Build MCP server config for the SPUR callback server (stdio-based UDS).
        // The MCP callback server exposes a Unix domain socket; we model it as a
        // stdio-based MCP server whose command is `socat` connecting to the socket.
        // However, the cleaner approach per ACP spec is to pass the socket path as
        // a stdio MCP server that the agent can connect to.
        let mcp_servers = vec![McpServer::Stdio(
            McpServerStdio::new("spur-mcp", &mcp_endpoint.socket_path)
                .args(Vec::new()),
        )];

        let session_response = crate::skip_perm::new_session_with_bypass(
            &mut *connection,
            &brain_config,
            self.repo_root.clone(),
            mcp_servers,
        )
        .await
        .context("Failed to create brain session")?;

        // 7. Send prompt and stream events.
        let prompt_request = PromptRequest::new(
            session_response.session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(prompt_text.clone()))],
        );

        let mut stream = connection
            .prompt(prompt_request)
            .await
            .context("Failed to send prompt to brain")?;

        // 8. Process brain output + delegation callbacks concurrently.
        let pr_url: Option<String> = None;
        let success = true;

        // Spawn delegation handler.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.event_tx.clone(),
            self.review_sink.clone(),
        ));

        // Stream brain output.
        while let Some(notification) = stream.next().await {
            match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        print!("{}", tc.text);
                    }
                }
                SessionUpdate::ToolCall(tc) => {
                    debug!(tool = %tc.title, "Brain calling tool");
                }
                _ => {}
            }

            self.emit(SpurEvent::now(SpurEventBody::AgentNotification {
                session: session_id.clone(),
                notification: Box::new(notification),
            }));
        }

        // 9. Clean up.
        let _ = connection.shutdown().await;
        let _ = mcp_server.shutdown();
        delegation_handle.abort();

        let duration = start.elapsed();

        // 10. Log session end.
        if let Some(ref ct) = self.cost_tracker {
            let status = if success { "completed" } else { "failed" };
            let _ = ct.end_session(&session_id, status, duration, brain_config.cost_tier);
        }

        let total_cost = spur_cost::estimator::estimate_cost(brain_config.cost_tier, duration);

        self.emit(SpurEvent::now(SpurEventBody::SessionCompleted {
            session: session_id.clone(),
            success,
        }));

        println!();
        info!(
            session = %session_id,
            duration_secs = duration.as_secs(),
            cost_usd = format!("{:.2}", total_cost),
            "Run complete"
        );

        Ok(RunResult {
            session_id,
            success,
            pr_url,
            total_cost_usd: total_cost,
        })
    }

    /// Run an interactive session: multi-turn loop that accepts user input
    /// between brain turns. Used by `spur watch`.
    pub async fn run_interactive(
        mut self,
        mut user_input_rx: mpsc::Receiver<InteractiveInput>,
        brain_override: Option<String>,
        permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Result<()> {
        let mut brain: Option<BrainSession> = None;
        let mut pending_messages: VecDeque<InteractiveInput> = VecDeque::new();
        // Pre-connected (initialized) agent connection, ready for create_brain_session
        // or load_brain_session without re-running connect_brain.
        let mut agent_connection: Option<(Box<dyn spur_acp::AgentConnection>, String)> = None;

        loop {
            // ── Get next input (from queue or user) ────────────────────
            let input = if !pending_messages.is_empty() {
                pending_messages.pop_front().unwrap()
            } else {
                match user_input_rx.recv().await {
                    Some(i) => i,
                    None => break, // TUI closed
                }
            };

            match input {
                // ── ListSessions ────────────────────────────────────────
                InteractiveInput::ListSessions => {
                    // Connect if we don't already have an initialized connection.
                    let (mut conn, brain_name) = match agent_connection.take() {
                        Some(existing) => existing,
                        None => {
                            match self
                                .connect_brain(brain_override.as_deref(), permission_tx.clone())
                                .await
                            {
                                Ok(pair) => pair,
                                Err(e) => {
                                    error!(error = %e, "Failed to connect brain for list_sessions");
                                    self.emit(SpurEvent::now(SpurEventBody::SessionsListError {
                                        message: e.to_string(),
                                    }));
                                    continue;
                                }
                            }
                        }
                    };

                    // Scope to the repo's cwd so we get project-local sessions, not every
                    // session the agent has ever tracked. The ACP SDK treats an absent
                    // cwd as "list everything globally" (for claude-agent-acp: all 194
                    // sessions across all projects vs the 37 that belong to this repo).
                    let list_req = ListSessionsRequest::new().cwd(self.repo_root.clone());
                    let sessions_result = match conn.list_sessions(list_req).await {
                        Ok(response) => Ok(response.sessions),
                        Err(e) => {
                            // Fallback: read sessions from agent's local storage.
                            warn!(error = %e, "list_sessions failed, trying filesystem fallback");
                            Self::list_sessions_from_disk(&brain_name)
                        }
                    };

                    match sessions_result {
                        Ok(sessions) => {
                            self.emit(SpurEvent::now(SpurEventBody::SessionsListed {
                                agent: brain_name.clone(),
                                sessions,
                            }));
                        }
                        Err(e) => {
                            error!(error = %e, "list_sessions failed (no fallback available)");
                            self.emit(SpurEvent::now(SpurEventBody::SessionsListError {
                                message: e.to_string(),
                            }));
                        }
                    }

                    // Stash the connection for future use.
                    agent_connection = Some((conn, brain_name));
                }

                // ── ResumeSession ───────────────────────────────────────
                InteractiveInput::ResumeSession { session_id } => {
                    // If a brain is already active, retire its session-level state so
                    // the incoming ResumeSession replaces it cleanly. The initialized
                    // connection is preserved in `agent_connection` for reuse below.
                    Self::retire_active_brain(&mut brain, &mut agent_connection);

                    // Use pre-connected or connect fresh.
                    let (connection, brain_name) = match agent_connection.take() {
                        Some(existing) => existing,
                        None => {
                            match self
                                .connect_brain(brain_override.as_deref(), permission_tx.clone())
                                .await
                            {
                                Ok(pair) => pair,
                                Err(e) => {
                                    error!(error = %e, "Failed to connect brain for resume");
                                    self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                        session: SessionId::new(),
                                        message: e.to_string(),
                                    }));
                                    continue;
                                }
                            }
                        }
                    };

                    let original_session_id = session_id.clone();
                    match self
                        .load_brain_session(connection, brain_name, permission_tx.clone(), session_id)
                        .await
                    {
                        Ok((session, mut history_stream)) => {
                            let spur_id = session.spur_session_id.clone();
                            // Drain history stream (populated if load_session worked,
                            // empty if we fell back to new_session).
                            let mut history_count = 0usize;
                            while let Some(notification) = history_stream.next().await {
                                history_count += 1;
                                self.emit(SpurEvent::now(SpurEventBody::AgentNotification {
                                    session: spur_id.clone(),
                                    notification: Box::new(notification),
                                }));
                            }

                            // If no history came from the agent (new_session fallback),
                            // replay conversation from disk so the user sees context.
                            if history_count == 0 {
                                let entries = Self::read_session_history_from_disk(&original_session_id);
                                if !entries.is_empty() {
                                    info!(count = entries.len(), "Replaying conversation history from disk");
                                    self.emit(SpurEvent::now(SpurEventBody::SessionHistory {
                                        session: spur_id.clone(),
                                        entries,
                                    }));
                                }
                            }

                            brain = Some(session);
                            self.emit(SpurEvent::now(SpurEventBody::TurnComplete { session: spur_id }));
                        }
                        Err(e) => {
                            error!(error = %e, "Failed to load brain session");
                            self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                session: SessionId::new(),
                                message: e.to_string(),
                            }));
                        }
                    }
                }

                // ── VendorExec ───────────────────────────────────────────
                InteractiveInput::VendorExec { session, method, mut params } => {
                    if let Some(b) = brain.as_mut() {
                        // Inject ACP session ID — TUI doesn't know it.
                        // Contract: submit_router always produces a JSON object.
                        // Warn (don't fail) if a future args_template emits a
                        // non-object — the call still goes through, minus sessionId.
                        if let Some(obj) = params.as_object_mut() {
                            obj.insert(
                                "sessionId".into(),
                                serde_json::json!(b.acp_session_id),
                            );
                        } else {
                            warn!(
                                method = %method,
                                "VendorExec params is not a JSON object; sessionId not injected"
                            );
                        }
                        match b.connection.call_ext(&method, params).await {
                            Ok(resp) => {
                                self.emit(SpurEvent::now(
                                    SpurEventBody::AgentExtNotification {
                                        session: session.clone(),
                                        method: format!("{}/response", method),
                                        params: resp,
                                    },
                                ));
                            }
                            Err(e) => {
                                warn!(
                                    brain = %b.brain_name,
                                    method = %method,
                                    error = %e,
                                    "vendor exec call failed"
                                );
                                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                    session,
                                    message: format!(
                                        "vendor exec `{}` failed: {}", method, e
                                    ),
                                }));
                            }
                        }
                    } else {
                        warn!(method = %method, "VendorExec received but no active brain session");
                    }
                }

                // ── SetSessionMode ───────────────────────────────────────
                InteractiveInput::SetSessionMode { mode_id } => {
                    if let Some(b) = brain.as_mut() {
                        let req = SetSessionModeRequest::new(
                            agent_client_protocol::SessionId::new(b.acp_session_id.clone()),
                            agent_client_protocol::SessionModeId::new(
                                std::sync::Arc::<str>::from(mode_id.as_str()),
                            ),
                        );
                        if let Err(e) = b.connection.set_session_mode(req).await {
                            warn!(
                                brain = %b.brain_name,
                                session_id = %b.spur_session_id,
                                mode_id = %mode_id,
                                error = %e,
                                "set_session_mode failed"
                            );
                        }
                    } else {
                        warn!(mode_id = %mode_id, "SetSessionMode received but no active brain session");
                    }
                }

                // ── Message ─────────────────────────────────────────────
                InteractiveInput::Message { blocks, interrupt } => {
                    // Flatten interrupt messages (they were queued during streaming).
                    // When interrupt is true, the user typed `!…`; strip the leading
                    // bang from the *first* text block so downstream agents don't see it.
                    let blocks = if interrupt {
                        strip_bang_prefix(blocks)
                    } else {
                        blocks
                    };

                    // ── Lazy-spawn brain on first message (or after crash) ──
                    if brain.is_none() {
                        // Use pre-connected agent if available; otherwise connect_brain.
                        let result = match agent_connection.take() {
                            Some((connection, brain_name)) => {
                                self.create_brain_session(connection, brain_name, permission_tx.clone())
                                    .await
                            }
                            None => {
                                self.spawn_brain_session(brain_override.as_deref(), permission_tx.clone())
                                    .await
                            }
                        };

                        match result {
                            Ok(b) => brain = Some(b),
                            Err(e) => {
                                error!(error = %e, "Failed to spawn brain");
                                if Self::is_auth_required_error(&e) {
                                    self.emit(SpurEvent::now(SpurEventBody::AuthRequired {
                                        session: SessionId(String::new()),
                                        message: Self::auth_required_banner(),
                                    }));
                                } else {
                                    self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                        session: SessionId::new(),
                                        message: e.to_string(),
                                    }));
                                }
                                continue;
                            }
                        }
                    }
                    let b = brain.as_mut().unwrap();

                    // ── Send prompt ─────────────────────────────────────
                    let prompt_request = PromptRequest::new(
                        b.acp_session_id.clone(),
                        blocks,
                    );

                    let prompt_started_at = std::time::Instant::now();
                    let mut stream = match b.connection.prompt(prompt_request).await {
                        Ok(s) => s,
                        Err(e) => {
                            error!(error = %e, "Brain prompt failed");
                            if Self::is_auth_required_error(&e) {
                                self.emit(SpurEvent::now(SpurEventBody::AuthRequired {
                                    session: b.spur_session_id.clone(),
                                    message: Self::auth_required_banner(),
                                }));
                            } else {
                                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                    session: b.spur_session_id.clone(),
                                    message: e.to_string(),
                                }));
                            }
                            b.delegation_handle.abort();
                            let _ = b.connection.shutdown().await;
                            brain = None;
                            continue;
                        }
                    };

                    // ── Stream output + check for interrupts ────────────
                    let mut cancel_deadline: Option<tokio::time::Instant> = None;

                    loop {
                        tokio::select! {
                            item = stream.next() => {
                                match item {
                                    Some(notification) => {
                                        let variant = match &notification.update {
                                            spur_acp::SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
                                            spur_acp::SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
                                            spur_acp::SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
                                            spur_acp::SessionUpdate::ToolCall(_) => "tool_call",
                                            spur_acp::SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
                                            spur_acp::SessionUpdate::Plan(_) => "plan",
                                            spur_acp::SessionUpdate::AvailableCommandsUpdate(_) => "available_commands_update",
                                            spur_acp::SessionUpdate::CurrentModeUpdate(_) => "current_mode_update",
                                            _ => "other",
                                        };
                                        let text_len = match &notification.update {
                                            spur_acp::SessionUpdate::AgentMessageChunk(c)
                                            | spur_acp::SessionUpdate::AgentThoughtChunk(c)
                                            | spur_acp::SessionUpdate::UserMessageChunk(c) => {
                                                match &c.content {
                                                    spur_acp::ContentBlock::Text(tc) => tc.text.len(),
                                                    _ => 0,
                                                }
                                            }
                                            _ => 0,
                                        };
                                        tracing::debug!(
                                            streaming_probe = true,
                                            site = "C_orchestrator_emit",
                                            variant = variant,
                                            text_len = text_len,
                                            since_prompt_ms = prompt_started_at.elapsed().as_millis() as u64,
                                            session = %b.spur_session_id,
                                            "orchestrator emitting AgentNotification"
                                        );
                                        self.emit(SpurEvent::now(SpurEventBody::AgentNotification {
                                            session: b.spur_session_id.clone(),
                                            notification: Box::new(notification),
                                        }));
                                    }
                                    None => break, // Turn complete
                                }
                            }
                            Some(queued) = user_input_rx.recv() => {
                                match queued {
                                    InteractiveInput::Message { blocks: msg_blocks, interrupt: msg_interrupt } => {
                                        if msg_interrupt {
                                            let _ = b.connection.cancel(&b.acp_session_id).await;
                                            cancel_deadline = Some(
                                                tokio::time::Instant::now()
                                                    + std::time::Duration::from_secs(5),
                                            );
                                        }
                                        let queued_blocks = if msg_interrupt {
                                            strip_bang_prefix(msg_blocks)
                                        } else {
                                            msg_blocks
                                        };
                                        pending_messages.push_back(InteractiveInput::Message {
                                            blocks: queued_blocks,
                                            interrupt: false,
                                        });
                                    }
                                    other => {
                                        // Queue non-message inputs for after streaming completes.
                                        pending_messages.push_back(other);
                                    }
                                }
                            }
                            _ = async {
                                match cancel_deadline {
                                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                                    None => futures::future::pending().await,
                                }
                            } => {
                                warn!("Cancel timeout — force-ending stream");
                                break;
                            }
                        }
                    }

                    // Emit turn complete
                    self.emit(SpurEvent::now(SpurEventBody::TurnComplete {
                        session: b.spur_session_id.clone(),
                    }));
                }

                // ── NewSessionWithMessage ────────────────────────────────
                // Explicit "spawn a fresh brain and send first prompt atomically".
                // Shut down any existing brain first, then delegate to the
                // Message arm's lazy-spawn path by pushing back onto the queue.
                // Empty `blocks` means spawn-only (no first prompt) — we still
                // re-queue so the Message arm owns the spawn logic uniformly.
                InteractiveInput::NewSessionWithMessage { blocks, interrupt } => {
                    // Retire the active brain (if any) but preserve the initialized
                    // connection for the next Message arm's lazy-spawn to reuse.
                    Self::retire_active_brain(&mut brain, &mut agent_connection);

                    if blocks.is_empty() {
                        // Spawn-only: no prompt. Leave brain=None; the next Message
                        // will lazy-spawn using the preserved agent_connection.
                        info!("NewSessionWithMessage with empty blocks — spawn deferred to next Message");
                    } else {
                        pending_messages.push_back(InteractiveInput::Message { blocks, interrupt });
                    }
                }

                // ── SubmitReview ─────────────────────────────────────────
                // Intentional no-op: spur-cli routes SubmitReview to the
                // review_dispatcher_loop task, not to run_interactive. If
                // it somehow arrives here (e.g., in tests that send directly
                // to user_rx), we silently discard it to avoid double-routing.
                InteractiveInput::SubmitReview { .. } => {}
            }
        }

        // ── Cleanup ─────────────────────────────────────────────────────
        if let Some(mut b) = brain.take() {
            b.delegation_handle.abort();
            let _ = b.connection.shutdown().await;
            let _ = b.mcp_server.shutdown();
        }
        // Drop any pre-connected but unused connection.
        if let Some((mut conn, _)) = agent_connection.take() {
            let _ = conn.shutdown().await;
        }

        info!("Interactive session ended");
        Ok(())
    }

    /// Execute a task directly on a single agent (no brain, no delegation).
    pub async fn exec_direct(
        &mut self,
        agent_name: &str,
        task: &str,
    ) -> Result<RunResult> {
        let start = Instant::now();
        let session_id = SessionId::new();

        let agent_config = self
            .registry
            .get(agent_name)
            .ok_or_else(|| anyhow!("Agent '{}' not found in registry", agent_name))?
            .clone();

        info!(agent = %agent_name, session = %session_id, "Direct execution");

        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                agent_name,
                "worker",
                None,
                task,
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        let mut connection = self.create_connection(&agent_config, None);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        connection
            .initialize(init_request)
            .await
            .context("Failed to initialize agent")?;

        let session_response = crate::skip_perm::new_session_with_bypass(
            &mut *connection,
            &agent_config,
            self.repo_root.clone(),
            vec![],
        )
        .await
        .context("Failed to create agent session")?;

        let prompt_request = PromptRequest::new(
            session_response.session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(task.to_string()))],
        );

        let mut stream = connection.prompt(prompt_request).await?;

        let success = true;
        while let Some(notification) = stream.next().await {
            match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        print!("{}", tc.text);
                    }
                }
                _ => {}
            }
        }

        let _ = connection.shutdown().await;
        let duration = start.elapsed();

        if let Some(ref ct) = self.cost_tracker {
            let status = if success { "completed" } else { "failed" };
            let _ = ct.end_session(&session_id, status, duration, agent_config.cost_tier);
        }

        let total_cost = spur_cost::estimator::estimate_cost(agent_config.cost_tier, duration);
        println!();

        Ok(RunResult {
            session_id,
            success,
            pr_url: None,
            total_cost_usd: total_cost,
        })
    }

    /// Initialize: scan $PATH for known agents, populate registry.
    pub async fn init_agents(&mut self) -> Result<Vec<String>> {
        struct SeedAgent {
            name: &'static str,
            command: &'static str,
            args: Vec<&'static str>,
            transport: TransportKind,
            /// L1a mechanism: CLI args appended when skip_permissions is on.
            /// Empty means this agent's bypass is not a CLI flag.
            skip_permissions_args: Vec<&'static str>,
            /// L1b mechanism: ACP session mode set after new_session when
            /// skip_permissions is on. None means this agent's bypass is
            /// not an ACP session mode.
            skip_permissions_session_mode: Option<&'static str>,
        }

        let known_agents = [
            SeedAgent {
                name: "kiro",
                command: "kiro-cli",
                args: vec!["acp"],
                transport: TransportKind::Acp,
                skip_permissions_args: vec!["--trust-all-tools"],
                skip_permissions_session_mode: None,
            },
            SeedAgent {
                name: "claude-code",
                command: "claude",
                args: vec![
                    "-p",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--include-partial-messages",
                    "--permission-mode",
                    "acceptEdits",
                ],
                transport: TransportKind::StreamJson,
                skip_permissions_args: vec!["--dangerously-skip-permissions"],
                skip_permissions_session_mode: None,
            },
            SeedAgent {
                name: "claude-code-acp",
                command: "npx",
                args: vec!["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"],
                transport: TransportKind::Acp,
                // The npx wrapper takes no CLI flags — bypass is via
                // ACP session mode (verified in acp-agent.js source
                // and probed live, see design doc).
                skip_permissions_args: vec![],
                skip_permissions_session_mode: Some("bypassPermissions"),
            },
            SeedAgent {
                name: "codex",
                command: "codex",
                args: vec!["--acp"],
                transport: TransportKind::Acp,
                // Unknown bypass mechanism; operator can set
                // skip_permissions=true and get L2-only (every ACP
                // permission request silently auto-approved).
                skip_permissions_args: vec![],
                skip_permissions_session_mode: None,
            },
            SeedAgent {
                name: "gemini",
                command: "gemini",
                args: vec![],
                transport: TransportKind::CliWrap,
                skip_permissions_args: vec![],
                skip_permissions_session_mode: None,
            },
        ];

        let mut found = Vec::new();

        for seed in &known_agents {
            let which = tokio::process::Command::new("which")
                .arg(seed.command)
                .output()
                .await;

            if let Ok(output) = which {
                if output.status.success() {
                    let config = spur_acp::config::AgentConfig {
                        name: seed.name.to_string(),
                        command: seed.command.to_string(),
                        args: seed.args.iter().map(|s| s.to_string()).collect(),
                        transport: seed.transport,
                        role: AgentRole::Both,
                        capabilities: vec![],
                        cost_tier: CostTier::Medium,
                        rate_limit_window: None,
                        review: Default::default(),
                        display: Default::default(),
                        commands: Default::default(),
                        permissions: Default::default(),
                        skip_permissions: false,
                        skip_permissions_args: seed
                            .skip_permissions_args
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        skip_permissions_session_mode: seed
                            .skip_permissions_session_mode
                            .map(String::from),
                    };
                    self.registry.register(config);
                    found.push(seed.name.to_string());
                    info!(agent = %seed.name, command = %seed.command, "Found agent");
                }
            }
        }

        Ok(found)
    }

    /// Health-check all registered agents.
    pub async fn check_agents(&mut self) -> Vec<(String, AgentHealth)> {
        let agents: Vec<_> = self.registry.list().into_iter().cloned().collect();
        let mut results = Vec::new();

        for config in &agents {
            let mut connection = self.create_connection(config, None);
            let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
            let health = match connection.initialize(init_request).await {
                Ok(_) => {
                    let _ = connection.shutdown().await;
                    AgentHealth::Ready
                }
                Err(e) => AgentHealth::Error(e.to_string()),
            };
            results.push((config.name.clone(), health));
        }

        // Update health after iteration to avoid borrow conflict.
        for (name, health) in &results {
            self.registry.set_health(name, health.clone());
        }

        results
    }

    // ─── Private helpers ─────────────────────────────────────────────

    /// Retire the currently-active brain session's ephemeral state
    /// (delegation handler task, MCP server) while preserving the
    /// initialized ACP connection in `agent_connection` for reuse by the
    /// next `load_brain_session` / `create_brain_session`.
    ///
    /// Called at the top of any arm that replaces the current brain
    /// (`ResumeSession`, `NewSessionWithMessage`). Saves the cost of
    /// tearing down and reinitializing the agent subprocess on every
    /// session switch — for claude-code-acp that's ~1-3s of node startup
    /// per switch.
    ///
    /// The old ACP session id on the agent side is abandoned silently;
    /// the ACP protocol has no `close_session` and most agents treat
    /// unreferenced sessions as inert.
    fn retire_active_brain(
        brain: &mut Option<BrainSession>,
        agent_connection: &mut Option<(Box<dyn spur_acp::AgentConnection>, String)>,
    ) {
        if let Some(b) = brain.take() {
            b.delegation_handle.abort();
            let _ = b.mcp_server.shutdown();
            *agent_connection = Some((b.connection, b.brain_name));
        }
    }

    /// Resolve and initialize a brain agent connection without starting a full session.
    ///
    /// Steps: resolve brain name from config → get brain_config from registry →
    /// create connection → initialize. Returns (connection, brain_name).
    async fn connect_brain(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Result<(Box<dyn spur_acp::AgentConnection>, String)> {
        let brain_name = brain_override
            .unwrap_or(&self.config.brain.default)
            .to_string();

        let brain_config = self
            .registry
            .get(&brain_name)
            .ok_or_else(|| anyhow!("Brain agent '{}' not found in registry", brain_name))?
            .clone();

        let mut connection = self.create_connection(&brain_config, permission_tx);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        connection
            .initialize(init_request)
            .await
            .context("Failed to initialize brain agent")?;

        debug!(brain = %brain_name, "Brain agent connected and initialized");
        Ok((connection, brain_name))
    }

    /// Create a full brain session from an already-initialized connection.
    ///
    /// Emits BrainSpawned, starts MCP callback server, logs session start,
    /// calls new_session, spawns delegation handler. Returns BrainSession.
    async fn create_brain_session(
        &mut self,
        mut connection: Box<dyn spur_acp::AgentConnection>,
        brain_name: String,
        _permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Result<BrainSession> {
        let session_id = SessionId::new();

        info!(brain = %brain_name, session = %session_id, "Creating brain session");
        self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        }));

        // Start MCP callback server.
        let (mcp_server, delegation_channel) = McpCallbackServer::new(&session_id);
        let mut mcp_server = mcp_server;

        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .iter()
            .map(|c| WorkerInfo {
                name: c.name.clone(),
                description: c.capabilities.join(", "),
                cost_tier: c.cost_tier,
            })
            .collect();
        mcp_server.set_workers(workers);

        let mcp_endpoint = mcp_server.endpoint();
        let mcp_server = Arc::new(mcp_server);
        let _mcp_handle = mcp_server
            .clone()
            .start()
            .context("Failed to start MCP callback server")?;

        // Log session start.
        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                &brain_name,
                "brain",
                None,
                "(interactive)",
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        let mcp_servers = vec![McpServer::Stdio(
            McpServerStdio::new("spur-mcp", &mcp_endpoint.socket_path)
                .args(Vec::new()),
        )];

        let brain_cfg = self
            .registry
            .get(&brain_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!(
                "brain agent '{}' not in registry during create_brain_session",
                brain_name
            ))?;

        let session_response = crate::skip_perm::new_session_with_bypass(
            &mut *connection,
            &brain_cfg,
            self.repo_root.clone(),
            mcp_servers,
        )
        .await
        .context("Failed to create brain session")?;

        // Spawn delegation handler.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.event_tx.clone(),
            self.review_sink.clone(),
        ));

        // Spawn the vendor-extension notification pump (if the transport
        // supports it). Each payload becomes a `SpurEventBody::AgentExtNotification`
        // scoped to this brain session.
        if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
            let event_tx = self.event_tx.clone();
            let spur_session_id = session_id.clone();
            tokio::spawn(async move {
                while let Some(payload) = ext_rx.recv().await {
                    let _ = event_tx.send(SpurEvent::now(
                        SpurEventBody::AgentExtNotification {
                            session: spur_session_id.clone(),
                            method: payload.method,
                            params: payload.params,
                        },
                    ));
                }
            });
        }

        self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session_id.clone(),
            acp_session_id: session_response.session_id.to_string(),
            brain: brain_name.clone(),
            resumed: false,
        }));

        Ok(BrainSession {
            connection,
            acp_session_id: session_response.session_id.to_string(),
            spur_session_id: session_id,
            brain_name,
            mcp_server,
            delegation_handle,
        })
    }

    /// Load an existing session and return a BrainSession + history stream.
    ///
    /// Similar to create_brain_session but calls load_session instead of new_session.
    /// The history stream delivers past session notifications (historical context).
    async fn load_brain_session(
        &mut self,
        mut connection: Box<dyn spur_acp::AgentConnection>,
        brain_name: String,
        _permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
        acp_session_id: String,
    ) -> Result<(BrainSession, std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>>)> {
        let session_id = SessionId::new();

        info!(brain = %brain_name, session = %session_id, acp_session = %acp_session_id, "Loading brain session");
        self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        }));

        // Start MCP callback server.
        let (mcp_server, delegation_channel) = McpCallbackServer::new(&session_id);
        let mut mcp_server = mcp_server;

        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .iter()
            .map(|c| WorkerInfo {
                name: c.name.clone(),
                description: c.capabilities.join(", "),
                cost_tier: c.cost_tier,
            })
            .collect();
        mcp_server.set_workers(workers);

        let mcp_endpoint = mcp_server.endpoint();
        let mcp_server = Arc::new(mcp_server);
        let _mcp_handle = mcp_server
            .clone()
            .start()
            .context("Failed to start MCP callback server")?;

        // Log session start.
        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                &brain_name,
                "brain",
                None,
                "(resumed)",
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        let mcp_servers = vec![McpServer::Stdio(
            McpServerStdio::new("spur-mcp", &mcp_endpoint.socket_path)
                .args(Vec::new()),
        )];

        let brain_cfg = self
            .registry
            .get(&brain_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!(
                "brain agent '{}' not in registry during load_brain_session",
                brain_name
            ))?;

        // Try load_session first. If the agent doesn't support it (e.g. kiro-cli),
        // fall back to new_session so we have a working session for subsequent prompts.
        // The historical conversation is displayed from the disk fallback in either case.
        let (final_acp_session_id, history_stream, resumed) = match crate::skip_perm::load_session_with_bypass(
            &mut *connection,
            &brain_cfg,
            acp_session_id.clone(),
            self.repo_root.clone(),
            mcp_servers.clone(),
        )
        .await
        {
            Ok(stream) => {
                debug!(brain = %brain_name, "load_session succeeded");
                (acp_session_id, Some(stream), true)
            }
            Err(e) => {
                warn!(brain = %brain_name, error = %e, "load_session failed, falling back to new_session");
                let session_response = crate::skip_perm::new_session_with_bypass(
                    &mut *connection,
                    &brain_cfg,
                    self.repo_root.clone(),
                    mcp_servers,
                )
                .await
                .context("Failed to create fallback session after load_session failure")?;
                (session_response.session_id.to_string(), None, false)
            }
        };

        // Spawn delegation handler.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.event_tx.clone(),
            self.review_sink.clone(),
        ));

        // Pump vendor-extension notifications onto the event stream.
        if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
            let event_tx = self.event_tx.clone();
            let spur_session_id = session_id.clone();
            tokio::spawn(async move {
                while let Some(payload) = ext_rx.recv().await {
                    let _ = event_tx.send(SpurEvent::now(
                        SpurEventBody::AgentExtNotification {
                            session: spur_session_id.clone(),
                            method: payload.method,
                            params: payload.params,
                        },
                    ));
                }
            });
        }

        self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session_id.clone(),
            acp_session_id: final_acp_session_id.clone(),
            brain: brain_name.clone(),
            resumed,
        }));

        let brain_session = BrainSession {
            connection,
            acp_session_id: final_acp_session_id,
            spur_session_id: session_id,
            brain_name,
            mcp_server,
            delegation_handle,
        };

        // Return an empty stream if we fell back to new_session.
        let stream: std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>> =
            match history_stream {
                Some(s) => s,
                None => Box::pin(futures::stream::empty()),
            };

        Ok((brain_session, stream))
    }

    /// Spawn a brain agent session with MCP callback server and delegation handler.
    pub async fn spawn_brain_session(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Result<BrainSession> {
        let (connection, brain_name) = self
            .connect_brain(brain_override, permission_tx.clone())
            .await?;
        self.create_brain_session(connection, brain_name, permission_tx).await
    }

    /// Fallback: read sessions from an agent's local storage on disk.
    /// Currently supports kiro-cli (~/.kiro/sessions/cli/*.json).
    fn list_sessions_from_disk(agent_name: &str) -> Result<Vec<SessionInfo>> {
        // kiro-cli stores sessions in ~/.kiro/sessions/cli/<uuid>.json
        if agent_name.contains("kiro") {
            let home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let sessions_dir = home.join(".kiro/sessions/cli");

            if !sessions_dir.exists() {
                return Ok(Vec::new());
            }

            let mut sessions: Vec<SessionInfo> = Vec::new();
            for entry in std::fs::read_dir(&sessions_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }

                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Parse the minimal fields we need from kiro's session format.
                let json: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let session_id = match json.get("session_id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => continue,
                };
                let cwd = json
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = json
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let updated_at = json
                    .get("updated_at")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let mut info = SessionInfo::new(session_id, PathBuf::from(cwd));
                info = info.title(title);
                info = info.updated_at(updated_at);
                sessions.push(info);
            }

            // Sort by updated_at descending (most recent first).
            sessions.sort_by(|a, b| {
                let a_time = a.updated_at.as_deref().unwrap_or("");
                let b_time = b.updated_at.as_deref().unwrap_or("");
                b_time.cmp(a_time)
            });

            info!(count = sessions.len(), "Loaded sessions from kiro disk storage");
            return Ok(sessions);
        }

        anyhow::bail!("No filesystem fallback available for agent '{}'", agent_name)
    }

    /// Read conversation history from a kiro session's JSONL file on disk.
    /// Returns (role, text) pairs for Prompt and AssistantMessage entries.
    fn read_session_history_from_disk(session_uuid: &str) -> Vec<spur_acp::HistoryEntry> {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let jsonl_path = home.join(format!(".kiro/sessions/cli/{}.jsonl", session_uuid));

        let content = match std::fs::read_to_string(&jsonl_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let mut entries = Vec::new();
        for line in content.lines() {
            let json: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let kind = json.get("kind").and_then(|v| v.as_str()).unwrap_or("");

            // Concatenate ALL text content blocks (messages can have multiple).
            let text = json
                .pointer("/data/content")
                .and_then(|arr| arr.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let item_kind = item.get("kind").and_then(|v| v.as_str())?;
                            if item_kind == "text" {
                                item.get("data").and_then(|v| v.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();

            if text.is_empty() {
                continue;
            }

            match kind {
                "Prompt" => entries.push(spur_acp::HistoryEntry {
                    role: "user".into(),
                    text,
                }),
                "AssistantMessage" => entries.push(spur_acp::HistoryEntry {
                    role: "assistant".into(),
                    text,
                }),
                _ => {} // Skip ToolResults, etc. for v1
            }
        }
        entries
    }

    fn create_connection(
        &self,
        config: &spur_acp::config::AgentConfig,
        permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
    ) -> Box<dyn AgentConnection> {
        // L1a: effective_args folds skip_permissions_args into the spawn
        // args when bypass is on.
        let args = config.effective_args();
        let perms = config.effective_permissions();
        // L2: when bypass is on, short-circuit permission requests by
        // passing None, which activates spur-acp's auto_approve fast-path.
        // Only meaningful for transports that surface ACP permission
        // callbacks (ACP native); other transports ignore the value.
        let perm_tx = if perms.skip { None } else { permission_tx };

        build_connection_from_transport(config, args, perm_tx)
    }

    fn build_brain_prompt(&self, task: &str, issue: Option<&Issue>) -> String {
        let mut prompt = String::new();

        // System instructions.
        prompt.push_str(
            "You are coordinating a coding task. You have two kinds of tools:\n\
             \n\
             1. Your own tools (filesystem, bash, git) — use these to investigate and code directly.\n\
             2. SPUR delegation tools — use these to hand work to specialized worker agents.\n\
             \n\
             When to delegate vs do it yourself:\n\
             - Delegate when subtasks are INDEPENDENT and can run in parallel\n\
             - Delegate to match agent strengths\n\
             - Do it yourself for quick tasks or when you need tight iterative control\n\
             - Always review worker output before approving\n\n",
        );

        // Issue context.
        if let Some(issue) = issue {
            prompt.push_str(&format!(
                "## Issue #{}: {}\n\n{}\n\nLabels: {}\nStatus: {}\n\n",
                issue.id,
                issue.title,
                issue.body,
                issue.labels.join(", "),
                issue.status,
            ));
        }

        // Project-specific context.
        if let Some(ref append) = self.config.brain.prompt.append {
            prompt.push_str(&format!("## Project Context\n\n{}\n\n", append));
        }

        // Task.
        prompt.push_str(&format!("## Task\n\n{}\n", task));

        prompt
    }

    async fn fetch_issue_context(&self, issue_ref: &str) -> Result<Issue> {
        // Parse "github:owner/repo#42" format.
        if let Some(rest) = issue_ref.strip_prefix("github:") {
            if let Some((repo, id)) = rest.rsplit_once('#') {
                let adapter = GitHubAdapter::new(Some(repo.to_string()));
                return adapter.get_issue(id).await;
            }
        }
        Err(anyhow!(
            "Unsupported issue reference format: '{}'. Expected: github:owner/repo#42",
            issue_ref
        ))
    }

    fn emit(&self, event: SpurEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Handle delegation requests from the MCP callback server.
    ///
    /// Spawns each delegation as a separate tokio task, allowing multiple
    /// workers to run concurrently. A semaphore limits the number of
    /// simultaneous workers to `max_concurrent`.
    async fn handle_delegations(
        mut channel: DelegationChannel,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        max_concurrent: usize,
        event_tx: broadcast::Sender<SpurEvent>,
        review_sink: ReviewSink,
    ) {
        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        while let Some(request) = channel.request_rx.recv().await {
            // Destructure the request — it is not Clone, so we move each field.
            let DelegationRequest {
                id: request_id,
                agent,
                task,
                context_files,
                respond_to,
            } = request;

            debug!(
                agent = %agent,
                task = %task,
                "Received delegation request"
            );

            let repo_root = repo_root.clone();
            let agent_configs = agent_configs.clone();
            let semaphore = Arc::clone(&semaphore);
            let event_tx = event_tx.clone();
            let review_sink = review_sink.clone();

            tokio::spawn(async move {
                // Acquire a permit before starting the delegation.
                let _permit = match semaphore.acquire().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        error!("Semaphore closed — aborting delegation");
                        return;
                    }
                };

                // No outer timeout: the review gate's own `review_timeout`
                // bounds review waits (default 30 min, configurable per
                // agent). A previous hardcoded 300s outer timeout always
                // fired before the 1800s default review timeout, cancelling
                // the delegation mid-`select!`, dropping the ReviewSink
                // entry's receiver without emitting Resolved/TimedOut, and
                // returning `DelegationStatus::Timeout` (worker-hang) to
                // the brain. That broke the spec's worker `Timeout`
                // (hang) vs review `TimedOut` (nobody reviewed) split and
                // left the TUI stuck on `AwaitingReview` because
                // `DelegationCompleted` was never emitted for the right
                // session. v1 accepts that worker-hang detection is not
                // automatic — separate concern, separate fix.
                let (result, executor_id_opt) = Self::execute_delegation(
                    agent,
                    task,
                    context_files,
                    request_id,
                    repo_root,
                    agent_configs,
                    event_tx.clone(),
                    review_sink.clone(),
                )
                .await;

                if let Err(_returned_result) = respond_to.send(result) {
                    // Brain's MCP tool call was cancelled — the oneshot
                    // receiver was dropped before we could deliver the
                    // result. If a review was still pending on this
                    // delegation, emit an audit event so the lineage
                    // projection records the abandonment rather than
                    // leaving an orphaned review card indefinitely.
                    if let Some(ref eid) = executor_id_opt {
                        cleanup_cancelled_review(
                            eid,
                            "brain call cancelled",
                            &event_tx,
                            &review_sink,
                        )
                        .await;
                    }
                }
            });
        }
    }

    /// Execute a single delegation request.
    ///
    /// This method is fully self-contained: it creates its own
    /// `WorktreeManager` and `AgentRegistry` so it can run in an
    /// independent tokio task without shared mutable state.
    ///
    /// ## Retry loop (Task 10)
    ///
    /// When `agent_config.review.review_required == true` and the
    /// reviewer returns `ReviewDecision::Retry { new_constraints }`,
    /// this method despawns the current worker, appends the constraints
    /// to the original task, bumps `attempt_n`, emits
    /// `ExecutorRetryStarted`, and re-enters the worker-spawn +
    /// review-gate flow. Bounded by
    /// `agent_config.review.max_review_retries`. On exceed, returns
    /// `Failed { error: "retry limit exceeded after N attempts" }`.
    ///
    /// `executor_id` is stable across attempts (captured from the first
    /// worker session) so the lineage projection's attempt history
    /// accumulates on a single node.
    // TODO: consolidate args into an ExecuteDelegationParams struct to reduce arity.
    #[allow(clippy::too_many_arguments)]
    async fn execute_delegation(
        agent: String,
        original_task: String,
        _context_files: Vec<String>,
        request_id: String,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        event_tx: broadcast::Sender<SpurEvent>,
        review_sink: ReviewSink,
    ) -> (DelegationResult, Option<ExecutorId>) {
        // Special agent names for PM operations (from MCP server).
        if agent.starts_with("__") {
            return (
                DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("PM operations not yet wired: {}", agent),
                    },
                    diff: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                },
                None,
            );
        }

        let registry = AgentRegistry::load(agent_configs);

        let agent_config = match registry.get(&agent) {
            Some(c) => c.clone(),
            None => {
                return (
                    DelegationResult {
                        status: DelegationStatus::Failed {
                            error: format!("Worker agent '{}' not found", agent),
                        },
                        diff: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                    },
                    None,
                );
            }
        };

        let mut current_task = original_task.clone();
        let mut attempt_n: u32 = 1;
        // Stable across retries; captured from the first worker session.
        let mut executor_id: Option<ExecutorId> = None;
        // Accumulated cost across all attempts in this delegation.
        let mut total_cost: f64 = 0.0;

        // WorktreeManager owned here (not inside run_one_worker_attempt)
        // so execute_delegation can make post-gate commit/remove decisions.
        // Each delegation task gets its own manager (concurrent delegations
        // do not share mutable state). Retries reuse the same manager.
        let mut worktrees = WorktreeManager::new(repo_root);

        // Worker session for the *next* attempt. Generated here (not
        // inside run_one_worker_attempt) so the Retry arm can emit
        // ExecutorRetryStarted.new_session_id matching the session id
        // the next attempt will actually use — closing the lineage
        // Attempt.session_id ↔ worker event linkage.
        let first_worker_session = SessionId::new();
        let mut next_worker_session = first_worker_session;

        loop {
            let outcome = match run_one_worker_attempt(
                next_worker_session.clone(),
                &agent,
                &current_task,
                &request_id,
                &agent_config,
                &mut worktrees,
                &event_tx,
            )
            .await
            {
                Ok(o) => o,
                Err(setup_err) => {
                    // Setup failures short-circuit the entire
                    // delegation without retry — retrying a
                    // worktree-creation failure is not spec'd
                    // behavior. We still call finalize so
                    // DelegationCompleted is emitted (the worker
                    // session was named, even if no worker actually
                    // ran).
                    return (
                        finalize(
                            &event_tx,
                            next_worker_session,
                            DelegationStatus::Failed {
                                error: setup_err.to_string(),
                            },
                            None,
                            None,
                            total_cost,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            total_cost += outcome.cost;

            // On first attempt, capture executor_id from worker_session.
            if executor_id.is_none() {
                executor_id = Some(ExecutorId::new(outcome.worker_session.0.clone()));
            }
            let eid = executor_id.clone().unwrap();

            // No review gate — commit/remove then emit DelegationCompleted.
            if !agent_config.review.review_required {
                apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &outcome.candidate_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &event_tx,
                        outcome.worker_session,
                        outcome.candidate_status,
                        outcome.diff,
                        outcome.summary,
                        total_cost,
                    ),
                    executor_id.clone(),
                );
            }

            // Review gate: register FIRST, then emit events.
            // `ReviewSink` requires register-before-emit so a TUI
            // cannot race a `SubmitReview` past an unregistered sink.
            let rx = match register_gate(eid.clone(), attempt_n, &review_sink).await {
                Ok(rx) => rx,
                Err(e) => {
                    tracing::error!(
                        executor_id = %eid.0,
                        attempt_n,
                        error = %e,
                        "review_sink registration failed — skipping review gate"
                    );
                    // Worker DID run; emit DelegationCompleted via
                    // finalize so the lineage projection records the
                    // terminal Failed status (preserves the
                    // "every terminal emits DelegationCompleted"
                    // invariant). Registration failure → Failed (not
                    // preserved; no useful diff to inspect).
                    let failed_status = DelegationStatus::Failed {
                        error: format!("review registration failed: {e}"),
                    };
                    apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &failed_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &event_tx,
                            outcome.worker_session,
                            failed_status,
                            outcome.diff,
                            outcome.summary,
                            total_cost,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
                id: eid.0.clone(),
                phase: LifecycleState::AwaitingReview,
            }));

            let review_payload = ReviewPayload {
                summary: outcome.summary.clone().unwrap_or_default(),
                diff_summary: None,
                pr_url: None,
                error: None,
            };
            let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
                id: eid.0.clone(),
                attempt_n,
                kind: ReviewKind::Completion,
                payload: review_payload,
            }));

            // Inline decision-loop (so we can intercept Retry before
            // apply_decision_to_candidate maps it to Failed).
            use spur_acp::ReviewDecision;
            let decision_result = tokio::select! {
                r = rx => r.ok(),
                _ = tokio::time::sleep(agent_config.review.review_timeout) => {
                    review_sink.remove(&eid).await;
                    let final_status = DelegationStatus::TimedOut {
                        waited_for: agent_config.review.review_timeout,
                        fallback: agent_config.review.review_timeout_default.clone(),
                    };
                    // Emit cancellation so the lineage projection clears
                    // pending_review (DelegationCompleted alone does not).
                    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
                        id: eid.0.clone(),
                        reason: "review timeout".to_string(),
                    }));
                    // TimedOut → preserve worktree (no commit).
                    apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &event_tx,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.summary,
                            total_cost,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            match decision_result {
                Some(ReviewDecision::Approve) => {
                    let final_status = outcome.candidate_status.clone();
                    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Approve,
                    }));
                    // Approve → commit + remove.
                    apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &event_tx,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.summary,
                            total_cost,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Reject { reason }) => {
                    let final_status = DelegationStatus::Rejected { reason: reason.clone() };
                    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Reject { reason },
                    }));
                    // Rejected → no commit, preserve worktree.
                    apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &event_tx,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.summary,
                            total_cost,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Modify { note }) => {
                    let final_status = DelegationStatus::Modified { reviewer_note: note.clone() };
                    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Modify { note },
                    }));
                    // Modified → commit + remove (approved with reviewer note).
                    apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &event_tx,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.summary,
                            total_cost,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Retry { new_constraints }) => {
                    // NOTE: this retry logic is duplicated in
                    // run_gate_with_retries (the test helper). Keep
                    // the following invariants in sync if either site
                    // changes:
                    //   - Bound: attempt_n > max_review_retries
                    //   - Error message: "retry limit exceeded after N attempts"
                    //     where N == attempt_n (the actual count of
                    //     attempts that ran), NOT max_review_retries.
                    //     Example: max_review_retries=3 fires the bound at
                    //     attempt_n=4 (1 original + 3 retries), so the
                    //     message reports "4 attempts".
                    //   - Decision mapping: Approve→candidate,
                    //     Reject→Rejected, Modify→Modified
                    //
                    // `>` (not `>=`): spec's "Retry × 4 when
                    // max_review_retries = 3 produces Failed" means 3
                    // retries are allowed (attempts bump 1→2→3→4), and
                    // the 4th Retry decision fails.
                    if attempt_n > agent_config.review.max_review_retries {
                        let final_status = DelegationStatus::Failed {
                            error: format!(
                                "retry limit exceeded after {} attempts",
                                attempt_n
                            ),
                        };
                        // Retry limit → Failed (remove, no commit).
                        apply_worktree_cleanup(
                            &mut worktrees,
                            &outcome.worker_session,
                            &final_status,
                            &outcome.diff,
                            &agent,
                            &outcome.worktree_path,
                        )
                        .await;
                        return (
                            finalize(
                                &event_tx,
                                outcome.worker_session,
                                final_status,
                                outcome.diff,
                                outcome.summary,
                                total_cost,
                            ),
                            executor_id.clone(),
                        );
                    }

                    // Retry: generate the NEXT attempt's session id
                    // FIRST so we can announce it in
                    // ExecutorRetryStarted (matching what
                    // run_one_worker_attempt will use on the next
                    // iteration). The lineage projection treats
                    // new_session_id as the Attempt.session_id of
                    // the next attempt; emitting a fresh-but-unused
                    // id here would silently dangle.
                    let retry_session = SessionId::new();
                    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorRetryStarted {
                        id: eid.0.clone(),
                        attempt_n: attempt_n + 1,
                        reason: new_constraints.clone(),
                        new_session_id: retry_session.clone(),
                    }));

                    // Append constraints to the ORIGINAL task (not
                    // the accumulated one — prevents compounding
                    // constraint text across N retries).
                    current_task = format!(
                        "{}\n\n## Additional constraints\n{}",
                        original_task, new_constraints
                    );
                    attempt_n += 1;
                    next_worker_session = retry_session;

                    // Retry intermediates are never preserved — remove
                    // the current attempt's worktree before spawning
                    // the next attempt. No commit (intermediate diff is
                    // moot once the retry produces its own diff).
                    //
                    // Log (don't swallow) failures: a leftover directory
                    // will cause the next create_worktree to fail with
                    // a misleading "WorktreeFailed" — this warn lets the
                    // operator correlate cause and effect.
                    if let Err(e) = worktrees.remove_worktree(&outcome.worker_session).await {
                        tracing::warn!(
                            session = %outcome.worker_session,
                            error = %e,
                            "failed to remove retry-attempt worktree; next attempt may fail at create_worktree"
                        );
                    }
                    continue;
                }
                None => {
                    // Sender dropped — treat as timeout.
                    review_sink.remove(&eid).await;
                    let final_status = DelegationStatus::TimedOut {
                        waited_for: agent_config.review.review_timeout,
                        fallback: agent_config.review.review_timeout_default.clone(),
                    };
                    // Emit cancellation so the lineage projection clears
                    // pending_review (DelegationCompleted alone does not).
                    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
                        id: eid.0.clone(),
                        reason: "review sender dropped".to_string(),
                    }));
                    // Sender-drop TimedOut → preserve worktree (no commit).
                    apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &event_tx,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.summary,
                            total_cost,
                        ),
                        executor_id.clone(),
                    );
                }
            }
        }
    }
}

/// Emit `ExecutorReviewCancelled` and remove the sink entry.
///
/// Called from the brain-cancellation path — when `respond_to.send(result)`
/// returns `Err`, the brain has gone away, and any pending review for
/// this delegation must be recorded in the lineage projection as
/// abandoned (otherwise the TUI shows an orphaned review card
/// indefinitely).
///
/// Idempotent: if no review is registered, `review_sink.remove` is a
/// no-op, and the event is still emitted so the lineage projection
/// records the cancellation.
pub async fn cleanup_cancelled_review(
    executor_id: &ExecutorId,
    reason: &str,
    event_tx: &broadcast::Sender<SpurEvent>,
    review_sink: &ReviewSink,
) {
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
        id: executor_id.0.clone(),
        reason: reason.to_string(),
    }));
    review_sink.remove(executor_id).await;
}

/// Returns `true` if the worktree should be preserved (not removed) for
/// this final `DelegationStatus`.
///
/// Preserved:
///   - `Rejected` (human said no — operator may want to inspect diff).
///   - `TimedOut { fallback: Reject | Abandon }` (no human reviewed in
///     time AND the configured fallback says "treat as no" or "abandon";
///     preserve so a human can still inspect).
///
/// NOT preserved:
///   - `TimedOut { fallback: Approve }` — per spec, Approve fallback
///     means "auto-approve — worker's diff/summary retained as if
///     reviewed", so the diff must be committed and the worktree
///     removed (same lifecycle as a human Approve).
///   - `Success`/`Modified` (approved — changes merged into the brain's
///     tree).
///   - `Failed`/`Conflict`/`Timeout` (no real work to inspect — worker
///     hung or errored, or conflict blocked the run).
pub fn should_preserve_worktree(status: &DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Rejected { .. }
            | DelegationStatus::TimedOut {
                fallback: TimeoutFallback::Reject { .. } | TimeoutFallback::Abandon,
                ..
            }
    )
}

/// Returns `true` if the worker's diff should be committed into the
/// brain's branch based on the final `DelegationStatus`.
///
/// Commit on:
///   - `Success` (Approve).
///   - `Modified` (human-annotated approval).
///   - `TimedOut { fallback: Approve }` (auto-approve fallback — spec
///     says diff is "retained as if reviewed", so it must commit).
///
/// Do NOT commit on Rejected/TimedOut(Reject|Abandon) (preserve for
/// inspection), nor on Failed/Conflict/Timeout (no clean diff to merge).
pub fn should_commit_worker_diff(status: &DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Success
            | DelegationStatus::Modified { .. }
            | DelegationStatus::TimedOut {
                fallback: TimeoutFallback::Approve,
                ..
            }
    )
}

/// Post-gate cleanup: commit the worker diff (if approved) and either
/// preserve or remove the worktree based on the final status.
///
/// Called from every terminal arm in `execute_delegation`. On Retry,
/// only `remove_worktree` is called (no commit — intermediate attempts
/// do not get merged into the brain tree).
async fn apply_worktree_cleanup(
    worktrees: &mut WorktreeManager,
    worker_session: &SessionId,
    final_status: &DelegationStatus,
    diff: &Option<String>,
    agent: &str,
    worktree_path: &std::path::Path,
) {
    if should_commit_worker_diff(final_status) && diff.is_some() {
        if let Err(e) = worktrees
            .commit_worker_changes(
                worker_session,
                &format!("spur: worker {} output", agent),
            )
            .await
        {
            tracing::warn!(error = %e, "failed to commit worker diff");
        }
    }

    if should_preserve_worktree(final_status) {
        tracing::info!(
            worktree = %worktree_path.display(),
            status = ?final_status,
            "preserving worktree for review inspection"
        );
    } else {
        let _ = worktrees.remove_worktree(worker_session).await;
    }
}

/// Common terminal-arm helper: emits `DelegationCompleted` and
/// constructs the `DelegationResult`. Centralizing this makes the
/// "every terminal emits DelegationCompleted" invariant locally
/// verifiable (one call site per terminal arm in `execute_delegation`).
fn finalize(
    event_tx: &broadcast::Sender<SpurEvent>,
    worker_session: SessionId,
    final_status: DelegationStatus,
    diff: Option<String>,
    summary: Option<String>,
    total_cost: f64,
) -> DelegationResult {
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session,
        status: final_status.clone(),
    }));
    DelegationResult {
        status: final_status,
        diff,
        summary,
        estimated_cost_usd: total_cost,
    }
}

/// Setup-level error during a worker-spawn attempt. Distinct from the
/// worker's own output-level outcome (which lives in
/// `WorkerAttemptOutcome`). Setup errors short-circuit the entire
/// delegation without retry — retrying a worktree-creation failure is
/// not a spec'd behavior.
// All variants share the `Failed` suffix intentionally — they describe distinct
// failure phases (snapshot, worktree, init, session) and the suffix aids
// readability at match sites. Suppressing the lint is cleaner than renaming.
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
enum AttemptSetupError {
    SnapshotFailed(String),
    WorktreeFailed(String),
    InitFailed(String),
    SessionFailed(String),
}

impl std::fmt::Display for AttemptSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SnapshotFailed(e) => write!(f, "Failed to snapshot brain state: {e}"),
            Self::WorktreeFailed(e) => write!(f, "Failed to create worktree: {e}"),
            Self::InitFailed(e) => write!(f, "Failed to initialize worker: {e}"),
            Self::SessionFailed(e) => write!(f, "Failed to create worker session: {e}"),
        }
    }
}

/// Outcome of one worker attempt: whatever we'd need to close out the
/// delegation OR feed into the review gate.
struct WorkerAttemptOutcome {
    worker_session: SessionId,
    candidate_status: DelegationStatus,
    diff: Option<String>,
    summary: Option<String>,
    cost: f64,
    /// Path to the worktree that holds this attempt's diff.
    /// Used by `execute_delegation` to log a preserved path on
    /// `Rejected` / `TimedOut` — worktree removal is deferred to
    /// after the review gate.
    worktree_path: PathBuf,
}

/// Run a single worker attempt: snapshot brain state, create worktree,
/// spawn agent, prompt, collect diff.
///
/// `worker_session` is provided by the caller (rather than generated
/// inside) so `execute_delegation`'s Retry arm can announce the next
/// attempt's session id in `ExecutorRetryStarted.new_session_id` and
/// have it match what this function actually uses — closing the lineage
/// `Attempt.session_id ↔ worker event` linkage gap.
///
/// **Worktree lifecycle**: this function creates the worktree and
/// collects the diff, but does NOT commit or remove the worktree.
/// Commit and removal are deferred to `execute_delegation` so the
/// post-gate decision can determine whether to preserve
/// (`Rejected`/`TimedOut`) or remove (all other terminal statuses).
/// Exception: if a setup failure occurs AFTER the worktree is created
/// (e.g., agent init failure), the worktree IS cleaned up here
/// immediately — setup failures short-circuit without retry and the
/// caller's `finalize` records the error status.
///
/// Returns `Ok(WorkerAttemptOutcome)` for any flow that produced a
/// worker candidate status — success OR worker-reported errors — both
/// of which are retry-eligible (the human reviewer decides).
///
/// Returns `Err(AttemptSetupError)` only for pre-worker setup failures
/// (worktree creation, agent initialization, session creation). The
/// caller short-circuits the delegation without retry — consistent
/// with pre-T10 behavior. Per-attempt error shape is decoupled from
/// the public `DelegationResult` type.
/// Build a boxed `AgentConnection` from the transport declared in `config`.
///
/// Single source of truth for the `match transport { Acp/Stdio/CliWrap/StreamJson }`
/// arms. Both `Orchestrator::create_connection` (brain + resume paths) and
/// `run_one_worker_attempt` (worker spawn) call this — previously each had
/// its own copy of the match, and would drift when transports changed.
///
/// `spawn_args` is the final, bypass-aware spawn argv (callers invoke
/// `config.effective_args()` before passing them in). `permission_tx` is
/// honored only by the ACP transport; other transports ignore it.
fn build_connection_from_transport(
    config: &spur_acp::config::AgentConfig,
    spawn_args: Vec<String>,
    permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
) -> Box<dyn AgentConnection> {
    match config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
            permission_tx,
        )),
        TransportKind::Stdio => Box::new(StdioAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
    }
}

async fn run_one_worker_attempt(
    worker_session: SessionId,
    agent: &str,
    task: &str,
    request_id: &str,
    agent_config: &spur_acp::config::AgentConfig,
    worktrees: &mut WorktreeManager,
    event_tx: &broadcast::Sender<SpurEvent>,
) -> Result<WorkerAttemptOutcome, AttemptSetupError> {
    // NOTE: DelegationRequested is emitted per-attempt here. The legacy
    // lineage adapter (lineage/adapter.rs) keys task_spec population to
    // the FIRST matching empty-task_spec executor, so on retry the
    // constraint-augmented task silently drops at the adapter boundary.
    // This is part of the broader "adapter keys off worker_session, not
    // stable executor_id" limitation documented for follow-up work.
    // The projection path (apply_inner) sees each event correctly.
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationRequested {
        from: worker_session.clone(),
        to_agent: agent.to_string(),
        task: task.to_string(),
        request_id: request_id.to_string(),
    }));

    let start = Instant::now();

    // 1. Snapshot brain state and create worktree.
    let snapshot_branch = worktrees
        .snapshot_brain_state()
        .await
        .map_err(|e| AttemptSetupError::SnapshotFailed(e.to_string()))?;

    let worktree_info = worktrees
        .create_worktree(&worker_session, agent, &snapshot_branch)
        .await
        .map_err(|e| AttemptSetupError::WorktreeFailed(e.to_string()))?;

    // 2. Spawn worker agent in worktree via AgentConnection.
    // Workers never receive a permission_tx, so L2 auto-approve is
    // implicitly always on for them. skip_permissions still has effect
    // via L1a (spawn args).
    let spawn_args = agent_config.effective_args();
    let mut connection: Box<dyn AgentConnection> =
        build_connection_from_transport(agent_config, spawn_args, None);

    let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
    if let Err(e) = connection.initialize(init_request).await {
        let _ = worktrees.remove_worktree(&worker_session).await;
        return Err(AttemptSetupError::InitFailed(e.to_string()));
    }

    // Emit WorkerSpawned event.
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: agent.to_string(),
        session: worker_session.clone(),
        worktree: worktree_info.path.clone(),
    }));
    // Correlate this executor with the brain's delegate_to_worker call
    // so the brain-side session_detail view can render an inline card.
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: worker_session.clone(),
        request_id: request_id.to_string(),
        executor_id: worker_session.0.clone(),
    }));

    // Workers get no MCP servers (per spec).
    let session_response = match crate::skip_perm::new_session_with_bypass(
        &mut *connection,
        agent_config,
        worktree_info.path.clone(),
        vec![],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = connection.shutdown().await;
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(AttemptSetupError::SessionFailed(e.to_string()));
        }
    };

    // 3. Send task to worker.
    let prompt_text = format!(
        "Working directory: {}\n\nTask: {}",
        worktree_info.path.display(),
        task
    );
    let prompt_request = PromptRequest::new(
        session_response.session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(prompt_text))],
    );

    let mut output_text = String::new();
    let mut worker_success = true;

    match connection.prompt(prompt_request).await {
        Ok(mut stream) => {
            while let Some(notification) = stream.next().await {
                match &notification.update {
                    SessionUpdate::AgentThoughtChunk(chunk)
                    | SessionUpdate::AgentMessageChunk(chunk) => {
                        if let ContentBlock::Text(tc) = &chunk.content {
                            output_text.push_str(&tc.text);
                        }
                    }
                    _ => {}
                }
            }
        }
        Err(e) => {
            worker_success = false;
            output_text = format!("Failed to prompt worker: {e}");
        }
    }

    let _ = connection.shutdown().await;

    // 4. Collect diff.
    let diff = worktrees
        .collect_diff(&worker_session)
        .await
        .unwrap_or(None);

    // 5. Capture worktree path for execute_delegation's post-gate cleanup.
    // Commit and removal are deferred — see function doc.
    let worktree_path = worktrees
        .active
        .get(&worker_session.to_string())
        .map(|i| i.path.clone())
        .unwrap_or_default();

    let duration = start.elapsed();
    let cost = spur_cost::estimator::estimate_cost(agent_config.cost_tier, duration);

    let summary = if output_text.len() > 500 {
        Some(format!("{}...", &output_text[..500]))
    } else if output_text.is_empty() {
        None
    } else {
        Some(output_text)
    };

    let candidate_status = if worker_success {
        DelegationStatus::Success
    } else {
        DelegationStatus::Failed {
            error: "Worker reported errors".into(),
        }
    };

    Ok(WorkerAttemptOutcome {
        worker_session,
        candidate_status,
        diff,
        summary,
        cost,
        worktree_path,
    })
}

/// Expand ~ to home directory.
fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return format!("{}/{}", home, rest);
        }
    }
    path.to_string()
}

fn dirs_home() -> Option<String> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_string_lossy().to_string())
}

/// Strip a leading `!` from the first text block in `blocks`, if any.
///
/// The TUI forwards interrupt commands (`!stop`) as a text block with a
/// leading bang. We strip it once here before forwarding to the agent so
/// the agent sees clean prompt text.
fn strip_bang_prefix(mut blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    if let Some(ContentBlock::Text(tc)) = blocks.first_mut() {
        if tc.text.starts_with('!') {
            tc.text = tc.text.strip_prefix('!').unwrap_or(&tc.text).to_string();
        }
    }
    blocks
}

// ─── Review dispatcher ────────────────────────────────────────────────

/// Dispatcher loop: forwards `SubmitReview` messages to the `ReviewSink`.
/// All other `InteractiveInput` variants are ignored by this loop (they
/// are consumed by `run_interactive`'s own loop, not this one).
///
/// This is spawned as a separate task so review-decision latency is
/// decoupled from brain-turn I/O latency — see spec "Unit 3" for
/// rationale.
pub async fn review_dispatcher_loop(
    mut rx: mpsc::Receiver<InteractiveInput>,
    sink: ReviewSink,
) {
    while let Some(input) = rx.recv().await {
        if let InteractiveInput::SubmitReview { executor_id, attempt_n, decision } = input {
            let _ = sink.submit(ExecutorId::new(executor_id), attempt_n, decision).await;
        }
        // All other variants: noop in this loop.
    }
}

// ─── Review gate helper ───────────────────────────────────────────────

/// Register a pending review on the sink. Returns the receiver the
/// caller awaits.
///
/// MUST be called BEFORE emitting `ExecutorReviewRequested` so the TUI
/// cannot race a `SubmitReview` past an unregistered sink — see
/// `ReviewSink` docs for the invariant.
pub async fn register_gate(
    executor_id: ExecutorId,
    attempt_n: u32,
    review_sink: &ReviewSink,
) -> Result<tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>, ReviewSinkError> {
    review_sink.register(executor_id, attempt_n).await
}

/// Wait for a review decision (or timeout) and shape the final
/// `DelegationStatus`. The caller MUST have already called
/// `register_gate` and MUST pass the receiver returned from that call.
///
/// **Does NOT handle `Retry`** — if a `ReviewDecision::Retry` arrives,
/// this function returns a `DelegationStatus::Failed` with an
/// explanatory message. Task 10 wraps this helper in a retry loop that
/// intercepts `Retry` decisions before they reach this function, so in
/// practice this arm is unreachable once Task 10 is integrated; the
/// explicit arm exists for safety if someone calls this helper
/// directly without a wrapper.
///
/// On timeout or sender-drop: explicitly removes the sink entry (to
/// prevent stale entries per the spec's error-handling
/// "explicit-remove" contract) and returns
/// `TimedOut { waited_for, fallback }`.
pub async fn wait_gate(
    rx: tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>,
    executor_id: ExecutorId,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_fallback: TimeoutFallback,
    review_sink: ReviewSink,
) -> DelegationStatus {
    tokio::select! {
        recv_result = rx => {
            match recv_result {
                Ok(decision) => apply_decision_to_candidate(decision, candidate_status),
                Err(_) => {
                    // Sender dropped before sending — treat as timeout.
                    review_sink.remove(&executor_id).await;
                    DelegationStatus::TimedOut {
                        waited_for: review_timeout,
                        fallback: timeout_fallback,
                    }
                }
            }
        }
        _ = tokio::time::sleep(review_timeout) => {
            // Explicit-remove contract (spec error-handling section).
            review_sink.remove(&executor_id).await;
            DelegationStatus::TimedOut {
                waited_for: review_timeout,
                fallback: timeout_fallback,
            }
        }
    }
}

/// Register + wait composition. Exists primarily for unit tests that
/// want to exercise the full gate shape in one call; production code
/// in `execute_delegation` calls `register_gate` and `wait_gate`
/// separately so event emission can be sequenced between them (the
/// register-before-emit ordering the `ReviewSink` invariant requires).
///
/// On register failure (already-registered double-register): returns
/// `Failed` with an explanatory error.
pub async fn run_gate_for_candidate(
    executor_id: ExecutorId,
    attempt_n: u32,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_fallback: TimeoutFallback,
    review_sink: ReviewSink,
) -> DelegationStatus {
    let rx = match register_gate(executor_id.clone(), attempt_n, &review_sink).await {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!(
                executor_id = %executor_id.0,
                error = %e,
                "review_sink registration failed"
            );
            return DelegationStatus::Failed {
                error: format!("review registration failed: {e}"),
            };
        }
    };
    wait_gate(
        rx,
        executor_id,
        candidate_status,
        review_timeout,
        timeout_fallback,
        review_sink,
    )
    .await
}

/// Test helper: wraps `register_gate` + `wait_gate` in a retry loop,
/// re-using the same `candidate_status` for each attempt (since this
/// helper doesn't spawn workers — production code in `execute_delegation`
/// respawns the worker and produces a fresh candidate each iteration).
///
/// On `Retry`, bumps `attempt_n` and re-enters. Bounded by
/// `max_review_retries`. On exceed, returns
/// `Failed { error: "retry limit exceeded after N attempts" }`.
///
/// NOTE: this mirrors execute_delegation's production retry loop. See
/// the cross-reference comment at the Retry match arm there for
/// invariants. Drift hazard: tests passing here do not guarantee the
/// production loop behaves the same. Changes to retry semantics
/// should touch both.
pub async fn run_gate_with_retries(
    executor_id: ExecutorId,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_fallback: TimeoutFallback,
    max_review_retries: u32,
    review_sink: ReviewSink,
) -> DelegationStatus {
    let mut attempt_n: u32 = 1;
    loop {
        let rx = match register_gate(executor_id.clone(), attempt_n, &review_sink).await {
            Ok(rx) => rx,
            Err(e) => {
                return DelegationStatus::Failed {
                    error: format!("review registration failed: {e}"),
                };
            }
        };

        // One iteration of the gate. We inline the select! here (rather than
        // calling wait_gate) so we can intercept Retry BEFORE it gets mapped
        // to Failed by apply_decision_to_candidate.
        use spur_acp::ReviewDecision;
        let decision_result = tokio::select! {
            r = rx => r.ok(),
            _ = tokio::time::sleep(review_timeout) => {
                review_sink.remove(&executor_id).await;
                return DelegationStatus::TimedOut {
                    waited_for: review_timeout,
                    fallback: timeout_fallback,
                };
            }
        };

        match decision_result {
            Some(ReviewDecision::Approve) => return candidate_status,
            Some(ReviewDecision::Reject { reason }) => {
                return DelegationStatus::Rejected { reason }
            }
            Some(ReviewDecision::Modify { note }) => {
                return DelegationStatus::Modified { reviewer_note: note }
            }
            Some(ReviewDecision::Retry { .. }) => {
                // `>` (not `>=`): see execute_delegation for rationale.
                // Error message reports `attempt_n` (the actual attempt
                // count that ran), NOT `max_review_retries`. See the
                // execute_delegation cross-reference for the worked
                // example.
                if attempt_n > max_review_retries {
                    return DelegationStatus::Failed {
                        error: format!(
                            "retry limit exceeded after {} attempts",
                            attempt_n
                        ),
                    };
                }
                attempt_n += 1;
                continue;
            }
            None => {
                // Sender dropped — treat as timeout.
                review_sink.remove(&executor_id).await;
                return DelegationStatus::TimedOut {
                    waited_for: review_timeout,
                    fallback: timeout_fallback,
                };
            }
        }
    }
}

fn apply_decision_to_candidate(
    decision: spur_acp::ReviewDecision,
    candidate: DelegationStatus,
) -> DelegationStatus {
    use spur_acp::ReviewDecision;
    match decision {
        ReviewDecision::Approve => candidate,
        ReviewDecision::Reject { reason } => DelegationStatus::Rejected { reason },
        ReviewDecision::Modify { note } => DelegationStatus::Modified { reviewer_note: note },
        ReviewDecision::Retry { .. } => DelegationStatus::Failed {
            error: "internal: Retry reached run_gate_for_candidate \
                    (caller must wrap with retry loop)"
                .into(),
        },
    }
}
