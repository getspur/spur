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

    /// Compiles one request and returns optional family-owned proof scope.
    ///
    /// Most families do not need response-level scope metadata. Bounded
    /// analyses can override this hook so the metadata is derived in the same
    /// validated compilation pass as the solver request.
    fn compile_with_evaluation_scope(
        &self,
        input: Value,
    ) -> Result<(FamilyCompilation, Option<RuleEvaluationScope>), FamilyCompileError> {
        self.compile(input).map(|compiled| (compiled, None))
    }

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

/// Finite domain in which a catalog result was established.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleEvaluationScope {
    /// One exact finite workflow trace unrolling.
    BoundedTrace {
        /// Number of transition steps represented by the trace.
        horizon: u64,
        /// Effective bound for every selected reachability binding.
        reachability: Vec<BoundedReachabilityScope>,
    },
}

/// Effective bound for one caller-ordered bounded-reachability binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BoundedReachabilityScope {
    /// Stable catalog rule ID.
    pub rule_id: String,
    /// Caller-order binding index.
    pub binding_index: usize,
    /// Inclusive last state index searched by this binding.
    pub effective_bound: u64,
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
