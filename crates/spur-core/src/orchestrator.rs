use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{broadcast, Semaphore};
use tracing::{debug, error, info, warn};

use spur_acp::config::SpurConfig;
use spur_acp::registry::AgentRegistry;
use spur_acp::transport::{AcpTransport, AgentTransport, CliWrapTransport};
use spur_acp::types::*;
use spur_pm::Issue;

use spur_cost::CostTracker;
use spur_mcp::{DelegationChannel, DelegationRequest, McpCallbackServer, WorkerInfo};
use spur_pm::adapter::PmAdapter;
use spur_pm::GitHubAdapter;
use spur_worktree::WorktreeManager;

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

// ─── Orchestrator ────────────────────────────────────────────────────

/// The central orchestrator that drives the brain-worker pipeline.
pub struct Orchestrator {
    pub registry: AgentRegistry,
    pub config: SpurConfig,
    pub worktrees: WorktreeManager,
    pub cost_tracker: Option<CostTracker>,
    pub event_tx: broadcast::Sender<SpurEvent>,
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

        Ok(Self {
            registry,
            config,
            worktrees,
            cost_tracker,
            event_tx,
            repo_root,
        })
    }

    /// Subscribe to orchestrator events (for TUI, logging, etc.).
    pub fn subscribe(&self) -> broadcast::Receiver<SpurEvent> {
        self.event_tx.subscribe()
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
        self.emit(SpurEvent::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        });

        // 2. Optionally fetch issue context.
        let issue_context = if let Some(ref issue_ref) = opts.issue {
            match self.fetch_issue_context(issue_ref).await {
                Ok(issue) => {
                    self.emit(SpurEvent::IssueReceived {
                        source: format!("{:?}", issue.source),
                        id: issue.id.clone(),
                    });
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

        // 6. Spawn brain agent.
        let mut transport = self.create_transport(&brain_config);
        let capabilities = transport
            .initialize(Some(mcp_endpoint))
            .await
            .context("Failed to initialize brain agent")?;

        debug!(
            brain = %brain_name,
            supports_mcp = capabilities.supports_mcp,
            "Brain agent initialized"
        );

        let brain_session = transport
            .create_session()
            .await
            .context("Failed to create brain session")?;

        // 7. Send prompt and stream events.
        let prompt = vec![PromptBlock::Text {
            text: prompt_text.clone(),
        }];

        let mut stream = transport
            .prompt(brain_session.clone(), prompt)
            .await
            .context("Failed to send prompt to brain")?;

        // 8. Process brain output + delegation callbacks concurrently.
        let pr_url: Option<String> = None;
        let mut success = true;

        // Spawn delegation handler.
        let max_concurrent = self.config.worktree.max_concurrent;
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.event_tx.clone(),
        ));

        // Stream brain output.
        use futures::StreamExt;
        while let Some(event) = stream.next().await {
            match &event {
                SessionEvent::TextDelta(text) => {
                    print!("{text}");
                }
                SessionEvent::ToolCallStart { name, .. } => {
                    debug!(tool = %name, "Brain calling tool");
                }
                SessionEvent::Error { code, message } => {
                    error!(code, message = %message, "Brain agent error");
                    success = false;
                }
                SessionEvent::RateLimitHit { retry_after } => {
                    warn!(retry_after = ?retry_after, "Brain hit rate limit");
                    self.emit(SpurEvent::RateLimitDetected {
                        agent: brain_name.clone(),
                        retry_after: *retry_after,
                    });
                }
                SessionEvent::Complete { .. } => {
                    info!(brain = %brain_name, "Brain session completed");
                }
                _ => {}
            }

            self.emit(SpurEvent::AgentOutput {
                session: session_id.clone(),
                event,
            });
        }

        // 9. Clean up.
        let _ = transport.shutdown().await;
        let _ = mcp_server.shutdown();
        delegation_handle.abort();

        let duration = start.elapsed();

        // 10. Log session end.
        if let Some(ref ct) = self.cost_tracker {
            let status = if success { "completed" } else { "failed" };
            let _ = ct.end_session(&session_id, status, duration, brain_config.cost_tier);
        }

        let total_cost = spur_cost::estimator::estimate_cost(brain_config.cost_tier, duration);

        self.emit(SpurEvent::SessionCompleted {
            session: session_id.clone(),
            success,
        });

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

        let mut transport = self.create_transport(&agent_config);
        transport
            .initialize(None)
            .await
            .context("Failed to initialize agent")?;

        let agent_session = transport.create_session().await?;

        let prompt = vec![PromptBlock::Text {
            text: task.to_string(),
        }];

        let mut stream = transport.prompt(agent_session, prompt).await?;

        let mut success = true;
        use futures::StreamExt;
        while let Some(event) = stream.next().await {
            match &event {
                SessionEvent::TextDelta(text) => print!("{text}"),
                SessionEvent::Error { message, .. } => {
                    error!(message = %message, "Agent error");
                    success = false;
                }
                _ => {}
            }
        }

        let _ = transport.shutdown().await;
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
        let known_agents = [
            ("kiro", "kiro-cli", vec!["acp"], TransportKind::Acp),
            (
                "claude-code",
                "claude",
                vec!["--experimental-acp"],
                TransportKind::Acp,
            ),
            ("codex", "codex", vec!["--acp"], TransportKind::Acp),
            ("gemini", "gemini", vec![], TransportKind::CliWrap),
        ];

        let mut found = Vec::new();

        for (name, command, args, transport) in &known_agents {
            let which = tokio::process::Command::new("which")
                .arg(command)
                .output()
                .await;

            if let Ok(output) = which {
                if output.status.success() {
                    let config = spur_acp::config::AgentConfig {
                        name: name.to_string(),
                        command: command.to_string(),
                        args: args.iter().map(|s| s.to_string()).collect(),
                        transport: *transport,
                        role: AgentRole::Both,
                        capabilities: vec![],
                        cost_tier: CostTier::Medium,
                        rate_limit_window: None,
                    };
                    self.registry.register(config);
                    found.push(name.to_string());
                    info!(agent = %name, command = %command, "Found agent");
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
            let mut transport = self.create_transport(config);
            let health = match transport.initialize(None).await {
                Ok(_) => {
                    let _ = transport.shutdown().await;
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

    fn create_transport(
        &self,
        config: &spur_acp::config::AgentConfig,
    ) -> Box<dyn AgentTransport> {
        match config.transport {
            TransportKind::Acp => Box::new(AcpTransport::new(
                config.name.clone(),
                config.command.clone(),
                config.args.clone(),
            )),
            TransportKind::CliWrap => Box::new(CliWrapTransport::new(
                config.name.clone(),
                config.command.clone(),
                config.args.clone(),
            )),
            TransportKind::Stdio => {
                // Stdio transport is Phase 2; fall back to CliWrap.
                Box::new(CliWrapTransport::new(
                    config.name.clone(),
                    config.command.clone(),
                    config.args.clone(),
                ))
            }
        }
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
    ) {
        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        while let Some(request) = channel.request_rx.recv().await {
            debug!(
                agent = %request.agent,
                task = %request.task,
                "Received delegation request"
            );

            let response_tx = channel.response_tx.clone();
            let repo_root = repo_root.clone();
            let agent_configs = agent_configs.clone();
            let semaphore = Arc::clone(&semaphore);
            let event_tx = event_tx.clone();

            tokio::spawn(async move {
                // Acquire a permit before starting the delegation.
                let _permit = match semaphore.acquire().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        error!("Semaphore closed — aborting delegation");
                        return;
                    }
                };

                let result = Self::execute_delegation(
                    request.clone(),
                    repo_root,
                    agent_configs,
                    event_tx,
                )
                .await;

                let response = spur_mcp::DelegationResponse {
                    id: request.id,
                    result,
                };

                if response_tx.send(response).await.is_err() {
                    error!("Failed to send delegation response — brain disconnected");
                }
            });
        }
    }

    /// Execute a single delegation request.
    ///
    /// This method is fully self-contained: it creates its own
    /// `WorktreeManager` and `AgentRegistry` so it can run in an
    /// independent tokio task without shared mutable state.
    async fn execute_delegation(
        request: DelegationRequest,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        event_tx: broadcast::Sender<SpurEvent>,
    ) -> DelegationResult {
        // Special agent names for PM operations (from MCP server).
        if request.agent.starts_with("__") {
            return DelegationResult {
                status: DelegationStatus::Failed {
                    error: format!("PM operations not yet wired: {}", request.agent),
                },
                diff: None,
                summary: None,
                estimated_cost_usd: 0.0,
            };
        }

        let registry = AgentRegistry::load(agent_configs);

        let agent_config = match registry.get(&request.agent) {
            Some(c) => c.clone(),
            None => {
                return DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("Worker agent '{}' not found", request.agent),
                    },
                    diff: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                };
            }
        };

        let worker_session = SessionId::new();

        // Emit DelegationRequested event.
        let _ = event_tx.send(SpurEvent::DelegationRequested {
            from: worker_session.clone(),
            to_agent: request.agent.clone(),
            task: request.task.clone(),
        });

        let start = Instant::now();

        // Each delegation gets its own WorktreeManager so concurrent
        // delegations do not share mutable state.
        let mut worktrees = WorktreeManager::new(repo_root);

        // 1. Snapshot brain state and create worktree.
        let snapshot_branch = match worktrees.snapshot_brain_state().await {
            Ok(b) => b,
            Err(e) => {
                return DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("Failed to snapshot brain state: {e}"),
                    },
                    diff: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                };
            }
        };

        let worktree_info = match worktrees
            .create_worktree(&worker_session, &request.agent, &snapshot_branch)
            .await
        {
            Ok(info) => info,
            Err(e) => {
                return DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("Failed to create worktree: {e}"),
                    },
                    diff: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                };
            }
        };

        // 2. Spawn worker agent in worktree.
        let mut transport: Box<dyn AgentTransport> = match agent_config.transport {
            TransportKind::Acp => Box::new(AcpTransport::new(
                agent_config.name.clone(),
                agent_config.command.clone(),
                agent_config.args.clone(),
            )),
            _ => Box::new(CliWrapTransport::new(
                agent_config.name.clone(),
                agent_config.command.clone(),
                agent_config.args.clone(),
            )),
        };

        if let Err(e) = transport.initialize(None).await {
            let _ = worktrees.remove_worktree(&worker_session).await;
            return DelegationResult {
                status: DelegationStatus::Failed {
                    error: format!("Failed to initialize worker: {e}"),
                },
                diff: None,
                summary: None,
                estimated_cost_usd: 0.0,
            };
        }

        // Emit WorkerSpawned event.
        let _ = event_tx.send(SpurEvent::WorkerSpawned {
            agent: request.agent.clone(),
            session: worker_session.clone(),
            worktree: worktree_info.path.clone(),
        });

        let session = match transport.create_session().await {
            Ok(s) => s,
            Err(e) => {
                let _ = transport.shutdown().await;
                let _ = worktrees.remove_worktree(&worker_session).await;
                return DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("Failed to create worker session: {e}"),
                    },
                    diff: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                };
            }
        };

        // 3. Send task to worker.
        let prompt = vec![PromptBlock::Text {
            text: format!(
                "Working directory: {}\n\nTask: {}",
                worktree_info.path.display(),
                request.task
            ),
        }];

        let mut output_text = String::new();
        let mut worker_success = true;

        match transport.prompt(session, prompt).await {
            Ok(mut stream) => {
                use futures::StreamExt;
                while let Some(event) = stream.next().await {
                    match event {
                        SessionEvent::TextDelta(text) => {
                            output_text.push_str(&text);
                        }
                        SessionEvent::Error { message, .. } => {
                            worker_success = false;
                            output_text.push_str(&format!("\nError: {message}"));
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

        let _ = transport.shutdown().await;

        // 4. Collect diff.
        let diff = worktrees
            .collect_diff(&worker_session)
            .await
            .unwrap_or(None);

        // 5. Commit and clean up worktree.
        if diff.is_some() {
            let _ = worktrees
                .commit_worker_changes(&worker_session, &format!("spur: worker {} output", request.agent))
                .await;
        }
        let _ = worktrees.remove_worktree(&worker_session).await;

        let duration = start.elapsed();
        let cost = spur_cost::estimator::estimate_cost(agent_config.cost_tier, duration);

        let summary = if output_text.len() > 500 {
            Some(format!("{}...", &output_text[..500]))
        } else if output_text.is_empty() {
            None
        } else {
            Some(output_text)
        };

        let status = if worker_success {
            DelegationStatus::Success
        } else {
            DelegationStatus::Failed {
                error: "Worker reported errors".into(),
            }
        };

        // Emit DelegationCompleted event.
        let _ = event_tx.send(SpurEvent::DelegationCompleted {
            worker_session,
            status: status.clone(),
        });

        DelegationResult {
            status,
            diff,
            summary,
            estimated_cost_usd: cost,
        }
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
