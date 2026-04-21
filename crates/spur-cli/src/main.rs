mod commands;
mod onboarding;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use tracing_subscriber::prelude::*;

use commands::auth::AuthCommands;
use spur_acp::config::SpurConfig;
use spur_acp::SessionId;
use spur_core::{Orchestrator, RunOpts};
use spur_license::SpurLicense;

/// Returns an optional guard that must be held until process exit to flush buffered logs.
fn init_tracing(
    tui_mode: bool,
    repo_root: &Path,
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    if tui_mode {
        let log_dir = repo_root.join(".spur").join("logs");
        std::fs::create_dir_all(&log_dir)?;
        let file_appender = tracing_appender::rolling::daily(log_dir, "spur.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
            .init();
        Ok(Some(guard))
    } else {
        tracing_subscriber::fmt::init();
        Ok(None)
    }
}

/// User-facing install commands per seed agent. Surfaced by `spur init`
/// when a seed agent's binary is not on $PATH. Kept here (not in the
/// schema) because hints are onboarding copy — they don't belong in
/// every user's round-tripped `.spur/config.toml`.
///
/// Contract: every agent in `spur_acp::config::load_seed_template()`
/// must have an entry here. Enforced by `tests/init_ux.rs`.

#[derive(Parser)]
#[command(name = "spur", about = "Multi-agent orchestrator — issue in, PR out")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize SPUR: detect agents, create config
    Init {
        /// Overwrite existing .spur/config.toml.
        #[arg(long)]
        force: bool,
    },
    /// List and manage registered agents
    Agents {
        #[command(subcommand)]
        command: Option<AgentsCommands>,
    },
    /// Run an ad-hoc task through the brain agent
    Run {
        /// The task description
        task: String,
        /// Override the brain agent
        #[arg(long)]
        brain: Option<String>,
        /// Pull issue context (e.g., "github:owner/repo#42")
        #[arg(long)]
        issue: Option<String>,
        /// Run in background
        #[arg(long)]
        background: bool,
    },
    /// Execute a task directly on a specific agent (no brain, no delegation)
    Exec {
        /// Which agent to use
        #[arg(long)]
        agent: String,
        /// The task description
        task: String,
    },
    /// List and manage active sessions
    Sessions {
        #[command(subcommand)]
        command: Option<SessionsCommands>,
    },
    /// Show cost summary
    Cost {
        /// Show weekly breakdown
        #[arg(long)]
        week: bool,
        /// Group by dimension
        #[arg(long, value_name = "DIMENSION")]
        by: Option<String>,
        /// Export format
        #[arg(long, value_name = "FORMAT")]
        export: Option<String>,
    },
    /// Authenticate with a PM tool
    Connect {
        /// Service name (e.g., "github")
        service: String,
    },
    /// Manage commercial licensing
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    /// Manage workflows
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Validate .spur/config.toml shape
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Launch interactive TUI dashboard
    Watch {
        /// Override the brain agent (default from config)
        #[arg(long)]
        brain: Option<String>,
        /// Show session picker on launch
        #[arg(long)]
        sessions: bool,
        /// Land on Dashboard instead of auto-resuming last session.
        #[arg(long)]
        dashboard: bool,
    },
}

#[derive(Subcommand)]
enum AgentsCommands {
    /// Register a custom agent
    Add { path: String },
    /// Remove an agent
    Remove { name: String },
    /// Health-check all agents
    Check,
}

#[derive(Subcommand)]
enum SessionsCommands {
    /// Show session detail
    Show { id: String },
    /// Terminate a session
    Kill { id: String },
}

