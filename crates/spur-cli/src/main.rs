mod commands;
mod onboarding;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use tracing_subscriber::prelude::*;

use commands::auth::AuthCommands;
use commands::flags::FlagsCommands;
use spur_acp::config::SpurConfig;
use spur_acp::SessionId;
use spur_core::{Orchestrator, RunOpts};
use spur_license::SpurLicense;

/// Returns an optional guard that must be held until process exit to flush buffered logs.
fn init_tracing(
    tui_mode: bool,
    repo_root: &Path,
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    use tracing_subscriber::EnvFilter;

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
        // Quiet default for user-facing subcommands. `--verbose`/`-v` or
        // explicit `RUST_LOG` raises the level.
        let verbose = std::env::args().any(|a| a == "--verbose" || a == "-v");
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(if verbose {
                "info"
            } else {
                // Silence config-hygiene lints emitted on every invocation;
                // they fire from spur_acp::registry's lint pass and aren't
                // actionable from a user-facing subcommand.
                "warn,spur_acp::agents::defaults=warn,spur_acp::registry=error"
            })
        });
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
        Ok(None)
    }
}

fn resolve_landing(
    new: bool,
    sessions: bool,
    dashboard: bool,
    brain_override: Option<&str>,
    meta: &spur_tui::session_metadata::SessionMetadataStore,
    registry: &spur_acp::AgentRegistry,
) -> spur_tui::landing::LandingDecision {
    use spur_tui::landing::LandingDecision;
    if new {
        return LandingDecision::ShowDashboard;
    }
    if sessions && !dashboard {
        return LandingDecision::ShowPicker;
    }
    if dashboard {
        return LandingDecision::ShowDashboard;
    }

    if registry.list().is_empty() {
        return LandingDecision::SetupRequired;
    }

    if let Some((acp, stored_brain)) = meta.last_active_acp() {
        let brain_matches = match brain_override {
            Some(requested) => requested == stored_brain,
            None => true,
        };
        if brain_matches && meta.last_active_at_is_fresh(std::time::Duration::from_secs(86400)) {
            return LandingDecision::AutoResume {
                acp_id: acp,
                brain: stored_brain,
            };
        }
    }

    if meta.has_any_session() {
        return LandingDecision::ShowPicker;
    }

    LandingDecision::ShowDashboard
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
    Init {
        /// Overwrite existing .spur/config.toml.
        #[arg(long)]
        force: bool,
        /// Also install SpurPower skills after config init.
        #[arg(long)]
        with_skills: bool,
    },
    /// Install SpurPower skills into adapter directories
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
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
        /// Restrict to today only (DuckDB engine). Without --today, --week,
        /// or --range, the default window is the last 7 days.
        #[arg(long)]
        today: bool,
        /// Show the last 7 days (default for DuckDB engine).
        #[arg(long)]
        week: bool,
        /// Group by dimension
        #[arg(long, value_name = "DIMENSION")]
        by: Option<String>,
        /// Export format
        #[arg(long, value_name = "FORMAT")]
        export: Option<String>,
        /// Data engine — "sqlite" (default, time-based from cost.db) or
        /// "duckdb" (token-based, reads agent JSONL). DuckDB requires
        /// --experimental until Phase 2.5 ships a persistent cache.
        #[arg(long, value_name = "ENGINE", default_value = "sqlite")]
        engine: String,
        /// Opt in to experimental DuckDB engine path.
        #[arg(long)]
        experimental: bool,
        /// Date range `YYYY-MM-DD..YYYY-MM-DD` (DuckDB engine only;
        /// overrides --today / --week).
        #[arg(long, value_name = "RANGE")]
        range: Option<String>,
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
    /// List and inspect runtime feature flags
    Flags {
        #[command(subcommand)]
        command: FlagsCommands,
    },
    /// Bot frontend commands
    Bot {
        #[command(subcommand)]
        command: BotCommands,
    },
    /// Launch interactive TUI dashboard
    #[command(visible_alias = "watch")]
    Tui {
        /// Override the brain agent (default from config)
        #[arg(long)]
        brain: Option<String>,
        /// Show session picker on launch
        #[arg(long)]
        sessions: bool,
        /// Land on Dashboard instead of auto-resuming last session.
        #[arg(long)]
        dashboard: bool,
        /// Force Dashboard — do not auto-resume last session.
        #[arg(long)]
        new: bool,
        /// Profile the watch session and generate a flamegraph
        #[arg(long)]
        profile: bool,
        /// Profiling duration in seconds (requires --profile)
        #[arg(long, default_value = "30")]
        duration: u64,
    },
    /// Performance profiling and monitoring
    Profile {
        #[command(subcommand)]
        command: Option<commands::profile::ProfileCommands>,
    },
}

