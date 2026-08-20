//! Discrete finite-horizon, non-preemptive scheduling and allocation rules.

use crate::rules::catalog::RuleRegistry;

/// Returns the validated scheduling catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("scheduling")
        .expect("scheduling manifest registry")
}
