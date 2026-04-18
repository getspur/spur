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
    match command {
        AuthCommands::Login { key } => login(&key).await,
        AuthCommands::Status => {
            let license = license();
            print_state(&license.current_state());
            Ok(())
        }
        AuthCommands::Refresh => refresh().await,
        AuthCommands::Logout => logout().await,
    }
}

async fn login(key: &str) -> Result<()> {
    let license = configured_license()?;
    let state = license.activate(key).await?;
    print_state(&state);
    Ok(())
}

async fn refresh() -> Result<()> {
    let license = configured_license()?;
    let state = license.validate().await?;
    print_state(&state);
    Ok(())
}

async fn logout() -> Result<()> {
    let license = configured_license()?;
    let state = license.deactivate().await?;
    print_state(&state);
    Ok(())
}

fn license() -> SpurLicense {
    SpurLicense::from_env_or_disabled()
}

fn configured_license() -> Result<SpurLicense> {
    let license = license();
    if matches!(license.current_state().status, LicenseStatus::ConfigError) {
        return Err(anyhow!(
            "license provider is not configured; set SPUR_LICENSESEAT_API_KEY and SPUR_LICENSESEAT_PRODUCT_SLUG"
        ));
    }
    Ok(license)
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