#[derive(Subcommand)]
enum WorkflowCommands {
    /// Validate a TOML workflow definition
    Validate { file: String },
    /// Execute a workflow
    Run {
        file: String,
        #[arg(long)]
        issue: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate that every [agents.entries] block has a coherent configuration.
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir()?;

    let tui_mode = matches!(cli.command, Commands::Watch { .. });
    let _tracing_guard = init_tracing(tui_mode, &repo_root)?;

    match cli.command {
        Commands::Init { force } => commands::init::run(repo_root, force).await,
        Commands::Agents { command } => cmd_agents(repo_root, command).await,
        Commands::Run {
            task,
            brain,
            issue,
            background,
        } => {
            let mut orch = load_orchestrator(repo_root)?;
            let result = orch
                .run_adhoc(
                    &task,
                    RunOpts {
                        brain,
                        issue,
                        background,
                    },
                )
                .await?;
            println!(
                "[spur] Session {} {} (${:.2})",
                result.session_id,
                if result.success {
                    "completed"
                } else {
                    "failed"
                },
                result.total_cost_usd,
            );
            if let Some(url) = result.pr_url {
                println!("[spur] PR: {url}");
            }
            Ok(())
        }
        Commands::Exec { agent, task } => {
            let mut orch = load_orchestrator(repo_root)?;
            let result = orch.exec_direct(&agent, &task).await?;
            println!(
                "[spur] Session {} {} (${:.2})",
                result.session_id,
                if result.success {
                    "completed"
                } else {
                    "failed"
                },
                result.total_cost_usd,
            );
            Ok(())
        }
        Commands::Sessions { command } => {
            let orch = load_orchestrator(repo_root)?;
            match command {
                None => {
                    if let Some(ref ct) = orch.cost_tracker {
                        let sessions = ct.recent_sessions(20)?;
                        if sessions.is_empty() {
                            println!("No sessions recorded yet.");
                        } else {
                            println!(
                                "{:<14} {:<14} {:<9} {:<12} {:<12} {:>8}",
                                "ID (short)", "Agent", "Role", "Status", "Duration", "Cost"
                            );
                            println!("{}", "\u{2500}".repeat(73));
                            for s in &sessions {
                                let short_id = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                                let duration = s
                                    .duration_seconds
                                    .map(format_duration)
                                    .unwrap_or_else(|| "-".into());
                                let cost = s
                                    .estimated_cost_usd
                                    .map(|c| format!("${:.2}", c))
                                    .unwrap_or_else(|| "-".into());
                                println!(
                                    "{:<14} {:<14} {:<9} {:<12} {:<12} {:>8}",
                                    short_id, s.agent, s.role, s.status, duration, cost,
                                );
                            }
                        }
                    } else {
                        println!("Cost tracking not available.");
                    }
                }
                Some(SessionsCommands::Show { id }) => {
                    if let Some(ref ct) = orch.cost_tracker {
                        let sid = SessionId(id.clone());
                        match ct.session_detail(&sid)? {
                            Some(s) => {
                                println!("Session:  {}", s.id);
                                println!("Agent:    {}", s.agent);
                                println!("Role:     {}", s.role);
                                println!("Status:   {}", s.status);
                                println!("Started:  {}", s.started_at);
                                println!("Ended:    {}", s.ended_at.as_deref().unwrap_or("-"));
                                println!(
                                    "Duration: {}",
                                    s.duration_seconds
                                        .map(format_duration)
                                        .unwrap_or_else(|| "-".into())
                                );
                                println!(
                                    "Cost:     {}",
                                    s.estimated_cost_usd
                                        .map(|c| format!("${:.2}", c))
                                        .unwrap_or_else(|| "-".into())
                                );
                                println!("Task:     {}", s.task_summary.as_deref().unwrap_or("-"));

                                let delegations = ct.session_delegations(&sid)?;
                                if !delegations.is_empty() {
                                    println!("\nDelegations:");
                                    for d in &delegations {
                                        let del_duration = match (&d.requested_at, &d.completed_at)
                                        {
                                            (req, Some(comp)) => {
                                                if let (Ok(start), Ok(end)) = (
                                                    chrono::DateTime::parse_from_rfc3339(req),
                                                    chrono::DateTime::parse_from_rfc3339(comp),
                                                ) {
                                                    let secs = (end - start).num_seconds();
                                                    format_duration(secs)
                                                } else {
                                                    "-".to_string()
                                                }
                                            }
                                            _ => "-".to_string(),
                                        };
                                        println!(
                                            "  \u{2192} {}: \"{}\" [{}, {}]",
                                            d.agent, d.task, d.status, del_duration,
                                        );
                                    }
                                }
                            }
                            None => println!("Session not found: {id}"),
                        }
                    } else {
                        println!("Cost tracking not available.");
                    }
                }
                Some(SessionsCommands::Kill { id }) => {
                    println!(
                        "[spur] Would kill session {}, but active session tracking is not yet implemented.",
                        id
                    );
                }
            }
            Ok(())
        }
        Commands::Cost { week, by, export } => {
            let orch = load_orchestrator(repo_root)?;
            if let Some(ref ct) = orch.cost_tracker {
                let summaries = if week {
                    ct.week_summary()?
                } else {
                    ct.today_summary()?
                };
                if summaries.is_empty() {
                    println!("No cost data recorded yet.");
                } else if export.as_deref() == Some("csv") {
                    println!("agent,cost_usd,sessions,duration_seconds");
                    for s in &summaries {
                        println!(
                            "{},{:.2},{},{}",
                            s.agent, s.total_cost_usd, s.session_count, s.total_duration_seconds,
                        );
                    }
                } else {
                    let total: f64 = summaries.iter().map(|s| s.total_cost_usd).sum();
                    println!(
                        "{:<15} {:>10} {:>8} {:>10}",
                        "Agent", "Cost", "Sessions", "Duration"
                    );
                    println!("{}", "-".repeat(47));
                    for s in &summaries {
                        println!(
                            "{:<15} ${:>9.2} {:>8} {:>8}m",
                            s.agent,
                            s.total_cost_usd,
                            s.session_count,
                            s.total_duration_seconds / 60,
                        );
                    }
                    println!("{}", "-".repeat(47));
                    println!("{:<15} ${:>9.2}", "Total", total);

                    if let Some(ref dim) = by {
                        if dim == "project" {
                            println!("\nBy project:");
                            for p in ct.by_project()? {
                                println!(
                                    "  {}: ${:.2} ({} sessions)",
                                    p.project, p.total_cost_usd, p.session_count
                                );
                            }
                        }
                    }
                }
            } else {
                println!("Cost tracking not available.");
            }
            Ok(())
        }
        Commands::Connect { service } => {
            match service.as_str() {
                "github" => {
                    let cwd = std::env::current_dir()?;
                    let adapter = spur_pm::GitHubAdapter::connect(None, &cwd).await?;
                    println!(
                        "[spur] Connected to GitHub: {}",
                        adapter.repo.unwrap_or_default()
                    );
                }
                _ => println!("[spur] Unknown service: {service}. Supported: github"),
            }
            Ok(())
        }
        Commands::Auth { command } => commands::auth::run(command).await,
        Commands::Workflow { command } => {
            match command {
                WorkflowCommands::Validate { file } => {
                    println!("[spur] Validating: {file} (Phase 3)");
                }
                WorkflowCommands::Run { file, issue: _ } => {
                    println!("[spur] Running workflow: {file} (Phase 3)");
                }
            }
            Ok(())
        }
        Commands::Config { command } => match command {
            ConfigCommands::Check => {
                let exit = commands::config_check::run(&repo_root)?;
                std::process::exit(exit);
            }
        },
        Commands::Watch {
            brain,
            sessions,
            dashboard,
        } => {
            let config = match load_config() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[spur] warning: failed to load config.toml: {e}; using defaults");
                    Default::default()
                }
            };
            let config_arc = std::sync::Arc::new(config.clone());
            let license = SpurLicense::from_env_or_disabled();
            if let Err(e) = onboarding::maybe_prompt_first_run(&license).await {
                tracing::warn!("first-run prompt failed: {e}; continuing");
            }
            let initial_license_state =
                spur_core::license_runtime::to_event_state(license.current_state());

            // Create PmService (optional — returns None if no backend available)
            let pm_service = if license
                .feature_gate()
                .has(spur_license::FeatureKey::PM_INTEGRATION)
            {
                spur_pm::PmService::try_new(
                    config.pm.github.as_ref().and_then(|g| g.repo.clone()),
                    config.pm.beads.as_ref().is_none_or(|b| b.enabled),
                    config.pm.github.as_ref().is_none_or(|g| g.enabled),
                    &repo_root,
                    None,
                )
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("PM service initialization failed: {e}");
                    None
                })
            } else {
                tracing::info!("PM integration not available on current tier");
                None
            };
            let pm_arc = pm_service.map(std::sync::Arc::new);

            let orch = Orchestrator::new(repo_root.clone(), config, Some(license.feature_gate()))?;
            let mut orch = if let Some(pm) = pm_arc {
                orch.with_pm_service(pm)
            } else {
                orch
            };
            let _license_runtime = orch.spawn_license_runtime(license.clone());
            let event_rx = orch.subscribe();

            // Clone the review_sink BEFORE orch is moved.
            let review_sink_for_dispatcher = orch.review_sink.clone();

            // Create permission channel
            let (perm_tx, perm_rx) =
                tokio::sync::mpsc::unbounded_channel::<spur_acp::types::PermissionRequest>();

            // Channel feeding run_interactive (non-review InteractiveInput variants).
            let (user_tx, user_rx) = tokio::sync::mpsc::channel::<spur_core::InteractiveInput>(32);

            // Channel feeding the review dispatcher (SubmitReview only).
            let (dispatch_tx, dispatch_rx) =
                tokio::sync::mpsc::channel::<spur_core::InteractiveInput>(32);

            // Spawn the review dispatcher task.
            tokio::spawn(spur_core::review_dispatcher_loop(
                dispatch_rx,
                review_sink_for_dispatcher,
            ));

            // Retain a copy of the brain override before it is moved into the
            // orchestrator spawn below, so the auto-resume block can inspect it.
            let brain_for_resume = brain.clone();

            // Spawn interactive orchestrator (moves orch). `mut` so we can
            // `&mut orch_handle` inside a timeout for graceful shutdown below.
            let overflow_continuations = spur_core::continuation_bridge::new_overflow_buf();

            // Wire the ingress sender + overflow into the orchestrator so that
            // `McpCallbackServer` can route detached delegation completions back
            // through `report_detached_completion`.
            orch.set_continuation_tx(user_tx.clone(), overflow_continuations.clone());

            let mut orch_handle = tokio::spawn(async move {
                if let Err(e) = orch
                    .run_interactive(user_rx, brain, Some(perm_tx), overflow_continuations)
                    .await
                {
                    tracing::error!(error = %e, "Interactive session error");
                }
            });

            // TUI → spur-cli translation task: routes review decisions to dispatch_tx,
            // everything else to user_tx.
            let (tui_tx, mut tui_rx) = tokio::sync::mpsc::channel::<spur_tui::UserInput>(32);
            let user_tx_for_tui = user_tx.clone();
            tokio::spawn(async move {
                while let Some(input) = tui_rx.recv().await {
                    let converted = match input {
                        spur_tui::UserInput::Message {
                            blocks, interrupt, ..
                        } => spur_core::InteractiveInput::Message { blocks, interrupt },
                        spur_tui::UserInput::NewSessionWithMessage { blocks, interrupt } => {
                            spur_core::InteractiveInput::NewSessionWithMessage { blocks, interrupt }
                        }
                        spur_tui::UserInput::ListSessions => {
                            spur_core::InteractiveInput::ListSessions
                        }
                        spur_tui::UserInput::ResumeSession { session_id } => {
                            spur_core::InteractiveInput::ResumeSession { session_id }
                        }
                        spur_tui::UserInput::SetSessionMode { mode_id } => {
                            spur_core::InteractiveInput::SetSessionMode { mode_id }
                        }
                        spur_tui::UserInput::SubmitReview {
                            executor_id,
                            attempt_n,
                            decision,
                        } => spur_core::InteractiveInput::SubmitReview {
                            executor_id,
                            attempt_n,
                            decision,
                        },
                        spur_tui::UserInput::VendorExec {
                            session,
                            method,
                            params,
                        } => spur_core::InteractiveInput::VendorExec {
                            session,
                            method,
                            params,
                        },
                        spur_tui::UserInput::CancelStream { session } => {
                            spur_core::InteractiveInput::CancelStream { session }
                        }
                        spur_tui::UserInput::RefreshIssues => {
                            spur_core::InteractiveInput::RefreshIssues
                        }
                        spur_tui::UserInput::GetIssueDetail { id } => {
                            spur_core::InteractiveInput::GetIssueDetail { id }
                        }
                        spur_tui::UserInput::UpdateIssue { id, update } => {
                            spur_core::InteractiveInput::UpdateIssue { id, update }
                        }
                    };

                    // SubmitReview → dispatch_tx; everything else → user_tx.
                    if matches!(converted, spur_core::InteractiveInput::SubmitReview { .. }) {
                        if let Err(e) = dispatch_tx.send(converted).await {
                            tracing::warn!(error = %e, "review decision dropped — dispatcher channel closed");
                        }
                    } else {
                        if let Err(e) = user_tx_for_tui.send(converted).await {
                            tracing::warn!(error = %e, "user input dropped — orchestrator channel closed");
                        }
                    }
                }
            });

            // Landing decision: auto-resume last active session, or land in
            // the picker, or land on Dashboard. `--dashboard` forces Dashboard,
            // `--sessions` forces the picker, and otherwise we auto-resume
            // whichever session the metadata pointer names.
            let metadata_path = repo_root.join(".spur").join("session_metadata.json");
            let meta = spur_tui::session_metadata::SessionMetadataStore::load(&metadata_path);

            let force_picker = sessions && !dashboard;

            // Auto-resume is driven by the ACP session id (the agent-authoritative
            // id), not the SPUR in-process id. We also gate on the stored brain
            // matching the launch-time `--brain` override to avoid handing a
            // claude-owned session id to kiro (and vice versa).
            let auto_resume: Option<(String, String)> = if dashboard || sessions {
                None
            } else {
                match meta.last_active_acp() {
                    Some((acp, stored_brain)) => match brain_for_resume.as_deref() {
                        Some(requested) if requested != stored_brain => {
                            tracing::info!(
                                requested = requested,
                                stored = %stored_brain,
                                "auto-resume skipped: brain override mismatches stored brain"
                            );
                            None
                        }
                        _ => Some((acp, stored_brain)),
                    },
                    None => None,
                }
            };

            if let Some((acp_id, _stored_brain)) = auto_resume {
                let resume_tx = tui_tx.clone();
                tokio::spawn(async move {
                    let _ = resume_tx
                        .send(spur_tui::UserInput::ResumeSession { session_id: acp_id })
                        .await;
                });
            } else if !force_picker {
                let warm_tx = user_tx.clone();
                tokio::spawn(async move {
                    let _ = warm_tx.send(spur_core::InteractiveInput::WarmConnect).await;
                });
            }

            // Run TUI (blocks). Capture the result so we can run structured
            // shutdown before propagating any error — otherwise `?` would
            // leak the orchestrator/dispatcher/translator tasks.
            let tui_result = spur_tui::app::run_tui_with_license(
                event_rx,
                Some(tui_tx),
                Some(perm_rx),
                force_picker,
                config_arc,
                initial_license_state,
            )
            .await;

            // TUI exited — its `user_input_tx` is dropped, which causes the
            // translator task to exit, which drops `user_tx` and `dispatch_tx`,
            // which causes `run_interactive`'s `user_input_rx.recv()` and the
            // `review_dispatcher_loop` to return, letting the orchestrator's
            // cleanup path (`connection.shutdown()` → `killpg` on the agent's
            // pgid) run to completion.
            //
            // Wait up to 5s for that graceful chain; if it stalls, abort the
            // task — `Drop for NativeAcpConnection` will still SIGKILL the
            // process group, preventing zombie descendants.
            match tokio::time::timeout(std::time::Duration::from_secs(5), &mut orch_handle).await {
                Ok(_) => tracing::info!("orchestrator shut down gracefully"),
                Err(_) => {
                    tracing::warn!("orchestrator shutdown timed out after 5s; aborting");
                    orch_handle.abort();
                    // Swallow JoinError::Cancelled so we can surface the TUI
                    // error (if any) instead of the abort-induced cancellation.
                    let _ = (&mut orch_handle).await;
                }
            }

            // Propagate the TUI error (if any) after structured shutdown.
            tui_result?;

            Ok(())
        }
    }
}

