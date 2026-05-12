//! Token resolution for the GitHub adapter (spec §7.1).
//!
//! Resolution order: `SPUR_GITHUB_TOKEN` → `gh auth token` → OAuth Device Flow.

use std::env;

use octocrab::Octocrab;
use secrecy::{ExposeSecret, SecretString};
use tokio::process::Command;

use crate::sync::{SyncError, SyncResult};

/// Public client_id for the Spur GitHub OAuth app (Phase 1 placeholder; the
/// real client_id will replace this once the app is registered).
pub const SPUR_GITHUB_CLIENT_ID: &str = "Iv1.1234567890abcdef";

/// Default OAuth scopes for ingest. `repo` covers private repo metadata when
/// the user opts in; `read:org` lets us resolve org-private cross-references.
pub const DEFAULT_SCOPES: &[&str] = &["repo", "read:org"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    EnvVar,
    GhCli,
    DeviceFlow,
}

#[derive(Debug, Clone)]
pub struct GitHubToken {
    pub token: String,
    pub source: TokenSource,
}

/// Resolve a GitHub token using the Phase 1 fallback chain.
///
/// 1. `SPUR_GITHUB_TOKEN` env var (skip if empty/unset).
/// 2. `gh auth token` shell-out (skip on non-zero exit or missing binary).
/// 3. OAuth Device Flow via [`run_device_flow`] — interactive; prints the
///    user code and verification URL on stdout.
///
/// Phase 1 holds the device-flow token in process only; persistence to the
/// OS keyring lands in Phase 2.
pub async fn resolve_token() -> SyncResult<GitHubToken> {
    if let Some(token) = env_token() {
        return Ok(GitHubToken {
            token,
            source: TokenSource::EnvVar,
        });
    }

    if let Some(token) = gh_cli_token().await {
        return Ok(GitHubToken {
            token,
            source: TokenSource::GhCli,
        });
    }

    let token = run_device_flow(SPUR_GITHUB_CLIENT_ID, DEFAULT_SCOPES).await?;
    Ok(GitHubToken {
        token,
        source: TokenSource::DeviceFlow,
    })
}

/// Step 1: env var. Empty string treated as absent so a stale `export
/// SPUR_GITHUB_TOKEN=` does not block fallback to `gh`.
pub fn env_token() -> Option<String> {
    match env::var("SPUR_GITHUB_TOKEN") {
        Ok(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
        _ => None,
    }
}

/// Step 2: `gh auth token`. Returns `None` on missing binary, non-zero exit,
/// or empty stdout. Stderr is intentionally ignored — `gh` writes the "Token:"
/// line to stdout and informational warnings (e.g. token scope warnings) to
/// stderr; we only want the token.
pub async fn gh_cli_token() -> Option<String> {
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?;
    let trimmed = token.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Step 3: OAuth Device Flow. Prints the user code + verification URL on
/// stdout, polls the token endpoint until the user authorizes, and returns
/// the access token.
///
/// API verification (octocrab 0.50.0):
/// - `Octocrab::authenticate_as_device(&SecretString, scope_iter) -> Result<DeviceCodes>`
///   defined at `octocrab-0.50.0/src/auth.rs:151`.
/// - `DeviceCodes::poll_until_available(&Octocrab, &SecretString) -> Result<OAuth>`
///   defined at `octocrab-0.50.0/src/auth.rs:227` (gated on the `tokio` feature,
///   which is on by default).
/// - The crab used for the device-code request must have `base_uri` set to
///   `https://github.com` (the API base is `https://api.github.com`, but the
///   device-flow endpoints live on the marketing host). Documented in the
///   doc-comment at `octocrab-0.50.0/src/auth.rs:140`.
pub async fn run_device_flow(client_id: &str, scopes: &[&str]) -> SyncResult<String> {
    let crab = Octocrab::builder()
        .base_uri("https://github.com")
        .map_err(|e| SyncError::Other(anyhow::anyhow!("octocrab base_uri: {e}")))?
        .add_header(http::header::ACCEPT, "application/json".to_string())
        .build()
        .map_err(|e| SyncError::Other(anyhow::anyhow!("octocrab build: {e}")))?;

    let client_id_secret = SecretString::from(client_id.to_string());
    let codes = crab
        .authenticate_as_device(&client_id_secret, scopes.iter().copied())
        .await
        .map_err(|e| SyncError::NeedsAuth(format!("device flow start: {e}")))?;

    eprintln!(
        "Open {} and enter the code: {}",
        codes.verification_uri, codes.user_code
    );
    eprintln!(
        "Waiting for authorization (expires in {}s)…",
        codes.expires_in
    );

    let oauth = codes
        .poll_until_available(&crab, &client_id_secret)
        .await
        .map_err(|e| SyncError::NeedsAuth(format!("device flow poll: {e}")))?;

    Ok(oauth.access_token.expose_secret().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-5: env var path returns the raw token cleanly and trims whitespace.
    #[test]
    fn env_token_round_trip() {
        // SAFETY: tests run single-threaded by default in this crate; we
        // guard against parallel test runs by using a unique env-var name
        // would be ideal, but the spec pins SPUR_GITHUB_TOKEN. This test
        // explicitly asserts both set and unset behavior; if the developer
        // has it set in their shell we read that value.
        env::set_var("SPUR_GITHUB_TOKEN", "  ghp_test_token_001  ");
        assert_eq!(env_token(), Some("ghp_test_token_001".to_string()));

        env::set_var("SPUR_GITHUB_TOKEN", "");
        assert_eq!(env_token(), None);

        env::remove_var("SPUR_GITHUB_TOKEN");
        assert_eq!(env_token(), None);
    }

    /// T-5 (gh shim): when `gh` is missing on PATH the fallback returns
    /// None so resolution can move on to device flow.
    #[tokio::test]
    async fn gh_missing_binary_is_none() {
        let saved = env::var_os("PATH");
        env::set_var("PATH", "/nonexistent-spur-test-path");
        let got = gh_cli_token().await;
        if let Some(p) = saved {
            env::set_var("PATH", p);
        } else {
            env::remove_var("PATH");
        }
        assert_eq!(got, None);
    }
}
