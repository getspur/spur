use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use tracing_subscriber::prelude::*;

use spur_acp::config::SpurConfig;
use spur_acp::SessionId;
use spur_core::{Orchestrator, RunOpts};

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
    Init,
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
    /// Manage workflows
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommands,
    },
    /// Launch interactive TUI dashboard
    Watch {
        /// Override the brain agent (default from config)
        #[arg(long)]
        brain: Option<String>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir()?;

    let tui_mode = matches!(cli.command, Commands::Watch { .. });
    let _tracing_guard = init_tracing(tui_mode, &repo_root)?;

    match cli.command {
        Commands::Init => cmd_init(repo_root).await,
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
                if result.success { "completed" } else { "failed" },
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
                if result.success { "completed" } else { "failed" },
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
                                let short_id = if s.id.len() > 8 {
                                    &s.id[..8]
                                } else {
                                    &s.id
                                };
                                let duration = s.duration_seconds.map(format_duration).unwrap_or_else(|| "-".into());
                                let cost = s.estimated_cost_usd.map(|c| format!("${:.2}", c)).unwrap_or_else(|| "-".into());
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
                                println!(
                                    "Ended:    {}",
                                    s.ended_at.as_deref().unwrap_or("-")
                                );
                                println!("Duration: {}", s.duration_seconds.map(format_duration).unwrap_or_else(|| "-".into()));
                                println!("Cost:     {}", s.estimated_cost_usd.map(|c| format!("${:.2}", c)).unwrap_or_else(|| "-".into()));
                                println!(
                                    "Task:     {}",
                                    s.task_summary.as_deref().unwrap_or("-")
                                );

                                let delegations = ct.session_delegations(&sid)?;
                                if !delegations.is_empty() {
                                    println!("\nDelegations:");
                                    for d in &delegations {
                                        let del_duration = match (&d.requested_at, &d.completed_at) {
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
                            s.agent,
                            s.total_cost_usd,
                            s.session_count,
                            s.total_duration_seconds,
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
                                println!("  {}: ${:.2} ({} sessions)", p.project, p.total_cost_usd, p.session_count);
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
                    let mut adapter = spur_pm::GitHubAdapter::new(None);
                    use spur_pm::PmAdapter;
                    adapter.connect().await?;
                    println!("[spur] Connected to GitHub: {}", adapter.repo.unwrap_or_default());
                }
                _ => println!("[spur] Unknown service: {service}. Supported: github"),
            }
            Ok(())
        }
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
        Commands::Watch { brain } => {
            let orch = load_orchestrator(repo_root)?;
            let event_rx = orch.subscribe();

            // Create permission channel
            let (perm_tx, perm_rx) =
                tokio::sync::mpsc::unbounded_channel::<spur_acp::types::PermissionRequest>();

            // Create user input channel
            let (user_tx, user_rx) = tokio::sync::mpsc::channel::<spur_core::InteractiveInput>(32);

            // Spawn interactive orchestrator with permission channel (moves ownership)
            let orch_handle = tokio::spawn(async move {
                if let Err(e) = orch.run_interactive(user_rx, brain, Some(perm_tx)).await {
                    tracing::error!(error = %e, "Interactive session error");
                }
            });

            // Create a wrapper sender that converts TUI's UserInput to InteractiveInput
            let (tui_tx, mut tui_rx) = tokio::sync::mpsc::channel::<spur_tui::UserInput>(32);
            tokio::spawn(async move {
                while let Some(input) = tui_rx.recv().await {
                    let converted = match input {
                        spur_tui::UserInput::Message { text, interrupt, .. } => {
                            spur_core::InteractiveInput::Message { text, interrupt }
                        }
                        spur_tui::UserInput::ListSessions => {
                            spur_core::InteractiveInput::ListSessions
                        }
                        spur_tui::UserInput::ResumeSession { session_id } => {
                            spur_core::InteractiveInput::ResumeSession { session_id }
                        }
                    };
                    let _ = user_tx.send(converted).await;
                }
            });

            // Run TUI with permission channel (blocks main task)
            spur_tui::run_tui(event_rx, Some(tui_tx), Some(perm_rx), false).await?;

            // After TUI exits, abort orchestrator
            orch_handle.abort();
            Ok(())
        }
    }
}

async fn cmd_init(repo_root: PathBuf) -> Result<()> {
    println!("[spur] Scanning for agents...");
    let config = SpurConfig::default();
    let mut orch = Orchestrator::new(repo_root.clone(), config)?;
    let found = orch.init_agents().await?;

    if found.is_empty() {
        println!("[spur] No agents found on $PATH.");
        println!("[spur] Install one of: kiro-cli, claude, codex, gemini");
    } else {
        println!("[spur] Found {} agents:", found.len());
        for name in &found {
            println!("  - {name}");
        }
    }

    // Build config from discovered agents and write to .spur/config.toml
    let agents_entries: Vec<spur_acp::config::AgentConfig> = orch
        .registry
        .list()
        .into_iter()
        .cloned()
        .collect();
    let mut persist_config = SpurConfig::default();
    persist_config.agents.entries = agents_entries;

    let config_dir = repo_root.join(".spur");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");
    let toml_str = toml::to_string_pretty(&persist_config)?;
    std::fs::write(&config_path, toml_str)?;
    println!("[spur] Config written to {}", config_path.display());

    println!("[spur] Initialized.");
    Ok(())
}

async fn cmd_agents(repo_root: PathBuf, command: Option<AgentsCommands>) -> Result<()> {
    let config = load_config()?;
    let mut orch = Orchestrator::new(repo_root, config)?;

    match command {
        None => {
            let agents = orch.registry.list();
            if agents.is_empty() {
                println!("No agents registered. Run `spur init` first.");
            } else {
                println!(
                    "{:<15} {:<10} {:<8} {:<8} {}",
                    "Name", "Transport", "Role", "Cost", "Health"
                );
                println!("{}", "-".repeat(55));
                for agent in agents {
                    let health = orch.registry.health(&agent.name).cloned().unwrap_or(spur_acp::AgentHealth::Unknown);
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
    Orchestrator::new(repo_root, config)
}
