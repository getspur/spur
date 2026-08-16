//! Multi-family routing and domain result projection for `solve_rules`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{
    service::{SolverService, SolverServiceError},
    types::{
        ConstraintDecl, ConstraintItem, ModelValue, SolveConstraintsRequest,
        SolveConstraintsResponse, SolveStatus,
    },
};

use super::{
    families::design::{
        compile::{
            compile, CompiledDesignUnknown, DesignCompileError, DesignCompileRequest,
            DesignRuleBinding,
        },
        scene::{DesignScene, DesignUnknown},
    },
    CompiledRule, RuleOutcome, RuleSolveMode,
};

/// A compiled family request ready for the shared solver service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedRuleSolve {
    /// Selected family ID.
    pub family: String,
    /// Verification or synthesis semantics.
    pub mode: RuleSolveMode,
    /// Typed B-prime request.
    pub request: SolveConstraintsRequest,
    /// Identity-preserving predicates used for verification attribution.
    pub rules: Vec<CompiledRule>,
    /// Design-family model projection metadata.
    pub design_unknowns: Vec<CompiledDesignUnknown>,
}

/// Parses and compiles one family-specific request from the generic MCP envelope.
pub fn prepare(args: Value) -> Result<PreparedRuleSolve, PrepareRulesError> {
    let selector: FamilySelector = serde_json::from_value(args.clone()).map_err(|error| {
        PrepareRulesError::InvalidRequest {
            message: error.to_string(),
        }
    })?;

    match selector.family.as_str() {
        "design" => prepare_design(args),
        family => Err(PrepareRulesError::UnknownFamily {
            family: family.to_owned(),
        }),
    }
}

fn prepare_design(args: Value) -> Result<PreparedRuleSolve, PrepareRulesError> {
    let input: DesignToolRequest =
        serde_json::from_value(args).map_err(|error| PrepareRulesError::InvalidRequest {
            message: error.to_string(),
        })?;
    let compiled = compile(DesignCompileRequest {
        mode: input.mode,
        rules: input.rules,
        scene: input.scene,
        unknowns: input.unknowns,
        timeout_ms: input.timeout_ms,
        persist: input.persist,
        include_smt: input.include_smt,
    })?;

    Ok(PreparedRuleSolve {
        family: input.family,
        mode: input.mode,
        request: compiled.request,
        rules: compiled.rules,
        design_unknowns: compiled.unknowns,
    })
}

/// Executes one prepared rule request and attributes invalid complete models.
pub async fn run(
    service: &SolverService,
    prepared: PreparedRuleSolve,
) -> Result<SolveRulesResponse, SolverServiceError> {
    let solver = service.solve_constraints(prepared.request.clone()).await?;
    let rule_results = match (prepared.mode, solver.status) {
        (RuleSolveMode::Verify, SolveStatus::Sat) => prepared
            .rules
            .iter()
            .map(|rule| RuleResult::new(rule, SolveStatus::Sat))
            .collect(),
        (RuleSolveMode::Verify, SolveStatus::Unsat) => {
            let mut results = Vec::with_capacity(prepared.rules.len());
            for rule in &prepared.rules {
                let response = service
                    .solve_constraints(single_rule_request(
                        &prepared.request,
                        &prepared.family,
                        rule,
                    ))
                    .await?;
                results.push(RuleResult::new(rule, response.status));
            }
            results
        }
        _ => Vec::new(),
    };

    Ok(finish(prepared, solver, rule_results))
}

fn single_rule_request(
    base: &SolveConstraintsRequest,
    family: &str,
    rule: &CompiledRule,
) -> SolveConstraintsRequest {
    let mut request = base.clone();
    request.constraints = vec![ConstraintItem::Declared(ConstraintDecl {
        id: Some(rule.constraint_id(family)),
        soft: false,
        weight: None,
        expr: rule.predicate.clone(),
    })];
    request.persist = false;
    request.include_smt = false;
    request
}

