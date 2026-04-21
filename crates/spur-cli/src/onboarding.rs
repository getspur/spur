//! First-run TTY prompt for the Community-default onboarding path.
//!
//! Persists `~/.spur/onboarded` (a one-line JSON marker) once the user has
//! either pasted a license key or explicitly continued on Community. On
//! subsequent runs the marker presence skips the prompt.
//!
//! TTY-skip: `is_terminal()` short-circuits when stdin isn't interactive
//! (CI safe).

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use spur_license::{LicenseStatus, Plan, SpurLicense};

#[derive(Serialize, Deserialize)]
struct OnboardingMarker {
    version: u32,
    first_run_at: String,
}

pub fn marker_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".spur").join("onboarded"))
}

pub fn marker_exists() -> bool {
    marker_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn write_marker() -> Result<()> {
    let path = marker_path().context("no home directory; cannot write onboarding marker")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let marker = OnboardingMarker {
        version: 1,
        first_run_at: chrono::Utc::now().to_rfc3339(),
    };
    std::fs::write(&path, serde_json::to_string(&marker)?)?;
    Ok(())
}

/// Returns true if the prompt SHOULD run for this license state.
/// False when: not a TTY, marker present, or license already configured
/// (anything other than the bare Community-default state).
pub fn should_prompt(license: &SpurLicense) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    if marker_exists() {
        return false;
    }
    let state = license.current_state();
    matches!(state.plan, Plan::Community) && matches!(state.status, LicenseStatus::Active)
}

/// Run the first-run prompt. Public entry point called from main.rs.
/// On any error, logs and continues (never blocks startup).
pub async fn maybe_prompt_first_run(license: &SpurLicense) -> Result<()> {
    if !should_prompt(license) {
        return Ok(());
    }
    eprintln!(
        "spur is running on the Community tier (free). Paste a license key to unlock Pro now, or press Enter to continue."
    );
    eprint!("> ");
    std::io::stderr().flush().ok();

    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let trimmed = line.trim();

    if trimmed.is_empty() {
        eprintln!("Continuing on Community.");
    } else {
        match license.activate(trimmed).await {
            Ok(state) => {
                eprintln!("Activated: {} ({})", state.plan.label(), state.status_text);
            }
            Err(e) => {
                eprintln!("Activation failed: {e}. Continuing on Community.");
            }
        }
    }
    write_marker()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_serialization_round_trips() {
        let marker = OnboardingMarker {
            version: 1,
            first_run_at: "2026-04-19T00:00:00Z".into(),
        };
        let s = serde_json::to_string(&marker).unwrap();
        let back: OnboardingMarker = serde_json::from_str(&s).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.first_run_at, "2026-04-19T00:00:00Z");
    }
}
