use anyhow::Result;
use clap::{Subcommand, ValueEnum};

use spur_license::SpurLicense;

#[derive(Subcommand, Debug, Clone)]
pub enum FlagsCommands {
    /// List all runtime flags and their evaluated state.
    List {
        /// Output format
        #[arg(long, value_enum, default_value_t = FlagsOutputFormat::Plain)]
        format: FlagsOutputFormat,
    },
}

#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum FlagsOutputFormat {
    #[default]
    Plain,
    Json,
}

pub async fn run(command: FlagsCommands) -> Result<()> {
    let license = SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();
    match command {
        FlagsCommands::List { format } => list_flags(gate.as_ref(), format),
    }
}

fn list_flags(gate: &spur_license::FeatureGate, format: FlagsOutputFormat) -> Result<()> {
    match format {
        FlagsOutputFormat::Plain => {
            println!("{:<30} {:<10}", "Flag", "State");
            println!("{}", "-".repeat(42));
            for key in known_flag_keys() {
                match gate.is_flag_enabled(key) {
                    Some(true) => println!("{:<30} {:<10}", key, "on"),
                    Some(false) => println!("{:<30} {:<10}", key, "off"),
                    None => println!("{:<30} {:<10}", key, "—"),
                }
            }
        }
        FlagsOutputFormat::Json => {
            let mut entries = Vec::new();
            for key in known_flag_keys() {
                entries.push(serde_json::json!({
                    "key": key.as_str(),
                    "enabled": gate.is_flag_enabled(key),
                }));
            }
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
    }
    Ok(())
}

fn known_flag_keys() -> Vec<spur_license::FeatureKey> {
    use spur_license::FeatureKey;
    vec![
        FeatureKey::KILL_ADVANCED_PLANNER,
        FeatureKey::ENABLE_BROWSER_TOOL,
        FeatureKey::ENABLE_COMPACTION_V2,
        FeatureKey::ENABLE_TELEMETRY,
    ]
}
