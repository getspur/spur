use clap::{Parser, Subcommand};

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
    /// Launch TUI dashboard
    Watch,
}

#[derive(Subcommand)]
enum AgentsCommands {
    /// Register a custom agent
    Add {
        /// Path to agent binary
        path: String,
    },
    /// Remove an agent
    Remove {
        /// Agent name
        name: String,
    },
    /// Health-check all agents
    Check,
}

#[derive(Subcommand)]
enum SessionsCommands {
    /// Show session detail
    Show {
        /// Session ID
        id: String,
    },
    /// Terminate a session
    Kill {
        /// Session ID
        id: String,
    },
}

#[derive(Subcommand)]
enum WorkflowCommands {
    /// Validate a TOML workflow definition
    Validate {
        /// Path to workflow file
        file: String,
    },
    /// Execute a workflow
    Run {
        /// Path to workflow file
        file: String,
        /// Specific issue to trigger with
        #[arg(long)]
        issue: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            println!("Scanning for agents...");
            // TODO: scan $PATH, create ~/.spur/agents.toml
            println!("SPUR initialized.");
        }
        Commands::Agents { command } => match command {
            None => {
                // TODO: list agents from registry
                println!("No agents registered. Run `spur init` first.");
            }
            Some(AgentsCommands::Add { path }) => {
                println!("Registering agent at: {path}");
            }
            Some(AgentsCommands::Remove { name }) => {
                println!("Removing agent: {name}");
            }
            Some(AgentsCommands::Check) => {
                println!("Health-checking all agents...");
            }
        },
        Commands::Run {
            task,
            brain,
            issue,
            background,
        } => {
            let brain_name = brain.as_deref().unwrap_or("kiro");
            println!("[spur] Brain: {brain_name}");
            if let Some(ref issue) = issue {
                println!("[spur] Issue: {issue}");
            }
            if background {
                println!("[spur] Running in background...");
            }
            println!("[spur] Task: {task}");
            // TODO: orchestrator.run_adhoc()
        }
        Commands::Exec { agent, task } => {
            println!("[spur] Direct execution on: {agent}");
            println!("[spur] Task: {task}");
            // TODO: spawn agent, send task, stream output
        }
        Commands::Sessions { command } => match command {
            None => {
                println!("No active sessions.");
            }
            Some(SessionsCommands::Show { id }) => {
                println!("Session: {id}");
            }
            Some(SessionsCommands::Kill { id }) => {
                println!("Killing session: {id}");
            }
        },
        Commands::Cost { week, by, export } => {
            if week {
                println!("Weekly cost breakdown:");
            } else {
                println!("Today's cost summary:");
            }
            if let Some(ref dim) = by {
                println!("  Grouped by: {dim}");
            }
            if let Some(ref fmt) = export {
                println!("  Exporting as: {fmt}");
            }
        }
        Commands::Connect { service } => {
            println!("Connecting to: {service}");
        }
        Commands::Workflow { command } => match command {
            WorkflowCommands::Validate { file } => {
                println!("Validating: {file}");
            }
            WorkflowCommands::Run { file, issue } => {
                println!("Running workflow: {file}");
                if let Some(ref issue) = issue {
                    println!("  With issue: {issue}");
                }
            }
        },
        Commands::Watch => {
            println!("TUI dashboard (Phase 2)");
        }
    }

    Ok(())
}
