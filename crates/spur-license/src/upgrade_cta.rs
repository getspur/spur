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

use crate::policy::PolicyResolver;
use crate::{FeatureGateError, FeatureKey, Plan};

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

/// Return the lowest tier that grants `key`, walking the embedded
/// policy in ascending order (Community → Pro → Team → Enterprise).
/// Returns `None` if no tier grants the key (e.g. unknown / future
/// key not yet in policy).
///
/// The TUI upgrade-CTA modal uses this to surface "Required tier:
/// Pro" alongside "Current tier: Community" — the single most
/// conversion-relevant line in the modal.
///
/// Note: this is intentionally separate from `FeatureGate` itself,
/// which is per-instance (snapshot of resolved features for the
/// current license state). The required-tier query needs the
/// global policy, not a snapshot.
pub fn required_tier_for(key: FeatureKey) -> Option<Plan> {
    let resolver = PolicyResolver::embedded();
    let needle = key.as_str();
    for plan in [Plan::Community, Plan::Pro, Plan::Team, Plan::Enterprise] {
        let tier_label = plan_to_resolver_label(plan);
        match resolver.tier_features(tier_label) {
            Ok(features) if features.iter().any(|f| f == needle) => return Some(plan),
            _ => continue,
        }
    }
    None
}

fn plan_to_resolver_label(plan: Plan) -> &'static str {
    match plan {
        Plan::Community => "community",
        Plan::Pro => "pro",
        Plan::Team => "team",
        Plan::Enterprise => "enterprise",
        // LTD plans inherit the Pro feature set; treat as Pro for
        // the required-tier display.
        Plan::StarterLtd | Plan::BuilderLtd | Plan::FounderLtd => "pro",
        // Defensive default for an unknown plan: render as community
        // (the most conservative tier).
        Plan::Unknown => "community",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tier;

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

    #[test]
    fn required_tier_for_community_key_returns_community() {
        // `cli_core_init` is in the community feature set per the
        // embedded policy (verified against
        // `resources/default_policy.json`).
        assert_eq!(
            required_tier_for(FeatureKey::CLI_CORE_INIT),
            Some(Plan::Community),
        );
    }

    #[test]
    fn required_tier_for_pro_only_key_returns_pro() {
        // `pm_pro_beads_advanced` is in the Pro feature set but NOT
        // in the community feature set per the embedded policy
        // (verified against `resources/default_policy.json`). Pro
        // walks before Team / Enterprise so the lowest-granting
        // tier is Pro.
        assert_eq!(
            required_tier_for(FeatureKey::PM_PRO_BEADS_ADVANCED),
            Some(Plan::Pro),
        );
    }

    #[test]
    fn feature_gate_error_is_clone() {
        // Tier 2 TUI App owns a `FeatureGateError` inside
        // `App::upgrade_modal: Option<UpgradeModalState>`, so the
        // error must be `Clone`. This smoke also pins the derive
        // against accidental removal.
        let err = denied(FeatureKey::CLI_CORE_EXEC, Tier::Community);
        let _cloned: FeatureGateError = err.clone();
    }
}
