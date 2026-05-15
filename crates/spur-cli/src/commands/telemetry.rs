use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use std::io::{self, IsTerminal, Write};

#[derive(Subcommand, Debug, Clone)]
pub enum TelemetryCommands {
    /// Show telemetry status and config path.
    Status,
    /// Enable telemetry categories.
    Enable {
        #[arg(value_enum)]
        scope: TelemetryScope,
    },
    /// Disable telemetry categories.
    Disable {
        #[arg(value_enum)]
        scope: TelemetryScope,
    },
    /// Rotate the anonymous telemetry id.
    ResetId,
    /// Configure telemetry in interactive mode when TTY is present.
    Config,
    /// Flush queued telemetry and shut down telemetry sender.
    Flush,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum TelemetryScope {
    Crash,
    Perf,
    Usage,
    All,
}

pub fn run(command: TelemetryCommands) -> Result<()> {
    match command {
        TelemetryCommands::Status => print_status(),
        TelemetryCommands::Enable { scope } => set_scope(scope, true),
        TelemetryCommands::Disable { scope } => {
            if matches!(scope, TelemetryScope::Crash) {
                println!(
                    "Note: disabling crash telemetry does not delete existing crash files in ~/.spur/crash-reports."
                );
            }
            set_scope(scope, false)
        }
        TelemetryCommands::ResetId => {
            let cfg = spur_telemetry::reset_anonymous_id()?;
            println!("anonymous_id reset to {}", cfg.anonymous_id);
            println!("config: {}", spur_telemetry::config_path().display());
            Ok(())
        }
        TelemetryCommands::Config => run_config_mode(),
        TelemetryCommands::Flush => {
            spur_telemetry::shutdown_sync();
            println!("telemetry flush complete");
            Ok(())
        }
    }
}

fn print_status() -> Result<()> {
    let cfg = spur_telemetry::load_config_or_default();
    println!(
        "telemetry config: {}",
        spur_telemetry::config_path().display()
    );
    println!("anonymous_id: {}", cfg.anonymous_id);
    println!("crash: {}", on_off(cfg.tier1_crash));
    println!("perf: {}", on_off(cfg.tier1_perf));
    println!("usage: {}", on_off(cfg.tier2_usage));
    Ok(())
}

fn set_scope(scope: TelemetryScope, enabled: bool) -> Result<()> {
    let scope = match scope {
        TelemetryScope::Crash => spur_telemetry::TelemetryScope::Crash,
        TelemetryScope::Perf => spur_telemetry::TelemetryScope::Perf,
        TelemetryScope::Usage => spur_telemetry::TelemetryScope::Usage,
        TelemetryScope::All => spur_telemetry::TelemetryScope::All,
    };
    let cfg = spur_telemetry::set_enabled(scope, enabled)?;
    println!(
        "telemetry config updated: {}",
        spur_telemetry::config_path().display()
    );
    println!("crash: {}", on_off(cfg.tier1_crash));
    println!("perf: {}", on_off(cfg.tier1_perf));
    println!("usage: {}", on_off(cfg.tier2_usage));
    Ok(())
}

fn run_config_mode() -> Result<()> {
    if !io::stdout().is_terminal() {
        return print_status();
    }

    let mut cfg = spur_telemetry::load_config_or_default();
    println!(
        "Telemetry config path: {}",
        spur_telemetry::config_path().display()
    );
    println!("Press Enter to keep current value.");

    cfg.tier1_crash = prompt_toggle("Enable crash telemetry", cfg.tier1_crash)?;
    cfg.tier1_perf = prompt_toggle("Enable perf telemetry", cfg.tier1_perf)?;
    cfg.tier2_usage = prompt_toggle("Enable usage telemetry", cfg.tier2_usage)?;

    spur_telemetry::save_config(&cfg)?;
    println!("telemetry config saved");
    Ok(())
}

fn prompt_toggle(label: &str, current: bool) -> Result<bool> {
    loop {
        print!("{label}? [{}] ", if current { "Y/n" } else { "y/N" });
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let normalized = input.trim().to_ascii_lowercase();

        match normalized.as_str() {
            "" => return Ok(current),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {
                println!("Please answer with y/yes or n/no.");
            }
        }
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}
