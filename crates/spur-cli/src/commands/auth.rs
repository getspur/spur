use anyhow::{anyhow, Result};
use clap::Subcommand;

use spur_license::{LicenseState, LicenseStatus, SpurLicense};

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommands {
    /// Activate a license key for the current machine.
    Login {
        /// License key to activate.
        #[arg(long)]
        key: String,
    },
    /// Show the cached license status.
    Status,
    /// Refresh the cached license from the provider.
    Refresh,
    /// Deactivate the current license on this machine.
    Logout,
}

pub async fn run(command: AuthCommands) -> Result<()> {
    run_with_license(command, SpurLicense::from_env_or_disabled()).await
}

pub async fn run_with_license(command: AuthCommands, license: SpurLicense) -> Result<()> {
    match command {
        AuthCommands::Login { key } => login(&license, &key).await,
        AuthCommands::Status => {
            print_state(&license.current_state());
            Ok(())
        }
        AuthCommands::Refresh => refresh(&license).await,
        AuthCommands::Logout => logout(&license).await,
    }
}

async fn login(license: &SpurLicense, key: &str) -> Result<()> {
    ensure_configured(license)?;
    let state = license.activate(key).await?;
    print_state(&state);
    Ok(())
}

async fn refresh(license: &SpurLicense) -> Result<()> {
    ensure_configured(license)?;
    let state = license.validate().await?;
    print_state(&state);
    Ok(())
}

async fn logout(license: &SpurLicense) -> Result<()> {
    ensure_configured(license)?;
    let state = license.deactivate().await?;
    print_state(&state);
    Ok(())
}

fn ensure_configured(license: &SpurLicense) -> Result<()> {
    if matches!(license.current_state().status, LicenseStatus::ConfigError) {
        return Err(anyhow!(
            "license provider is not configured; set SPUR_LICENSESEAT_API_KEY and SPUR_LICENSESEAT_PRODUCT_SLUG"
        ));
    }
    Ok(())
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