#[derive(Subcommand)]
enum SkillsCommands {
    /// Render bundled+override skills into per-adapter agent dirs
    Init,
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

#[derive(Subcommand)]
enum BotCommands {
    /// Launch the Telegram bot frontend.
    Telegram {
        /// Override the brain agent (default from config)
        #[arg(long)]
        brain: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir()?;

    let tui_mode = matches!(cli.command, Commands::Tui { .. });
    let _tracing_guard = init_tracing(tui_mode, &repo_root)?;

    match cli.command {
        Commands::Init { force, with_skills } => {
            commands::init::run(repo_root, force, with_skills).await
        }
        Commands::Skills { command } => match command {
            SkillsCommands::Init => commands::init::run_skills_init(&repo_root),
        },
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
        Commands::Cost {
            today,
            week,
            by,
            export,
            engine,
            experimental,
            range,
        } => match engine.as_str() {
            "sqlite" => run_cost_sqlite(&repo_root, week, by.as_deref(), export.as_deref()),
            "duckdb" => {
                if !experimental {
                    eprintln!(
                        "Error: --engine duckdb is experimental; pass --experimental to opt in."
                    );
                    eprintln!("Note: the DuckDB engine rescans all agent JSONL on every");
                    eprintln!("      invocation until Phase 2.5 (persistent cache) ships.");
                    std::process::exit(2);
                }
                run_cost_duckdb(today, week, range.as_deref(), export.as_deref())
            }
            other => {
                eprintln!(
                    "Error: unknown --engine '{}'. Expected 'sqlite' or 'duckdb'.",
                    other
                );
                std::process::exit(2);
            }
        },
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
        Commands::Flags { command } => commands::flags::run(command).await,
        Commands::Bot {
            command: BotCommands::Telegram { brain },
        } => {
            let mut config = load_config()?;
            spur_bot::telegram::config::resolve_from_env(&mut config.bot.telegram);
            spur_bot::telegram::config::validate(&config.bot.telegram)?;
            let host = build_interactive_host(repo_root.clone(), config.clone(), brain).await?;
            spur_bot::telegram::run_telegram_bot(
                &config.bot.telegram,
                host,
                repo_root.join(".spur").join("bot").join("state.json"),
            )
            .await
        }
        Commands::Profile { command } => commands::profile::run(command).await,
        Commands::Tui {
            brain,
            sessions,
            dashboard,
            new,
            profile,
            duration,
        } => {
            if profile {
                let mut args = vec!["tui".to_string()];
                if let Some(ref b) = brain {
                    args.push(format!("--brain={}", b));
                }
                if sessions {
                    args.push("--sessions".to_string());
                }
                if dashboard {
                    args.push("--dashboard".to_string());
                }
                if new {
                    args.push("--new".to_string());
                }
                return commands::profile::run(Some(
                    commands::profile::ProfileCommands::Flamegraph {
                        bin: Some("spur".to_string()),
                        test: None,
                        bench: None,
                        example: None,
                        duration,
                        output: std::path::PathBuf::from("flamegraph.svg"),
                        args,
                    },
                ))
                .await;
            }

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
            let orch = if let Some(pm) = pm_arc {
                orch.with_pm_service(pm)
            } else {
                orch
            };
            let _license_runtime = orch.spawn_license_runtime(license.clone());

            // Retain a copy of the brain override before it is moved into the
            // orchestrator spawn below, so the auto-resume block can inspect it.
            let brain_for_resume = brain.clone();

            // Landing decision: resolve BEFORE orch is moved into host.
            let metadata_path = repo_root.join(".spur").join("session_metadata.json");
            let meta = spur_tui::session_metadata::SessionMetadataStore::load(&metadata_path);
            let landing = resolve_landing(
                new,
                sessions,
                dashboard,
                brain_for_resume.as_deref(),
                &meta,
                &orch.registry,
            );
            tracing::info!(?landing, "resolved TUI landing decision");

            let mut host = spur_interactive::InteractiveFrontendHost::spawn(orch, brain);
            let host_handle = host.handle();
            let event_rx = host.take_event_stream().expect("event stream");
            let perm_rx = host.take_permission_stream();

            // TUI → spur-cli translation task: routes SubmitReview through
            // send_review, everything else through send_command.
            let (tui_tx, mut tui_rx) = tokio::sync::mpsc::channel::<spur_tui::UserInput>(32);
            tokio::spawn(async move {
                while let Some(input) = tui_rx.recv().await {
                    match input {
                        spur_tui::UserInput::SubmitReview {
                            executor_id,
                            attempt_n,
                            decision,
                        } => {
                            let _ = host_handle
                                .send_review(spur_interactive::ReviewSubmission {
                                    executor_id,
                                    attempt_n,
                                    decision,
                                })
                                .await;
                        }
                        other => {
                            let _ = host_handle
                                .send_command(tui_input_to_interactive(other))
                                .await;
                        }
                    }
                }
            });

            use spur_tui::landing::LandingDecision;
            let force_picker = matches!(landing, LandingDecision::ShowPicker);

            match &landing {
                LandingDecision::AutoResume { acp_id, .. } => {
                    let resume_tx = tui_tx.clone();
                    let id = acp_id.clone();
                    tokio::spawn(async move {
                        let _ = resume_tx
                            .send(spur_tui::UserInput::ResumeSession { session_id: id })
                            .await;
                    });
                }
                LandingDecision::ShowPicker => {
                    // picker opened by start_in_picker = true below
                }
                LandingDecision::ShowDashboard | LandingDecision::SetupRequired => {
                    let warm_handle = host.handle();
                    tokio::spawn(async move {
                        let _ = warm_handle
                            .send_command(spur_core::InteractiveInput::WarmConnect)
                            .await;
                    });
                }
            }

            // Run TUI (blocks). Capture the result so we can run structured
            // shutdown before propagating any error — otherwise `?` would
            // leak the orchestrator/dispatcher/translator tasks.
            let tui_result = spur_tui::app::run_tui_with_license(
                event_rx,
                Some(tui_tx),
                perm_rx,
                force_picker,
                config_arc,
                initial_license_state,
                landing.clone(),
            )
            .await;

            // Structured shutdown through the shared host.
            if let Err(e) = host.shutdown().await {
                tracing::warn!(%e, "orchestrator shutdown timed out; aborting");
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

fn tui_input_to_interactive(input: spur_tui::UserInput) -> spur_core::InteractiveInput {
    match input {
        spur_tui::UserInput::Message {
            blocks, interrupt, ..
        } => spur_core::InteractiveInput::Message { blocks, interrupt },
        spur_tui::UserInput::NewSessionWithMessage { blocks, interrupt } => {
            spur_core::InteractiveInput::NewSessionWithMessage { blocks, interrupt }
        }
        spur_tui::UserInput::ListSessions => spur_core::InteractiveInput::ListSessions,
        spur_tui::UserInput::ResumeSession { session_id } => {
            spur_core::InteractiveInput::ResumeSession { session_id }
        }
        spur_tui::UserInput::SetSessionMode { mode_id } => {
            spur_core::InteractiveInput::SetSessionMode { mode_id }
        }
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
        spur_tui::UserInput::RefreshIssues => spur_core::InteractiveInput::RefreshIssues,
        spur_tui::UserInput::GetIssueDetail { id } => {
            spur_core::InteractiveInput::GetIssueDetail { id }
        }
        spur_tui::UserInput::UpdateIssue { id, update } => {
            spur_core::InteractiveInput::UpdateIssue { id, update }
        }
        spur_tui::UserInput::SubmitReview { .. } => {
            unreachable!("review routing is handled before translation")
        }
    }
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

async fn build_interactive_host(
    repo_root: PathBuf,
    config: SpurConfig,
    brain: Option<String>,
) -> Result<spur_interactive::InteractiveFrontendHost> {
    let license = SpurLicense::from_env_or_disabled();
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
        .unwrap_or(None)
    } else {
        None
    };

    let orch = Orchestrator::new(repo_root, config, Some(license.feature_gate()))?;
    let orch = if let Some(pm) = pm_service.map(std::sync::Arc::new) {
        orch.with_pm_service(pm)
    } else {
        orch
    };
    let _license_runtime = orch.spawn_license_runtime(license);
    Ok(spur_interactive::InteractiveFrontendHost::spawn(
        orch, brain,
    ))
}

fn load_orchestrator(repo_root: PathBuf) -> Result<Orchestrator> {
    let config = load_config()?;
    let license = SpurLicense::from_env_or_disabled();
    Orchestrator::new(repo_root, config, Some(license.feature_gate()))
}

// ─── cost subcommand helpers ──────────────────────────────────────────

fn run_cost_sqlite(
    repo_root: &Path,
    week: bool,
    by: Option<&str>,
    export: Option<&str>,
) -> Result<()> {
    let orch = load_orchestrator(repo_root.to_path_buf())?;
    let Some(ref ct) = orch.cost_tracker else {
        println!("Cost tracking not available.");
        return Ok(());
    };
    let summaries = if week {
        ct.week_summary()?
    } else {
        ct.today_summary()?
    };
    if summaries.is_empty() {
        println!("No cost data recorded yet.");
        return Ok(());
    }
    if export == Some("csv") {
        println!("agent,cost_usd,sessions,duration_seconds");
        for s in &summaries {
            println!(
                "{},{:.2},{},{}",
                s.agent, s.total_cost_usd, s.session_count, s.total_duration_seconds,
            );
        }
        return Ok(());
    }
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

    if let Some(dim) = by {
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
    Ok(())
}

fn run_cost_duckdb(
    today: bool,
    week: bool,
    range: Option<&str>,
    export: Option<&str>,
) -> Result<()> {
    use chrono::NaiveDate;
    use spur_context::{AnalyticsEngine, Reporter};

    // Phase 2.5: use a persistent cache under ~/.spur/cache/cost.duckdb so
    // warm invocations are sub-second. Cold first run still scans all JSONL.
    let cache_dir = directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".spur").join("cache"))
        .unwrap_or_else(|| PathBuf::from(".spur/cache"));
    std::fs::create_dir_all(&cache_dir)?;
    let cache_path = cache_dir.join("cost.duckdb");

    let engine = AnalyticsEngine::open(&cache_path)?;
    engine.initialize()?;
    let status = engine.create_agent_views()?;
    engine.load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())?;
    let materialized = engine.refresh_cache()?;
    engine.use_cached_events()?;
    if materialized > 0 {
        eprintln!(
            "[spur] materialized {} rows into cache at {}",
            materialized,
            cache_path.display()
        );
    } else {
        eprintln!(
            "[spur] cache at {} is up-to-date (no JSONL files newer than last refresh)",
            cache_path.display()
        );
    }

    let range = parse_range(range, today, week)?;
    let reporter = Reporter::new(engine);
    let reports = reporter.daily_report(range)?;

    // Collapse per-day × agent rows into per-agent totals for the chosen range.
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct AgentAgg {
        cost: f64,
        sessions: i64,
        input: i64,
        output: i64,
    }
    let mut by_agent: BTreeMap<String, AgentAgg> = BTreeMap::new();
    for r in &reports {
        for row in &r.agent_rows {
            let a = by_agent.entry(row.agent.clone()).or_default();
            a.cost += row.cost_usd;
            a.sessions += row.sessions;
            a.input += row.input_tokens;
            a.output += row.output_tokens;
        }
    }

    if by_agent.is_empty() {
        let status_hint = format!(
            "(engine views: claude={}, codex={}, kiro={})",
            status.claude, status.codex, status.kiro,
        );
        println!("No cost data found for the selected range. {}", status_hint);
        return Ok(());
    }

    if export == Some("csv") {
        println!("agent,cost_usd,sessions,input_tokens,output_tokens");
        for (agent, a) in &by_agent {
            println!(
                "{},{:.4},{},{},{}",
                agent, a.cost, a.sessions, a.input, a.output
            );
        }
        return Ok(());
    }

    let total: f64 = by_agent.values().map(|a| a.cost).sum();
    println!(
        "{:<18} {:>10} {:>10} {:>14} {:>14}",
        "Agent", "Cost", "Sessions", "Input tokens", "Output tokens"
    );
    println!("{}", "-".repeat(70));
    let mut rows: Vec<_> = by_agent.into_iter().collect();
    rows.sort_by(|a, b| {
        b.1.cost
            .partial_cmp(&a.1.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (agent, a) in rows {
        println!(
            "{:<18} ${:>9.4} {:>10} {:>14} {:>14}",
            agent, a.cost, a.sessions, a.input, a.output,
        );
    }
    println!("{}", "-".repeat(70));
    println!("{:<18} ${:>9.4}", "Total", total);
    // Return unused-variable silence for NaiveDate import when feature-gated.
    let _ = NaiveDate::from_ymd_opt;
    Ok(())
}

fn parse_range(range: Option<&str>, today: bool, _week: bool) -> Result<spur_context::ReportRange> {
    use anyhow::Context;
    use chrono::{DateTime, NaiveDate, Utc};
    use spur_context::ReportRange;

    // Precedence: explicit --range > --today > (--week or default) = last 7 days.
    // Defaulting to last-7-days matches user expectation for an interactive
    // cost summary; pass --today to recover the prior single-day scope.
    if let Some(s) = range {
        let (from, to) = s
            .split_once("..")
            .with_context(|| format!("invalid --range '{}': expected YYYY-MM-DD..YYYY-MM-DD", s))?;
        let from_date = NaiveDate::parse_from_str(from.trim(), "%Y-%m-%d")
            .with_context(|| format!("invalid --range start date '{}'", from))?;
        let to_date = NaiveDate::parse_from_str(to.trim(), "%Y-%m-%d")
            .with_context(|| format!("invalid --range end date '{}'", to))?;
        let from_dt: DateTime<Utc> = from_date
            .and_hms_opt(0, 0, 0)
            .context("bad start date conversion")?
            .and_utc();
        let to_dt: DateTime<Utc> = to_date
            .and_hms_opt(0, 0, 0)
            .context("bad end date conversion")?
            .and_utc();
        return Ok(ReportRange {
            from: from_dt,
            to: to_dt,
        });
    }
    if today {
        return Ok(ReportRange::today());
    }
    Ok(ReportRange::last_days(7))
}
