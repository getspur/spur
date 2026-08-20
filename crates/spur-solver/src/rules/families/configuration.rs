//! Finite configuration compatibility rules over caller-supplied facts.

pub mod compile;

use crate::rules::catalog::RuleRegistry;

pub use compile::COMPILER;

/// Returns the validated configuration catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("configuration")
        .expect("configuration manifest registry")
}
