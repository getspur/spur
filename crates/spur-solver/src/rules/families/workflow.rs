//! Finite bounded-trace workflow and state-transition rules.

use crate::rules::catalog::RuleRegistry;

pub mod compile;
pub use compile::COMPILER;

/// Returns the validated workflow catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("workflow")
        .expect("workflow manifest registry")
}
