//! Finite RBAC rules over caller-supplied roles, principals, and sessions.

pub mod compile;

use crate::rules::catalog::RuleRegistry;

pub use compile::COMPILER;

/// Returns the validated policy catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("policy").expect("policy manifest registry")
}
