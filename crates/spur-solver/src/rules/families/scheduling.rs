//! Discrete finite-horizon, non-preemptive scheduling and allocation rules.

pub mod compile;

use crate::rules::catalog::RuleRegistry;

pub use compile::COMPILER;

/// Returns the validated scheduling catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("scheduling")
        .expect("scheduling manifest registry")
}
