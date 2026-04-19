use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use spur_acp::config::SpurConfig;
use spur_acp::connection::{
    AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter,
};
use spur_acp::registry::AgentRegistry;
use spur_acp::types::*;
use spur_acp::{
    CancellationControl, DelegationResult, DelegationStatus, LifecycleState, ReviewKind,
    ReviewPayload, SpurEvent, SpurEventBody, TimeoutFallback,
};
use spur_pm::Issue;

use agent_client_protocol::{
    ContentBlock, InitializeRequest, ListSessionsRequest, McpServer, McpServerHttp, PromptRequest,
    ProtocolVersion, SessionInfo, SessionUpdate, SetSessionModeRequest, TextContent,
};

use spur_cost::CostTracker;
use spur_license::SpurLicense;
use spur_mcp::{
    build_worker_info, DelegationChannel, DelegationRequest, McpCallbackServer, WorkerInfo,
};
use spur_pm::PmService;
use spur_worktree::WorktreeManager;

use crate::lineage::ExecutorId;
use crate::review_sink::{ReviewSink, ReviewSinkError};

// ─── Agent name normalization ─────────────────────────────────────────

/// Normalize an agent name for equality comparison.
/// - Lowercases
/// - Trims surrounding whitespace
/// - Strips `-acp`, `_acp`, `-cli`, `_cli` suffixes
///
/// Used to compare `DelegationPlan.chosen` (possibly a short name
/// the brain chose) against the dispatched `agent` (possibly a
/// fully-qualified registered name like `claude-code-acp`).
pub fn normalize_agent_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    for suffix in ["-acp", "_acp", "-cli", "_cli"].iter() {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    lower
}

/// Format a worker task string with an optional `## Relevant Files`
/// section prepended.
///
/// - When `context_files.is_empty()`, the task string is returned
///   unchanged (no section prepended).
/// - Otherwise a `## Relevant Files` header is prepended with each
///   path as a Markdown bullet, followed by a `## Task` header and
///   the original task body. Order of the bullets preserves the input
///   order.
///
/// This function does no file I/O. The worker's own Read tool is
/// responsible for opening the listed paths.
pub(crate) fn format_worker_task(task: &str, context_files: &[String]) -> String {
    if context_files.is_empty() {
        return task.to_string();
    }
    let mut out = String::with_capacity(task.len() + 128 + context_files.len() * 64);
    out.push_str("## Relevant Files\n\n");
    out.push_str(
        "The following files were declared as relevant by the caller. \
         Open them with your Read tool as needed.\n\n",
    );
    for path in context_files {
        out.push_str("- ");
        out.push_str(path);
        out.push('\n');
    }
    out.push_str("\n## Task\n\n");
    out.push_str(task);
    out
}

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
    pub delegation_handle: JoinHandle<()>,
    /// Background task accepting connections on the HTTP MCP callback
    /// listener. Must be aborted on session retire — otherwise the
    /// listener keeps its port open and the task + TcpListener are
    /// leaked for the lifetime of the process.
    pub mcp_handle: JoinHandle<()>,
    /// Task that drains the connection's session-notification broadcast
    /// and republishes each item onto the `SpurEvent` bus. `None` for
    /// transports that return `None` from `subscribe_session_notifications`
    /// (stdio, cli_wrap, stream_json). Must be aborted whenever the
    /// session is retired — otherwise a pump subscribed against the
    /// reused connection keeps emitting events tagged with this
    /// (now-stale) `spur_session_id`.
    pub notification_pump_handle: Option<JoinHandle<()>>,
}

