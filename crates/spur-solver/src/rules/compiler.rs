//! Family-neutral compilation and model projection contracts.

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::types::{ModelValue, SolveConstraintsRequest, SolveModel};

use super::{CompiledRule, RuleSolveMode};

/// One family compiler behind the shared `solve_rules` tool.
pub trait RuleFamilyCompiler: Send + Sync {
    /// Stable family discriminator.
    fn id(&self) -> &'static str;

    /// Closed JSON Schema branch for this family request.
    fn input_schema(&self) -> Value;

    /// Parses, validates, and lowers one family request.
    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError>;

    /// Projects only caller-declared unknowns from a satisfiable backend model.
    fn project_model(
        &self,
        projections: &[ModelProjection],
        model: &SolveModel,
    ) -> Vec<RuleAssignment> {
        projections
            .iter()
            .filter_map(|projection| {
                model
                    .get(&projection.variable)
                    .cloned()
                    .map(|value| RuleAssignment {
                        node: projection.subject.clone(),
                        field: projection.field.clone(),
                        value,
                    })
            })
            .collect()
    }
}

/// Shared output of every family compiler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FamilyCompilation {
    /// Verification or synthesis semantics.
    pub mode: RuleSolveMode,
    /// Typed request consumed by the generic solver service.
    pub request: SolveConstraintsRequest,
    /// Identity-preserving predicates in caller order.
    pub rules: Vec<CompiledRule>,
    /// Backend-variable to caller-fact projection bindings.
    pub projections: Vec<ModelProjection>,
}

/// One backend variable mapped to a family fact path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelProjection {
    /// Backend model key.
    pub variable: String,
    /// Family subject ID.
    pub subject: String,
    /// Family-specific field path.
    pub field: String,
}

/// One projected assignment returned by `solve_rules`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleAssignment {
    /// Family subject ID. The `node` name is retained for wire compatibility.
    pub node: String,
    /// Family-specific field path.
    pub field: String,
    /// Scalar value returned by the generic model parser.
    pub value: ModelValue,
}

/// Family compilation failure normalized for the shared MCP handler.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{family} rule compilation failed: {message}")]
pub struct FamilyCompileError {
    /// Stable family ID.
    pub family: String,
    /// Deterministic family-specific validation message.
    pub message: String,
}

impl FamilyCompileError {
    /// Wraps one family-specific error without erasing the family ID.
    #[must_use]
    pub fn new(family: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            family: family.into(),
            message: message.into(),
        }
    }
}
