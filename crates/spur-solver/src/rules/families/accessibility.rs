//! Standard-backed accessibility rules over caller-supplied UI facts.

pub mod compile;

use crate::rules::catalog::RuleRegistry;

pub use compile::COMPILER;

/// Returns the validated accessibility catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("accessibility")
        .expect("accessibility manifest registry")
}
