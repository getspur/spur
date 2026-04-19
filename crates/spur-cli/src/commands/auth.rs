use anyhow::{anyhow, Result};
use clap::{Subcommand, ValueEnum};

use spur_license::{LicenseState, LicenseStatus, SpurLicense};

/// Output format for auth subcommands. JSON uses the stable `LicenseStateEvent`
/// schema from `spur_acp::events`.
#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Plain,
    Json,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommands {
    /// Activate a license key for the current machine.
    Login {
        /// License key to activate.
        #[arg(long)]
        key: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
    /// Show the cached license status.
    Status {
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
    /// Refresh the cached license from the provider.
    Refresh {
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
    /// Deactivate the current license on this machine.
    Logout {
        #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
        format: OutputFormat,
    },
}

pub async fn run(command: AuthCommands) -> Result<()> {
    run_with_license(command, SpurLicense::from_env_or_disabled()).await
}

pub async fn run_with_license(command: AuthCommands, license: SpurLicense) -> Result<()> {
    match command {
        AuthCommands::Login { key, format } => {
            let state = login_inner(&license, &key).await?;
            print_by_format(&state, format);
            Ok(())
        }
        AuthCommands::Status { format } => {
            let state = license.current_state();
            print_by_format(&state, format);
            Ok(())
        }
        AuthCommands::Refresh { format } => {
            let state = refresh_inner(&license).await?;
            print_by_format(&state, format);
            Ok(())
        }
        AuthCommands::Logout { format } => {
            let state = logout_inner(&license).await?;
            print_by_format(&state, format);
            Ok(())
        }
    }
}

async fn login_inner(license: &SpurLicense, key: &str) -> Result<LicenseState> {
    ensure_configured(license)?;
    Ok(license.activate(key).await?)
}

async fn refresh_inner(license: &SpurLicense) -> Result<LicenseState> {
    ensure_configured(license)?;
    Ok(license.validate().await?)
}

async fn logout_inner(license: &SpurLicense) -> Result<LicenseState> {
    ensure_configured(license)?;
    Ok(license.deactivate().await?)
}

fn ensure_configured(license: &SpurLicense) -> Result<()> {
    if matches!(license.current_state().status, LicenseStatus::ConfigError) {
        return Err(anyhow!(
            "license provider is not configured; set SPUR_LICENSESEAT_API_KEY and SPUR_LICENSESEAT_PRODUCT_SLUG"
        ));
    }
    Ok(())
}

fn print_by_format(state: &LicenseState, format: OutputFormat) {
    match format {
        OutputFormat::Plain => print_state(state),
        OutputFormat::Json => {
            let event = spur_core::license_runtime::to_event_state(state.clone());
            match serde_json::to_string(&event) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("{{\"error\":\"serialization failed: {e}\"}}"),
            }
        }
    }
}

fn print_state(state: &LicenseState) {
    println!("[spur] License status: {:?}", state.status);
    println!("[spur] Plan: {}", state.plan.label());
    println!("[spur] Subject: {:?}", state.subject_kind);
    println!("[spur] Binding: {:?}", state.binding_mode);
    println!(
        "[spur] Offline: {}",
        if state.offline_ok { "yes" } else { "no" }
    );
    println!("[spur] Details: {}", state.status_text);
}
