//! Multi-family routing and domain result projection for `solve_rules`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::time::Instant;

use crate::{
    service::{SolverService, SolverServiceError},
    types::{
        ConstraintDecl, ConstraintItem, SolveConstraintsRequest, SolveConstraintsResponse,
        SolveStatus,
    },
};

use super::{
    compiler::{FamilyCompileError, ModelProjection, RuleAssignment},
    families, CompiledRule, RuleOutcome, RuleSolveMode,
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
    /// Family-neutral model projection metadata.
    pub projections: Vec<ModelProjection>,
}

/// Parses and compiles one family-specific request from the generic MCP envelope.
pub fn prepare(args: Value) -> Result<PreparedRuleSolve, PrepareRulesError> {
    let selector: FamilySelector = serde_json::from_value(args.clone()).map_err(|error| {
        PrepareRulesError::InvalidRequest {
            message: error.to_string(),
        }
    })?;

    let compiler =
        families::compiler(&selector.family).ok_or_else(|| PrepareRulesError::UnknownFamily {
            family: selector.family.clone(),
        })?;
    let compiled = compiler.compile(args)?;

    Ok(PreparedRuleSolve {
        family: selector.family,
        mode: compiled.mode,
        request: compiled.request,
        rules: compiled.rules,
        projections: compiled.projections,
    })
}

/// Executes one prepared rule request and attributes invalid complete models.
pub async fn run(
    service: &SolverService,
    prepared: PreparedRuleSolve,
) -> Result<SolveRulesResponse, SolverServiceError> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(prepared.request.timeout_ms);
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
                let Some(timeout_ms) = remaining_timeout_ms(deadline) else {
                    results.push(RuleResult::new(rule, SolveStatus::Timeout));
                    continue;
                };
                let response = service
                    .solve_constraints(single_rule_request(
                        &prepared.request,
                        &prepared.family,
                        rule,
                        timeout_ms,
                    ))
                    .await?;
                results.push(RuleResult::new(rule, response.status));
            }
            results
        }
        _ => Vec::new(),
    };

    let mut response = finish(prepared, solver, rule_results);
    response.total_duration_ms = elapsed_ms(started);
    Ok(response)
}

fn remaining_timeout_ms(deadline: Instant) -> Option<u64> {
    let now = Instant::now();
    if now >= deadline {
        return None;
    }
    u64::try_from((deadline - now).as_millis())
        .ok()
        .filter(|remaining| *remaining > 0)
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn single_rule_request(
    base: &SolveConstraintsRequest,
    family: &str,
    rule: &CompiledRule,
    timeout_ms: u64,
) -> SolveConstraintsRequest {
    let mut request = base.clone();
    request.constraints = vec![ConstraintItem::Declared(ConstraintDecl {
        id: Some(rule.constraint_id(family)),
        group: None,
        soft: false,
        weight: None,
        expr: rule.predicate.clone(),
    })];
    request.persist = false;
    request.include_smt = false;
    request.timeout_ms = timeout_ms;
    request
}

/// Adds family semantics and scene assignments without changing raw solver status.
#[must_use]
pub fn finish(
    prepared: PreparedRuleSolve,
    solver: SolveConstraintsResponse,
    rule_results: Vec<RuleResult>,
) -> SolveRulesResponse {
    let total_duration_ms = solver.duration_ms;
    let assignments = solver.model.as_ref().map_or_else(Vec::new, |model| {
        families::compiler(&prepared.family).map_or_else(Vec::new, |compiler| {
            compiler.project_model(&prepared.projections, model)
        })
    });

    SolveRulesResponse {
        family: prepared.family,
        mode: prepared.mode,
        outcome: outcome_for(prepared.mode, solver.status),
        assignments,
        rule_results,
        total_duration_ms,
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
    /// Aggregate solve plus serial failure-attribution wall time.
    pub total_duration_ms: u64,
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

#[derive(Deserialize)]
struct FamilySelector {
    family: String,
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
    /// Family-specific validation or lowering failed.
    #[error(transparent)]
    Family(#[from] FamilyCompileError),
}
