//! CTA (call-to-action) renderer for [`FeatureGateError`].
//!
//! Translates a typed gate denial into structured stderr/UI output
//! with concrete recovery affordances.
//!
//! Lives in `spur-license` (alongside the typed-error contract) so
//! every error-rendering crate (`spur-cli`, future `spur-tui`
//! capability-tease modal, future `spur-mcp` JSON denial response)
//! can reuse the same formatter without circular crate dependencies.
//!
//! Plan C Tier 1 (CLI surface) → Tier 2 (TUI modal surface) →
//! Tier 3 (trial JWT CTA refinements). All three tiers consume
//! [`format_upgrade_cta`].

use crate::FeatureGateError;

/// Walk an `anyhow::Error` chain looking for a [`FeatureGateError`]
/// root cause. Returns `Some(&FeatureGateError)` if found.
///
/// The chain walk is required (not just `downcast_ref` on the top
/// error) because gate-checks may be wrapped via `.context(...)`
/// in callers we don't directly control. anyhow's `chain()` walks
/// the source links; the first matching downcast wins.
pub fn find_gate_error(err: &anyhow::Error) -> Option<&FeatureGateError> {
    err.chain()
        .find_map(|e| e.downcast_ref::<FeatureGateError>())
}

/// Format the structured CTA for a [`FeatureGateError`]. Returns
/// the multi-line stderr string. Caller is responsible for
/// printing it (so tests can capture without writing to stderr).
///
/// Output shape:
///
/// ```text
/// Error: feature `cli_core_exec` is not available on tier `Community`
///
/// To unlock this feature:
///   • View tier comparison:  spur auth status
///   • Activate a license:    spur auth login --key <KEY>
///
/// If you have a license but it appears stripped or expired, run
/// `spur auth logout` then re-login to fall back to a fresh
/// community-tier baseline before activating.
/// ```
///
/// Tier-aware copy variants (e.g. trial-expired, tampered-Pro,
/// Team/Enterprise paths) are deferred to Tier 2 / Tier 3 — they
/// can branch on `key` and `tier` from the error variant.
pub fn format_upgrade_cta(gate_err: &FeatureGateError) -> String {
    let mut out = String::new();
    out.push_str(&format!("Error: {gate_err}\n"));
    out.push('\n');
    out.push_str("To unlock this feature:\n");
    out.push_str("  \u{2022} View tier comparison:  spur auth status\n");
    out.push_str("  \u{2022} Activate a license:    spur auth login --key <KEY>\n");
    out.push('\n');
    out.push_str(
        "If you have a license but it appears stripped or expired, run\n\
         `spur auth logout` then re-login to fall back to a fresh\n\
         community-tier baseline before activating.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FeatureKey, Tier};

    fn denied(key: FeatureKey, tier: Tier) -> FeatureGateError {
        FeatureGateError::Denied { key, tier }
    }

    #[test]
    fn cta_names_the_denied_key() {
        let out = format_upgrade_cta(&denied(FeatureKey::CLI_CORE_EXEC, Tier::Community));
        assert!(out.contains("cli_core_exec"), "CTA must name key: {out}");
    }

    #[test]
    fn cta_lists_recovery_affordances() {
        let out = format_upgrade_cta(&denied(FeatureKey::CLI_CORE_RUN, Tier::Community));
        assert!(
            out.contains("spur auth status"),
            "CTA must mention status: {out}"
        );
        assert!(
            out.contains("spur auth login --key"),
            "CTA must mention login: {out}"
        );
        assert!(
            out.contains("spur auth logout"),
            "CTA must mention logout for tampered-tier recovery: {out}"
        );
    }

    #[test]
    fn find_gate_error_returns_some_when_root_is_gate_error() {
        let err = anyhow::Error::from(denied(FeatureKey::CLI_CORE_INIT, Tier::Community));
        assert!(
            find_gate_error(&err).is_some(),
            "must find gate error at root"
        );
    }

    #[test]
    fn find_gate_error_walks_anyhow_context_chain() {
        let err = anyhow::Error::from(denied(FeatureKey::CLI_CORE_TUI, Tier::Community))
            .context("while preparing TUI startup");
        assert!(
            find_gate_error(&err).is_some(),
            "must find gate error through .context() wrap"
        );
    }

    #[test]
    fn find_gate_error_returns_none_for_unrelated_anyhow_error() {
        let err = anyhow::anyhow!("totally unrelated I/O failure");
        assert!(find_gate_error(&err).is_none());
    }
}