async fn cmd_agents(repo_root: PathBuf, command: Option<AgentsCommands>) -> Result<()> {
    let config = load_config()?;
    let mut orch = Orchestrator::new(repo_root, config, None)?;

    match command {
        None => {
            let agents = orch.registry.list();
            if agents.is_empty() {
                println!("No agents registered. Run `spur init` first.");
            } else {
                println!(
                    "{:<15} {:<10} {:<8} {:<8} Health",
                    "Name", "Transport", "Role", "Cost"
                );
                println!("{}", "-".repeat(55));
                for agent in agents {
                    let health = orch
                        .registry
                        .health(&agent.name)
                        .cloned()
                        .unwrap_or(spur_acp::AgentHealth::Unknown);
                    let health_str = match health {
                        spur_acp::AgentHealth::Ready => "ready",
                        spur_acp::AgentHealth::Unknown => "unknown",
                        spur_acp::AgentHealth::Busy => "busy",
                        spur_acp::AgentHealth::Error(_) => "error",
                        spur_acp::AgentHealth::RateLimited { .. } => "rate-limited",
                    };
                    println!(
                        "{:<15} {:<10} {:<8} {:<8} {}",
                        agent.name,
                        format!("{:?}", agent.transport),
                        format!("{:?}", agent.role),
                        format!("{:?}", agent.cost_tier),
                        health_str,
                    );
                }
            }
        }
        Some(AgentsCommands::Add { path }) => {
            println!("[spur] Registering agent at: {path}");
        }
        Some(AgentsCommands::Remove { name }) => {
            if orch.registry.remove(&name) {
                println!("[spur] Removed agent: {name}");
            } else {
                println!("[spur] Agent not found: {name}");
            }
        }
        Some(AgentsCommands::Check) => {
            println!("[spur] Health-checking all agents...");
            let results = orch.check_agents().await;
            for (name, health) in results {
                let status = match health {
                    spur_acp::AgentHealth::Ready => "ready".to_string(),
                    spur_acp::AgentHealth::Error(e) => format!("error: {e}"),
                    other => format!("{:?}", other),
                };
                println!("  {name}: {status}");
            }
        }
    }
    Ok(())
}

fn format_duration(secs: i64) -> String {
    format!("{}m {:02}s", secs / 60, secs % 60)
}

fn load_config() -> Result<SpurConfig> {
    // Try project config first, then user config.
    let project_config = PathBuf::from(".spur/config.toml");
    let user_config = directories::BaseDirs::new()
        .map(|d| d.home_dir().join(".spur/config.toml"))
        .unwrap_or_default();

    if project_config.exists() {
        let content = std::fs::read_to_string(&project_config)?;
        Ok(toml::from_str(&content)?)
    } else if user_config.exists() {
        let content = std::fs::read_to_string(&user_config)?;
        Ok(toml::from_str(&content)?)
    } else {
        Ok(SpurConfig::default())
    }
}

fn load_orchestrator(repo_root: PathBuf) -> Result<Orchestrator> {
    let config = load_config()?;
    Orchestrator::new(repo_root, config, None)
}