/// A user input message from the TUI.
#[derive(Debug)]
#[non_exhaustive]
pub enum InteractiveInput {
    Message {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    /// Spawn a fresh brain session and send these blocks as the first prompt
    /// atomically. If a brain is already attached, it is shut down first.
    /// Empty `blocks` means spawn-only with no first prompt.
    NewSessionWithMessage {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    ListSessions,
    ResumeSession {
        session_id: String,
    },
    /// Request `set_session_mode` on the active brain session. No-op if
    /// there is no active brain session.
    SetSessionMode {
        mode_id: String,
    },
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
    /// Halt the currently streaming prompt (if any) via `AgentConnection::cancel`.
    /// When received inside the streaming `select!`, calls `cancel()` and arms
    /// the 5s force-timeout. When received outside the streaming loop (no
    /// active turn), dropped with a debug log (the view guards against emitting
    /// this unless a stream is in-flight, but a TurnComplete-vs-Esc race can
    /// still produce a stray one).
    CancelStream {
        session: SessionId,
    },
    /// Refresh the issue list and re-emit IssuesLoaded.
    RefreshIssues,
    /// Fetch full issue detail and emit IssueDetailFetched.
    GetIssueDetail {
        id: String,
    },
    /// Update an issue and emit IssueUpdated.
    UpdateIssue {
        id: String,
        update: spur_pm::IssueUpdate,
    },
    /// Detached delegation completion returned to the orchestrator for
    /// scheduled brain re-entry. Never constructed by the TUI. See
    /// `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
    SystemContinuation {
        session: SessionId,
        continuation: spur_acp::domain::BrainContinuation,
    },
}

/// Convert spur_pm::IssueSummary to the spur_acp mirror type for event bus transmission.
fn to_summary_event(
    issue: &spur_pm::IssueSummary,
    source: &str,
) -> spur_acp::domain::events::IssueSummaryEvent {
    spur_acp::domain::events::IssueSummaryEvent {
        id: issue.id.clone(),
        source: source.into(),
        title: issue.title.clone(),
        status: issue.status.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        assignee: issue.assignee.clone(),
    }
}

/// Emit a `GraphAlertsSummary` event from a triage report's alert list.
fn emit_alerts_from_report(
    report: &spur_pm::graph::TriageReport,
    funnel: &crate::event_funnel::FunnelHandle,
) {
    let alerts = &report.triage.alerts;
    let critical = alerts
        .iter()
        .filter(|a| a.severity.as_deref() == Some("critical"))
        .count();
    let warning = alerts
        .iter()
        .filter(|a| a.severity.as_deref() == Some("warning"))
        .count();
    let details: Vec<String> = alerts
        .iter()
        .take(5)
        .filter_map(|a| a.message.clone())
        .collect();
    funnel.emit(SpurEventBody::GraphAlertsSummary {
        total: alerts.len(),
        critical,
        warning,
        details,
    });
}

/// Build a brain-prompt summary from a triage report.
fn build_graph_prompt_summary(report: &spur_pm::graph::TriageReport) -> Option<String> {
    let qr = &report.triage.quick_ref;
    let health = &report.triage.project_health;
    let mut lines = vec![
        "## Project Graph Intelligence".to_string(),
        String::new(),
        format!(
            "Project: {} open, {} actionable, {} blocked, {} in progress.",
            qr.open_count, qr.actionable_count, qr.blocked_count, qr.in_progress_count,
        ),
    ];
    if let Some(top) = qr.top_picks.first() {
        lines.push(format!(
            "Top recommendation: {} (score {:.2}) — \"{}\"",
            top.id, top.score, top.title,
        ));
    }
    if health.graph.has_cycles {
        lines.push(format!(
            "Warning: {} cycles detected in dependency graph.",
            health.graph.cycle_count,
        ));
    }
    if !report.triage.quick_wins.is_empty() {
        let ids: Vec<_> = report
            .triage
            .quick_wins
            .iter()
            .take(3)
            .map(|q| q.id.as_str())
            .collect();
        lines.push(format!("Quick wins: {}", ids.join(", ")));
    }
    lines.push(String::new());
    lines.push(
        "Use `graph_triage` for full analysis. \
         Use `graph_plan` for parallel execution tracks."
            .to_string(),
    );
    Some(lines.join("\n"))
}

/// Parallel-fetch issues + graph triage, emit `IssuesLoaded` +
/// `GraphAlertsSummary` events via `tokio::join!`. When `for_prompt` is
/// true, also returns a graph summary string for brain prompt enrichment.
///
/// Replaces the previous sequential `list_issues` → `emit_graph_alerts`
/// pattern at all 4 call sites. Wall-time: `max(T_br, T_bv)` instead of
/// `T_br + T_bv`.
async fn refresh_pm_state(
    pm: &spur_pm::PmService,
    funnel: &crate::event_funnel::FunnelHandle,
    limit: Option<usize>,
    for_prompt: bool,
) -> Option<String> {
    let issues_fut = pm.list_issues(spur_pm::IssueFilter {
        status: Some("open".into()),
        limit,
        ..Default::default()
    });

    let triage_fut = async {
        match pm.analyzer() {
            Some(bv) => bv.triage(None).await.ok(),
            None => None,
        }
    };

    let (issues_result, triage_opt) = tokio::join!(issues_fut, triage_fut);

    // Emit issues.
    match issues_result {
        Ok(issues) => {
            let event_issues: Vec<_> = issues
                .iter()
                .map(|i| to_summary_event(i, pm.source_str()))
                .collect();
            tracing::info!(
                count = issues.len(),
                "Loaded open issues from {}",
                pm.source_str()
            );
            funnel.emit(SpurEventBody::IssuesLoaded {
                issues: event_issues,
            });
        }
        Err(e) => tracing::warn!("Failed to load issues: {e}"),
    }

    // Emit alerts + optionally build prompt summary.
    if let Some(report) = triage_opt {
        emit_alerts_from_report(&report, funnel);
        if for_prompt {
            return build_graph_prompt_summary(&report);
        }
    }
    None
}

/// Convert spur_pm::Issue to the spur_acp mirror type for event bus transmission.
fn issue_to_detail_event(issue: &spur_pm::Issue) -> spur_acp::IssueDetailEvent {
    spur_acp::IssueDetailEvent {
        id: issue.id.clone(),
        source: issue.source.to_string(),
        title: issue.title.clone(),
        body: issue.body.clone(),
        status: issue.status.clone(),
        labels: issue.labels.clone(),
        assignee: issue.assignee.clone(),
        url: issue.url.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        blocked_by: issue.blocked_by.clone(),
        due_at: issue.due_at,
        created_at: issue.created_at,
        updated_at: issue.updated_at,
    }
}

// ─── Orchestrator ────────────────────────────────────────────────────

/// The central orchestrator that drives the brain-worker pipeline.
pub struct Orchestrator {
    pub registry: AgentRegistry,
    pub config: SpurConfig,
    pub worktrees: WorktreeManager,
    pub cost_tracker: Option<CostTracker>,
    pub event_tx: broadcast::Sender<SpurEvent>,
    /// Monotonic sequence counter for the S2 funnel. The funnel task
    /// owns the write end via `fetch_add`; retained on the struct so
    /// tests/diagnostics can inspect the current count if needed.
    #[allow(dead_code)]
    event_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// S2 funnel handle — every orchestrator emit flows through this.
    /// Internally writes `SpurEventBody` into an mpsc that the funnel
    /// task drains onto `event_tx`, stamping monotonic `seq` +
    /// `occurred_at` in strict enqueue order (Pitfall P1).
    funnel: crate::event_funnel::FunnelHandle,
    pub review_sink: ReviewSink, // Clone type, shares inner Arc<Mutex>
    repo_root: PathBuf,
    pub pm_service: Option<Arc<PmService>>,
    /// INV-6: per-delegation cancellation token registry.
    cancellation_control: CancellationControl,
    /// Sender half of the `run_interactive` ingress channel.  Set by
    /// `set_continuation_tx` so the MCP server can route detached
    /// delegation completions back to the orchestrator.
    continuation_tx: Option<mpsc::Sender<InteractiveInput>>,
    /// Overflow buffer for detached continuations.  Mirrors the buffer
    /// passed to `run_interactive`; set alongside `continuation_tx`.
    continuation_overflow: Option<crate::continuation_bridge::OverflowBuf>,
}

/// Detect whether an error from an `AgentConnection` RPC indicates the
/// underlying subprocess has died (pipe closed, ACP thread exited, etc.),
/// versus a normal request-level error (auth needed, invalid session, etc.).
///
/// Pragmatic string-match against the two known "subprocess is gone"
/// patterns emitted by `NativeAcpConnection` and the ACP SDK. A more
/// structured signal would require a new trait method on `AgentConnection`;
/// revisit if the set of transports grows.
pub(crate) fn is_connection_death(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("ACP thread died") || msg.contains("server shut down unexpectedly")
}

// ─── Free function: log-cap enforcer ──────────────────────────────────────────

fn enforce_log_cap(dir: &std::path::Path, cap: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.modified().ok()?, m.len()))
        })
        .collect();
    let total: u64 = files.iter().map(|(_, _, s)| s).sum();
    if total <= cap {
        return;
    }
    files.sort_by_key(|(_, mtime, _)| *mtime); // oldest first
    let mut to_free = total - cap;
    for (path, _, size) in files {
        if to_free == 0 {
            break;
        }
        let _ = std::fs::remove_file(&path);
        to_free = to_free.saturating_sub(size);
    }
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

        // S1.d — 4096 supports ~2.5s of events at 1600 evt/s peak
        // (20 workers × 80 evt/s). Subscribers that still lag get
        // RecvError::Lagged (logged at WARN; see S1.d Lagged audit).
        let (event_tx, _) = broadcast::channel(4096);
        // S2 — spawn the singleton funnel. Every orchestrator emit
        // flows through `funnel.emit(body)`; the funnel task stamps
        // monotonic seq + wall-clock time and forwards on `event_tx`.
        let event_seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let funnel = crate::event_funnel::spawn_funnel(event_tx.clone(), event_seq.clone());
        // S3 — durable JSONL sink subscribes to the same broadcast.
        crate::event_sink::spawn_sink(event_tx.subscribe());
        let review_sink = ReviewSink::new();

        Ok(Self {
            registry,
            config,
            worktrees,
            cost_tracker,
            event_tx,
            event_seq,
            funnel,
            review_sink,
            repo_root,
            pm_service: None,
            cancellation_control: CancellationControl::new(),
            continuation_tx: None,
            continuation_overflow: None,
        })
    }

    /// Attach a PM service. Must be called before `run_adhoc` or `run_interactive`.
    pub fn with_pm_service(mut self, pm: Arc<PmService>) -> Self {
        self.pm_service = Some(pm);
        self
    }

    /// Wire in the sender half of the `run_interactive` ingress channel so
    /// the MCP server can route detached delegation completions back to the
    /// orchestrator. Call this before `run_interactive`.
    pub fn set_continuation_tx(
        &mut self,
        tx: mpsc::Sender<InteractiveInput>,
        overflow: crate::continuation_bridge::OverflowBuf,
    ) {
        self.continuation_tx = Some(tx);
        self.continuation_overflow = Some(overflow);
    }

    /// Build a `DetachedContinuationCtx` for `McpCallbackServer::new`.
    ///
    /// Wires the `on_complete` async callback to `report_detached_completion`,
    /// capturing the funnel handle (for UI event emission) and the
    /// orchestrator ingress sender + overflow (for model-visible continuation).
    ///
    /// If no `continuation_tx` has been wired (e.g. `run_adhoc`), the
    /// callback is a no-op — continuations are silently dropped, which is
    /// correct for the one-shot batch path.
    fn build_continuation_ctx(
        &self,
        brain_session_id: spur_acp::types::SessionId,
    ) -> spur_mcp::server::DetachedContinuationCtx {
        let funnel = self.funnel.clone();
        match (self.continuation_tx.clone(), self.continuation_overflow.clone()) {
            (Some(tx), Some(overflow)) => {
                let session = brain_session_id.clone();
                spur_mcp::server::DetachedContinuationCtx {
                    on_complete: std::sync::Arc::new(move |cont, worker_session_str| {
                        let tx = tx.clone();
                        let overflow = overflow.clone();
                        let session = session.clone();
                        let funnel = funnel.clone();
                        let worker_session =
                            spur_acp::types::SessionId(worker_session_str);
                        Box::pin(async move {
                            // INV-C3: UI event BEFORE model-visible ingress.
                            // FunnelHandle already implements ContinuationEventSink
                            // (see continuation_bridge.rs).
                            crate::continuation_bridge::report_detached_completion(
                                &funnel,
                                &tx,
                                &overflow,
                                session,
                                worker_session,
                                cont,
                            )
                            .await;
                        })
                    }),
                }
            }
            _ => {
                // No ingress channel wired — produce a no-op ctx so the
                // constructor signature is satisfied (run_adhoc path).
                spur_mcp::server::DetachedContinuationCtx {
                    on_complete: std::sync::Arc::new(|_cont, _worker| {
                        Box::pin(async {})
                    }),
                }
            }
        }
    }

    /// Subscribe to orchestrator events (for TUI, logging, etc.).
    pub fn subscribe(&self) -> broadcast::Receiver<SpurEvent> {
        self.event_tx.subscribe()
    }

    /// INV-6: Return a clonable handle to the cancellation token registry.
    /// Pass a clone to `McpCallbackServer` so `handle_cancel_delegation` can
    /// signal running delegations without routing through the delegation channel.
    pub fn cancellation_control(&self) -> CancellationControl {
        self.cancellation_control.clone()
    }

    /// Spawn the licensing runtime helper against this orchestrator's event funnel.
    pub fn spawn_license_runtime(&self, license: SpurLicense) -> JoinHandle<()> {
        crate::license_runtime::spawn_license_runtime(license, self.funnel.clone())
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

        // 1b. Parallel-fetch issues + graph intelligence for TUI + brain prompt.
        let graph_summary = if let Some(pm) = &self.pm_service {
            refresh_pm_state(pm, &self.funnel, None, true).await
        } else {
            None
        };

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

        // 3. Build brain prompt (enriched with graph intelligence).
        let enriched_task = match &graph_summary {
            Some(summary) => format!("{summary}\n\n{task}"),
            None => task.to_string(),
        };
        let prompt_text = self.build_brain_prompt(
            &enriched_task,
            issue_context.as_ref(),
            &session_id,
            &brain_name,
        );

        // 4. Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id: spur_acp::BrainSessionId = session_id.clone().into();
        let adhoc_ctx = self.build_continuation_ctx(session_id.clone());
        let (mcp_server, delegation_channel) =
            McpCallbackServer::new(&brain_session_id, self.pm_service.clone(), sink, adhoc_ctx);
        let mut mcp_server = mcp_server;

        // Populate available workers.
        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .into_iter()
            .map(build_worker_info)
            .collect();
        mcp_server.set_workers(workers);
        // INV-6: wire the cancellation side-channel.
        mcp_server.set_cancellation_control(self.cancellation_control.clone());

        let mcp_server = Arc::new(mcp_server);
        let (mcp_url, mcp_handle) = mcp_server
            .clone()
            .start()
            .await
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

        // MCP callback server is now HTTP — pass URL directly.
        let mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-mcp", &mcp_url))];

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

        // 8. Process brain output + delegation callbacks concurrently.
        let pr_url: Option<String> = None;
        let success = true;

        // Spawn delegation handler BEFORE prompt so delegation requests
        // that arrive during the prompt turn are not queued indefinitely.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.funnel.clone(),
            self.review_sink.clone(),
            self.pm_service.clone(),
            self.cancellation_control.clone(),
        ));

        // Stream brain output. For native (ACP-transport) agents prompt()
        // returns an empty stream; notifications arrive via the
        // connection-scoped broadcast instead. drive_prompt_notifications
        // handles both paths transparently.
        let funnel_for_notif = self.funnel.clone();
        let session_id_for_notif = session_id.clone();
        crate::notification_drain::drive_prompt_notifications(
            &mut *connection,
            prompt_request,
            |notification| {
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
                funnel_for_notif.emit(SpurEventBody::AgentNotification {
                    session: session_id_for_notif.clone(),
                    notification: Box::new(notification),
                });
            },
        )
        .await
        .context("Failed to send prompt to brain")?;

        // 9. Clean up.
        let _ = connection.shutdown().await;
        delegation_handle.abort();
        mcp_handle.abort();

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
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        overflow_continuations: crate::continuation_bridge::OverflowBuf,
    ) -> Result<()> {
        let mut brain: Option<BrainSession> = None;
        let mut scheduler = crate::scheduler::BrainScheduler::new(
            None, // active_session set when first brain spawns
        );
        // Pre-connected (initialized) agent connection, ready for create_brain_session
        // or load_brain_session without re-running connect_brain.
        let mut agent_connection: Option<(Box<dyn spur_acp::AgentConnection>, String)> = None;

        let mut reconnect_failures: std::collections::VecDeque<std::time::Instant> =
            std::collections::VecDeque::new();
        const RECONNECT_CIRCUIT_LIMIT: usize = 3;
        const RECONNECT_CIRCUIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

        // Startup: parallel-fetch issues + graph alerts for TUI display.
        if let Some(pm) = &self.pm_service {
            refresh_pm_state(pm, &self.funnel, None, false).await;
        }

        // Startup guidance: surface actionable install hints for missing PM tools.
        if self.pm_service.is_none() && self.repo_root.join(".beads").is_dir() {
            self.funnel.emit(SpurEventBody::IssueCommandError {
                operation: "startup".into(),
                error: "br (beads) not installed — issue tracking disabled. \
                        Install: cargo install --git https://github.com/Dicklesworthstone/beads_rust.git"
                    .into(),
            });
        } else if let Some(pm) = &self.pm_service {
            if pm.analyzer().is_none() {
                self.funnel.emit(SpurEventBody::IssueCommandError {
                    operation: "startup".into(),
                    error: "bv (beads_viewer) not installed — graph analysis disabled. \
                            Install: brew install dicklesworthstone/tap/bv"
                        .into(),
                });
            }
        }

        loop {
            // ── (a) Drain overflow buffer so scheduler sees fresh state ──
            {
                let mut over = overflow_continuations.lock().await;
                while let Some((_sid, c)) = over.pop_front() {
                    scheduler.push_continuation(c);
                }
            }

            // ── (b) Ask scheduler what to do ────────────────────────────
            let now = std::time::Instant::now();
            let action = scheduler.next(now);

            // ── (c) Idle: recv next input and dispatch immediately ───────
            if matches!(action, crate::scheduler::ScheduledAction::Idle) {
                let raw = match user_input_rx.recv().await {
                    Some(i) => i,
                    None => break, // channel closed — shutdown
                };

                match raw {
                    // Continuation — route to scheduler for next tick.
                    InteractiveInput::SystemContinuation { continuation, .. } => {
                        scheduler.push_continuation(continuation);
                        continue;
                    }
                    // Prompt-class — push to scheduler; will be dispatched next tick.
                    InteractiveInput::Message { .. } => {
                        scheduler.push_user(raw);
                        continue;
                    }
                    // NewSessionWithMessage — retire brain, then push Message to scheduler.
                    InteractiveInput::NewSessionWithMessage { blocks, interrupt } => {
                        Self::retire_active_brain(&mut brain, &mut agent_connection);
                        let evicted = scheduler.note_session_swap(None);
                        for c in evicted {
                            self.funnel.emit(SpurEventBody::ContinuationDropped {
                                delegation_id: c.delegation_id,
                                reason: spur_acp::domain::events::ContinuationDropReason::SessionSwap,
                            });
                        }
                        if blocks.is_empty() {
                            info!("NewSessionWithMessage with empty blocks — spawn deferred to next Message");
                        } else {
                            scheduler.push_user(InteractiveInput::Message { blocks, interrupt });
                        }
                        continue;
                    }

                    // ── ListSessions ──────────────────────────────────────
                    InteractiveInput::ListSessions => {
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

                        let list_req = ListSessionsRequest::new().cwd(self.repo_root.clone());
                        let sessions_result = match conn.list_sessions(list_req).await {
                            Ok(response) => Ok(response.sessions),
                            Err(e) => {
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

                        agent_connection = Some((conn, brain_name));
                    }

                    // ── ResumeSession ─────────────────────────────────────
                    InteractiveInput::ResumeSession { session_id } => {
                        Self::retire_active_brain(&mut brain, &mut agent_connection);

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
                            .load_brain_session(
                                connection,
                                brain_name,
                                permission_tx.clone(),
                                session_id,
                                None,
                                false,
                            )
                            .await
                        {
                            Ok((session, mut history_stream, _load_outcome)) => {
                                let spur_id = session.spur_session_id.clone();
                                let mut history_count = 0usize;
                                while let Some(notification) = history_stream.next().await {
                                    history_count += 1;
                                    self.emit(SpurEvent::now(SpurEventBody::AgentNotification {
                                        session: spur_id.clone(),
                                        notification: Box::new(notification),
                                    }));
                                }

                                if history_count == 0 {
                                    let entries =
                                        Self::read_session_history_from_disk(&original_session_id);
                                    if !entries.is_empty() {
                                        info!(
                                            count = entries.len(),
                                            "Replaying conversation history from disk"
                                        );
                                        self.emit(SpurEvent::now(SpurEventBody::SessionHistory {
                                            session: spur_id.clone(),
                                            entries,
                                        }));
                                    }
                                }

                                brain = Some(session);
                                self.emit(SpurEvent::now(SpurEventBody::TurnComplete {
                                    session: spur_id,
                                }));
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

                    // ── VendorExec ────────────────────────────────────────
                    InteractiveInput::VendorExec {
                        session,
                        method,
                        mut params,
                    } => {
                        if let Some(b) = brain.as_mut() {
                            if let Some(obj) = params.as_object_mut() {
                                obj.insert("sessionId".into(), serde_json::json!(b.acp_session_id));
                            } else {
                                warn!(
                                    method = %method,
                                    "VendorExec params is not a JSON object; sessionId not injected"
                                );
                            }
                            let brain_name_for_log = b.brain_name.clone();
                            let call_result = b.connection.call_ext(&method, params).await;
                            match call_result {
                                Ok(resp) => {
                                    self.emit(SpurEvent::now(SpurEventBody::AgentExtNotification {
                                        session: session.clone(),
                                        method: format!("{}/response", method),
                                        params: resp,
                                    }));
                                }
                                Err(e) => {
                                    warn!(
                                        brain = %brain_name_for_log,
                                        method = %method,
                                        error = %e,
                                        "vendor exec call failed"
                                    );
                                    if is_connection_death(&e) {
                                        if let Some(dead) = brain.take() {
                                            let reason =
                                                format!("vendor exec `{method}` died: {e}");
                                            if let Some(new_brain) = self
                                                .reconnect_with_events(
                                                    dead,
                                                    permission_tx.clone(),
                                                    brain_override.as_deref(),
                                                    reason,
                                                    &mut reconnect_failures,
                                                    RECONNECT_CIRCUIT_LIMIT,
                                                    RECONNECT_CIRCUIT_WINDOW,
                                                )
                                                .await
                                            {
                                                brain = Some(new_brain);
                                            }
                                        }
                                    } else {
                                        self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                            session,
                                            message: format!(
                                                "vendor exec `{}` failed: {}",
                                                method, e
                                            ),
                                        }));
                                    }
                                }
                            }
                        } else {
                            warn!(method = %method, "VendorExec received but no active brain session");
                        }
                    }

                    // ── SetSessionMode ────────────────────────────────────
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
                            warn!(
                                mode_id = %mode_id,
                                "SetSessionMode received but no active brain session"
                            );
                        }
                    }

                    // ── CancelStream (outside active turn) ────────────────
                    InteractiveInput::CancelStream { session } => {
                        tracing::debug!(
                            session = %session,
                            "CancelStream received outside active turn; dropping (no stream to cancel)"
                        );
                    }

                    // ── RefreshIssues ─────────────────────────────────────
                    InteractiveInput::RefreshIssues => {
                        if let Some(pm) = &self.pm_service {
                            refresh_pm_state(pm, &self.funnel, Some(50), false).await;
                        } else {
                            self.funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "RefreshIssues".into(),
                                error: "No issue tracker configured".into(),
                            });
                        }
                    }

                    // ── GetIssueDetail ────────────────────────────────────
                    InteractiveInput::GetIssueDetail { id } => {
                        if let Some(pm) = &self.pm_service {
                            match pm.get_issue(&id).await {
                                Ok(issue) => {
                                    self.funnel.emit(SpurEventBody::IssueDetailFetched {
                                        requested_id: id,
                                        issue: issue_to_detail_event(&issue),
                                    });
                                }
                                Err(e) => {
                                    self.funnel.emit(SpurEventBody::IssueCommandError {
                                        operation: "GetIssueDetail".into(),
                                        error: e.to_string(),
                                    });
                                }
                            }
                        } else {
                            self.funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "GetIssueDetail".into(),
                                error: "No issue tracker configured".into(),
                            });
                        }
                    }

                    // ── UpdateIssue ───────────────────────────────────────
                    InteractiveInput::UpdateIssue { id, update } => {
                        if let Some(pm) = &self.pm_service {
                            match pm.update_issue(&id, update.clone()).await {
                                Ok(()) => {
                                    self.funnel.emit(SpurEventBody::IssueUpdated {
                                        source: pm.source_str().into(),
                                        id,
                                        status: update.status.clone(),
                                        assignee: update.assignee.clone(),
                                    });
                                }
                                Err(e) => {
                                    self.funnel.emit(SpurEventBody::IssueCommandError {
                                        operation: "UpdateIssue".into(),
                                        error: e.to_string(),
                                    });
                                }
                            }
                        } else {
                            self.funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "UpdateIssue".into(),
                                error: "No issue tracker configured".into(),
                            });
                        }
                    }

                    // ── SubmitReview ──────────────────────────────────────
                    // Intentional no-op: spur-cli routes SubmitReview to the
                    // review_dispatcher_loop task, not to run_interactive.
                    InteractiveInput::SubmitReview { .. } => {}
                }

                // Done handling non-prompt variant — go back to top of loop.
                continue;
            }

            // ── (d) Scheduler returned a prompt action — fire the brain turn ──
            //
            // Decompose the ScheduledAction into (user_input_opt, continuations).
            let (user_input_opt, continuations): (Option<InteractiveInput>, Vec<spur_acp::domain::BrainContinuation>) = match action {
                crate::scheduler::ScheduledAction::UserPrompt(u) => (Some(u), vec![]),
                crate::scheduler::ScheduledAction::MergedPrompt { user, continuations } => {
                    (Some(user), continuations)
                }
                crate::scheduler::ScheduledAction::ContinuationPrompt(cs) => (None, cs),
                crate::scheduler::ScheduledAction::Idle => unreachable!("handled above"),
            };

            // ── Build the blocks for this turn ─────────────────────────
            let prompt_blocks: Vec<ContentBlock> = match &user_input_opt {
                Some(InteractiveInput::Message { blocks, interrupt }) => {
                    let base = if *interrupt {
                        strip_bang_prefix(blocks.clone())
                    } else {
                        blocks.clone()
                    };
                    if continuations.is_empty() {
                        base
                    } else {
                        let (merged, spilled) =
                            crate::continuation_bridge::render_merged_turn_with_spill(
                                &base,
                                &continuations,
                                crate::continuation_bridge::MERGE_BUDGET_DEFAULT_BYTES,
                            );
                        for c in spilled {
                            scheduler.push_continuation(c);
                        }
                        merged
                    }
                }
                None => {
                    // Autonomous continuation-only turn.
                    crate::continuation_bridge::render_autonomous_continuation_turn(&continuations)
                }
                Some(other) => {
                    // Defensive: unexpected variant in scheduler (e.g. a non-Message
                    // that somehow got pushed). Log and skip.
                    tracing::warn!(?other, "unexpected non-Message variant dequeued from scheduler; skipping turn");
                    continue;
                }
            };

            // ── Lazy-spawn brain on first turn (or after crash) ─────────
            if brain.is_none() {
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
                    Ok(b) => {
                        // Wire the new session into the scheduler.
                        let new_sid = Some(SessionId(b.acp_session_id.clone()));
                        let evicted = scheduler.note_session_swap(new_sid);
                        for c in evicted {
                            self.funnel.emit(SpurEventBody::ContinuationDropped {
                                delegation_id: c.delegation_id,
                                reason: spur_acp::domain::events::ContinuationDropReason::SessionSwap,
                            });
                        }
                        brain = Some(b);
                    }
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

            // ── Send prompt ──────────────────────────────────────────────
            let prompt_request = PromptRequest::new(b.acp_session_id.clone(), prompt_blocks);
            let spur_sid_for_log = b.spur_session_id.clone();

            let prompt_started_at = std::time::Instant::now();
            let mut stream = match b.connection.prompt(prompt_request).await {
                Ok(s) => s,
                Err(e) => {
                    error!(error = %e, "Brain prompt failed");
                    if Self::is_auth_required_error(&e) {
                        self.emit(SpurEvent::now(SpurEventBody::AuthRequired {
                            session: spur_sid_for_log,
                            message: Self::auth_required_banner(),
                        }));
                        let mut dead = brain.take().expect("brain.as_mut() just held it");
                        dead.delegation_handle.abort();
                        if let Some(h) = dead.notification_pump_handle.take() {
                            h.abort();
                        }
                        dead.mcp_handle.abort();
                        let _ = dead.connection.shutdown().await;
                        continue;
                    }
                    if is_connection_death(&e) {
                        let dead = brain.take().expect("brain.as_mut() just held it");
                        let reason = format!("prompt died: {e}");
                        if let Some(new_brain) = self
                            .reconnect_with_events(
                                dead,
                                permission_tx.clone(),
                                brain_override.as_deref(),
                                reason,
                                &mut reconnect_failures,
                                RECONNECT_CIRCUIT_LIMIT,
                                RECONNECT_CIRCUIT_WINDOW,
                            )
                            .await
                        {
                            brain = Some(new_brain);
                        }
                        continue;
                    }
                    self.emit(SpurEvent::now(SpurEventBody::BrainError {
                        session: spur_sid_for_log,
                        message: e.to_string(),
                    }));
                    let mut dead = brain.take().expect("brain.as_mut() just held it");
                    dead.delegation_handle.abort();
                    if let Some(h) = dead.notification_pump_handle.take() {
                        h.abort();
                    }
                    dead.mcp_handle.abort();
                    let _ = dead.connection.shutdown().await;
                    continue;
                }
            };

            // ── Stream output + check for interrupts ─────────────────────
            // note_turn_started() / note_turn_finished() bracket the streaming
            // loop. We call note_turn_started() here (before the loop) and
            // note_turn_finished() unconditionally after the loop exits. All
            // early-exit `continue` paths above skip this block entirely, so
            // the scheduler never sees a spurious in-flight flag on error paths.
            scheduler.note_turn_started();
            let mut cancel_deadline: Option<tokio::time::Instant> = None;
            let mut cancel_resolved = false;
            {
                let b = brain.as_mut().unwrap();

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
                                        arm_cancel_deadline(&mut cancel_deadline);
                                    }
                                    let queued_blocks = if msg_interrupt {
                                        strip_bang_prefix(msg_blocks)
                                    } else {
                                        msg_blocks
                                    };
                                    scheduler.push_user(InteractiveInput::Message {
                                        blocks: queued_blocks,
                                        interrupt: false,
                                    });
                                }
                                InteractiveInput::CancelStream { session } => {
                                    let _ = session;
                                    let _ = b.connection.cancel(&b.acp_session_id).await;
                                    arm_cancel_deadline(&mut cancel_deadline);
                                }
                                InteractiveInput::SystemContinuation { continuation, .. } => {
                                    scheduler.push_continuation(continuation);
                                }
                                other => {
                                    // Non-prompt, non-cancel variants arriving mid-stream:
                                    // push to scheduler as user input so they run after the turn.
                                    scheduler.push_user(other);
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
                            cancel_resolved = true;
                            break;
                        }
                    }
                }
            }
            // Unconditional: turn finished regardless of how the loop exited.
            scheduler.note_turn_finished();

            // If cancel was the exit path, note it for the scheduler's grace window.
            if cancel_resolved || cancel_deadline.is_some() {
                scheduler.note_cancel_resolved(std::time::Instant::now());
            }

            // Emit turn complete
            let b = brain.as_mut().unwrap();
            self.emit(SpurEvent::now(SpurEventBody::TurnComplete {
                session: b.spur_session_id.clone(),
            }));
        }

        // ── Cleanup ─────────────────────────────────────────────────────
        if let Some(mut b) = brain.take() {
            b.delegation_handle.abort();
            if let Some(h) = b.notification_pump_handle.take() {
                h.abort();
            }
            b.mcp_handle.abort();
            let _ = b.connection.shutdown().await;
        }
        // Drop any pre-connected but unused connection.
        if let Some((mut conn, _)) = agent_connection.take() {
            let _ = conn.shutdown().await;
        }

        info!("Interactive session ended");
        Ok(())
    }

    /// Execute a task directly on a single agent (no brain, no delegation).
    pub async fn exec_direct(&mut self, agent_name: &str, task: &str) -> Result<RunResult> {
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

        let success = true;
        crate::notification_drain::drive_prompt_notifications(
            &mut *connection,
            prompt_request,
            |notification| match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        print!("{}", tc.text);
                    }
                }
                _ => {}
            },
        )
        .await?;

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

    /// Initialize: scan $PATH for agents declared in the embedded seed
    /// template (`spur_acp::config::load_seed_template`), register those
    /// whose `command` is on $PATH.
    pub async fn init_agents(&mut self) -> Result<Vec<String>> {
        let seeds = spur_acp::config::load_seed_template();
        let mut found = Vec::new();
        for seed in seeds.entries {
            let ok = tokio::process::Command::new("which")
                .arg(&seed.command)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                info!(agent = %seed.name, command = %seed.command, "Found agent");
                found.push(seed.name.clone());
                self.registry.register(seed);
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
            if let Some(h) = b.notification_pump_handle {
                h.abort();
            }
            b.mcp_handle.abort();
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
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
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
        _permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
    ) -> Result<BrainSession> {
        let session_id = SessionId::new();

        info!(brain = %brain_name, session = %session_id, "Creating brain session");
        self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        }));

        // Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id: spur_acp::BrainSessionId = session_id.clone().into();
        let cont_ctx = self.build_continuation_ctx(session_id.clone());
        let (mcp_server, delegation_channel) =
            McpCallbackServer::new(&brain_session_id, self.pm_service.clone(), sink, cont_ctx);
        let mut mcp_server = mcp_server;

        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .into_iter()
            .map(build_worker_info)
            .collect();
        mcp_server.set_workers(workers);
        // INV-6: wire the cancellation side-channel.
        mcp_server.set_cancellation_control(self.cancellation_control.clone());

        let mcp_server = Arc::new(mcp_server);
        let (mcp_url, mcp_handle) = mcp_server
            .clone()
            .start()
            .await
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

        let mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-mcp", &mcp_url))];

        let brain_cfg = self.registry.get(&brain_name).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "brain agent '{}' not in registry during create_brain_session",
                brain_name
            )
        })?;

        // Pre-subscribe BEFORE new_session so notifications the agent emits
        // during session setup (e.g. claude-code-acp's initial
        // `available_commands_update`) land on a live receiver. Broadcast
        // `send()` returns `Err(SendError)` only when every receiver has
        // been dropped; holding `presub_notif_rx` here keeps sends
        // succeeding until we hand the receiver to the pump below.
        let presub_notif_rx = connection.subscribe_session_notifications();

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
            self.funnel.clone(),
            self.review_sink.clone(),
            self.pm_service.clone(),
            self.cancellation_control.clone(),
        ));

        // Spawn the vendor-extension notification pump (if the transport
        // supports it). Each payload becomes a `SpurEventBody::AgentExtNotification`
        // scoped to this brain session.
        if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
            let funnel = self.funnel.clone();
            let spur_session_id = session_id.clone();
            tokio::spawn(async move {
                while let Some(payload) = ext_rx.recv().await {
                    funnel.emit(SpurEventBody::AgentExtNotification {
                        session: spur_session_id.clone(),
                        method: payload.method,
                        params: payload.params,
                    });
                }
            });
        }

        // Fan out session notifications from the connection's broadcast
        // into the SpurEvent bus — see notification_pump::spawn_session_notification_pump.
        // `presub_notif_rx` was subscribed before new_session so we don't
        // miss notifications emitted during session setup.
        let notification_pump_handle = presub_notif_rx.map(|notif_rx| {
            crate::notification_pump::spawn_session_notification_pump(
                notif_rx,
                session_id.clone(),
                self.funnel.clone(),
            )
        });

        self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session_id.clone(),
            acp_session_id: session_response.session_id.to_string(),
            brain: brain_name.clone(),
            resumed: false,
            cancel_mode: cancel_mode_for(brain_cfg.transport),
        }));

        Ok(BrainSession {
            connection,
            acp_session_id: session_response.session_id.to_string(),
            spur_session_id: session_id,
            brain_name,
            delegation_handle,
            notification_pump_handle,
            mcp_handle,
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
        _permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        acp_session_id: String,
        preserve_spur_session_id: Option<SessionId>,
        force_new_session: bool,
    ) -> Result<(
        BrainSession,
        std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>>,
        spur_acp::LoadOutcome,
    )> {
        let (session_id, is_reconnect) = match preserve_spur_session_id {
            Some(sid) => (sid, true),
            None => (SessionId::new(), false),
        };

        info!(brain = %brain_name, session = %session_id, acp_session = %acp_session_id, "Loading brain session");
        if !is_reconnect {
            self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
                agent: brain_name.clone(),
                session: session_id.clone(),
            }));
        }

        // Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id: spur_acp::BrainSessionId = session_id.clone().into();
        let cont_ctx = self.build_continuation_ctx(session_id.clone());
        let (mcp_server, delegation_channel) =
            McpCallbackServer::new(&brain_session_id, self.pm_service.clone(), sink, cont_ctx);
        let mut mcp_server = mcp_server;

        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .into_iter()
            .map(build_worker_info)
            .collect();
        mcp_server.set_workers(workers);
        // INV-6: wire the cancellation side-channel.
        mcp_server.set_cancellation_control(self.cancellation_control.clone());

        let mcp_server = Arc::new(mcp_server);
        let (mcp_url, mcp_handle) = mcp_server
            .clone()
            .start()
            .await
            .context("Failed to start MCP callback server")?;

        // Log session start.
        if !is_reconnect {
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
        }

        let mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-mcp", &mcp_url))];

        let brain_cfg = self.registry.get(&brain_name).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "brain agent '{}' not in registry during load_brain_session",
                brain_name
            )
        })?;

        // Pre-subscribe BEFORE load_session so the entire history replay
        // (published via the broadcast during load_session for native
        // transports) lands on a live receiver. Holding `presub_notif_rx`
        // here keeps broadcast sends succeeding until we hand the receiver
        // to the pump below.
        let presub_notif_rx = connection.subscribe_session_notifications();

        // Try load_session first. If the agent doesn't support it (e.g. kiro-cli),
        // fall back to new_session so we have a working session for subsequent prompts.
        // The historical conversation is displayed from the disk fallback in either case.
        let (final_acp_session_id, history_stream, resumed, load_outcome) = if force_new_session {
            // Escalated reconnect: don't even try session/load — just spawn fresh.
            let session_response = crate::skip_perm::new_session_with_bypass(
                &mut *connection,
                &brain_cfg,
                self.repo_root.clone(),
                mcp_servers.clone(),
            )
            .await
            .context("Failed to create fresh session during escalated reconnect")?;
            (
                session_response.session_id.to_string(),
                None,
                false,
                spur_acp::LoadOutcome::FellBackToNew {
                    reason: "escalated to fresh session after repeated failures".into(),
                },
            )
        } else {
            match crate::skip_perm::load_session_with_bypass(
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
                    (
                        acp_session_id,
                        Some(stream),
                        true,
                        spur_acp::LoadOutcome::Restored,
                    )
                }
                Err(e) => {
                    warn!(brain = %brain_name, error = %e, "load_session failed, falling back to new_session");
                    let fallback_reason = e.to_string();
                    let session_response = crate::skip_perm::new_session_with_bypass(
                        &mut *connection,
                        &brain_cfg,
                        self.repo_root.clone(),
                        mcp_servers,
                    )
                    .await
                    .context("Failed to create fallback session after load_session failure")?;
                    (
                        session_response.session_id.to_string(),
                        None,
                        false,
                        spur_acp::LoadOutcome::FellBackToNew {
                            reason: fallback_reason,
                        },
                    )
                }
            }
        };

        // Spawn delegation handler.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.funnel.clone(),
            self.review_sink.clone(),
            self.pm_service.clone(),
            self.cancellation_control.clone(),
        ));

        // Pump vendor-extension notifications onto the event stream.
        if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
            let funnel = self.funnel.clone();
            let spur_session_id = session_id.clone();
            tokio::spawn(async move {
                while let Some(payload) = ext_rx.recv().await {
                    funnel.emit(SpurEventBody::AgentExtNotification {
                        session: spur_session_id.clone(),
                        method: payload.method,
                        params: payload.params,
                    });
                }
            });
        }

        // Fan out session notifications from the connection's broadcast
        // into the SpurEvent bus — see notification_pump::spawn_session_notification_pump.
        // `presub_notif_rx` was subscribed before load_session so history
        // replay items aren't missed.
        let notification_pump_handle = presub_notif_rx.map(|notif_rx| {
            crate::notification_pump::spawn_session_notification_pump(
                notif_rx,
                session_id.clone(),
                self.funnel.clone(),
            )
        });

        self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session_id.clone(),
            acp_session_id: final_acp_session_id.clone(),
            brain: brain_name.clone(),
            resumed,
            cancel_mode: cancel_mode_for(brain_cfg.transport),
        }));

        let brain_session = BrainSession {
            connection,
            acp_session_id: final_acp_session_id,
            spur_session_id: session_id,
            brain_name,
            delegation_handle,
            notification_pump_handle,
            mcp_handle,
        };

        // Return an empty stream if we fell back to new_session.
        let stream: std::pin::Pin<
            Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>,
        > = match history_stream {
            Some(s) => s,
            None => Box::pin(futures::stream::empty()),
        };

        Ok((brain_session, stream, load_outcome))
    }

    /// Attempt to reconnect after a brain subprocess death. Drops the
    /// dead `BrainSession` (closing its stdio and aborting its helper
    /// tasks), spawns a fresh connection via `connect_brain`, then
    /// reattaches via `load_brain_session` using the old
    /// `acp_session_id`.
    ///
    /// On success returns the new `BrainSession` and the `LoadOutcome`
    /// distinguishing "session/load restored state" from "we fell back
    /// to a new session". On failure the caller must surface
    /// `BrainReconnectFailed` and leave `brain = None`.
    ///
    /// The caller (not this helper) is responsible for emitting
    /// `BrainReconnecting` BEFORE invoking this, and
    /// `BrainReconnected` / `BrainReconnectFailed` after.
    async fn try_reconnect_brain(
        &mut self,
        dead_brain: BrainSession,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        brain_override: Option<&str>,
        force_new_session: bool,
    ) -> Result<(BrainSession, spur_acp::LoadOutcome)> {
        let acp_session_id = dead_brain.acp_session_id.clone();
        let preserve_spur_id = dead_brain.spur_session_id.clone();
        let brain_name_hint = dead_brain.brain_name.clone();

        // Drop the dead session: abort helper tasks, close stdio.
        dead_brain.delegation_handle.abort();
        if let Some(h) = dead_brain.notification_pump_handle {
            h.abort();
        }
        dead_brain.mcp_handle.abort();
        drop(dead_brain.connection);

        // Fresh connection + reattach.
        let (connection, brain_name) = self
            .connect_brain(brain_override, permission_tx.clone())
            .await
            .with_context(|| format!("reconnect: connect_brain failed for '{brain_name_hint}'"))?;

        let (new_session, mut history_stream, outcome) = self
            .load_brain_session(
                connection,
                brain_name,
                permission_tx,
                acp_session_id,
                Some(preserve_spur_id),
                force_new_session,
            )
            .await
            .with_context(|| {
                format!("reconnect: load_brain_session failed for '{brain_name_hint}'")
            })?;

        // Drain the history stream to keep the pump contract (same
        // pattern as the ResumeSession arm). We do NOT re-emit
        // AgentNotification events here — the TUI already rendered the
        // pre-death transcript.
        while let Some(_notification) = history_stream.next().await {}

        Ok((new_session, outcome))
    }

    /// Wrap `try_reconnect_brain` with the three event emissions and
    /// the circuit-breaker bookkeeping. Returns `Some(new_brain)` if
    /// reconnect succeeded; `None` if the breaker is open or reconnect
    /// failed (in which case `BrainReconnectFailed` was already
    /// emitted).
    #[allow(clippy::too_many_arguments)]
    async fn reconnect_with_events(
        &mut self,
        dead_brain: BrainSession,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        brain_override: Option<&str>,
        trigger_reason: String,
        failures: &mut std::collections::VecDeque<std::time::Instant>,
        limit: usize,
        window: std::time::Duration,
    ) -> Option<BrainSession> {
        let spur_session_id = dead_brain.spur_session_id.clone();
        let brain_name = dead_brain.brain_name.clone();

        // Trim stale death timestamps and record this death.
        let now = std::time::Instant::now();
        while let Some(front) = failures.front() {
            if now.duration_since(*front) > window {
                failures.pop_front();
            } else {
                break;
            }
        }
        failures.push_back(now);

        // Decide tier: if we've exceeded the budget of deaths in the window,
        // escalate to a fresh-session reconnect.
        let escalate = failures.len() > limit;
        let (reconnecting_reason, force_new) = if escalate {
            (
                format!(
                    "{} — escalating to fresh session after {} deaths within {:?}",
                    trigger_reason,
                    failures.len(),
                    window
                ),
                true,
            )
        } else {
            (trigger_reason, false)
        };

        self.emit(SpurEvent::now(SpurEventBody::BrainReconnecting {
            session: spur_session_id.clone(),
            brain_name: brain_name.clone(),
            reason: reconnecting_reason,
        }));

        match self
            .try_reconnect_brain(dead_brain, permission_tx, brain_override, force_new)
            .await
        {
            Ok((new_brain, outcome)) => {
                // Tier 1 success clears the window; Tier 2 success keeps the
                // record so a quick re-death after escalation still trips.
                if !escalate {
                    failures.clear();
                }
                self.emit(SpurEvent::now(SpurEventBody::BrainReconnected {
                    session: new_brain.spur_session_id.clone(),
                    brain_name: new_brain.brain_name.clone(),
                    outcome,
                }));
                Some(new_brain)
            }
            Err(e) => {
                self.emit(SpurEvent::now(SpurEventBody::BrainReconnectFailed {
                    session: spur_session_id,
                    brain_name,
                    reason: e.to_string(),
                }));
                None
            }
        }
    }

    /// Spawn a brain agent session with MCP callback server and delegation handler.
    pub async fn spawn_brain_session(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
    ) -> Result<BrainSession> {
        let (connection, brain_name) = self
            .connect_brain(brain_override, permission_tx.clone())
            .await?;
        self.create_brain_session(connection, brain_name, permission_tx)
            .await
    }

    /// Fallback: read sessions from an agent's local storage on disk.
    /// Currently supports kiro-cli (~/.kiro/sessions/cli/*.json).
    fn list_sessions_from_disk(agent_name: &str) -> Result<Vec<SessionInfo>> {
        // kiro-cli stores sessions in ~/.kiro/sessions/cli/<uuid>.json
        if agent_name.contains("kiro") {
            let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
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

            info!(
                count = sessions.len(),
                "Loaded sessions from kiro disk storage"
            );
            return Ok(sessions);
        }

        anyhow::bail!(
            "No filesystem fallback available for agent '{}'",
            agent_name
        )
    }

    /// Read conversation history from a kiro session's JSONL file on disk.
    /// Returns (role, text) pairs for Prompt and AssistantMessage entries.
    fn read_session_history_from_disk(session_uuid: &str) -> Vec<spur_acp::HistoryEntry> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
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
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
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

    fn build_brain_prompt(
        &self,
        task: &str,
        issue: Option<&Issue>,
        session_id: &SessionId,
        brain_name: &str,
    ) -> String {
        if self.config.brain.delegation.framework == "v1" {
            self.build_brain_prompt_v1(task, issue, session_id, brain_name)
        } else {
            self.build_brain_prompt_legacy(task, issue)
        }
    }

    fn build_brain_prompt_legacy(&self, task: &str, issue: Option<&Issue>) -> String {
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

    fn build_brain_prompt_v1(
        &self,
        task: &str,
        issue: Option<&Issue>,
        session_id: &SessionId,
        brain_name: &str,
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(&self.render_header());
        prompt.push_str(&self.render_workers_block());
        if let Some(framework) = crate::skills::load_skill("brain-delegation", &self.repo_root) {
            prompt.push_str(&framework);
        }
        let agent_skill = format!("brain-delegation-{}", brain_name);
        if let Some(guidance) = crate::skills::load_skill(&agent_skill, &self.repo_root) {
            prompt.push_str(&guidance);
        }
        self.append_issue_and_task(&mut prompt, task, issue);
        self.log_prompt_once(&prompt, session_id);
        prompt
    }

    fn render_header(&self) -> String {
        "You are a brain coordinating a coding task. You have two kinds of tools:\n\
         \n\
         1. Your own tools (filesystem, bash, git) — for investigation and direct edits.\n\
         2. SPUR delegation tools (delegate_to_worker, delegate_parallel, list_available_workers) — for handing work to worker agents that run in isolated worktrees.\n\n".into()
    }

    fn render_workers_block(&self) -> String {
        let mut out = String::from("## Available worker agents\n\n");
        let mut agents: Vec<_> = self.registry.worker_capable().into_iter().collect();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        let mut any_listed = false;
        for agent in agents {
            if agent.delegation.good_for.is_empty() {
                continue;
            }
            any_listed = true;
            let tier = agent
                .delegation
                .tier
                .map(|t| match t {
                    spur_acp::config::Tier::Specialist => "specialist",
                    spur_acp::config::Tier::Generalist => "generalist",
                })
                .unwrap_or("generalist");
            let cost = format!("{:?}", agent.cost_tier).to_lowercase();
            let desc = agent
                .delegation
                .description
                .as_deref()
                .unwrap_or("(no description)");
            out.push_str(&format!(
                "### {}  ({}, cost: {})\n{}\n\n",
                agent.name, tier, cost, desc,
            ));
        }
        if !any_listed {
            out.push_str("(no worker-capable agents with descriptors configured)\n\n");
        }
        out
    }

    fn append_issue_and_task(&self, prompt: &mut String, task: &str, issue: Option<&Issue>) {
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
    }

    fn log_prompt_once(&self, prompt: &str, session_id: &SessionId) {
        let dir = self.repo_root.join(".spur/logs/brain-prompts");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::debug!(error = %e, "could not create brain-prompts log dir");
            return;
        }
        // Use the spur session id as the filename so that repeated calls within
        // the same session overwrite the prior log (one log per session intent).
        // SessionId wraps a UUID string, which is filename-safe by construction.
        let path = dir.join(format!("{}.md", session_id));
        if let Err(e) = std::fs::write(&path, prompt) {
            tracing::debug!(error = %e, path = %path.display(), "could not write prompt log");
        }
        enforce_log_cap(&dir, 50 * 1024 * 1024);
    }

    async fn fetch_issue_context(&self, issue_ref: &str) -> Result<Issue> {
        let pm = self
            .pm_service
            .as_ref()
            .ok_or_else(|| anyhow!("No issue tracker configured"))?;

        // Strip prefix if present (e.g., "github:owner/repo#42" → "42")
        let id = if let Some(rest) = issue_ref.strip_prefix("github:") {
            rest.rsplit_once('#').map(|(_, id)| id).unwrap_or(rest)
        } else if let Some(rest) = issue_ref.strip_prefix("beads:") {
            rest
        } else {
            issue_ref
        };

        pm.get_issue(id).await
    }

    /// Emit an event through the S2 funnel. The funnel stamps `seq` +
    /// `occurred_at`, so the caller's `event.occurred_at` is discarded —
    /// the funnel's value is more accurate (wall-clock at send-to-broadcast
    /// moment). Signature unchanged so the ~22 method-scope
    /// `self.emit(SpurEvent::now(body))` callers compile transparently.
    fn emit(&self, event: SpurEvent) {
        self.funnel.emit(event.body);
    }

    /// Handle delegation requests from the MCP callback server.
    ///
    /// Spawns each delegation as a separate tokio task, allowing multiple
    /// workers to run concurrently. A semaphore limits the number of
    /// simultaneous workers to `max_concurrent`.
    #[allow(clippy::too_many_arguments)]
    async fn handle_delegations(
        mut channel: DelegationChannel,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        max_concurrent: usize,
        funnel: crate::event_funnel::FunnelHandle,
        review_sink: ReviewSink,
        pm_service: Option<Arc<PmService>>,
        cancellation_control: CancellationControl,
    ) {
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        // Debounce: skip post-delegation refresh if another completed <3s ago.
        // Initial value is in the past so the first refresh always runs.
        let last_refresh_at = Arc::new(tokio::sync::Mutex::new(
            tokio::time::Instant::now() - std::time::Duration::from_secs(60),
        ));

        while let Some(request) = channel.request_rx.recv().await {
            // Destructure the request — it is not Clone, so we move each field.
            let DelegationRequest {
                id: request_id,
                agent,
                task,
                context_files,
                respond_to,
                brain_session_id,
                delegation_plan,
                issue_id,
            } = request;

            debug!(
                agent = %agent,
                task = %task,
                "Received delegation request"
            );

            let repo_root = repo_root.clone();
            let agent_configs = agent_configs.clone();
            let semaphore = Arc::clone(&semaphore);
            let funnel = funnel.clone();
            let review_sink = review_sink.clone();
            let pm_service = pm_service.clone();
            let last_refresh_at = Arc::clone(&last_refresh_at);

            // INV-6: register a cancellation token BEFORE spawning so
            // cancel() arriving between dispatch and spawn still works.
            let cancel_token = {
                let cc = cancellation_control.clone();
                cc.register(request_id.clone()).await
            };
            let cancellation_control_for_task = cancellation_control.clone();

            tokio::spawn(async move {
                let mut guard = DelegationGuard {
                    funnel: funnel.clone(),
                    respond_to: Some(respond_to),
                    request_id: request_id.clone(),
                    disarmed: false,
                };

                // Acquire a permit before starting the delegation.
                let _permit = match semaphore.acquire().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        error!("Semaphore closed — aborting delegation");
                        // Clean up the token if we abort early.
                        cancellation_control_for_task.remove(&request_id).await;
                        return; // guard fires DelegationCompleted(Failed)
                    }
                };

                // Claim issue on delegation start (10f).
                if let (Some(ref issue_id), Some(ref pm)) = (&issue_id, &pm_service) {
                    let worker_name = format!("spur-worker-{}", request_id);
                    if let Err(e) = pm
                        .update_issue(
                            issue_id,
                            spur_pm::IssueUpdate {
                                status: Some("in_progress".into()),
                                assignee: Some(worker_name.clone()),
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        tracing::warn!(issue = %issue_id, "Failed to claim issue: {e}");
                    } else {
                        funnel.emit(SpurEventBody::IssueUpdated {
                            source: pm.source_str().into(),
                            id: issue_id.clone(),
                            status: Some("in_progress".into()),
                            assignee: Some(worker_name),
                        });
                    }
                }

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
                //
                // INV-6: race execute_delegation against the per-delegation
                // cancellation token. If cancel() arrives first, we return
                // DelegationStatus::Cancelled without waiting for the worker.
                let (result, executor_id_opt) = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        let status = DelegationStatus::Cancelled {
                            reason: "brain requested cancel".into(),
                        };
                        // Emit DelegationCompleted so TUI, lineage, and
                        // other funnel subscribers don't see a stale
                        // "active" entry for this delegation.
                        funnel.emit(SpurEventBody::DelegationCompleted {
                            worker_session: spur_acp::types::SessionId(request_id.clone()),
                            status: status.clone(),
                        });
                        (
                            DelegationResult {
                                status,
                                diff: None,
                                diff_summary: None,
                                summary: None,
                                estimated_cost_usd: 0.0,
                                worker_branch: None,
                            },
                            None,
                        )
                    }
                    r = Self::execute_delegation(
                        agent,
                        task,
                        context_files,
                        request_id.clone(),
                        brain_session_id,
                        delegation_plan,
                        issue_id.clone(),
                        repo_root,
                        agent_configs,
                        funnel.clone(),
                        review_sink.clone(),
                    ) => r,
                };
                // Always clean up the token entry (avoids stale entries
                // when the delegation completes normally before cancel fires).
                cancellation_control_for_task.remove(&request_id).await;

                // Comment on / revert issue on completion (10g).
                if let (Some(ref issue_id), Some(ref pm)) = (&issue_id, &pm_service) {
                    let (new_status, comment) = match &result.status {
                        // Success — DON'T close, just comment. Brain decides when to close.
                        DelegationStatus::Success => {
                            (None, format!("Completed by SPUR delegation {}", request_id))
                        }
                        DelegationStatus::Rejected { .. } => {
                            (Some("open"), format!("Delegation {} rejected", request_id))
                        }
                        DelegationStatus::Failed { error } => (
                            Some("open"),
                            format!("Delegation {} failed: {}", request_id, error),
                        ),
                        _ => (Some("open"), format!("Delegation {} ended", request_id)),
                    };

                    let update = spur_pm::IssueUpdate {
                        status: new_status.map(String::from),
                        comment: Some(comment),
                        ..Default::default()
                    };

                    if let Err(e) = pm.update_issue(issue_id, update).await {
                        tracing::warn!(issue = %issue_id, "Failed to transition issue: {e}");
                    } else if let Some(status) = new_status {
                        funnel.emit(SpurEventBody::IssueUpdated {
                            source: pm.source_str().into(),
                            id: issue_id.clone(),
                            status: Some(status.into()),
                            assignee: None,
                        });
                    }
                }

                // Refresh issue list + graph alerts after delegation completes
                // so TUI picks up changes made by the worker (F19).
                // Debounce: skip if another delegation refreshed <3s ago
                // (prevents thundering herd from delegate_parallel).
                if let Some(ref pm) = pm_service {
                    let mut last = last_refresh_at.lock().await;
                    if last.elapsed() >= std::time::Duration::from_secs(3) {
                        *last = tokio::time::Instant::now();
                        drop(last); // release lock before async work
                        refresh_pm_state(pm, &funnel, Some(50), false).await;
                    } else {
                        tracing::debug!("Skipping post-delegation refresh (debounced)");
                    }
                }

                // Normal path: disarm the guard and send result manually.
                guard.disarmed = true;
                let respond_to = guard.respond_to.take().unwrap();

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
                            &funnel,
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
        context_files: Vec<String>,
        request_id: String,
        brain_session_id: spur_acp::BrainSessionId,
        delegation_plan: Option<spur_acp::domain::DelegationPlan>,
        issue_id: Option<String>,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        funnel: crate::event_funnel::FunnelHandle,
        review_sink: ReviewSink,
    ) -> (DelegationResult, Option<ExecutorId>) {
        // Shadow `original_task` with the Relevant Files-prepended form
        // so retry loops at orchestrator.rs:3013 reuse the formatted
        // base. No-op when context_files is empty.
        let original_task = format_worker_task(&original_task, &context_files);
        // `__`-prefixed agent names are reserved for internal operations.
        // __cancel_delegation no longer routes through this path (INV-6 —
        // cancellation now goes through CancellationControl). Any other
        // `__`-prefixed name is an unsupported internal operation.
        if agent.starts_with("__") {
            return (
                DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("Unsupported internal operation: {agent}"),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
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
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                    },
                    None,
                );
            }
        };

        let mut current_task = original_task.clone();
        // Retry-history accumulator. Each retry attempt pushes its
        // prior attempt's (summary, diff_summary, reviewer feedback)
        // so the NEXT attempt's prompt can reference what was tried.
        // 2 KB bloat cap drops oldest entries first.
        let mut retry_history: Vec<RetryAttempt> = Vec::new();
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
                &WorkerAttemptCtx {
                    brain_session_id: &brain_session_id,
                    agent: &agent,
                    task: &current_task,
                    request_id: &request_id,
                    agent_config: &agent_config,
                    delegation_plan: delegation_plan.clone(),
                    issue_id: issue_id.clone(),
                },
                &mut worktrees,
                &funnel,
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
                            &funnel,
                            next_worker_session,
                            DelegationStatus::Failed {
                                error: setup_err.to_string(),
                            },
                            None,
                            None,
                            None,
                            total_cost,
                            None,
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
                let preserved_branch = apply_worktree_cleanup(
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
                        &funnel,
                        outcome.worker_session,
                        outcome.candidate_status,
                        outcome.diff,
                        outcome.diff_summary,
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                    ),
                    executor_id.clone(),
                );
            }

            // INV-4: obtain a ReviewHandle first — it is the ONLY way to
            // emit `ExecutorReviewRequested` for this slot, enforced at
            // the type level. `register_handle` wraps `ReviewSink::register`
            // so the ordering invariant (register-before-emit) is preserved.
            let handle = match review_sink.register_handle(eid.clone(), attempt_n).await {
                Ok(h) => h,
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
                    let preserved_branch = apply_worktree_cleanup(
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
                            &funnel,
                            outcome.worker_session,
                            failed_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            funnel.emit(SpurEventBody::ExecutorPhaseChanged {
                id: eid.0.clone(),
                phase: LifecycleState::AwaitingReview,
            });

            let plan = delegation_plan.as_ref();
            let chosen_matches_dispatched = plan
                .and_then(|p| p.chosen.as_ref())
                .map(|c| normalize_agent_name(c) == normalize_agent_name(&agent));

            if chosen_matches_dispatched == Some(false) {
                tracing::warn!(
                    session = %brain_session_id,
                    chosen = %plan.and_then(|p| p.chosen.as_deref()).unwrap_or(""),
                    dispatched = %agent,
                    "delegation_plan.chosen does not match dispatched agent",
                );
            }

            let review_payload = ReviewPayload {
                summary: outcome.summary.clone().unwrap_or_default(),
                diff_summary: outcome.diff_summary.clone(),
                pr_url: None,
                error: None,
                delegation_plan: delegation_plan.clone(),
                chosen_matches_dispatched,
            };
            // Emit via the handle — type-enforced: no handle → no emit.
            handle.emit_requested(&funnel, ReviewKind::Completion, review_payload);

            // Consume the handle to get the receiver for the decision loop.
            let rx = handle.into_rx();

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
                    funnel.emit(SpurEventBody::ExecutorReviewCancelled {
                        id: eid.0.clone(),
                        reason: "review timeout".to_string(),
                    });
                    // TimedOut → preserve worktree (no commit).
                    let preserved_branch = apply_worktree_cleanup(
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
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            match decision_result {
                Some(ReviewDecision::Approve) => {
                    let final_status = outcome.candidate_status.clone();
                    funnel.emit(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Approve,
                    });
                    // Approve → commit + remove.
                    let preserved_branch = apply_worktree_cleanup(
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
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Reject { reason }) => {
                    let final_status = DelegationStatus::Rejected {
                        reason: reason.clone(),
                    };
                    funnel.emit(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Reject { reason },
                    });
                    // Rejected → no commit, preserve worktree.
                    let preserved_branch = apply_worktree_cleanup(
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
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Modify { note }) => {
                    let final_status = DelegationStatus::Modified {
                        reviewer_note: note.clone(),
                    };
                    funnel.emit(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Modify { note },
                    });
                    // Modified → commit + remove (approved with reviewer note).
                    let preserved_branch = apply_worktree_cleanup(
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
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Retry { new_constraints }) => {
                    // DN-2: bound check + exhaustion status live in
                    // `crate::retry_loop::RetryLoop` — shared with
                    // `test_support::run_gate_with_retries`. Both sites
                    // share the strict `>` semantic and the exact error
                    // string format. Changes to retry semantics should
                    // touch `retry_loop.rs`, not this site.
                    //
                    // `>` (not `>=`): spec's "Retry × 4 when
                    // max_review_retries = 3 produces Failed" means 3
                    // retries are allowed (attempts bump 1→2→3→4), and
                    // the 4th Retry decision fails.
                    if let Some(final_status) = crate::retry_loop::RetryLoop::check_exceeded(
                        attempt_n,
                        agent_config.review.max_review_retries,
                    ) {
                        // Retry limit → Failed (remove, no commit).
                        let preserved_branch = apply_worktree_cleanup(
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
                                &funnel,
                                outcome.worker_session,
                                final_status,
                                outcome.diff,
                                outcome.diff_summary.clone(),
                                outcome.summary,
                                total_cost,
                                preserved_branch,
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
                    funnel.emit(SpurEventBody::ExecutorRetryStarted {
                        id: eid.0.clone(),
                        attempt_n: attempt_n + 1,
                        reason: new_constraints.clone(),
                        new_session_id: retry_session.clone(),
                    });

                    // Record this attempt in the retry history before re-prompting.
                    // See docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md
                    // for the rationale — inverts the original
                    // "prevent compounding" choice in favor of the
                    // Reflexion pattern, with a 2KB bloat cap as the
                    // mitigation.
                    retry_history.push(RetryAttempt {
                        attempt_n,
                        summary: outcome.summary.clone().unwrap_or_default(),
                        diff_summary: outcome.diff_summary.clone(),
                        feedback: new_constraints.clone(),
                    });
                    apply_bloat_cap(&mut retry_history, 2048);

                    current_task =
                        render_retry_context(&retry_history, &original_task, &new_constraints);
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

                    // Exponential backoff: 1s, 2s, 4s, 8s, … capped at 30s.
                    let backoff_secs =
                        std::cmp::min(1u64 << (attempt_n.saturating_sub(1) as u64), 30);
                    tracing::info!(
                        attempt_n = attempt_n,
                        backoff_secs = backoff_secs,
                        "retry backoff before next attempt"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;

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
                    funnel.emit(SpurEventBody::ExecutorReviewCancelled {
                        id: eid.0.clone(),
                        reason: "review sender dropped".to_string(),
                    });
                    // Sender-drop TimedOut → preserve worktree (no commit).
                    let preserved_branch = apply_worktree_cleanup(
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
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
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
    funnel: &crate::event_funnel::FunnelHandle,
    review_sink: &ReviewSink,
) {
    funnel.emit(SpurEventBody::ExecutorReviewCancelled {
        id: executor_id.0.clone(),
        reason: reason.to_string(),
    });
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
            // INV-6: preserve partial work for cancelled delegations so
            // the brain/user can inspect what was done before cancellation.
            | DelegationStatus::Cancelled { .. }
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
) -> Option<String> {
    if should_commit_worker_diff(final_status) && diff.is_some() {
        if let Err(e) = worktrees
            .commit_worker_changes(worker_session, &format!("spur: worker {} output", agent))
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
        None
    } else if should_commit_worker_diff(final_status) {
        // Approved work: remove worktree dir but keep branch for merge.
        match worktrees.detach_worktree(worker_session).await {
            Ok(branch) => Some(branch),
            Err(e) => {
                tracing::warn!(error = %e, "detach_worktree failed, falling back to full remove");
                let _ = worktrees.remove_worktree(worker_session).await;
                None
            }
        }
    } else {
        let _ = worktrees.remove_worktree(worker_session).await;
        None
    }
}

/// RAII guard that ensures every delegation emits `DelegationCompleted`
/// even on early-exit or task abort. Disarmed by the normal completion
/// path; fires on Drop otherwise.
struct DelegationGuard {
    funnel: crate::event_funnel::FunnelHandle,
    respond_to: Option<tokio::sync::oneshot::Sender<DelegationResult>>,
    request_id: String,
    disarmed: bool,
}

impl Drop for DelegationGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        error!(
            request_id = %self.request_id,
            "DelegationGuard fired — emitting DelegationCompleted(Failed)"
        );
        self.funnel.emit(SpurEventBody::DelegationCompleted {
            worker_session: SessionId(self.request_id.clone()),
            status: DelegationStatus::Failed {
                error: "delegation aborted (early exit or task cancelled)".into(),
            },
        });
        if let Some(tx) = self.respond_to.take() {
            let _ = tx.send(DelegationResult {
                status: DelegationStatus::Failed {
                    error: "delegation aborted".into(),
                },
                diff: None,
                diff_summary: None,
                summary: None,
                estimated_cost_usd: 0.0,
                worker_branch: None,
            });
        }
    }
}

/// Common terminal-arm helper: emits `DelegationCompleted` and
/// constructs the `DelegationResult`. Centralizing this makes the
/// "every terminal emits DelegationCompleted" invariant locally
/// verifiable (one call site per terminal arm in `execute_delegation`).
#[allow(clippy::too_many_arguments)]
fn finalize(
    funnel: &crate::event_funnel::FunnelHandle,
    worker_session: SessionId,
    final_status: DelegationStatus,
    diff: Option<String>,
    diff_summary: Option<spur_acp::DiffSummary>,
    summary: Option<String>,
    total_cost: f64,
    worker_branch: Option<String>,
) -> DelegationResult {
    funnel.emit(SpurEventBody::DelegationCompleted {
        worker_session,
        status: final_status.clone(),
    });
    DelegationResult {
        status: final_status,
        diff,
        diff_summary,
        summary,
        estimated_cost_usd: total_cost,
        worker_branch,
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
    diff_summary: Option<spur_acp::DiffSummary>,
    summary: Option<String>,
    cost: f64,
    /// Path to the worktree that holds this attempt's diff.
    /// Used by `execute_delegation` to log a preserved path on
    /// `Rejected` / `TimedOut` — worktree removal is deferred to
    /// after the review gate.
    worktree_path: PathBuf,
}

/// Map a transport kind to its `CancelMode`. Single source of truth used
/// by `AgentSessionReady` emitters so the TUI can render transport-aware
/// cancel feedback without re-inspecting `AgentConfig`.
pub(crate) fn cancel_mode_for(transport: spur_acp::types::TransportKind) -> spur_acp::CancelMode {
    use spur_acp::types::TransportKind;
    match transport {
        TransportKind::Acp => spur_acp::CancelMode::AcpSoft,
        TransportKind::Stdio | TransportKind::CliWrap | TransportKind::StreamJson => {
            spur_acp::CancelMode::ProcessKill
        }
    }
}

/// Arm the 5-second force-end deadline used by the streaming `select!`.
/// Factored out so both the `Message { interrupt: true }` arm and the
/// new `CancelStream` arm set the deadline identically and so it is
/// directly unit-testable without a full mock orchestrator.
pub(crate) fn arm_cancel_deadline(deadline: &mut Option<tokio::time::Instant>) {
    *deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5));
}

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

// ─── WorkerFileTouched synthesis (S5 / Task 17) ──────────────────────
//
// Workers using general-purpose agents (kiro, claude-code, codex) do
// NOT emit `_spur/file_touched` ExtNotifications. Instead, the
// orchestrator synthesizes `WorkerFileTouched` events by observing
// the worker's ToolCall stream for known file-op tool names and
// extracting the `path`/`file_path` input field. A 200ms de-dup
// window coalesces repeated ToolCall / ToolCallUpdate events for the
// same (executor, path, kind) so a single logical file operation
// emits at most one `WorkerFileTouched` per 200ms window.
//
// Note: this dedup is local to the synthesizer's stream loop. It
// does NOT coordinate with `spur_ext_interp::interpret`, which
// handles the explicit `_spur/file_touched` ExtNotification path —
// if a SPUR-aware worker ever emits both an explicit event AND a
// matching ToolCall, the subscriber would see two events. Future
// work: share an `Arc<FileTouchDedup>` across both paths to
// guarantee at-most-one emit per (executor, path, kind).

/// De-dup key for the 200ms file-touch window.
#[derive(Hash, Eq, PartialEq, Clone)]
struct FileTouchKey {
    executor_id: String,
    path: std::path::PathBuf,
    kind: spur_acp::domain::events::FileTouchKind,
}

/// Per-worker-attempt de-dup for `WorkerFileTouched` synthesis.
/// Coalesces repeated ToolCall / ToolCallUpdate events for the same
/// (executor, path, kind) within a 200ms window, so a single logical
/// file operation emits at most one `WorkerFileTouched` per window.
///
/// Scope is a single `run_one_worker_attempt` invocation; cross-worker
/// coordination isn't needed because `executor_id` is unique per worker.
struct FileTouchDedup {
    last_seen: std::sync::Mutex<std::collections::HashMap<FileTouchKey, std::time::Instant>>,
    ttl: std::time::Duration,
}

impl FileTouchDedup {
    fn new() -> Self {
        Self {
            last_seen: std::sync::Mutex::new(std::collections::HashMap::new()),
            ttl: std::time::Duration::from_millis(200),
        }
    }

    /// Returns true if this (executor, path, kind) is fresh and should
    /// be emitted. Updates the last-seen map.
    fn should_emit(&self, key: &FileTouchKey) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.last_seen.lock().unwrap();
        // Garbage collect stale entries opportunistically.
        map.retain(|_, t| now.duration_since(*t) < self.ttl * 5);
        match map.get(key) {
            Some(last) if now.duration_since(*last) < self.ttl => false,
            _ => {
                map.insert(key.clone(), now);
                true
            }
        }
    }
}

/// If `notification` is a ToolCall matching a known file-op tool name,
/// synthesize a WorkerFileTouched event (subject to dedup).
///
/// The `title` field of the ACP `ToolCall` struct carries the tool name
/// as populated by adapters (e.g. claude_events maps Anthropic's
/// `tool_use.name` into `title`). Path extraction tries `raw_input`'s
/// `path` / `file_path` fields first, then falls back to the first
/// entry in `locations` if raw_input is missing the key.
fn maybe_synthesize_file_touch(
    notification: &agent_client_protocol::SessionNotification,
    brain_session_id: &spur_acp::types::SessionId,
    executor_id: &str,
    dedup: &FileTouchDedup,
    funnel: &crate::event_funnel::FunnelHandle,
) {
    let tc = match &notification.update {
        SessionUpdate::ToolCall(tc) => tc,
        _ => return,
    };
    let kind = match tc.title.as_str() {
        "read_file" | "Read" => spur_acp::domain::events::FileTouchKind::Read,
        "write_file" | "Write" | "edit_file" | "Edit" => {
            spur_acp::domain::events::FileTouchKind::Write
        }
        _ => return,
    };
    // Prefer explicit raw_input path; fall back to first location entry.
    let path = tc
        .raw_input
        .as_ref()
        .and_then(|v| {
            v.get("path")
                .and_then(|p| p.as_str())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    v.get("file_path")
                        .and_then(|p| p.as_str())
                        .map(std::path::PathBuf::from)
                })
        })
        .or_else(|| tc.locations.first().map(|loc| loc.path.clone()));
    let Some(path) = path else { return };
    let key = FileTouchKey {
        executor_id: executor_id.to_string(),
        path: path.clone(),
        kind,
    };
    if dedup.should_emit(&key) {
        funnel.emit(SpurEventBody::WorkerFileTouched {
            brain_session_id: brain_session_id.clone(),
            executor_id: executor_id.to_string(),
            path,
            kind,
        });
    }
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
/// Read-only context shared across worker attempt retries.
struct WorkerAttemptCtx<'a> {
    brain_session_id: &'a spur_acp::BrainSessionId,
    agent: &'a str,
    task: &'a str,
    request_id: &'a str,
    agent_config: &'a spur_acp::config::AgentConfig,
    delegation_plan: Option<spur_acp::domain::DelegationPlan>,
    issue_id: Option<String>,
}

/// Returns `Ok(WorkerAttemptOutcome)` for any flow that produced a
/// worker candidate status — success OR worker-reported errors — both
/// of which are retry-eligible (the human reviewer decides).
///
/// Returns `Err(AttemptSetupError)` only for pre-worker setup failures
/// (worktree creation, agent initialization, session creation). The
/// caller short-circuits the delegation without retry — consistent
/// with pre-T10 behavior. Per-attempt error shape is decoupled from
/// the public `DelegationResult` type.
async fn run_one_worker_attempt(
    worker_session: SessionId,
    ctx: &WorkerAttemptCtx<'_>,
    worktrees: &mut WorktreeManager,
    funnel: &crate::event_funnel::FunnelHandle,
) -> Result<WorkerAttemptOutcome, AttemptSetupError> {
    // NOTE: DelegationRequested is emitted per-attempt here. The legacy
    // lineage adapter (lineage/adapter.rs) keys task_spec population to
    // the FIRST matching empty-task_spec executor, so on retry the
    // constraint-augmented task silently drops at the adapter boundary.
    // This is part of the broader "adapter keys off worker_session, not
    // stable executor_id" limitation documented for follow-up work.
    // The projection path (apply_inner) sees each event correctly.
    funnel.emit(SpurEventBody::DelegationRequested {
        from: ctx.brain_session_id.as_session_id().clone(),
        to_agent: ctx.agent.to_string(),
        task: ctx.task.to_string(),
        request_id: ctx.request_id.to_string(),
        delegation_plan: ctx.delegation_plan.clone(),
        issue_id: ctx.issue_id.clone(),
    });

    let start = Instant::now();

    // 1. Snapshot brain state and create worktree.
    let snapshot_branch = worktrees
        .snapshot_brain_state()
        .await
        .map_err(|e| AttemptSetupError::SnapshotFailed(e.to_string()))?;

    let worktree_info = worktrees
        .create_worktree(&worker_session, ctx.agent, &snapshot_branch)
        .await
        .map_err(|e| AttemptSetupError::WorktreeFailed(e.to_string()))?;

    // 2. Spawn worker agent in worktree via AgentConnection.
    // Workers never receive a permission_tx, so L2 auto-approve is
    // implicitly always on for them. skip_permissions still has effect
    // via L1a (spawn args).
    let spawn_args = ctx.agent_config.effective_args();
    let mut connection: Box<dyn AgentConnection> =
        build_connection_from_transport(ctx.agent_config, spawn_args, None);

    // S5 — consume `_spur/*` ExtNotifications from this worker and
    // translate them into SpurEvent variants via the funnel. Must run
    // before `connection` is moved; `take_ext_notification_rx` only
    // needs `&mut self` but can be called exactly once per connection.
    if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
        let funnel_for_ext = funnel.clone();
        let executor_id_for_ext = worker_session.0.clone();
        let brain_session_for_ext = ctx.brain_session_id.as_session_id().clone();
        tokio::spawn(async move {
            while let Some(payload) = ext_rx.recv().await {
                crate::spur_ext_interp::interpret(
                    payload,
                    brain_session_for_ext.clone(),
                    executor_id_for_ext.clone(),
                    &funnel_for_ext,
                );
            }
        });
    }

    let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
    if let Err(e) = connection.initialize(init_request).await {
        let _ = worktrees.remove_worktree(&worker_session).await;
        return Err(AttemptSetupError::InitFailed(e.to_string()));
    }

    // Emit WorkerSpawned event.
    funnel.emit(SpurEventBody::WorkerSpawned {
        agent: ctx.agent.to_string(),
        session: worker_session.clone(),
        worktree: worktree_info.path.clone(),
    });
    // Correlate this executor with the brain's delegate_to_worker call
    // so the brain-side session_detail view can render an inline card.
    funnel.emit(SpurEventBody::DelegationDispatched {
        from: ctx.brain_session_id.as_session_id().clone(),
        request_id: ctx.request_id.to_string(),
        executor_id: worker_session.0.clone(),
    });

    // Workers get no MCP servers (per spec).
    let session_response = match crate::skip_perm::new_session_with_bypass(
        &mut *connection,
        ctx.agent_config,
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
        ctx.task
    );
    let prompt_request = PromptRequest::new(
        session_response.session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(prompt_text))],
    );

    let mut output_text = String::new();
    let mut worker_success = true;

    // S5 — Per-worker-attempt file-touch dedup. Owned locally (no Arc
    // needed) because the synthesizer is called synchronously from the
    // stream loop — nothing else clones or moves the instance.
    let file_touch_dedup = FileTouchDedup::new();

    // For native (ACP-transport) workers prompt() returns an empty stream;
    // notifications arrive via the connection-scoped broadcast instead.
    // drive_prompt_notifications handles both paths transparently.
    if let Err(e) = crate::notification_drain::drive_prompt_notifications(
        &mut *connection,
        prompt_request,
        |notification| {
            // S5 — synthesize WorkerFileTouched from file-op ToolCalls
            // before any other notification handling.
            maybe_synthesize_file_touch(
                &notification,
                ctx.brain_session_id.as_session_id(),
                &worker_session.0,
                &file_touch_dedup,
                funnel,
            );
            // Phase 1 — stream worker notifications to TUI via event bus.
            funnel.emit(SpurEventBody::WorkerNotification {
                brain_session_id: ctx.brain_session_id.as_session_id().clone(),
                executor_id: worker_session.0.clone(),
                notification: Box::new(notification.clone()),
            });
            match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        output_text.push_str(&tc.text);
                    }
                }
                _ => {}
            }
        },
    )
    .await
    {
        worker_success = false;
        output_text = format!("Failed to prompt worker: {e}");
    }

    let _ = connection.shutdown().await;

    // 4. Collect diff. `basis` is either "HEAD" (uncommitted) or
    // "<base>..HEAD" (worker self-committed). We need it to compute the
    // matching diff_summary with the SAME git range — otherwise stats and
    // raw text disagree.
    let (diff, diff_basis) = worktrees
        .collect_diff(&worker_session)
        .await
        .unwrap_or((None, "HEAD"));

    // 5. Capture worktree path for execute_delegation's post-gate cleanup.
    // Commit and removal are deferred — see function doc.
    let worktree_path = worktrees
        .active
        .get(&worker_session.to_string())
        .map(|i| i.path.clone())
        .unwrap_or_default();

    // Compute structured diff stats on the SAME basis as the raw diff.
    // When collect_diff returned base..HEAD, we need to resolve the placeholder
    // to the real spec — fetch the base_commit from worktrees.
    let diff_summary = if diff.is_some() {
        let basis_spec = if diff_basis == "base_commit..HEAD" {
            // Resolve the placeholder with the actual base SHA.
            worktrees
                .active
                .get(&worker_session.to_string())
                .map(|i| format!("{}..HEAD", i.base_commit))
                .unwrap_or_else(|| "HEAD".to_string())
        } else {
            "HEAD".to_string()
        };
        build_diff_summary(&worktree_path, &basis_spec)
            .await
            .ok()
            .filter(|s| s.files_changed > 0)
    } else {
        None
    };

    let duration = start.elapsed();
    let cost = spur_cost::estimator::estimate_cost(ctx.agent_config.cost_tier, duration);

    let summary = if output_text.is_empty() {
        None
    } else {
        Some(truncate_summary_env_default(&output_text))
    };

    let candidate_status = if worker_success {
        DelegationStatus::Success
    } else {
        // Capture the last ~500 bytes of the already-truncated summary
        // as the error message (last 500 bytes of the UTF-8-safe text
        // the previous step produced, aligned to a char boundary). For
        // LLM/tool workers this is almost always the actual failure
        // (compiler error, test assertion, panic). `summary` already
        // ran through truncate_summary_env_default, so reusing its
        // tail avoids re-running the full truncation logic.
        let error = summary
            .as_deref()
            .map(|s| {
                let tail_bytes = 500usize.min(s.len());
                let start = {
                    let mut i = s.len().saturating_sub(tail_bytes);
                    while i < s.len() && !s.is_char_boundary(i) {
                        i += 1;
                    }
                    i
                };
                s[start..].to_string()
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Worker reported errors (no output captured)".into());
        DelegationStatus::Failed { error }
    };

    Ok(WorkerAttemptOutcome {
        worker_session,
        candidate_status,
        diff,
        diff_summary,
        summary,
        cost,
        worktree_path,
    })
}

/// Tail-weighted, UTF-8-safe truncation for worker summaries.
///
/// Why tail-weighted: LLM worker output opens with task restatement
/// and closes with a crisp conclusion + file list. The middle holds
/// verbose tool-call transcripts with low decision-density. Brain-
/// relevant information is concentrated at the tail.
///
/// Returns `text` unchanged if `text.len() <= cap`. Otherwise keeps
/// `cap/4` head bytes and `cap - cap/4` tail bytes (both aligned to
/// char boundaries), joined by an omission marker.
fn truncate_summary(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let head_budget = cap / 4;
    let tail_budget = cap - head_budget;

    let head_end = {
        let mut i = head_budget.min(text.len());
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let tail_start = {
        let mut i = text.len().saturating_sub(tail_budget);
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
        i
    };

    // Clamp degenerate case where head and tail would overlap.
    let tail_start = tail_start.max(head_end);

    // Use char count (not byte diff) so the marker is meaningful for
    // multi-byte input — the very case this helper is designed to handle.
    let omitted = text[head_end..tail_start].chars().count();
    format!(
        "{}\n\n[... {} chars omitted ...]\n\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

fn truncate_summary_env_default(text: &str) -> String {
    let cap: usize = std::env::var("SPUR_SUMMARY_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000);
    truncate_summary(text, cap)
}

/// Compute a `DiffSummary` for a worktree via `git diff --numstat <basis>`.
///
/// `basis` must match what `collect_diff` used for the raw diff — either
/// "HEAD" or "<base_commit>..HEAD" (rendered with the actual SHA). Otherwise
/// the raw diff text and the structured summary disagree.
///
/// Preferred over regex-parsing the unified diff text because numstat
/// emits tab-separated stats directly and handles binary files (`-\t-\tpath`),
/// renames, and mode-only changes without ambiguity.
///
/// Cost: ~10-100ms. Same budget as `collect_diff`.
async fn build_diff_summary(
    worktree_path: &std::path::Path,
    basis: &str,
) -> anyhow::Result<spur_acp::DiffSummary> {
    use tokio::process::Command;

    let output = Command::new("git")
        .arg("diff")
        .arg("--numstat")
        .arg(basis)
        .current_dir(worktree_path)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files_changed = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    let mut files = Vec::new();

    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("");
        let del = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        // Rename notation: "old => new" (top-level) or "dir/{old => new}" (nested).
        // Extract destination path so downstream consumers see a real filename.
        let path = if let Some(arrow_pos) = path.find(" => ") {
            let after_arrow = &path[arrow_pos + 4..];
            // Nested form: "dir/{old => new}/tail" — strip the trailing '}' and
            // reconstruct as "dir/" + destination + "/tail". For the simple
            // top-level form "old => new" there are no braces and this just
            // returns `new`.
            if let Some(brace_pos) = path[..arrow_pos].rfind('{') {
                let prefix = &path[..brace_pos];
                let dest = after_arrow.trim_end_matches('}');
                // Handle "dir/{old => new}/tail" — find where the '}' lived.
                let (dest_clean, tail) = match dest.find('}') {
                    Some(i) => (&dest[..i], &dest[i + 1..]),
                    None => (dest, ""),
                };
                format!("{}{}{}", prefix, dest_clean, tail)
            } else {
                after_arrow.to_string()
            }
        } else {
            path.to_string()
        };
        files_changed += 1;
        // numstat emits "-" for binary files. Non-"-" values parse as usize.
        insertions += ins.parse::<usize>().unwrap_or(0);
        deletions += del.parse::<usize>().unwrap_or(0);
        files.push(std::path::PathBuf::from(&path));
    }

    Ok(spur_acp::DiffSummary {
        files_changed,
        insertions,
        deletions,
        files,
    })
}

/// One retry attempt's surviving state, kept in memory across the
/// retry loop so later attempts can see the history. Module-local;
/// does not leak into public API.
#[derive(Debug, Clone)]
struct RetryAttempt {
    attempt_n: u32,
    summary: String,
    diff_summary: Option<spur_acp::DiffSummary>,
    /// Reviewer's `new_constraints` verbatim, the feedback that
    /// triggered this retry decision.
    feedback: String,
}

/// Render the augmented task prompt fed to the NEXT retry attempt.
///
/// Layout:
///   {original_task}
///
///   --- Previous attempts ---
///   Attempt N:
///     What was tried: {summary}
///     Files touched: {files_changed} changed, +{ins}/-{del}
///     Reviewer feedback: {feedback}
///   ...
///
///   --- Your task ---
///   Address the reviewer's most recent feedback above. Do NOT repeat
///   approaches that were rejected earlier — the reviewer sees the
///   same history and will reject a repeat.
///
///   Most recent feedback:
///   {current_feedback}
fn render_retry_context(
    history: &[RetryAttempt],
    original_task: &str,
    current_feedback: &str,
) -> String {
    let mut out = String::with_capacity(original_task.len() + current_feedback.len() + 512);
    out.push_str(original_task);

    if !history.is_empty() {
        out.push_str("\n\n--- Previous attempts ---\n");
        for a in history {
            out.push_str(&format!("\nAttempt {}:\n", a.attempt_n));
            out.push_str(&format!("  What was tried: {}\n", a.summary));
            if let Some(ds) = &a.diff_summary {
                out.push_str(&format!(
                    "  Files touched: {} changed, +{}/-{}\n",
                    ds.files_changed, ds.insertions, ds.deletions
                ));
            }
            out.push_str(&format!("  Reviewer feedback: {}\n", a.feedback));
        }
    }

    out.push_str(
        "\n--- Your task ---\n\
         Address the reviewer's most recent feedback above. Do NOT repeat \
         approaches that were rejected earlier — the reviewer sees the \
         same history and will reject a repeat.\n\n\
         Most recent feedback:\n",
    );
    out.push_str(current_feedback);
    out
}

/// Drop oldest attempts until the total in-memory summary+feedback
/// footprint fits under `max_bytes`. Preserves the most recent
/// attempts (those are most relevant to the current feedback).
fn apply_bloat_cap(history: &mut Vec<RetryAttempt>, max_bytes: usize) {
    fn size(a: &RetryAttempt) -> usize {
        a.summary.len() + a.feedback.len()
    }
    while history.iter().map(size).sum::<usize>() > max_bytes && !history.is_empty() {
        history.remove(0);
    }
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

#[doc(hidden)]
pub mod test_support {
    //! Public shims for integration tests. Not part of the stable API.
    use spur_acp::DiffSummary;

    pub struct RetryAttemptPublic {
        pub attempt_n: u32,
        pub summary: String,
        pub diff_summary: Option<DiffSummary>,
        pub feedback: String,
    }

    pub fn render_retry_context_public(
        history: &[RetryAttemptPublic],
        original_task: &str,
        current_feedback: &str,
    ) -> String {
        let internal: Vec<super::RetryAttempt> = history
            .iter()
            .map(|a| super::RetryAttempt {
                attempt_n: a.attempt_n,
                summary: a.summary.clone(),
                diff_summary: a.diff_summary.clone(),
                feedback: a.feedback.clone(),
            })
            .collect();
        super::render_retry_context(&internal, original_task, current_feedback)
    }

    // ─── Review gate helpers ──────────────────────────────────────────
    // Test-only. Production code uses ReviewSink::register_handle (INV-4).

    use super::{
        apply_decision_to_candidate, DelegationStatus, ExecutorId, ReviewSink, ReviewSinkError,
        TimeoutFallback,
    };

    /// Register a pending review on the sink. Returns the receiver the
    /// caller awaits.
    ///
    /// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
    pub async fn register_gate(
        executor_id: ExecutorId,
        attempt_n: u32,
        review_sink: &ReviewSink,
    ) -> Result<tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>, ReviewSinkError> {
        review_sink.register(executor_id, attempt_n).await
    }

    /// Wait for a review decision (or timeout) and shape the final
    /// `DelegationStatus`.
    ///
    /// **Does NOT handle `Retry`** — returns `Failed` if Retry arrives.
    ///
    /// **Test-only** — production code uses `ReviewHandle::into_rx` (INV-4).
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
                        review_sink.remove(&executor_id).await;
                        DelegationStatus::TimedOut {
                            waited_for: review_timeout,
                            fallback: timeout_fallback,
                        }
                    }
                }
            }
            _ = tokio::time::sleep(review_timeout) => {
                review_sink.remove(&executor_id).await;
                DelegationStatus::TimedOut {
                    waited_for: review_timeout,
                    fallback: timeout_fallback,
                }
            }
        }
    }

    /// Register + wait composition.
    ///
    /// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
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

    /// Wraps `register_gate` + `wait_gate` in a retry loop.
    ///
    /// On `Retry`, bumps `attempt_n` and re-enters. Bounded by
    /// `max_review_retries`.
    ///
    /// Uses `crate::retry_loop::RetryLoop` for the bound check and
    /// exhaustion status — shares invariants with the production retry
    /// gate in `execute_delegation`.
    ///
    /// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
    pub async fn run_gate_with_retries(
        executor_id: ExecutorId,
        candidate_status: DelegationStatus,
        review_timeout: std::time::Duration,
        timeout_fallback: TimeoutFallback,
        max_review_retries: u32,
        review_sink: ReviewSink,
    ) -> DelegationStatus {
        use crate::retry_loop::{RetryLoop, RetryOutcome};
        use spur_acp::ReviewDecision;

        RetryLoop::new(max_review_retries)
            .run(|attempt_n| {
                let executor_id = executor_id.clone();
                let review_sink = review_sink.clone();
                let candidate_status = candidate_status.clone();
                let timeout_fallback = timeout_fallback.clone();
                async move {
                    let rx = match register_gate(executor_id.clone(), attempt_n, &review_sink).await
                    {
                        Ok(rx) => rx,
                        Err(e) => {
                            return RetryOutcome::Terminal(DelegationStatus::Failed {
                                error: format!("review registration failed: {e}"),
                            });
                        }
                    };

                    let decision = tokio::select! {
                        r = rx => r.ok(),
                        _ = tokio::time::sleep(review_timeout) => {
                            review_sink.remove(&executor_id).await;
                            return RetryOutcome::Terminal(DelegationStatus::TimedOut {
                                waited_for: review_timeout,
                                fallback: timeout_fallback,
                            });
                        }
                    };

                    match decision {
                        Some(ReviewDecision::Approve) => RetryOutcome::Terminal(candidate_status),
                        Some(ReviewDecision::Reject { reason }) => {
                            RetryOutcome::Terminal(DelegationStatus::Rejected { reason })
                        }
                        Some(ReviewDecision::Modify { note }) => {
                            RetryOutcome::Terminal(DelegationStatus::Modified {
                                reviewer_note: note,
                            })
                        }
                        Some(ReviewDecision::Retry { .. }) => RetryOutcome::Retry,
                        None => {
                            review_sink.remove(&executor_id).await;
                            RetryOutcome::Terminal(DelegationStatus::TimedOut {
                                waited_for: review_timeout,
                                fallback: timeout_fallback,
                            })
                        }
                    }
                }
            })
            .await
    }
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
pub async fn review_dispatcher_loop(mut rx: mpsc::Receiver<InteractiveInput>, sink: ReviewSink) {
    while let Some(input) = rx.recv().await {
        if let InteractiveInput::SubmitReview {
            executor_id,
            attempt_n,
            decision,
        } = input
        {
            let _ = sink
                .submit(ExecutorId::new(executor_id), attempt_n, decision)
                .await;
        }
        // All other variants: noop in this loop.
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
        ReviewDecision::Modify { note } => DelegationStatus::Modified {
            reviewer_note: note,
        },
        ReviewDecision::Retry { .. } => DelegationStatus::Failed {
            error: "internal: Retry reached run_gate_for_candidate \
                    (caller must wrap with retry loop)"
                .into(),
        },
    }
}

#[cfg(test)]
mod cancel_mode_helper_tests {
    use super::cancel_mode_for;
    use spur_acp::{types::TransportKind, CancelMode};

    #[test]
    fn acp_transport_is_acp_soft() {
        assert_eq!(cancel_mode_for(TransportKind::Acp), CancelMode::AcpSoft);
    }

    #[test]
    fn subprocess_transports_are_process_kill() {
        assert_eq!(
            cancel_mode_for(TransportKind::Stdio),
            CancelMode::ProcessKill
        );
        assert_eq!(
            cancel_mode_for(TransportKind::CliWrap),
            CancelMode::ProcessKill
        );
        assert_eq!(
            cancel_mode_for(TransportKind::StreamJson),
            CancelMode::ProcessKill
        );
    }
}

#[cfg(test)]
mod cancel_stream_variant_tests {
    use super::InteractiveInput;
    use spur_acp::SessionId;

    #[test]
    fn cancel_stream_variant_constructs() {
        let _ = InteractiveInput::CancelStream {
            session: SessionId("s".to_string()),
        };
    }
}

#[cfg(test)]
mod cancel_deadline_arm_tests {
    use super::arm_cancel_deadline;

    #[tokio::test]
    async fn arm_cancel_deadline_sets_5s_from_now() {
        let mut deadline = None;
        let before = tokio::time::Instant::now();
        arm_cancel_deadline(&mut deadline);
        let set = deadline.expect("arm_cancel_deadline must populate Some(deadline)");
        let delta = set.saturating_duration_since(before);
        assert!(
            delta >= std::time::Duration::from_millis(4_900)
                && delta <= std::time::Duration::from_millis(5_100),
            "expected ~5s deadline, got {delta:?}"
        );
    }

    #[tokio::test]
    async fn arm_cancel_deadline_overwrites_existing() {
        let old = tokio::time::Instant::now() - std::time::Duration::from_secs(60);
        let mut deadline = Some(old);
        arm_cancel_deadline(&mut deadline);
        assert!(deadline.unwrap() > old + std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod truncate_summary_tests {
    use super::truncate_summary;

    #[test]
    fn under_cap_returns_unchanged() {
        let input = "short text";
        assert_eq!(truncate_summary(input, 4000), "short text");
    }

    #[test]
    fn exact_cap_returns_unchanged() {
        let input = "x".repeat(100);
        assert_eq!(truncate_summary(&input, 100), input);
    }

    #[test]
    fn over_cap_preserves_head_and_tail_with_marker() {
        let input: String = (0..5000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let cap = 4000;
        let out = truncate_summary(&input, cap);
        assert!(out.len() < input.len(), "output must be shorter than input");
        assert!(out.contains("chars omitted"), "omission marker must appear");
        let tail_start = input.len() - 3000;
        assert!(
            out.ends_with(&input[tail_start..]),
            "output must end with the last 3000 chars of input"
        );
        assert!(
            out.starts_with(&input[..1000]),
            "output must start with the first 1000 chars of input"
        );
    }

    #[test]
    fn utf8_boundary_does_not_panic() {
        let input = "—".repeat(20);
        let out = truncate_summary(&input, 10);
        assert!(out.chars().count() > 0);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(truncate_summary("", 4000), "");
    }

    #[test]
    fn env_var_overrides_default_cap() {
        // This test mutates process-global env state. It is safe only
        // because no other test in this binary reads SPUR_SUMMARY_MAX_BYTES
        // concurrently. If that changes (future Task 6 integration test,
        // etc.), gate with #[serial] from the serial_test crate.
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", "50") };
        let input = "x".repeat(200);
        let out = super::truncate_summary_env_default(&input);
        assert!(out.len() < input.len());
        assert!(
            out.len() <= 100,
            "output must respect env override, got {}",
            out.len()
        );
        match prev {
            Some(v) => unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) },
            None => unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") },
        }
    }
}

#[cfg(test)]
mod build_diff_summary_tests {
    use super::build_diff_summary;
    use spur_acp::DiffSummary;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_repo() -> tempfile::TempDir {
        fn git(path: &std::path::Path, args: &[&str]) {
            let out = Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let dir = tempdir().unwrap();
        let path = dir.path();
        git(path, &["init"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), "hello\nworld\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "init"]);
        dir
    }

    #[tokio::test]
    async fn clean_worktree_returns_zero_summary() {
        let dir = init_repo();
        let summary: DiffSummary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 0);
        assert_eq!(summary.insertions, 0);
        assert_eq!(summary.deletions, 0);
        assert!(summary.files.is_empty());
    }

    #[tokio::test]
    async fn modified_file_produces_expected_stats() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\nnew line\n").unwrap();
        let summary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 1);
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("a.txt")]);
    }

    #[tokio::test]
    async fn binary_file_is_counted_but_numbers_stay_zero() {
        let dir = init_repo();
        // numstat emits "-\t-\tpath" for binary files.
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 3, 0xFF]).unwrap();
        Command::new("git")
            .args(["add", "b.bin"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "bin"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("b.bin"), [9u8, 8, 7]).unwrap();
        let summary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(
            summary.insertions, 0,
            "binary diff reports '-' for line counts"
        );
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("b.bin")]);
    }

    #[tokio::test]
    async fn renamed_file_reports_destination_path() {
        let dir = init_repo();
        let path = dir.path();
        // Create a second file to make git rename-detection engage reliably.
        std::fs::write(path.join("a.txt"), "hello\nworld\nextra\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "grow"])
            .current_dir(path)
            .output()
            .unwrap();
        // Rename a.txt -> b.txt with a small tweak so line counts are non-zero.
        std::fs::rename(path.join("a.txt"), path.join("b.txt")).unwrap();
        std::fs::write(path.join("b.txt"), "hello\nworld\nextra\nrenamed\n").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();

        let summary = build_diff_summary(path, "HEAD").await.unwrap();
        // Either git reports a rename (1 entry, path=b.txt) OR a delete+add pair
        // (2 entries, both a.txt and b.txt). Both are acceptable — the key
        // invariant is: no path contains " => " after our rename-stripping.
        assert!(
            summary
                .files
                .iter()
                .all(|p| !p.to_string_lossy().contains(" => ")),
            "rename notation leaked into path: {:?}",
            summary.files
        );
        // b.txt must appear in the file list under either shape.
        assert!(
            summary
                .files
                .iter()
                .any(|p| p.file_name().and_then(|s| s.to_str()) == Some("b.txt")),
            "b.txt not in file list: {:?}",
            summary.files
        );
    }
}

