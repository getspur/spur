//! Resource capacity and placement rules over caller-supplied platform facts.

pub mod compile;

use crate::rules::catalog::RuleRegistry;

pub use compile::COMPILER;

/// Returns the validated resource catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("resource")
        .expect("resource manifest registry")
}