/// Adds family semantics and scene assignments without changing raw solver status.
#[must_use]
pub fn finish(
    prepared: PreparedRuleSolve,
    solver: SolveConstraintsResponse,
    rule_results: Vec<RuleResult>,
) -> SolveRulesResponse {
    let assignments = solver
        .model
        .as_ref()
        .map(|model| {
            prepared
                .design_unknowns
                .iter()
                .filter_map(|unknown| {
                    model
                        .get(&unknown.variable)
                        .cloned()
                        .map(|value| RuleAssignment {
                            node: unknown.node.clone(),
                            field: unknown.field,
                            value,
                        })
                })
                .collect()
        })
        .unwrap_or_default();

    SolveRulesResponse {
        family: prepared.family,
        mode: prepared.mode,
        outcome: outcome_for(prepared.mode, solver.status),
        assignments,
        rule_results,
        solver,
    }
}

/// Interprets one status without treating inconclusive states as proof.
#[must_use]
pub const fn outcome_for(mode: RuleSolveMode, status: SolveStatus) -> RuleOutcome {
    match (mode, status) {
        (RuleSolveMode::Verify, SolveStatus::Sat) => RuleOutcome::Pass,
        (RuleSolveMode::Verify, SolveStatus::Unsat) => RuleOutcome::Fail,
        (RuleSolveMode::Synthesize, SolveStatus::Sat) => RuleOutcome::Solution,
        (RuleSolveMode::Synthesize, SolveStatus::Unsat) => RuleOutcome::Infeasible,
        (_, SolveStatus::Unknown) => RuleOutcome::Unknown,
        (_, SolveStatus::Timeout) => RuleOutcome::Timeout,
        (_, SolveStatus::Error) => RuleOutcome::Error,
        (_, SolveStatus::Ended) => RuleOutcome::Ended,
    }
}

/// Generic rule execution response with the original solver envelope flattened.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SolveRulesResponse {
    /// Selected family ID.
    pub family: String,
    /// Requested proof mode.
    pub mode: RuleSolveMode,
    /// Domain interpretation of `status`.
    pub outcome: RuleOutcome,
    /// Family-specific model projection.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assignments: Vec<RuleAssignment>,
    /// Per-binding verification results in caller order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rule_results: Vec<RuleResult>,
    /// Raw solver status, model, duration, persistence, and diagnostics.
    #[serde(flatten)]
    pub solver: SolveConstraintsResponse,
}

/// Solver-backed result for one selected rule binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleResult {
    /// Stable catalog rule ID.
    pub rule_id: String,
    /// Caller-order binding index.
    pub binding_index: usize,
    /// Raw status from the per-rule query, or aggregate `sat` for an implied pass.
    pub status: SolveStatus,
    /// Verification interpretation of `status`.
    pub outcome: RuleOutcome,
}

impl RuleResult {
    fn new(rule: &CompiledRule, status: SolveStatus) -> Self {
        Self {
            rule_id: rule.rule_id.clone(),
            binding_index: rule.binding_index,
            status,
            outcome: outcome_for(RuleSolveMode::Verify, status),
        }
    }
}

/// One solved design geometry field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleAssignment {
    /// Scene node ID.
    pub node: String,
    /// Rectangle field.
    pub field: super::families::design::scene::DesignField,
    /// Scalar value returned by the generic model parser.
    pub value: ModelValue,
}

#[derive(Deserialize)]
struct FamilySelector {
    family: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesignToolRequest {
    family: String,
    mode: RuleSolveMode,
    rules: Vec<DesignRuleBinding>,
    scene: DesignScene,
    #[serde(default)]
    unknowns: Vec<DesignUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

const fn default_timeout_ms() -> u64 {
    crate::types::DEFAULT_TIMEOUT_MS
}

/// Family selection, typed parsing, and compilation errors.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PrepareRulesError {
    /// The generic envelope or family payload did not deserialize.
    #[error("invalid solve_rules request: {message}")]
    InvalidRequest { message: String },
    /// No compiler is registered for the requested family.
    #[error("unknown rule family `{family}`")]
    UnknownFamily { family: String },
    /// Design-family validation or lowering failed.
    #[error(transparent)]
    Design(#[from] DesignCompileError),
}