#[cfg(test)]
mod retry_context_tests {
    use super::{apply_bloat_cap, render_retry_context, RetryAttempt};
    use spur_acp::DiffSummary;
    use std::path::PathBuf;

    fn att(n: u32, summary: &str, feedback: &str) -> RetryAttempt {
        RetryAttempt {
            attempt_n: n,
            summary: summary.into(),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 10,
                deletions: 2,
                files: vec![PathBuf::from("f.rs")],
            }),
            feedback: feedback.into(),
        }
    }

    #[test]
    fn render_includes_original_task_and_all_attempts_and_current_feedback() {
        let history = vec![
            att(1, "tried approach A", "needs tests"),
            att(2, "tried approach B", "still too slow"),
        ];
        let out = render_retry_context(&history, "make foo fast", "use async");
        assert!(out.contains("make foo fast"));
        assert!(out.contains("Attempt 1"));
        assert!(out.contains("tried approach A"));
        assert!(out.contains("needs tests"));
        assert!(out.contains("Attempt 2"));
        assert!(out.contains("tried approach B"));
        assert!(out.contains("still too slow"));
        assert!(out.contains("use async"));
        assert!(out.contains("1 changed"));
        assert!(out.contains("+10"));
        assert!(out.contains("-2"));
    }

    #[test]
    fn render_handles_empty_history() {
        let out = render_retry_context(&[], "task", "feedback");
        assert!(out.contains("task"));
        assert!(out.contains("feedback"));
        assert!(!out.contains("Attempt 1"));
    }

    #[test]
    fn apply_bloat_cap_drops_oldest_first() {
        let big = "x".repeat(1000);
        let mut history = vec![
            att(1, &big, "fb1"),
            att(2, &big, "fb2"),
            att(3, &big, "fb3"),
        ];
        apply_bloat_cap(&mut history, 2000);
        assert!(history.iter().all(|a| a.attempt_n != 1));
        assert!(history.iter().any(|a| a.attempt_n == 3));
    }

    #[test]
    fn apply_bloat_cap_is_noop_when_under_cap() {
        let mut history = vec![att(1, "s", "f")];
        apply_bloat_cap(&mut history, 10_000);
        assert_eq!(history.len(), 1);
    }
}

