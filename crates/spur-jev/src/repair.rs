//! Diagnostic-driven repair questions and gated compile actions.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use thiserror::Error;

use crate::{
    compile::{compile_bprime, CompileError},
    gate::{AmbiguityReport, DecisionReview, Gate},
    wire::{Answer, Question},
};

/// Question identifier used for the repair choice and its confidence gate.
pub const REPAIR_DECISION: &str = "repair_action";

/// Diagnostic fields that are repeated in every repair question.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepairDiagnostic {
    /// Stable machine-readable diagnostic category.
    pub code: String,
    /// JSON-like path to the rejected expression.
    pub path: String,
    /// Human-readable validation failure.
    pub message: String,
    /// Sort or JSON type observed by validation.
    pub found: Option<String>,
}

/// Deterministic compiler action selected by a gated repair decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompileRuleAction {
    /// Re-run B-prime compilation so a top-level enum label becomes equality.
    WrapEnumEquality,
    /// Remove the rejected constraint.
    DropConstraint,
    /// Replace the rejected expression with a Boolean variable reference.
    ReplaceWithBooleanVariable,
    /// Retain only the rejected enum-label expression.
    KeepEnumLabelOnly,
}

impl CompileRuleAction {
    /// Recompiles a B-prime request when this action is owned by that compiler.
    ///
    /// Only [`Self::WrapEnumEquality`] maps to an existing deterministic
    /// compile rule. Other actions remain explicit for a caller or a later
    /// diagnostic cycle instead of silently falling back to enum wrapping.
    pub fn recompile_bprime(
        self,
        answers: &BTreeMap<String, Answer>,
        rejected_request: &Value,
    ) -> Result<Value, RepairError> {
        if self != Self::WrapEnumEquality {
            return Err(RepairError::UnsupportedCompileAction { action: self });
        }
        compile_bprime(answers, rejected_request).map_err(RepairError::Compile)
    }
}

/// Failures while gating or translating a repair decision.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum RepairError {
    /// The weakest-link confidence gate rejected the repair.
    #[error("repair decision is ambiguous")]
    Ambiguous(AmbiguityReport),
    /// The response was not a choice answer.
    #[error("repair decision must be a choice answer")]
    ExpectedChoice,
    /// The selected candidate has no compile-rule mapping.
    #[error("unknown repair candidate `{candidate}`")]
    UnknownCandidate {
        /// Candidate returned by Jev.
        candidate: String,
    },
    /// The selected action is not owned by the B-prime compiler.
    #[error("repair action `{action:?}` is not supported by B-prime recompilation")]
    UnsupportedCompileAction {
        /// Explicit action that the caller must handle.
        action: CompileRuleAction,
    },
    /// Existing deterministic compilation rejected the request.
    #[error(transparent)]
    Compile(CompileError),
}

/// Builds one deterministic repair question from a solver diagnostic.
///
/// The diagnostic and rejected expression live in structured instructions;
/// each candidate is a literal choice criterion rather than free-form prose.
#[must_use]
pub fn build_repair_battery(
    diagnostic: &RepairDiagnostic,
    rejected_expr: &Value,
    candidates: &[&str],
) -> BTreeMap<String, Question> {
    let criteria = candidates
        .iter()
        .map(|candidate| {
            (
                (*candidate).to_owned(),
                Value::String(candidate_description(candidate)),
            )
        })
        .collect();
    let instructions = json!({
        "task": "Select the compile-rule action that repairs the rejected expression without changing its intended constraint.",
        "diagnostic": {
            "code": diagnostic.code,
            "path": diagnostic.path,
            "message": diagnostic.message,
            "found": diagnostic.found,
        },
        "rejected_expr": rejected_expr,
    });

    BTreeMap::from([(
        REPAIR_DECISION.to_owned(),
        Question::Choice {
            instructions: Some(instructions),
            criteria,
        },
    )])
}

/// Gates one repair answer and maps its chosen candidate to a compile action.
///
/// Low or invalid confidence returns the gate's [`AmbiguityReport`]. No
/// candidate is substituted when the decision is blocked or unknown.
pub fn apply_repair(answer: &Answer) -> Result<CompileRuleAction, RepairError> {
    let Answer::Choice { choice, .. } = answer else {
        return Err(RepairError::ExpectedChoice);
    };
    let answers = BTreeMap::from([(REPAIR_DECISION.to_owned(), answer.clone())]);
    let review = DecisionReview::from_answers(&answers, &[]);
    if let Gate::Blocked(report) = review.gate {
        return Err(RepairError::Ambiguous(report));
    }

    match choice.as_str() {
        "eq_wrap" => Ok(CompileRuleAction::WrapEnumEquality),
        "drop_constraint" => Ok(CompileRuleAction::DropConstraint),
        "bool_var" => Ok(CompileRuleAction::ReplaceWithBooleanVariable),
        "label_only" => Ok(CompileRuleAction::KeepEnumLabelOnly),
        _ => Err(RepairError::UnknownCandidate {
            candidate: choice.clone(),
        }),
    }
}

fn candidate_description(candidate: &str) -> String {
    match candidate {
        "eq_wrap" => "Wrap the enum-label assertion in eq(var, enum_label) so the top-level expression is Boolean.".to_owned(),
        "drop_constraint" => "Remove the rejected constraint from the request.".to_owned(),
        "bool_var" => "Replace the rejected expression with a Boolean variable reference.".to_owned(),
        "label_only" => "Keep the enum-label expression without adding a Boolean wrapper.".to_owned(),
        candidate => format!("Apply the repair candidate named `{candidate}` exactly as supplied."),
    }
}
