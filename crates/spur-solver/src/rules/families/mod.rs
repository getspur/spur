//! Built-in rule-family implementations.

use super::{catalog::RuleRegistry, compiler::RuleFamilyCompiler};

pub mod accessibility;
pub mod design;
pub mod policy;
pub mod resource;

static COMPILERS: [&dyn RuleFamilyCompiler; 4] = [
    &accessibility::COMPILER,
    &design::COMPILER,
    &policy::COMPILER,
    &resource::COMPILER,
];

/// Returns the validated registry containing every built-in family.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    super::manifest::manifest_registry()
}

/// Returns family compilers in stable family-ID order.
#[must_use]
pub fn compilers() -> &'static [&'static dyn RuleFamilyCompiler] {
    &COMPILERS
}

/// Looks up one family compiler by exact ID.
#[must_use]
pub fn compiler(id: &str) -> Option<&'static dyn RuleFamilyCompiler> {
    compilers()
        .iter()
        .copied()
        .find(|compiler| compiler.id() == id)
}