#[cfg(test)]
mod is_connection_death_tests {
    use super::*;

    #[test]
    fn is_connection_death_detects_known_patterns() {
        let e1 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died during ext_method");
        assert!(is_connection_death(&e1));

        let e2 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died");
        assert!(is_connection_death(&e2));

        let e3 = anyhow::anyhow!("Internal error: \"server shut down unexpectedly\"");
        assert!(is_connection_death(&e3));

        let e4 = anyhow::anyhow!("prompt rejected: invalid session id");
        assert!(!is_connection_death(&e4));
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_agent_name;

    #[test]
    fn strips_acp_suffix() {
        assert_eq!(normalize_agent_name("claude-code-acp"), "claude-code");
        assert_eq!(normalize_agent_name("kiro-acp"), "kiro");
    }

    #[test]
    fn strips_cli_suffix() {
        assert_eq!(normalize_agent_name("gemini-cli"), "gemini");
    }

    #[test]
    fn lowercases() {
        assert_eq!(normalize_agent_name("CLAUDE"), "claude");
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(normalize_agent_name("  kiro  "), "kiro");
    }

    #[test]
    fn same_agent_matches_across_variants() {
        assert_eq!(
            normalize_agent_name("Claude-Code-ACP"),
            normalize_agent_name("claude-code")
        );
    }

    #[test]
    fn distinct_agents_do_not_collide() {
        assert_ne!(
            normalize_agent_name("our-claude"),
            normalize_agent_name("claude"),
        );
    }

    #[test]
    fn mismatch_detection_chosen_vs_dispatched_strings() {
        let dispatched = "kiro";
        let chosen = "claude";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        assert!(!matched);

        let dispatched = "claude-code-acp";
        let chosen = "claude";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        // claude-code-acp normalizes to "claude-code", so "claude" != "claude-code"
        assert!(!matched);

        let dispatched = "claude-code-acp";
        let chosen = "claude-code-acp";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        assert!(matched);
    }
}

#[cfg(test)]
mod prompt_v1_tests {
    // --- Bundled skill content tests: verify SKILL.md bodies contain required keywords ---

