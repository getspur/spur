mod commands;
mod onboarding;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use tracing_subscriber::prelude::*;

use commands::auth::AuthCommands;
use commands::flags::FlagsCommands;
use spur_acp::config::SpurConfig;
use spur_acp::{BrainSessionId, SessionId};
use spur_cli::log_writer;
use spur_cli::pm_service_gate_allows_construction;
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

        // Load SpurConfig to get [log] settings using the same repo-local
        // then user-global precedence as orchestrator startup.
        let spur_config = load_config_for_repo(repo_root)?;
        let log_cfg = &spur_config.log;

        // Read [log].level from config; fall back to default. RUST_LOG overrides.
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&log_cfg.level));

        let rotator =
            log_writer::build_rotator(&log_dir, log_cfg.max_file_bytes, log_cfg.max_files);
        let (non_blocking, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .lossy(true)
            .buffered_lines_limit(log_cfg.buffered_lines_limit)
            .finish(rotator);

        tracing_subscriber::registry()
            .with(env_filter)
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

/// Lazy gate-check used by all gated `Commands::*` arms.
///
/// Constructs `SpurLicense` + `FeatureGate` on first call. Non-gated
/// arms (Skills, Workflow, Config, Flags, Gc, Bot, Profile, Auth)
/// never invoke this helper, so they pay zero gate-construction cost.
///
/// `from_env_or_disabled` is fast for community-tier daily drivers
/// (no `SPUR_LICENSESEAT_*` env vars set ⇒ embedded `CommunityProvider`,
/// no I/O); for Pro users it reads the cached license JWT once.
fn require_cli_gate(key: spur_license::FeatureKey) -> Result<()> {
    let license = SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();
    spur_license::require_feature(&gate, key)?;
    Ok(())
}

fn resolve_landing(
    new: bool,
    sessions: bool,
    dashboard: bool,
    session: Option<&str>,
    brain_override: Option<&str>,
    meta: &spur_tui::session_metadata::SessionMetadataStore,
    registry: &spur_acp::AgentRegistry,
) -> spur_tui::landing::LandingDecision {
    use spur_tui::landing::LandingDecision;
    if new {
        return LandingDecision::ShowDashboard;
    }
    if let Some(acp) = session {
        let stored_brain = meta.brain_for_acp(acp);
        if let (Some(requested), Some(stored)) = (brain_override, stored_brain.as_deref()) {
            if requested != stored {
                tracing::warn!(
                    session = %acp,
                    requested_brain = %requested,
                    stored_brain = %stored,
                    "ignoring --brain override for explicit session attach"
                );
            }
        }
        let brain = stored_brain
            .or_else(|| brain_override.map(str::to_string))
            .unwrap_or_else(|| "claude-code".to_string());
        return LandingDecision::AttachExplicit {
            acp_id: acp.to_string(),
            brain,
        };
    }
    if sessions && !dashboard {
        return LandingDecision::ShowPicker { preselect: None };
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
        return LandingDecision::ShowPicker { preselect: None };
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
    /// Garbage-collect outcome blobs
    Gc {
        #[command(subcommand)]
        cmd: GcCmd,
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
        /// Attach to a specific ACP session by id.
        #[arg(long)]
        session: Option<String>,
        /// Profile the watch session and generate a flamegraph
        #[arg(long)]
        profile: bool,
        /// Profiling duration in seconds (requires --profile)
        #[arg(long, default_value = "30")]
        duration: u64,
        /// Test-only: run the orphan sweep and exit before entering the TUI.
        #[arg(long, hide = true)]
        exit_after_sweep: bool,
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
    /// Set a configuration value (e.g., `tui.edit_mode vim`).
    Set {
        /// Dotted key path. Supported: tui.edit_mode
        key: String,
        /// Value (e.g., vim or emacs).
        value: String,
        /// Write to ~/.spur/config.toml instead of repo-local config.
        #[arg(long)]
        global: bool,
    },
}

#[derive(Debug, Subcommand)]
enum GcCmd {
    /// Sweep outcome blobs older than the TTL
    Outcomes {
        /// Don't actually delete, just report
        #[arg(long)]
        dry_run: bool,
        /// TTL override in days; accepts Nd, Ndays, or bare N
        #[arg(long, value_parser = parse_duration_days)]
        older_than: Option<Duration>,
        /// Optional brain session id namespace to delete
        #[arg(long)]
        namespace: Option<String>,
    },
}

fn parse_duration_days(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let n_str = s
        .strip_suffix("days")
        .or_else(|| s.strip_suffix('d'))
        .unwrap_or(s);
    let days: u64 = n_str
        .trim()
        .parse()
        .map_err(|_| format!("expected integer days, got {s:?}"))?;
    if days == 0 {
        return Err("TTL floor is 1 day".into());
    }
    let secs = days
        .checked_mul(86_400)
        .ok_or_else(|| format!("days * 86_400 overflows u64: {days}"))?;
    Ok(Duration::from_secs(secs))
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
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            render_top_level_error(&err);
            ExitCode::FAILURE
        }
    }
}

/// Render the top-level error. If stderr is a TTY (or
/// `SPUR_FORCE_TTY=<non-empty>` in debug builds) and the error chain
/// contains a [`spur_license::FeatureGateError`], render the
/// structured upgrade CTA. Otherwise fall through to anyhow's
/// `{:#}` formatter, which walks the Display chain (root cause +
/// every `.context(...)` link) but does NOT include Debug or
/// backtrace frames. Non-TTY stderr (piped/scripted output)
/// always uses the plain path so tooling parsing isn't broken.
fn render_top_level_error(err: &anyhow::Error) {
    if is_tty_or_forced() {
        if let Some(gate_err) = spur_license::upgrade_cta::find_gate_error(err) {
            eprint!(
                "{}",
                spur_license::upgrade_cta::format_upgrade_cta(gate_err)
            );
            return;
        }
    }
    eprintln!("Error: {err:#}");
}

/// True if stderr is a real terminal, or — in debug builds only —
/// if `SPUR_FORCE_TTY` is set to a non-empty value. The
/// `cfg(debug_assertions)` gate means this branch is not present in
/// default release builds (i.e., when `debug_assertions` is off);
/// it exists solely to let `assert_cmd`-driven binary tests force
/// the CTA dispatch path without allocating a pty for the spawned
/// child. Note: `RUSTFLAGS=-C debug-assertions=on` in a release
/// profile would re-enable this branch; that's an opt-in build, not
/// the default `cargo build --release`.
fn is_tty_or_forced() -> bool {
    if std::io::stderr().is_terminal() {
        return true;
    }
    #[cfg(debug_assertions)]
    {
        if std::env::var("SPUR_FORCE_TTY").is_ok_and(|v| !v.is_empty()) {
            return true;
        }
    }
    false
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = std::env::current_dir()?;

    let tui_mode = matches!(cli.command, Commands::Tui { .. });
    let _tracing_guard = init_tracing(tui_mode, &repo_root)?;

    let reaped_orphans: Vec<spur_acp::orphan_registry::PgidRecord> = {
        use spur_acp::orphan_sweeper::OrphanSweeper;
        use spur_acp::process_inspector::production_inspector;
        let pgids_dir = repo_root.join(".spur").join("pgids");
        let report = OrphanSweeper::new(&pgids_dir, production_inspector()).run();
        if !report.killed.is_empty() {
            tracing::warn!(
                killed = report.killed.len(),
                recycled = report.skipped_recycled,
                "orphan_sweeper: cleaned up stale agent trees from prior session"
            );
        }
        report.killed
    };

    match cli.command {
        Commands::Init { force, with_skills } => {
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_INIT)?;
            commands::init::run(repo_root, force, with_skills).await
        }
        Commands::Skills { command } => match command {
            SkillsCommands::Init => commands::init::run_skills_init(&repo_root),
        },
        Commands::Agents { command } => {
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_AGENTS)?;
            cmd_agents(repo_root, command).await
        }
        Commands::Run {
            task,
            brain,
            issue,
            background,
        } => {
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_RUN)?;
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
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_EXEC)?;
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
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_SESSIONS)?;
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
        } => {
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_COST)?;
            match engine.as_str() {
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
            }
        }
        Commands::Connect { service } => {
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_CONNECT)?;
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
            ConfigCommands::Set { key, value, global } => {
                commands::config_set::run(&repo_root, &key, &value, global)?;
                Ok(())
            }
        },
        Commands::Flags { command } => commands::flags::run(command).await,
        Commands::Gc {
            cmd:
                GcCmd::Outcomes {
                    dry_run,
                    older_than,
                    namespace,
                },
        } => run_gc_outcomes(dry_run, older_than, namespace).await,
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
            session,
            profile,
            duration,
            exit_after_sweep,
        } => {
            // Test-only escape hatch: the orphan sweep already ran above
            // (see `reaped_orphans` block); exit before entering the TUI
            // loop so integration tests can verify reaping without
            // rendezvousing with the full TUI lifecycle.
            if exit_after_sweep {
                return Ok(());
            }
            // Gate fires BEFORE the `--profile` re-spawn block: otherwise
            // `spur tui --profile` would profile the parent successfully
            // and then the child would fail the gate, producing a confusing
            // "profile of an error exit" instead of failing fast on the
            // parent invocation.
            require_cli_gate(spur_license::FeatureKey::CLI_CORE_TUI)?;
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
                if let Some(ref session) = session {
                    args.push(format!("--session={session}"));
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

            // Phase A: Community singleton lock. One TUI orchestrator per repo
            // on Community; Pro removes this limit (Phase B will land cross-
            // instance state coordination). Lock guard lives for TUI lifetime.
            let _community_singleton_guard = if matches!(
                spur_license::Tier::from_plan(license.current_state().plan),
                spur_license::Tier::Community
            ) {
                let lock_path = repo_root.join(".spur").join(".spur-tui.pid");
                if let Some(parent) = lock_path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("creating singleton-lock parent dir {}", parent.display())
                    })?;
                }
                match spur_pm::pidfile::PidFileGuard::acquire(&lock_path) {
                    Ok(guard) => Some(guard),
                    Err(e) => {
                        eprintln!("Another SPUR TUI is already running on this repo.");
                        eprintln!();
                        eprintln!("{e}");
                        eprintln!();
                        eprintln!("Community runs one SPUR TUI per repository.");
                        eprintln!("Pro removes this limit and adds parallel workers within one");
                        eprintln!("orchestrator with shared lineage.");
                        eprintln!();
                        eprintln!("Activate a license: spur auth login --key <KEY>");
                        return Ok(());
                    }
                }
            } else {
                // Pro / Team / Enterprise: no singleton lock. Cross-instance
                // state coordination ships in Phase B.
                None
            };

            // Create PmService (optional — returns None if no backend available)
            let pm_service = if pm_service_gate_allows_construction(license.feature_gate().as_ref())
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

            // Surface orphan-sweep results in the TUI activity log. Sent
            // directly on `event_tx` (not via the funnel) because the
            // funnel handle is private; `seq=0` is fine here — these
            // events arrive before any other startup events.
            let now_secs = chrono::Utc::now().timestamp();
            for rec in &reaped_orphans {
                let _ = orch.event_tx.send(spur_acp::SpurEvent::now(
                    spur_acp::SpurEventBody::OrphanReaped {
                        agent_name: rec.agent_name.clone(),
                        pgid: rec.pgid,
                        age_secs: now_secs - rec.spawned_at,
                    },
                ));
            }

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
                session.as_deref(),
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
            let start_in_picker_with_preselect: Option<Option<String>> = match &landing {
                LandingDecision::AutoResume { acp_id, .. } => Some(Some(acp_id.clone())),
                LandingDecision::AttachExplicit { acp_id, .. } => Some(Some(acp_id.clone())),
                LandingDecision::ShowPicker { preselect } => Some(preselect.clone()),
                _ => None,
            };

            match &landing {
                LandingDecision::AttachExplicit { acp_id, .. } => {
                    let resume_tx = tui_tx.clone();
                    let id = acp_id.clone();
                    tokio::spawn(async move {
                        let _ = resume_tx
                            .send(spur_tui::UserInput::ResumeSession { session_id: id })
                            .await;
                    });
                }
                LandingDecision::AutoResume { .. } => {
                    // picker preselects; user must press Enter.
                }
                LandingDecision::ShowPicker { .. } => {
                    // picker opened by start_in_picker_with_preselect below
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
                start_in_picker_with_preselect,
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
        spur_tui::UserInput::SetSessionConfigOption { config_id, value } => {
            spur_core::InteractiveInput::SetSessionConfigOption { config_id, value }
        }
        spur_tui::UserInput::SetSessionModel {
            session_id: _,
            value,
        } => spur_core::InteractiveInput::SetSessionModel { value },
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
    load_config_for_repo(&std::env::current_dir()?)
}

fn load_config_for_repo(repo_root: &Path) -> Result<SpurConfig> {
    // Try project config first, then user config.
    let project_config = repo_root.join(".spur").join("config.toml");
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
    let pm_service = if pm_service_gate_allows_construction(license.feature_gate().as_ref()) {
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

async fn run_gc_outcomes(
    dry_run: bool,
    older_than: Option<Duration>,
    namespace: Option<String>,
) -> Result<()> {
    use spur_blob_store::OutcomeStore;
    use spur_worktree::git_blob_store::GitBlobOutcomeStore;
    use std::sync::Arc;

    let repo_root = std::env::current_dir()?;
    let store: Arc<dyn OutcomeStore> = Arc::new(GitBlobOutcomeStore::new(repo_root));

    if let Some(namespace) = namespace {
        let session_id = BrainSessionId::new(SessionId(namespace));
        if dry_run {
            println!("Would delete namespace {session_id}");
            return Ok(());
        }

        let report = store.delete_namespace(&session_id).await?;
        tracing::info!(
            target: "spur.metrics.outcome_namespace_deleted",
            brain_session_id = %session_id,
            artifact_count = report.count,
            total_bytes = report.total_bytes,
            source = "cli.gc_outcomes",
        );
        println!(
            "Deleted {} blobs ({} bytes) in namespace {session_id}",
            report.count, report.total_bytes,
        );
        return Ok(());
    }

    let ttl = older_than.unwrap_or_else(|| {
        let days: u64 = match std::env::var("SPUR_OUTCOME_TTL_DAYS") {
            Ok(raw) => match raw.parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => {
                    tracing::warn!(
                        env = %raw,
                        "SPUR_OUTCOME_TTL_DAYS is set but not a positive integer; using default 7"
                    );
                    7
                }
            },
            Err(_) => 7,
        };
        Duration::from_secs(days.saturating_mul(86_400))
    });

    if dry_run {
        println!("Dry-run: would sweep namespaces older than {:?}", ttl);
        return Ok(());
    }

    let report = store.sweep_older_than(ttl).await?;
    println!(
        "Swept {} namespaces / {} blobs / {} bytes (effective_ttl={:?})",
        report.namespaces_swept, report.blobs_swept, report.bytes_freed, report.effective_ttl
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_outcomes_parses_older_than() {
        let args = Cli::try_parse_from(["spur", "gc", "outcomes", "--older-than=14d"]).unwrap();
        if let Commands::Gc {
            cmd:
                GcCmd::Outcomes {
                    older_than,
                    dry_run,
                    ..
                },
        } = args.command
        {
            assert_eq!(
                older_than,
                Some(std::time::Duration::from_secs(14 * 86_400))
            );
            assert!(!dry_run);
        } else {
            panic!("wrong subcommand");
        }
    }

    #[test]
    fn parse_duration_days_accepts_common_forms() {
        assert!(parse_duration_days("30d").is_ok());
        assert!(parse_duration_days("30").is_ok());
        assert!(parse_duration_days("30days").is_ok());
        assert_eq!(
            parse_duration_days("30").unwrap(),
            std::time::Duration::from_secs(30 * 86_400)
        );
    }

    #[test]
    fn parse_duration_days_rejects_invalid_input() {
        assert!(parse_duration_days("30h").is_err());
        assert!(parse_duration_days("notanumber").is_err());
        assert!(parse_duration_days("0").is_err());
        assert!(parse_duration_days("0d").is_err());
    }

    #[test]
    fn parse_duration_days_rejects_overflow() {
        // u64::MAX / 86_400 ~ 2.13e14; 1e18 overflows when * 86_400.
        let huge = "1000000000000000000";
        let err = parse_duration_days(huge).expect_err("overflow must error");
        assert!(err.contains("overflow"), "expected overflow message: {err}");
    }

    #[test]
    fn gc_outcomes_parses_namespace() {
        let args = Cli::try_parse_from([
            "spur",
            "gc",
            "outcomes",
            "--namespace",
            "550e8400-e29b-41d4-a716-446655440000",
        ])
        .expect("parse namespace");
        if let Commands::Gc {
            cmd:
                GcCmd::Outcomes {
                    namespace,
                    older_than,
                    dry_run,
                },
        } = args.command
        {
            assert_eq!(
                namespace.as_deref(),
                Some("550e8400-e29b-41d4-a716-446655440000")
            );
            assert!(older_than.is_none());
            assert!(!dry_run);
        } else {
            panic!("wrong subcommand");
        }
    }

    #[test]
    fn gc_outcomes_parses_dry_run_only() {
        let args =
            Cli::try_parse_from(["spur", "gc", "outcomes", "--dry-run"]).expect("parse dry-run");
        if let Commands::Gc {
            cmd:
                GcCmd::Outcomes {
                    namespace,
                    older_than,
                    dry_run,
                },
        } = args.command
        {
            assert!(dry_run);
            assert!(older_than.is_none());
            assert!(namespace.is_none());
        } else {
            panic!("wrong subcommand");
        }
    }
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

#[cfg(feature = "duckdb")]
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

    let (engine, _recovered) = AnalyticsEngine::open(&cache_path)?;
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

#[cfg(not(feature = "duckdb"))]
fn run_cost_duckdb(
    _today: bool,
    _week: bool,
    _range: Option<&str>,
    _export: Option<&str>,
) -> Result<()> {
    eprintln!("Error: DuckDB support is not compiled into this build.");
    eprintln!("       Rebuild with --features duckdb or use the default features.");
    std::process::exit(2);
}

#[cfg(feature = "duckdb")]
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

#[cfg(test)]
mod resolve_landing_tests {
    use super::*;

    fn empty_store() -> spur_tui::session_metadata::SessionMetadataStore {
        let file = tempfile::NamedTempFile::new().unwrap();
        spur_tui::session_metadata::SessionMetadataStore::load(file.path())
    }

    #[test]
    fn explicit_session_returns_attach_explicit() {
        let landing = resolve_landing(
            false,
            false,
            false,
            Some("abc-123"),
            None,
            &empty_store(),
            &spur_acp::AgentRegistry::new(),
        );
        assert!(matches!(
            landing,
            spur_tui::landing::LandingDecision::AttachExplicit { acp_id, .. } if acp_id == "abc-123"
        ));
    }

    #[test]
    fn new_flag_overrides_session_flag() {
        let landing = resolve_landing(
            true,
            false,
            false,
            Some("abc-123"),
            None,
            &empty_store(),
            &spur_acp::AgentRegistry::new(),
        );
        assert!(matches!(
            landing,
            spur_tui::landing::LandingDecision::ShowDashboard
        ));
    }
}
