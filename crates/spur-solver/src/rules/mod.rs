//! Versioned rule families that compile domain predicates to solver requests.

pub mod catalog;
pub mod compiler;
pub mod execute;
pub mod families;
pub mod manifest_format;
pub mod primitives;
pub mod spec;

use catalog::RuleRegistry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::types::ConstraintExpr;

/// Shared verification and synthesis semantics for every rule family.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSolveMode {
    /// Evaluate a complete supplied model against every selected rule.
    Verify,
    /// Complete explicitly declared bounded unknowns under every selected rule.
    Synthesize,
}

/// One identity-preserving rule predicate compiled by a family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledRule {
    /// Stable catalog rule ID.
    pub rule_id: String,
    /// Caller-order binding index.
    pub binding_index: usize,
    /// Typed B-prime predicate for this binding.
    pub predicate: ConstraintExpr,
    /// Variables referenced by the predicate in stable order.
    pub required_variables: Vec<String>,
}

impl CompiledRule {
    /// Creates one compiled rule and derives its variable dependencies.
    #[must_use]
    pub fn new(
        rule_id: impl Into<String>,
        binding_index: usize,
        predicate: ConstraintExpr,
    ) -> Self {
        let mut required_variables = BTreeSet::new();
        collect_variables(&predicate, &mut required_variables);
        Self {
            rule_id: rule_id.into(),
            binding_index,
            predicate,
            required_variables: required_variables.into_iter().collect(),
        }
    }

    /// Returns a stable backend constraint ID for one family binding.
    #[must_use]
    pub fn constraint_id(&self, family: &str) -> String {
        format!(
            "{family}_rule_{}_{}",
            self.binding_index,
            self.rule_id.replace('.', "_")
        )
    }
}

fn collect_variables(expression: &ConstraintExpr, variables: &mut BTreeSet<String>) {
    match expression {
        ConstraintExpr::Var { name } => {
            variables.insert(name.clone());
        }
        ConstraintExpr::EnumLabel { var, .. } => {
            variables.insert(var.clone());
        }
        ConstraintExpr::Op { args, .. } => {
            for argument in args {
                collect_variables(argument, variables);
            }
        }
        ConstraintExpr::Int { .. }
        | ConstraintExpr::Bool { .. }
        | ConstraintExpr::Real { .. }
        | ConstraintExpr::Bv { .. } => {}
    }
}

/// Domain interpretation of a raw solver status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOutcome {
    /// The supplied complete model satisfies every selected rule.
    Pass,
    /// At least one selected rule rejects the supplied complete model.
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
    families::builtin_registry()
}