    #[test]
    fn brain_delegation_skill_contains_dispatch_procedure() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .expect("bundled brain-delegation skill must exist");
        assert!(body.contains("When to delegate vs. do it yourself"));
        assert!(body.contains("Do it yourself when:"));
        assert!(body.contains("Delegate when:"));
        assert!(body.contains("specialist"));
        assert!(body.contains("avoid_for is a SOFT signal"));
    }

    #[test]
    fn brain_delegation_skill_contains_plan_requirement() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .unwrap();
        assert!(body.contains("delegation_plan"));
        assert!(body.contains("candidates"));
        assert!(body.contains("decomposition"));
        assert!(body.contains("minimum shape"));
        assert!(body.contains(">=2 subtasks OR >3 files"));
    }

    #[test]
    fn brain_delegation_skill_contains_task_structure() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .unwrap();
        assert!(body.contains("CONTEXT:"));
        assert!(body.contains("GOAL:"));
        assert!(body.contains("CONSTRAINTS:"));
        assert!(body.contains("EXPECTED OUTPUT"));
    }

    #[test]
    fn brain_delegation_skill_contains_canonical_example() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .unwrap();
        assert!(body.contains("Canonical example"));
        assert!(body.contains("delegate_to_worker"));
        assert!(body.contains("delegation_plan"));
    }

    #[test]
    fn per_agent_skill_exists_for_known_brains() {
        let fake = std::path::Path::new("/nonexistent");
        for agent in ["claude-code-acp", "kiro", "codex", "gemini"] {
            let name = format!("brain-delegation-{}", agent);
            assert!(
                crate::skills::load_skill(&name, fake).is_some(),
                "missing bundled skill for {agent}"
            );
        }
    }

    #[test]
    fn unknown_agent_skill_returns_none() {
        let fake = std::path::Path::new("/nonexistent");
        assert!(crate::skills::load_skill("brain-delegation-unknown-agent-xyz", fake).is_none());
    }

    // --- Workers-block rendering: build minimal fixtures from AgentConfig ---

    use spur_acp::config::{AgentConfig, Tier};

    fn cfg_with_good_for(name: &str, good_for: Vec<String>) -> AgentConfig {
        let mut cfg = AgentConfig::with_defaults(name);
        cfg.delegation.good_for = good_for;
        cfg.delegation.description = Some(format!("{} test descriptor", name));
        cfg.delegation.tier = Some(Tier::Generalist);
        cfg
    }

    /// Render the workers block over an explicit agent slice, bypassing
    /// orchestrator self. Mirrors the logic of `render_workers_block`.
    fn render_workers_block_over(agents: &[AgentConfig]) -> String {
        let mut out = String::from("## Available worker agents\n\n");
        let mut any = false;
        for agent in agents {
            if agent.delegation.good_for.is_empty() {
                continue;
            }
            any = true;
            let tier = agent
                .delegation
                .tier
                .map(|t| match t {
                    Tier::Specialist => "specialist",
                    Tier::Generalist => "generalist",
                })
                .unwrap_or("generalist");
            let desc = agent
                .delegation
                .description
                .as_deref()
                .unwrap_or("(no description)");
            out.push_str(&format!(
                "### {}  ({}, cost: medium)\n{}\n\n",
                agent.name, tier, desc,
            ));
        }
        if !any {
            out.push_str("(no worker-capable agents with descriptors configured)\n\n");
        }
        out
    }

    #[test]
    fn workers_block_lists_agents_with_non_empty_good_for() {
        let agents = vec![
            cfg_with_good_for("claude-x", vec!["refactors".into()]),
            cfg_with_good_for("kiro-x", vec!["specs".into()]),
        ];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("claude-x"));
        assert!(block.contains("kiro-x"));
    }

    #[test]
    fn workers_block_excludes_empty_good_for_agents() {
        let agents = vec![
            cfg_with_good_for("has-good-for", vec!["real".into()]),
            cfg_with_good_for("bare", vec![]), // will be excluded
        ];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("has-good-for"));
        assert!(!block.contains("bare"));
    }

    #[test]
    fn workers_block_says_none_when_all_excluded() {
        let agents = vec![cfg_with_good_for("bare", vec![])];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("(no worker-capable agents with descriptors configured)"));
    }

    #[test]
    fn workers_block_is_deterministic_for_same_input() {
        let agents = vec![cfg_with_good_for("a", vec!["x".into()])];
        let a = render_workers_block_over(&agents);
        let b = render_workers_block_over(&agents);
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod format_worker_task_tests {
    use super::format_worker_task;

    #[test]
    fn empty_list_passes_task_through_unchanged() {
        let task = "Do the thing.";
        let out = format_worker_task(task, &[]);
        assert_eq!(out, task);
    }

    #[test]
    fn single_path_prepends_relevant_files_section() {
        let task = "Do the thing.";
        let files = vec!["src/a.rs".to_string()];
        let out = format_worker_task(task, &files);
        assert!(
            out.starts_with("## Relevant Files\n\n"),
            "expected Relevant Files header first, got: {out}",
        );
        assert!(out.contains("- src/a.rs"));
        assert!(out.contains("## Task\n\nDo the thing."));
    }

    #[test]
    fn multiple_paths_produce_ordered_bullets() {
        let files = vec![
            "crates/spur-mcp/src/server.rs".to_string(),
            "crates/spur-acp/src/adapter/claude.rs".to_string(),
        ];
        let out = format_worker_task("Go.", &files);
        let idx_first = out
            .find("- crates/spur-mcp/src/server.rs")
            .expect("first bullet");
        let idx_second = out
            .find("- crates/spur-acp/src/adapter/claude.rs")
            .expect("second bullet");
        assert!(idx_first < idx_second, "order must be preserved");
    }

    #[test]
    fn whitespace_task_body_still_gets_section_when_files_nonempty() {
        let out = format_worker_task("   ", &["x.rs".into()]);
        assert!(out.starts_with("## Relevant Files\n\n"));
        assert!(out.ends_with("   "));
    }
}

#[cfg(test)]
mod context_files_wiring_tests {
    use super::format_worker_task;

    /// Regression guard: the helper is imported where execute_delegation
    /// lives. If a refactor moves or renames it, the import here breaks
    /// before the wiring silently regresses.
    #[test]
    fn format_worker_task_is_available_in_orchestrator_module() {
        let out = format_worker_task("t", &["x".into()]);
        assert!(out.contains("## Relevant Files"));
    }
}

#[cfg(test)]
mod interactive_input_tests {
    use super::InteractiveInput;
    use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::types::SessionId;
    use std::time::Instant;

    #[test]
    fn system_continuation_variant_constructs() {
        let c = BrainContinuation {
            delegation_id: "abc".into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None, diff_summary: None, worker_branch: None,
            },
            created_at: Instant::now(),
        };
        let input = InteractiveInput::SystemContinuation {
            session: SessionId::new(),
            continuation: c,
        };
        match input {
            InteractiveInput::SystemContinuation { .. } => (),
            _ => panic!("expected SystemContinuation variant"),
        }
    }
}
