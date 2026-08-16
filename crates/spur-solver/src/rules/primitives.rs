//! Shared typed-IR constructors used by rule-family compilers.

use crate::types::{
    ConstraintDecl, ConstraintExpr, ConstraintItem, ConstraintOp, ObjectivePriority, SessionOp,
    SolveConstraintsRequest, Variable,
};

use super::CompiledRule;

/// Integer literal.
#[must_use]
pub const fn int(value: i64) -> ConstraintExpr {
    ConstraintExpr::Int { value }
}

/// Boolean literal.
#[must_use]
pub const fn boolean(value: bool) -> ConstraintExpr {
    ConstraintExpr::Bool { value }
}

/// Variable reference.
#[must_use]
pub fn var(name: impl Into<String>) -> ConstraintExpr {
    ConstraintExpr::Var { name: name.into() }
}

/// Generic operation.
#[must_use]
pub fn op(operator: ConstraintOp, args: Vec<ConstraintExpr>) -> ConstraintExpr {
    ConstraintExpr::Op { op: operator, args }
}

/// Addition.
#[must_use]
pub fn add(args: Vec<ConstraintExpr>) -> ConstraintExpr {
    op(ConstraintOp::Add, args)
}

/// Subtraction.
#[must_use]
pub fn sub(left: ConstraintExpr, right: ConstraintExpr) -> ConstraintExpr {
    op(ConstraintOp::Sub, vec![left, right])
}

/// Multiplication.
#[must_use]
pub fn mul(args: Vec<ConstraintExpr>) -> ConstraintExpr {
    op(ConstraintOp::Mul, args)
}

/// Equality.
#[must_use]
pub fn eq(left: ConstraintExpr, right: ConstraintExpr) -> ConstraintExpr {
    op(ConstraintOp::Eq, vec![left, right])
}

/// Strict less-than.
#[must_use]
pub fn lt(left: ConstraintExpr, right: ConstraintExpr) -> ConstraintExpr {
    op(ConstraintOp::Lt, vec![left, right])
}

/// Less-than or equal.
#[must_use]
pub fn le(left: ConstraintExpr, right: ConstraintExpr) -> ConstraintExpr {
    op(ConstraintOp::Le, vec![left, right])
}

/// Strict greater-than.
#[must_use]
pub fn gt(left: ConstraintExpr, right: ConstraintExpr) -> ConstraintExpr {
    op(ConstraintOp::Gt, vec![left, right])
}

/// Greater-than or equal.
#[must_use]
pub fn ge(left: ConstraintExpr, right: ConstraintExpr) -> ConstraintExpr {
    op(ConstraintOp::Ge, vec![left, right])
}

/// Boolean conjunction.
#[must_use]
pub fn and(args: Vec<ConstraintExpr>) -> ConstraintExpr {
    op(ConstraintOp::And, args)
}

/// Boolean disjunction.
#[must_use]
pub fn or(args: Vec<ConstraintExpr>) -> ConstraintExpr {
    op(ConstraintOp::Or, args)
}

/// Boolean negation.
#[must_use]
pub fn not(argument: ConstraintExpr) -> ConstraintExpr {
    op(ConstraintOp::Not, vec![argument])
}

/// Builds the generic request shared by family compilers.
#[must_use]
pub fn request(
    family: &str,
    vars: Vec<Variable>,
    rules: &[CompiledRule],
    timeout_ms: u64,
    persist: bool,
    include_smt: bool,
) -> SolveConstraintsRequest {
    SolveConstraintsRequest {
        vars,
        constraints: rules
            .iter()
            .map(|rule| {
                ConstraintItem::Declared(ConstraintDecl {
                    id: Some(rule.constraint_id(family)),
                    soft: false,
                    weight: None,
                    expr: rule.predicate.clone(),
                })
            })
            .collect(),
        objectives: Vec::new(),
        objective_priority: ObjectivePriority::Lex,
        timeout_ms,
        persist,
        include_smt,
        use_cache: true,
        session_id: None,
        session_op: SessionOp::None,
    }
}
