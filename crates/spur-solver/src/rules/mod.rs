//! Versioned rule families that compile domain predicates to solver requests.

pub mod catalog;
pub mod execute;
pub mod families;
pub mod spec;

use catalog::RuleRegistry;
use serde::{Deserialize, Serialize};

/// Shared verification and synthesis semantics for every rule family.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSolveMode {
    /// Search for any assignment that violates the selected rule set.
    Verify,
    /// Search for an assignment that satisfies every selected rule.
    Synthesize,
}

/// Domain interpretation of a raw solver status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOutcome {
    /// Verification proved no violating assignment exists.
    Pass,
    /// Verification found a counterexample.
    Fail,
    /// Synthesis found a feasible assignment.
    Solution,
    /// Synthesis proved no feasible assignment exists.
    Infeasible,
    /// The solver could not decide.
    Unknown,
    /// The shared wall-clock budget expired.
    Timeout,
    /// Solver validation, process, or parsing failed.
    Error,
    /// An incremental solver session ended without a proof or model.
    Ended,
}

/// Returns the process-wide registry containing every built-in rule family.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    families::design::builtin_registry()
}
