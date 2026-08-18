//! Solver request and response types.

use std::{
    collections::{hash_map::Entry, BTreeMap, HashMap, HashSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};

/// Default wall-clock budget for a solve, in milliseconds.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Maximum accepted wall-clock budget for a solve, in milliseconds.
pub const MAX_TIMEOUT_MS: u64 = 60_000;
/// Maximum number of variables in one typed solve.
pub const MAX_VARIABLES: usize = 64;
/// Maximum number of top-level constraints in one typed solve.
pub const MAX_CONSTRAINTS: usize = 256;
/// Maximum number of parent-to-child edges in a constraint expression.
pub const MAX_EXPRESSION_DEPTH: usize = 32;
/// Default weight applied to soft constraints when `weight` is omitted.
pub const DEFAULT_SOFT_WEIGHT: i64 = 1;
/// Maximum number of νZ objectives in one typed solve.
pub const MAX_OBJECTIVES: usize = 4;
/// Default number of Pareto solutions collected before the terminal probe.
pub const DEFAULT_MAX_SOLUTIONS: usize = 16;
/// Maximum number of optimization solutions collected in one typed solve.
pub const MAX_SOLUTIONS: usize = 64;
/// Maximum BitVec width accepted by the typed surface.
pub const MAX_BITVEC_WIDTH: u32 = 64;
/// Maximum concurrent incremental sessions per process.
pub const MAX_SOLVE_SESSIONS: usize = 8;
/// Maximum push-frame depth for one incremental session.
pub const MAX_SESSION_FRAMES: usize = 16;

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

const fn default_false() -> bool {
    false
}

const fn default_true() -> bool {
    true
}

const fn default_objective_priority() -> ObjectivePriority {
    ObjectivePriority::Lex
}

const fn default_max_solutions() -> usize {
    DEFAULT_MAX_SOLUTIONS
}

const fn default_session_op() -> SessionOp {
    SessionOp::None
}

/// A declared variable in the B′ constraint language.
///
/// The enum is internally tagged by the JSON `type` field. Each variant
/// serializes its `name` alongside its domain-specific fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Variable {
    /// An unbounded Boolean variable.
    Bool {
        /// Surface name used by constraint expressions and returned models.
        name: String,
    },
    /// An unbounded signed integer variable.
    Int {
        /// Surface name used by constraint expressions and returned models.
        name: String,
    },
    /// A signed integer variable constrained to an inclusive range.
    IntRange {
        /// Surface name used by constraint expressions and returned models.
        name: String,
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
    },
    /// A finite enumeration represented by labels at the API boundary.
    Enum {
        /// Surface name used by constraint expressions and returned models.
        name: String,
        /// Allowed labels.
        values: Vec<String>,
    },
    /// An unbounded real (SMT `Real`) variable.
    Real {
        /// Surface name used by constraint expressions and returned models.
        name: String,
    },
    /// A fixed-width bit-vector variable (`(_ BitVec width)`).
    BitVec {
        /// Surface name used by constraint expressions and returned models.
        name: String,
        /// Bit width in `1..=MAX_BITVEC_WIDTH`.
        width: u32,
    },
}

impl Variable {
    /// Returns the surface name shared by every variable variant.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Bool { name }
            | Self::Int { name }
            | Self::IntRange { name, .. }
            | Self::Enum { name, .. }
            | Self::Real { name }
            | Self::BitVec { name, .. } => name,
        }
    }
}

/// Closed set of operations supported by the B′ constraint language.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintOp {
    /// Equality comparison.
    Eq,
    /// Inequality comparison.
    Ne,
    /// Strictly-less-than comparison.
    Lt,
    /// Less-than-or-equal comparison.
    Le,
    /// Strictly-greater-than comparison.
    Gt,
    /// Greater-than-or-equal comparison.
    Ge,
    /// Integer addition.
    Add,
    /// Integer subtraction.
    Sub,
    /// Integer / real / bit-vector multiplication.
    Mul,
    /// Boolean conjunction.
    And,
    /// Boolean disjunction.
    Or,
    /// Boolean negation.
    Not,
    /// Bit-vector bitwise AND (same width).
    BvAnd,
    /// Bit-vector bitwise OR (same width).
    BvOr,
    /// Bit-vector bitwise XOR (same width).
    BvXor,
    /// Bit-vector bitwise NOT.
    BvNot,
    /// Bit-vector addition (same width).
    BvAdd,
    /// Bit-vector subtraction (same width).
    BvSub,
    /// Bit-vector multiplication (same width).
    BvMul,
    /// Unsigned bit-vector `<`.
    BvUlt,
    /// Unsigned bit-vector `≤`.
    BvUle,
    /// Unsigned bit-vector `>`.
    BvUgt,
    /// Unsigned bit-vector `≥`.
    BvUge,
}

impl ConstraintOp {
    /// Returns the operation's canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::And => "and",
            Self::Or => "or",
            Self::Not => "not",
            Self::BvAnd => "bv_and",
            Self::BvOr => "bv_or",
            Self::BvXor => "bv_xor",
            Self::BvNot => "bv_not",
            Self::BvAdd => "bv_add",
            Self::BvSub => "bv_sub",
            Self::BvMul => "bv_mul",
            Self::BvUlt => "bv_ult",
            Self::BvUle => "bv_ule",
            Self::BvUgt => "bv_ugt",
            Self::BvUge => "bv_uge",
        }
    }

    const fn arity(self) -> Arity {
        match self {
            Self::Eq
            | Self::Ne
            | Self::Lt
            | Self::Le
            | Self::Gt
            | Self::Ge
            | Self::Sub
            | Self::BvAnd
            | Self::BvOr
            | Self::BvXor
            | Self::BvAdd
            | Self::BvSub
            | Self::BvMul
            | Self::BvUlt
            | Self::BvUle
            | Self::BvUgt
            | Self::BvUge => Arity::Exact(2),
            Self::Add | Self::Mul => Arity::AtLeast(2),
            Self::And | Self::Or => Arity::AtLeast(1),
            Self::Not | Self::BvNot => Arity::Exact(1),
        }
    }
}

impl fmt::Display for ConstraintOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One tagged expression node in the B′ constraint language.
///
/// Bare JSON strings, numbers, and booleans are deliberately not accepted:
/// every leaf and operation has an explicit `kind`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConstraintExpr {
    /// Reference to a declared variable.
    Var {
        /// Surface variable name.
        name: String,
    },
    /// Signed 64-bit integer literal.
    Int {
        /// Literal value.
        value: i64,
    },
    /// Boolean literal.
    Bool {
        /// Literal value.
        value: bool,
    },
    /// Label from a declared enum variable's domain.
    EnumLabel {
        /// Enum variable whose domain contains `label`.
        var: String,
        /// Label in the referenced enum variable's `values`.
        label: String,
    },
    /// Rational real literal `num/den` with `den > 0`.
    Real {
        /// Numerator.
        num: i64,
        /// Positive denominator.
        den: i64,
    },
    /// Unsigned bit-vector literal of fixed width.
    Bv {
        /// Bit width in `1..=MAX_BITVEC_WIDTH`.
        width: u32,
        /// Unsigned value; must fit in `width` bits.
        value: u64,
    },
    /// Application of a closed B′ operation.
    Op {
        /// Operation to apply.
        op: ConstraintOp,
        /// Tagged child expressions.
        args: Vec<Self>,
    },
}

/// Multi-objective combination mode for νZ.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectivePriority {
    /// Lexicographic objectives in request order (Z3 default).
    #[default]
    Lex,
    /// Pareto front (Z3 `:opt.priority pareto`).
    Pareto,
    /// Independent box objectives (Z3 `:opt.priority box`).
    Box,
}

impl ObjectivePriority {
    /// SMT-LIB option atom for `:opt.priority`.
    #[must_use]
    pub const fn as_smt(self) -> &'static str {
        match self {
            Self::Lex => "lex",
            Self::Pareto => "pareto",
            Self::Box => "box",
        }
    }
}

/// Incremental session control on a typed solve request.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOp {
    /// Stateless solve (default); may still attach to a session for full re-solve.
    #[default]
    None,
    /// Start a new session with this request's variables and first constraint frame.
    Begin,
    /// Push a new constraint frame onto `session_id` and solve.
    Push,
    /// Pop the newest constraint frame of `session_id` and solve remainder.
    Pop,
    /// Drop `session_id` (no solve required if constraints empty).
    End,
}

/// A top-level constraint entry.
///
/// Bare [`ConstraintExpr`] values remain accepted for backward compatibility
/// (always hard, unnamed). Named and soft constraints use [`ConstraintDecl`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ConstraintItem {
    /// Named and/or soft constraint wrapper.
    Declared(ConstraintDecl),
    /// Legacy bare boolean expression (hard, unnamed).
    Bare(ConstraintExpr),
}

impl ConstraintItem {
    /// Returns the boolean expression carried by this entry.
    #[must_use]
    pub fn expr(&self) -> &ConstraintExpr {
        match self {
            Self::Declared(decl) => &decl.expr,
            Self::Bare(expr) => expr,
        }
    }

    /// Returns the optional surface id used for unsat cores / soft tracking.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Declared(decl) => decl.id.as_deref(),
            Self::Bare(_) => None,
        }
    }

    /// Returns the optional repeatable Z3 soft-objective group.
    #[must_use]
    pub fn group(&self) -> Option<&str> {
        match self {
            Self::Declared(decl) => decl.group.as_deref(),
            Self::Bare(_) => None,
        }
    }

    /// Returns whether this entry is a soft (preference) constraint.
    #[must_use]
    pub fn is_soft(&self) -> bool {
        match self {
            Self::Declared(decl) => decl.soft,
            Self::Bare(_) => false,
        }
    }

    /// Effective soft weight (`DEFAULT_SOFT_WEIGHT` when soft and omitted).
    #[must_use]
    pub fn soft_weight(&self) -> Option<i64> {
        match self {
            Self::Declared(decl) if decl.soft => Some(decl.weight.unwrap_or(DEFAULT_SOFT_WEIGHT)),
            Self::Declared(_) | Self::Bare(_) => None,
        }
    }
}

impl From<ConstraintExpr> for ConstraintItem {
    fn from(expr: ConstraintExpr) -> Self {
        Self::Bare(expr)
    }
}

/// Named and/or soft wrapper around a boolean constraint expression.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConstraintDecl {
    /// Optional unique diagnostic identifier for cores and soft-result rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional repeatable Z3 objective group, valid only for soft constraints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// When true, encode as `assert-soft` (preference), not a hard `assert`.
    #[serde(default = "default_false")]
    pub soft: bool,
    /// Soft weight; defaults to [`DEFAULT_SOFT_WEIGHT`] when `soft` and omitted.
    /// Must be strictly positive when present. Forbidden when `soft` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
    /// Boolean expression that must hold (hard) or is preferred (soft).
    pub expr: ConstraintExpr,
}

/// Direction of a νZ objective over an integer expression.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveOp {
    /// Prefer larger values of the expression among hard-feasible models.
    Maximize,
    /// Prefer smaller values of the expression among hard-feasible models.
    Minimize,
}

impl ObjectiveOp {
    /// Wire / SMT command name (`maximize` / `minimize`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Maximize => "maximize",
            Self::Minimize => "minimize",
        }
    }
}

impl fmt::Display for ObjectiveOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One integer objective for the typed νZ path.
///
/// Objectives are evaluated after hard (and soft) constraints. Multiple
/// objectives are emitted in request order (Z3 lexicographic default).
/// This is *optimized under νZ*, not a proof of unique global optimum over
/// an infinite discrete space unless the domain is fully forced by hard rules.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    /// Maximize or minimize.
    pub op: ObjectiveOp,
    /// Integer, real, or bit-vector expression to optimize.
    pub expr: ConstraintExpr,
}

/// Request for typed B′ constraint model-finding.
///
/// # Examples
///
/// ```
/// use spur_solver::types::{
///     ConstraintExpr, ConstraintOp, SolveConstraintsRequest, Variable, DEFAULT_TIMEOUT_MS,
/// };
///
/// let request = SolveConstraintsRequest {
///     vars: vec![Variable::IntRange {
///         name: "workers".to_owned(),
///         min: 1,
///         max: 16,
///     }],
///     constraints: vec![ConstraintExpr::Op {
///         op: ConstraintOp::Ge,
///         args: vec![
///             ConstraintExpr::Var {
///                 name: "workers".to_owned(),
///             },
///             ConstraintExpr::Int { value: 4 },
///         ],
///     }
///     .into()],
///     objectives: vec![],
///     objective_priority: spur_solver::types::ObjectivePriority::Lex,
///     max_solutions: spur_solver::types::DEFAULT_MAX_SOLUTIONS,
///     timeout_ms: DEFAULT_TIMEOUT_MS,
///     persist: false,
///     include_smt: false,
///     use_cache: true,
///     session_id: None,
///     session_op: spur_solver::types::SessionOp::None,
/// };
///
/// assert!(request.validate().is_ok());
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SolveConstraintsRequest {
    /// Variables available to constraint expressions.
    pub vars: Vec<Variable>,
    /// Boolean constraints (bare expressions, named hard, or soft).
    pub constraints: Vec<ConstraintItem>,
    /// Optional νZ objectives over numeric expressions.
    #[serde(default)]
    pub objectives: Vec<Objective>,
    /// How multiple objectives are combined (lex / pareto / box).
    #[serde(default = "default_objective_priority")]
    pub objective_priority: ObjectivePriority,
    /// Maximum Pareto solutions to collect before a terminal status probe.
    #[serde(default = "default_max_solutions")]
    pub max_solutions: usize,
    /// Wall-clock budget in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Whether the service should persist the solve for later retrieval.
    #[serde(default = "default_false")]
    pub persist: bool,
    /// When true, echo the generated SMT-LIB2 script in the response `smt` field.
    #[serde(default = "default_false")]
    pub include_smt: bool,
    /// When true (default), consult the process-wide request fingerprint cache.
    #[serde(default = "default_true")]
    pub use_cache: bool,
    /// Incremental session identifier (`sess_` + 16 hex) when continuing a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Incremental session control (begin / push / pop / end).
    #[serde(default = "default_session_op")]
    pub session_op: SessionOp,
}

impl SolveConstraintsRequest {
    /// Validates request limits, declarations, expression arity, and type rules.
    ///
    /// Validation is deterministic and runs entirely in-process; it never
    /// invokes Z3.
    ///
    /// # Errors
    ///
    /// Returns the first [`ValidationError`] in request order.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.vars.len() > MAX_VARIABLES {
            return Err(ValidationError::new(
                ValidationErrorKind::TooManyVariables,
                "vars",
                format!(
                    "variable count {} exceeds maximum {MAX_VARIABLES}",
                    self.vars.len()
                ),
            ));
        }
        if self.constraints.len() > MAX_CONSTRAINTS {
            return Err(ValidationError::new(
                ValidationErrorKind::TooManyConstraints,
                "constraints",
                format!(
                    "constraint count {} exceeds maximum {MAX_CONSTRAINTS}",
                    self.constraints.len()
                ),
            ));
        }
        if self.objectives.len() > MAX_OBJECTIVES {
            return Err(ValidationError::new(
                ValidationErrorKind::TooManyObjectives,
                "objectives",
                format!(
                    "objective count {} exceeds maximum {MAX_OBJECTIVES}",
                    self.objectives.len()
                ),
            ));
        }
        if self.max_solutions == 0 || self.max_solutions > MAX_SOLUTIONS {
            return Err(ValidationError::new(
                ValidationErrorKind::InvalidMaxSolutions,
                "max_solutions",
                format!(
                    "max_solutions must be in 1..={MAX_SOLUTIONS}, found {}",
                    self.max_solutions
                ),
            ));
        }
        if self.timeout_ms > MAX_TIMEOUT_MS {
            return Err(ValidationError::new(
                ValidationErrorKind::TimeoutTooLarge,
                "timeout_ms",
                format!(
                    "timeout {} ms exceeds maximum {MAX_TIMEOUT_MS} ms",
                    self.timeout_ms
                ),
            ));
        }

        let variables = VariableTable::build(&self.vars)?;
        let mut seen_ids = HashSet::new();
        let mut soft_group_weights: HashMap<Option<&str>, i64> = HashMap::new();
        for (constraint_index, constraint) in self.constraints.iter().enumerate() {
            if let ConstraintItem::Declared(decl) = constraint {
                if let Some(id) = decl.id.as_deref() {
                    if !is_valid_surface_name(id) {
                        return Err(ValidationError::new(
                            ValidationErrorKind::InvalidConstraintId,
                            format!("constraints[{constraint_index}].id"),
                            format!("constraint id {id:?} must match [A-Za-z_][A-Za-z0-9_]*"),
                        ));
                    }
                    if !seen_ids.insert(id) {
                        return Err(ValidationError::new(
                            ValidationErrorKind::DuplicateConstraintId,
                            format!("constraints[{constraint_index}].id"),
                            format!("constraint id {id:?} is declared more than once"),
                        ));
                    }
                }
                if let Some(group) = decl.group.as_deref() {
                    if !is_valid_surface_name(group) {
                        return Err(ValidationError::new(
                            ValidationErrorKind::InvalidConstraintGroup,
                            format!("constraints[{constraint_index}].group"),
                            format!("constraint group {group:?} must match [A-Za-z_][A-Za-z0-9_]*"),
                        ));
                    }
                    if !decl.soft {
                        return Err(ValidationError::new(
                            ValidationErrorKind::GroupWithoutSoft,
                            format!("constraints[{constraint_index}].group"),
                            "group is only valid when soft is true",
                        ));
                    }
                }
                if decl.soft {
                    if let Some(weight) = decl.weight {
                        if weight <= 0 {
                            return Err(ValidationError::new(
                                ValidationErrorKind::InvalidSoftWeight,
                                format!("constraints[{constraint_index}].weight"),
                                format!("soft constraint weight must be > 0, found {weight}"),
                            ));
                        }
                    }
                    let weight = decl.weight.unwrap_or(DEFAULT_SOFT_WEIGHT);
                    let total = soft_group_weights.entry(decl.group.as_deref()).or_default();
                    *total = total.checked_add(weight).ok_or_else(|| {
                        let group = decl.group.as_deref().unwrap_or("<anonymous>");
                        ValidationError::new(
                            ValidationErrorKind::SoftGroupWeightOverflow,
                            format!("constraints[{constraint_index}].weight"),
                            format!(
                                "soft objective group {group:?} total exceeds signed 64-bit range"
                            ),
                        )
                    })?;
                } else if decl.weight.is_some() {
                    return Err(ValidationError::new(
                        ValidationErrorKind::WeightWithoutSoft,
                        format!("constraints[{constraint_index}].weight"),
                        "weight is only valid when soft is true",
                    ));
                }
            }

            let mut child_path = Vec::new();
            let sort = infer_expression(
                constraint.expr(),
                &variables,
                constraint_index,
                0,
                &mut child_path,
            )?;
            if !matches!(sort, ExpressionSort::Bool(_)) {
                return Err(ValidationError::new(
                    ValidationErrorKind::TopLevelNotBoolean,
                    expression_path(constraint_index, &child_path),
                    format!(
                        "top-level constraint must be Bool, found {}",
                        sort.description()
                    ),
                ));
            }
        }

        for (objective_index, objective) in self.objectives.iter().enumerate() {
            let mut child_path = Vec::new();
            let sort = match infer_expression(
                &objective.expr,
                &variables,
                objective_index,
                0,
                &mut child_path,
            ) {
                Ok(sort) => sort,
                Err(error) => {
                    return Err(ValidationError::new(
                        error.kind,
                        rewrite_objective_path(objective_index, &error.path),
                        error.message,
                    ));
                }
            };
            if !matches!(
                sort,
                ExpressionSort::Int | ExpressionSort::Real | ExpressionSort::BitVec(_)
            ) {
                return Err(ValidationError::new(
                    ValidationErrorKind::ObjectiveNotNumeric,
                    format!("objectives[{objective_index}].expr"),
                    format!(
                        "objective expression must be Int, Real, or BitVec, found {}",
                        sort.description()
                    ),
                ));
            }
        }

        if matches!(
            self.session_op,
            SessionOp::Push | SessionOp::Pop | SessionOp::End
        ) && self.session_id.is_none()
        {
            return Err(ValidationError::new(
                ValidationErrorKind::MissingSessionId,
                "session_id",
                "session_op push/pop/end requires session_id",
            ));
        }
        if let Some(session_id) = self.session_id.as_deref() {
            if !is_valid_session_id(session_id) {
                return Err(ValidationError::new(
                    ValidationErrorKind::InvalidSessionId,
                    "session_id",
                    format!(
                        "session_id {session_id:?} must match sess_ followed by 16 lowercase hex digits"
                    ),
                ));
            }
        }

        Ok(())
    }

    /// Returns true when any constraint is soft (preference / assert-soft).
    #[must_use]
    pub fn has_soft_constraints(&self) -> bool {
        self.constraints.iter().any(ConstraintItem::is_soft)
    }

    /// Returns true when any hard constraint carries a surface id (unsat cores).
    #[must_use]
    pub fn has_named_hard_constraints(&self) -> bool {
        self.constraints
            .iter()
            .any(|item| !item.is_soft() && item.id().is_some())
    }

    /// Returns true when the request declares one or more νZ objectives.
    #[must_use]
    pub fn has_objectives(&self) -> bool {
        !self.objectives.is_empty()
    }

    /// Unsat cores are available only for pure named-hard feasibility queries.
    ///
    /// Soft constraints and νZ objectives use Z3's optimize path, which does
    /// not combine with `produce-unsat-cores` in one call.
    #[must_use]
    pub fn wants_unsat_cores(&self) -> bool {
        self.has_named_hard_constraints() && !self.has_soft_constraints() && !self.has_objectives()
    }
}

/// Request for raw SMT-LIB2 model-finding.
///
/// The raw script bypasses B′ expression validation, but
/// [`crate::smt_gate`] still enforces the script byte cap and top-level command
/// allowlist before any subprocess is invoked.
///
/// # Examples
///
/// ```
/// use spur_solver::types::{SolveSmtRequest, DEFAULT_TIMEOUT_MS};
///
/// let request = SolveSmtRequest {
///     smt_lib: "(declare-const answer Int)\n(check-sat)\n".to_owned(),
///     timeout_ms: DEFAULT_TIMEOUT_MS,
///     persist: false,
///     include_smt: false,
/// };
///
/// assert!(request.smt_lib.contains("check-sat"));
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SolveSmtRequest {
    /// Complete SMT-LIB2 script passed to the fixed solver stdin.
    pub smt_lib: String,
    /// Wall-clock budget in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Whether the service should persist the solve for later retrieval.
    #[serde(default = "default_false")]
    pub persist: bool,
    /// When true, echo the submitted SMT-LIB2 script in the response `smt` field.
    #[serde(default = "default_false")]
    pub include_smt: bool,
}

/// Status reported by the solver service.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SolveStatus {
    /// The constraints are satisfiable and a model is present.
    Sat,
    /// The constraints are unsatisfiable.
    Unsat,
    /// The solver could not determine satisfiability.
    Unknown,
    /// The shared wall-clock budget expired.
    Timeout,
    /// Validation, process, output, or parsing failed.
    Error,
    /// An incremental session was closed (`session_op: end`); no model is present.
    ///
    /// Agents must not treat this as a feasible assignment. Check
    /// `session_id` + reason `"session ended"`.
    Ended,
}

/// A scalar value returned for one surface variable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ModelValue {
    /// Boolean model value.
    Bool(bool),
    /// Signed integer model value.
    Int(i64),
    /// Enum label, real decimal/fraction text, bit-vector text, or opaque SMT value.
    Enum(String),
}

/// Surface-name-to-value mapping returned for a satisfiable solve.
pub type SolveModel = BTreeMap<String, ModelValue>;

/// Lossless classification of one Z3 Optimize objective bound.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectiveBound {
    /// A closed, finite optimum.
    Finite {
        /// Exact SMT arithmetic text returned by Z3.
        exact: String,
    },
    /// An unbounded optimum containing positive or negative `oo`.
    Infinite {
        /// Exact SMT arithmetic text returned by Z3.
        exact: String,
    },
    /// An open optimum containing an infinitesimal `epsilon` term.
    Strict {
        /// Exact SMT arithmetic text returned by Z3.
        exact: String,
    },
}

/// Value and exact bound for one explicit objective.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveResult {
    /// Objective direction, unknown for raw SMT objective output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<ObjectiveOp>,
    /// Objective expression value in this solution's model.
    ///
    /// Raw `get-objectives` output omits this field because it reports bounds,
    /// not model evaluations. Typed solves always populate it from `get-value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<ModelValue>,
    /// Exact finite, infinite, or strict optimum reported by Z3.
    pub bound: ObjectiveBound,
}

/// Satisfaction diagnostics for one declared soft constraint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SoftConstraintResult {
    /// Constraint position in the request.
    pub index: usize,
    /// Unique diagnostic identifier, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Repeatable Z3 soft-objective group, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Effective positive weight, including the default weight of one.
    pub weight: i64,
    /// Whether this solution satisfies the soft expression.
    pub satisfied: bool,
}

/// Aggregate satisfied and violated weights for one soft-objective group.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SoftGroupResult {
    /// Repeatable group name, or `None` for the aggregate anonymous group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Total weight of satisfied soft constraints in this group.
    pub satisfied_weight: i64,
    /// Total weight of violated soft constraints in this group.
    pub violated_weight: i64,
}

/// One model and its exact optimization diagnostics.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationSolution {
    /// Concrete variable model for this optimization point.
    pub model: SolveModel,
    /// Explicit objective results in request order.
    #[serde(default)]
    pub objectives: Vec<ObjectiveResult>,
    /// Soft-constraint diagnostics in request declaration order.
    #[serde(default)]
    pub soft_constraints: Vec<SoftConstraintResult>,
    /// Soft-group totals in first-declaration order.
    #[serde(default)]
    pub groups: Vec<SoftGroupResult>,
}

/// Why optimization enumeration stopped.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationTermination {
    /// Z3 reported that enumeration was complete.
    Complete,
    /// Pareto enumeration reached the request's `max_solutions` bound.
    SolutionLimit,
    /// Z3 returned unknown after at least one solution.
    Unknown,
}

/// Additive typed results for a satisfiable optimization request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationResult {
    /// Objective priority, unknown for raw SMT objective output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<ObjectivePriority>,
    /// One or more model points in solver-returned order.
    pub solutions: Vec<OptimizationSolution>,
    /// Why solution collection stopped.
    pub termination: OptimizationTermination,
}

/// Result envelope returned by `solve_constraints`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SolveConstraintsResponse {
    /// Solver status.
    pub status: SolveStatus,
    /// Concrete model, present if and only if `status` is [`SolveStatus::Sat`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<SolveModel>,
    /// End-to-end solve duration in milliseconds.
    pub duration_ms: u64,
    /// Persisted result identifier, when persistence was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solve_id: Option<String>,
    /// Human-readable diagnostic for non-sat or error outcomes.
    pub reason: Option<String>,
    /// Optional generated or submitted SMT-LIB debug output (`include_smt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smt: Option<String>,
    /// Surface ids of a minimal hard unsat core, when available.
    ///
    /// Present only for [`SolveStatus::Unsat`] when the request used named hard
    /// constraints and no soft/objectives (Z3 cannot combine cores with optimize).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsat_core: Option<Vec<String>>,
    /// True when served from the process-wide request fingerprint cache.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached: bool,
    /// Incremental session identifier when begin/push/pop produced or used one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Exact typed optimization results for satisfiable optimize requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimization: Option<OptimizationResult>,
    /// Solver version string probed from the runner, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver_version: Option<String>,
}

impl SolveConstraintsResponse {
    /// Checks invariants on a solver response envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationErrorKind::ResponseModelMismatch`] unless a model
    /// is present exactly when the response status is [`SolveStatus::Sat`].
    /// Returns [`ValidationErrorKind::ResponseCoreMismatch`] when `unsat_core`
    /// is present for a non-unsat status.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let model_is_valid = match self.status {
            SolveStatus::Sat => self.model.is_some(),
            SolveStatus::Unsat
            | SolveStatus::Unknown
            | SolveStatus::Timeout
            | SolveStatus::Error
            | SolveStatus::Ended => self.model.is_none(),
        };
        if !model_is_valid {
            return Err(ValidationError::new(
                ValidationErrorKind::ResponseModelMismatch,
                "model",
                "model must be present if and only if status is sat",
            ));
        }
        if self.unsat_core.is_some() && self.status != SolveStatus::Unsat {
            return Err(ValidationError::new(
                ValidationErrorKind::ResponseCoreMismatch,
                "unsat_core",
                "unsat_core may only be present when status is unsat",
            ));
        }
        if let Some(optimization) = &self.optimization {
            if self.status != SolveStatus::Sat {
                return Err(ValidationError::new(
                    ValidationErrorKind::ResponseOptimizationMismatch,
                    "optimization",
                    "optimization may only be present when status is sat",
                ));
            }
            let Some(first_solution) = optimization.solutions.first() else {
                return Err(ValidationError::new(
                    ValidationErrorKind::ResponseOptimizationMismatch,
                    "optimization.solutions",
                    "optimization must contain at least one solution",
                ));
            };
            if self.model.as_ref() != Some(&first_solution.model) {
                return Err(ValidationError::new(
                    ValidationErrorKind::ResponseOptimizationMismatch,
                    "optimization.solutions[0].model",
                    "the first optimization model must equal the top-level model",
                ));
            }
        }

        Ok(())
    }
}

/// Stable category for a typed-request or response validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationErrorKind {
    /// The request declares more than [`MAX_VARIABLES`] variables.
    TooManyVariables,
    /// The request contains more than [`MAX_CONSTRAINTS`] constraints.
    TooManyConstraints,
    /// The request's timeout exceeds [`MAX_TIMEOUT_MS`].
    TimeoutTooLarge,
    /// A declared surface name violates `[A-Za-z_][A-Za-z0-9_]*`.
    InvalidVariableName,
    /// Two declarations use the same surface name.
    DuplicateVariableName,
    /// An integer range has `min > max`.
    InvalidIntegerRange,
    /// An enum declaration has no labels.
    EmptyEnum,
    /// An enum declaration repeats a label.
    DuplicateEnumLabel,
    /// An expression exceeds [`MAX_EXPRESSION_DEPTH`].
    ExpressionTooDeep,
    /// An expression refers to an undeclared variable.
    UnknownVariable,
    /// An `enum_label` node refers to a non-enum variable.
    ExpectedEnumVariable,
    /// An `enum_label` node names a label outside the referenced enum domain.
    UnknownEnumLabel,
    /// An operation has too few or too many arguments.
    WrongArity,
    /// Operation operands do not satisfy the B′ type rules.
    TypeMismatch,
    /// A top-level constraint is not Boolean-sorted.
    TopLevelNotBoolean,
    /// A constraint id violates `[A-Za-z_][A-Za-z0-9_]*`.
    InvalidConstraintId,
    /// A soft group violates `[A-Za-z_][A-Za-z0-9_]*`.
    InvalidConstraintGroup,
    /// Two constraint declarations share the same surface id.
    DuplicateConstraintId,
    /// A soft constraint weight is missing positivity.
    InvalidSoftWeight,
    /// A soft objective group's aggregate weight exceeds signed 64-bit range.
    SoftGroupWeightOverflow,
    /// `weight` was set on a non-soft constraint.
    WeightWithoutSoft,
    /// `group` was set on a non-soft constraint.
    GroupWithoutSoft,
    /// More than [`MAX_OBJECTIVES`] objectives were declared.
    TooManyObjectives,
    /// `max_solutions` is zero or exceeds [`MAX_SOLUTIONS`].
    InvalidMaxSolutions,
    /// An objective expression is not Int/Real/BitVec.
    ObjectiveNotNumeric,
    /// BitVec width is zero or exceeds [`MAX_BITVEC_WIDTH`].
    InvalidBitVecWidth,
    /// BitVec literal value does not fit in its width.
    BitVecValueTooWide,
    /// Real literal has non-positive denominator.
    InvalidRealLiteral,
    /// Session op requires a session_id that was missing.
    MissingSessionId,
    /// Session id fails the pinned wire format.
    InvalidSessionId,
    /// A response's model presence does not match its status.
    ResponseModelMismatch,
    /// A response's unsat core presence does not match its status.
    ResponseCoreMismatch,
    /// A response's optimization envelope is inconsistent with its status/model.
    ResponseOptimizationMismatch,
}

impl fmt::Display for ValidationErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::TooManyVariables => "too_many_variables",
            Self::TooManyConstraints => "too_many_constraints",
            Self::TimeoutTooLarge => "timeout_too_large",
            Self::InvalidVariableName => "invalid_variable_name",
            Self::DuplicateVariableName => "duplicate_variable_name",
            Self::InvalidIntegerRange => "invalid_integer_range",
            Self::EmptyEnum => "empty_enum",
            Self::DuplicateEnumLabel => "duplicate_enum_label",
            Self::ExpressionTooDeep => "expression_too_deep",
            Self::UnknownVariable => "unknown_variable",
            Self::ExpectedEnumVariable => "expected_enum_variable",
            Self::UnknownEnumLabel => "unknown_enum_label",
            Self::WrongArity => "wrong_arity",
            Self::TypeMismatch => "type_mismatch",
            Self::TopLevelNotBoolean => "top_level_not_boolean",
            Self::InvalidConstraintId => "invalid_constraint_id",
            Self::InvalidConstraintGroup => "invalid_constraint_group",
            Self::DuplicateConstraintId => "duplicate_constraint_id",
            Self::InvalidSoftWeight => "invalid_soft_weight",
            Self::SoftGroupWeightOverflow => "soft_group_weight_overflow",
            Self::WeightWithoutSoft => "weight_without_soft",
            Self::GroupWithoutSoft => "group_without_soft",
            Self::TooManyObjectives => "too_many_objectives",
            Self::InvalidMaxSolutions => "invalid_max_solutions",
            Self::ObjectiveNotNumeric => "objective_not_numeric",
            Self::InvalidBitVecWidth => "invalid_bitvec_width",
            Self::BitVecValueTooWide => "bitvec_value_too_wide",
            Self::InvalidRealLiteral => "invalid_real_literal",
            Self::MissingSessionId => "missing_session_id",
            Self::InvalidSessionId => "invalid_session_id",
            Self::ResponseModelMismatch => "response_model_mismatch",
            Self::ResponseCoreMismatch => "response_core_mismatch",
            Self::ResponseOptimizationMismatch => "response_optimization_mismatch",
        };
        formatter.write_str(name)
    }
}

/// One structured validation failure with a JSON-like request path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    /// Stable error category.
    pub kind: ValidationErrorKind,
    /// JSON-like path to the invalid value.
    pub path: String,
    /// Human-readable explanation.
    pub message: String,
}

impl ValidationError {
    fn new(kind: ValidationErrorKind, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.kind, self.path, self.message
        )
    }
}

impl Error for ValidationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Arity {
    Exact(usize),
    AtLeast(usize),
}

impl Arity {
    const fn accepts(self, actual: usize) -> bool {
        match self {
            Self::Exact(expected) => actual == expected,
            Self::AtLeast(minimum) => actual >= minimum,
        }
    }

    fn describe(self) -> String {
        match self {
            Self::Exact(expected) => expected.to_string(),
            Self::AtLeast(minimum) => format!("at least {minimum}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VariableSort {
    Int,
    Bool,
    Enum(usize),
    Real,
    BitVec(u32),
}

#[derive(Clone, Copy)]
struct VariableInfo<'a> {
    declaration: &'a Variable,
    sort: VariableSort,
}

struct VariableTable<'a> {
    by_name: HashMap<&'a str, VariableInfo<'a>>,
}

impl<'a> VariableTable<'a> {
    fn build(variables: &'a [Variable]) -> Result<Self, ValidationError> {
        let mut by_name = HashMap::with_capacity(variables.len());
        let mut enum_domains: HashMap<Vec<&str>, usize> = HashMap::new();

        for (index, variable) in variables.iter().enumerate() {
            let name = variable.name();
            if !is_valid_surface_name(name) {
                return Err(ValidationError::new(
                    ValidationErrorKind::InvalidVariableName,
                    format!("vars[{index}].name"),
                    format!("variable name {name:?} must match [A-Za-z_][A-Za-z0-9_]*"),
                ));
            }
            if by_name.contains_key(name) {
                return Err(ValidationError::new(
                    ValidationErrorKind::DuplicateVariableName,
                    format!("vars[{index}].name"),
                    format!("variable name {name:?} is declared more than once"),
                ));
            }

            let sort = match variable {
                Variable::Bool { .. } => VariableSort::Bool,
                Variable::Int { .. } => VariableSort::Int,
                Variable::Real { .. } => VariableSort::Real,
                Variable::BitVec { width, .. } => {
                    if *width == 0 || *width > MAX_BITVEC_WIDTH {
                        return Err(ValidationError::new(
                            ValidationErrorKind::InvalidBitVecWidth,
                            format!("vars[{index}].width"),
                            format!(
                                "bitvec width must be in 1..={MAX_BITVEC_WIDTH}, found {width}"
                            ),
                        ));
                    }
                    VariableSort::BitVec(*width)
                }
                Variable::IntRange { min, max, .. } => {
                    if min > max {
                        return Err(ValidationError::new(
                            ValidationErrorKind::InvalidIntegerRange,
                            format!("vars[{index}]"),
                            format!("integer range requires min <= max, found {min} > {max}"),
                        ));
                    }
                    VariableSort::Int
                }
                Variable::Enum { values, .. } => {
                    if values.is_empty() {
                        return Err(ValidationError::new(
                            ValidationErrorKind::EmptyEnum,
                            format!("vars[{index}].values"),
                            "enum values must be non-empty",
                        ));
                    }

                    let mut unique_values = HashSet::with_capacity(values.len());
                    for (value_index, value) in values.iter().enumerate() {
                        if !unique_values.insert(value.as_str()) {
                            return Err(ValidationError::new(
                                ValidationErrorKind::DuplicateEnumLabel,
                                format!("vars[{index}].values[{value_index}]"),
                                format!("enum label {value:?} is repeated"),
                            ));
                        }
                    }

                    let mut canonical_domain: Vec<&str> =
                        values.iter().map(String::as_str).collect();
                    canonical_domain.sort_unstable();
                    let next_domain_id = enum_domains.len();
                    let domain_id = match enum_domains.entry(canonical_domain) {
                        Entry::Occupied(entry) => *entry.get(),
                        Entry::Vacant(entry) => {
                            entry.insert(next_domain_id);
                            next_domain_id
                        }
                    };
                    VariableSort::Enum(domain_id)
                }
            };

            by_name.insert(
                name,
                VariableInfo {
                    declaration: variable,
                    sort,
                },
            );
        }

        Ok(Self { by_name })
    }

    fn get(&self, name: &str) -> Option<VariableInfo<'a>> {
        self.by_name.get(name).copied()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoolOrigin {
    Literal,
    Variable,
    Compound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpressionSort {
    Int,
    Bool(BoolOrigin),
    Enum(usize),
    Real,
    BitVec(u32),
}

impl ExpressionSort {
    const fn description(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::Bool(_) => "Bool",
            Self::Enum(_) => "Enum",
            Self::Real => "Real",
            Self::BitVec(_) => "BitVec",
        }
    }
}

fn is_valid_surface_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_valid_session_id(session_id: &str) -> bool {
    let Some(hex) = session_id.strip_prefix("sess_") else {
        return false;
    };
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn bitvec_value_fits(width: u32, value: u64) -> bool {
    if width == 0 || width > 64 {
        return false;
    }
    if width == 64 {
        return true;
    }
    value < (1_u64 << width)
}

fn infer_expression(
    expression: &ConstraintExpr,
    variables: &VariableTable<'_>,
    constraint_index: usize,
    depth: usize,
    child_path: &mut Vec<usize>,
) -> Result<ExpressionSort, ValidationError> {
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(ValidationError::new(
            ValidationErrorKind::ExpressionTooDeep,
            expression_path(constraint_index, child_path),
            format!("expression nesting exceeds maximum {MAX_EXPRESSION_DEPTH}"),
        ));
    }

    match expression {
        ConstraintExpr::Var { name } => {
            let info = variables.get(name).ok_or_else(|| {
                ValidationError::new(
                    ValidationErrorKind::UnknownVariable,
                    expression_field_path(constraint_index, child_path, "name"),
                    format!("variable {name:?} is not declared"),
                )
            })?;
            Ok(match info.sort {
                VariableSort::Int => ExpressionSort::Int,
                VariableSort::Bool => ExpressionSort::Bool(BoolOrigin::Variable),
                VariableSort::Enum(domain_id) => ExpressionSort::Enum(domain_id),
                VariableSort::Real => ExpressionSort::Real,
                VariableSort::BitVec(width) => ExpressionSort::BitVec(width),
            })
        }
        ConstraintExpr::Int { .. } => Ok(ExpressionSort::Int),
        ConstraintExpr::Bool { .. } => Ok(ExpressionSort::Bool(BoolOrigin::Literal)),
        ConstraintExpr::Real { den, .. } => {
            if *den <= 0 {
                return Err(ValidationError::new(
                    ValidationErrorKind::InvalidRealLiteral,
                    expression_field_path(constraint_index, child_path, "den"),
                    format!("real denominator must be > 0, found {den}"),
                ));
            }
            Ok(ExpressionSort::Real)
        }
        ConstraintExpr::Bv { width, value } => {
            if *width == 0 || *width > MAX_BITVEC_WIDTH {
                return Err(ValidationError::new(
                    ValidationErrorKind::InvalidBitVecWidth,
                    expression_field_path(constraint_index, child_path, "width"),
                    format!("bitvec width must be in 1..={MAX_BITVEC_WIDTH}, found {width}"),
                ));
            }
            if !bitvec_value_fits(*width, *value) {
                return Err(ValidationError::new(
                    ValidationErrorKind::BitVecValueTooWide,
                    expression_field_path(constraint_index, child_path, "value"),
                    format!("bitvec value {value} does not fit in {width} bits"),
                ));
            }
            Ok(ExpressionSort::BitVec(*width))
        }
        ConstraintExpr::EnumLabel { var, label } => {
            let info = variables.get(var).ok_or_else(|| {
                ValidationError::new(
                    ValidationErrorKind::UnknownVariable,
                    expression_field_path(constraint_index, child_path, "var"),
                    format!("variable {var:?} is not declared"),
                )
            })?;
            let Variable::Enum { values, .. } = info.declaration else {
                return Err(ValidationError::new(
                    ValidationErrorKind::ExpectedEnumVariable,
                    expression_field_path(constraint_index, child_path, "var"),
                    format!("variable {var:?} is not an enum"),
                ));
            };
            if !values.iter().any(|value| value == label) {
                return Err(ValidationError::new(
                    ValidationErrorKind::UnknownEnumLabel,
                    expression_field_path(constraint_index, child_path, "label"),
                    format!("label {label:?} is not in enum variable {var:?}"),
                ));
            }
            let VariableSort::Enum(domain_id) = info.sort else {
                return Err(ValidationError::new(
                    ValidationErrorKind::ExpectedEnumVariable,
                    expression_field_path(constraint_index, child_path, "var"),
                    format!("variable {var:?} is not an enum"),
                ));
            };
            Ok(ExpressionSort::Enum(domain_id))
        }
        ConstraintExpr::Op { op, args } => {
            infer_operation(*op, args, variables, constraint_index, depth, child_path)
        }
    }
}

fn infer_operation(
    op: ConstraintOp,
    args: &[ConstraintExpr],
    variables: &VariableTable<'_>,
    constraint_index: usize,
    depth: usize,
    child_path: &mut Vec<usize>,
) -> Result<ExpressionSort, ValidationError> {
    let arity = op.arity();
    if !arity.accepts(args.len()) {
        return Err(ValidationError::new(
            ValidationErrorKind::WrongArity,
            expression_field_path(constraint_index, child_path, "args"),
            format!(
                "operation {op} expects {} arguments, found {}",
                arity.describe(),
                args.len()
            ),
        ));
    }

    match op {
        ConstraintOp::Eq | ConstraintOp::Ne => {
            let left = infer_child(&args[0], 0, variables, constraint_index, depth, child_path)?;
            let right = infer_child(&args[1], 1, variables, constraint_index, depth, child_path)?;
            if equality_is_valid(left, right) {
                Ok(ExpressionSort::Bool(BoolOrigin::Compound))
            } else {
                Err(type_mismatch(
                    constraint_index,
                    child_path,
                    format!(
                        "operation {op} cannot compare {} with {}",
                        left.description(),
                        right.description()
                    ),
                ))
            }
        }
        ConstraintOp::Lt | ConstraintOp::Le | ConstraintOp::Gt | ConstraintOp::Ge => {
            let left = infer_child(&args[0], 0, variables, constraint_index, depth, child_path)?;
            let right = infer_child(&args[1], 1, variables, constraint_index, depth, child_path)?;
            if ordered_numeric_pair(left, right) {
                Ok(ExpressionSort::Bool(BoolOrigin::Compound))
            } else {
                Err(type_mismatch(
                    constraint_index,
                    child_path,
                    format!(
                        "operation {op} requires homogeneous Int or Real operands, found {} and {}",
                        left.description(),
                        right.description()
                    ),
                ))
            }
        }
        ConstraintOp::Add | ConstraintOp::Sub | ConstraintOp::Mul => {
            let first = infer_child(&args[0], 0, variables, constraint_index, depth, child_path)?;
            if !matches!(first, ExpressionSort::Int | ExpressionSort::Real) {
                return Err(type_mismatch(
                    constraint_index,
                    child_path,
                    format!(
                        "operation {op} requires Int or Real operands, found {}",
                        first.description()
                    ),
                ));
            }
            for (child_index, argument) in args.iter().enumerate().skip(1) {
                let sort = infer_child(
                    argument,
                    child_index,
                    variables,
                    constraint_index,
                    depth,
                    child_path,
                )?;
                if sort != first {
                    return Err(type_mismatch(
                        constraint_index,
                        child_path,
                        format!(
                            "operation {op} requires homogeneous operands, found {} and {}",
                            first.description(),
                            sort.description()
                        ),
                    ));
                }
            }
            Ok(first)
        }
        ConstraintOp::And | ConstraintOp::Or | ConstraintOp::Not => {
            require_all_args(
                op,
                args,
                ExpressionClass::Bool,
                variables,
                constraint_index,
                depth,
                child_path,
            )?;
            Ok(ExpressionSort::Bool(BoolOrigin::Compound))
        }
        ConstraintOp::BvNot => {
            let sort = infer_child(&args[0], 0, variables, constraint_index, depth, child_path)?;
            match sort {
                ExpressionSort::BitVec(width) => Ok(ExpressionSort::BitVec(width)),
                other => Err(type_mismatch(
                    constraint_index,
                    child_path,
                    format!("bv_not requires BitVec, found {}", other.description()),
                )),
            }
        }
        ConstraintOp::BvAnd
        | ConstraintOp::BvOr
        | ConstraintOp::BvXor
        | ConstraintOp::BvAdd
        | ConstraintOp::BvSub
        | ConstraintOp::BvMul => {
            let left = infer_child(&args[0], 0, variables, constraint_index, depth, child_path)?;
            let right = infer_child(&args[1], 1, variables, constraint_index, depth, child_path)?;
            match (left, right) {
                (ExpressionSort::BitVec(w1), ExpressionSort::BitVec(w2)) if w1 == w2 => {
                    Ok(ExpressionSort::BitVec(w1))
                }
                _ => Err(type_mismatch(
                    constraint_index,
                    child_path,
                    format!(
                        "operation {op} requires same-width BitVec operands, found {} and {}",
                        left.description(),
                        right.description()
                    ),
                )),
            }
        }
        ConstraintOp::BvUlt | ConstraintOp::BvUle | ConstraintOp::BvUgt | ConstraintOp::BvUge => {
            let left = infer_child(&args[0], 0, variables, constraint_index, depth, child_path)?;
            let right = infer_child(&args[1], 1, variables, constraint_index, depth, child_path)?;
            match (left, right) {
                (ExpressionSort::BitVec(w1), ExpressionSort::BitVec(w2)) if w1 == w2 => {
                    Ok(ExpressionSort::Bool(BoolOrigin::Compound))
                }
                _ => Err(type_mismatch(
                    constraint_index,
                    child_path,
                    format!(
                        "operation {op} requires same-width BitVec operands, found {} and {}",
                        left.description(),
                        right.description()
                    ),
                )),
            }
        }
    }
}

fn infer_child(
    expression: &ConstraintExpr,
    child_index: usize,
    variables: &VariableTable<'_>,
    constraint_index: usize,
    parent_depth: usize,
    child_path: &mut Vec<usize>,
) -> Result<ExpressionSort, ValidationError> {
    child_path.push(child_index);
    let result = infer_expression(
        expression,
        variables,
        constraint_index,
        parent_depth.saturating_add(1),
        child_path,
    );
    child_path.pop();
    result
}

#[derive(Clone, Copy)]
enum ExpressionClass {
    Bool,
}

impl ExpressionClass {
    const fn accepts(self, sort: ExpressionSort) -> bool {
        matches!(self, Self::Bool if matches!(sort, ExpressionSort::Bool(_)))
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
        }
    }
}

fn require_all_args(
    op: ConstraintOp,
    args: &[ConstraintExpr],
    expected: ExpressionClass,
    variables: &VariableTable<'_>,
    constraint_index: usize,
    depth: usize,
    child_path: &mut Vec<usize>,
) -> Result<(), ValidationError> {
    for (child_index, argument) in args.iter().enumerate() {
        let sort = infer_child(
            argument,
            child_index,
            variables,
            constraint_index,
            depth,
            child_path,
        )?;
        if !expected.accepts(sort) {
            child_path.push(child_index);
            let error = type_mismatch(
                constraint_index,
                child_path,
                format!(
                    "operation {op} expects {} arguments, found {}",
                    expected.description(),
                    sort.description()
                ),
            );
            child_path.pop();
            return Err(error);
        }
    }
    Ok(())
}

const fn equality_is_valid(left: ExpressionSort, right: ExpressionSort) -> bool {
    match (left, right) {
        (ExpressionSort::Int, ExpressionSort::Int)
        | (ExpressionSort::Bool(_), ExpressionSort::Bool(_))
        | (ExpressionSort::Real, ExpressionSort::Real) => true,
        (ExpressionSort::Enum(left_domain), ExpressionSort::Enum(right_domain)) => {
            left_domain == right_domain
        }
        (ExpressionSort::BitVec(left_width), ExpressionSort::BitVec(right_width)) => {
            left_width == right_width
        }
        _ => false,
    }
}

const fn ordered_numeric_pair(left: ExpressionSort, right: ExpressionSort) -> bool {
    matches!(
        (left, right),
        (ExpressionSort::Int, ExpressionSort::Int) | (ExpressionSort::Real, ExpressionSort::Real)
    )
}

fn type_mismatch(
    constraint_index: usize,
    child_path: &[usize],
    message: impl Into<String>,
) -> ValidationError {
    ValidationError::new(
        ValidationErrorKind::TypeMismatch,
        expression_path(constraint_index, child_path),
        message,
    )
}

fn expression_path(constraint_index: usize, child_path: &[usize]) -> String {
    let mut path = format!("constraints[{constraint_index}]");
    for child_index in child_path {
        path.push_str(".args[");
        path.push_str(&child_index.to_string());
        path.push(']');
    }
    path
}

fn rewrite_objective_path(objective_index: usize, constraint_style_path: &str) -> String {
    // infer_expression builds paths under constraints[i]; rewrite for objectives.
    let suffix = constraint_style_path
        .find(']')
        .map(|idx| &constraint_style_path[idx + 1..])
        .unwrap_or("");
    format!("objectives[{objective_index}].expr{suffix}")
}

fn expression_field_path(constraint_index: usize, child_path: &[usize], field: &str) -> String {
    let mut path = expression_path(constraint_index, child_path);
    path.push('.');
    path.push_str(field);
    path
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{json, Value};

    use super::{
        ConstraintExpr, ConstraintItem, ConstraintOp, ModelValue, SolveConstraintsRequest,
        SolveConstraintsResponse, SolveStatus, ValidationErrorKind, Variable,
        DEFAULT_MAX_SOLUTIONS, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_EXPRESSION_DEPTH,
        MAX_TIMEOUT_MS, MAX_VARIABLES,
    };

    fn request(vars: Vec<Variable>, constraints: Vec<ConstraintExpr>) -> SolveConstraintsRequest {
        SolveConstraintsRequest {
            vars,
            constraints: constraints.into_iter().map(ConstraintItem::from).collect(),
            objectives: vec![],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        }
    }

    fn var(name: &str) -> ConstraintExpr {
        ConstraintExpr::Var {
            name: name.to_owned(),
        }
    }

    fn int(value: i64) -> ConstraintExpr {
        ConstraintExpr::Int { value }
    }

    fn bool_literal(value: bool) -> ConstraintExpr {
        ConstraintExpr::Bool { value }
    }

    fn enum_label(var: &str, label: &str) -> ConstraintExpr {
        ConstraintExpr::EnumLabel {
            var: var.to_owned(),
            label: label.to_owned(),
        }
    }

    fn op(op: ConstraintOp, args: Vec<ConstraintExpr>) -> ConstraintExpr {
        ConstraintExpr::Op { op, args }
    }

    #[test]
    fn variable_wire_form_round_trips_all_domains() {
        let cases = [
            (
                Variable::Bool {
                    name: "enabled".to_owned(),
                },
                json!({"name": "enabled", "type": "bool"}),
            ),
            (
                Variable::Int {
                    name: "count".to_owned(),
                },
                json!({"name": "count", "type": "int"}),
            ),
            (
                Variable::IntRange {
                    name: "workers".to_owned(),
                    min: 1,
                    max: 16,
                },
                json!({"name": "workers", "type": "int_range", "min": 1, "max": 16}),
            ),
            (
                Variable::Enum {
                    name: "mode".to_owned(),
                    values: vec!["fast".to_owned(), "safe".to_owned()],
                },
                json!({"name": "mode", "type": "enum", "values": ["fast", "safe"]}),
            ),
        ];

        for (variable, expected_json) in cases {
            assert_eq!(serde_json::to_value(&variable).unwrap(), expected_json);
            assert_eq!(
                serde_json::from_value::<Variable>(expected_json).unwrap(),
                variable
            );
        }
    }

    #[test]
    fn variable_wire_form_rejects_unknown_fields() {
        let error = serde_json::from_value::<Variable>(
            json!({"name": "enabled", "type": "bool", "min": 0}),
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn constraint_expr_wire_form_round_trips_tagged_nodes() {
        let expression = ConstraintExpr::Op {
            op: ConstraintOp::And,
            args: vec![
                ConstraintExpr::Op {
                    op: ConstraintOp::Ge,
                    args: vec![
                        ConstraintExpr::Var {
                            name: "workers".to_owned(),
                        },
                        ConstraintExpr::Int { value: 4 },
                    ],
                },
                ConstraintExpr::Op {
                    op: ConstraintOp::Eq,
                    args: vec![
                        ConstraintExpr::Var {
                            name: "enabled".to_owned(),
                        },
                        ConstraintExpr::Bool { value: true },
                    ],
                },
                ConstraintExpr::Op {
                    op: ConstraintOp::Eq,
                    args: vec![
                        ConstraintExpr::Var {
                            name: "mode".to_owned(),
                        },
                        ConstraintExpr::EnumLabel {
                            var: "mode".to_owned(),
                            label: "safe".to_owned(),
                        },
                    ],
                },
            ],
        };
        let expected_json = json!({
            "kind": "op",
            "op": "and",
            "args": [
                {
                    "kind": "op",
                    "op": "ge",
                    "args": [
                        {"kind": "var", "name": "workers"},
                        {"kind": "int", "value": 4}
                    ]
                },
                {
                    "kind": "op",
                    "op": "eq",
                    "args": [
                        {"kind": "var", "name": "enabled"},
                        {"kind": "bool", "value": true}
                    ]
                },
                {
                    "kind": "op",
                    "op": "eq",
                    "args": [
                        {"kind": "var", "name": "mode"},
                        {"kind": "enum_label", "var": "mode", "label": "safe"}
                    ]
                }
            ]
        });

        assert_eq!(serde_json::to_value(&expression).unwrap(), expected_json);
        assert_eq!(
            serde_json::from_value::<ConstraintExpr>(expected_json).unwrap(),
            expression
        );
    }

    #[test]
    fn constraint_expr_rejects_bare_leaves_at_root_and_in_args() {
        for invalid in [
            json!("workers"),
            json!(4),
            json!({"kind": "op", "op": "ge", "args": ["workers", 4]}),
        ] {
            assert!(serde_json::from_value::<ConstraintExpr>(invalid).is_err());
        }
    }

    #[test]
    fn constraint_expr_rejects_unknown_ops_and_fields() {
        for invalid in [
            json!({"kind": "op", "op": "div", "args": []}),
            json!({"kind": "int", "value": 4, "name": "workers"}),
        ] {
            assert!(serde_json::from_value::<ConstraintExpr>(invalid).is_err());
        }
    }

    #[test]
    fn constraint_expr_rejects_integer_literals_outside_i64() {
        let too_large: Value =
            serde_json::from_str(r#"{"kind":"int","value":9223372036854775808}"#).unwrap();
        let too_small: Value =
            serde_json::from_str(r#"{"kind":"int","value":-9223372036854775809}"#).unwrap();

        assert!(serde_json::from_value::<ConstraintExpr>(too_large).is_err());
        assert!(serde_json::from_value::<ConstraintExpr>(too_small).is_err());
    }

    #[test]
    fn request_defaults_optional_controls_and_round_trips() {
        let request: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [{"name": "workers", "type": "int_range", "min": 1, "max": 16}],
            "constraints": [{
                "kind": "op",
                "op": "ge",
                "args": [
                    {"kind": "var", "name": "workers"},
                    {"kind": "int", "value": 4}
                ]
            }]
        }))
        .unwrap();

        assert_eq!(request.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert!(!request.persist);
        assert!(!request.include_smt);
        assert_eq!(
            serde_json::from_value::<SolveConstraintsRequest>(
                serde_json::to_value(&request).unwrap()
            )
            .unwrap(),
            request
        );
    }

    #[test]
    fn named_and_soft_constraint_items_round_trip_and_validate() {
        let request: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [{"name": "x", "type": "int_range", "min": 0, "max": 10}],
            "constraints": [
                {
                    "id": "lower",
                    "expr": {
                        "kind": "op",
                        "op": "ge",
                        "args": [{"kind": "var", "name": "x"}, {"kind": "int", "value": 1}]
                    }
                },
                {
                    "id": "prefer_high",
                    "soft": true,
                    "weight": 3,
                    "expr": {
                        "kind": "op",
                        "op": "ge",
                        "args": [{"kind": "var", "name": "x"}, {"kind": "int", "value": 8}]
                    }
                }
            ]
        }))
        .unwrap();

        assert!(request.validate().is_ok());
        assert!(request.has_soft_constraints());
        assert!(request.has_named_hard_constraints());
        assert_eq!(request.constraints[0].id(), Some("lower"));
        assert!(!request.constraints[0].is_soft());
        assert_eq!(request.constraints[1].soft_weight(), Some(3));
    }

    #[test]
    fn objectives_require_integer_expressions_and_cap() {
        use super::{Objective, ObjectiveOp, MAX_OBJECTIVES};

        let good: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [{"name": "x", "type": "int_range", "min": 0, "max": 10}],
            "constraints": [],
            "objectives": [
                {"op": "maximize", "expr": {"kind": "var", "name": "x"}},
                {"op": "minimize", "expr": {
                    "kind": "op", "op": "sub",
                    "args": [{"kind": "int", "value": 10}, {"kind": "var", "name": "x"}]
                }}
            ]
        }))
        .unwrap();
        assert!(good.validate().is_ok());
        assert!(good.has_objectives());
        assert!(!good.wants_unsat_cores());

        let not_int: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [{"name": "flag", "type": "bool"}],
            "constraints": [],
            "objectives": [
                {"op": "maximize", "expr": {"kind": "var", "name": "flag"}}
            ]
        }))
        .unwrap();
        assert_eq!(
            not_int.validate().unwrap_err().kind,
            ValidationErrorKind::ObjectiveNotNumeric
        );

        let too_many = SolveConstraintsRequest {
            vars: vec![Variable::Int {
                name: "x".to_owned(),
            }],
            constraints: vec![],
            objectives: (0..=MAX_OBJECTIVES)
                .map(|_| Objective {
                    op: ObjectiveOp::Maximize,
                    expr: ConstraintExpr::Var {
                        name: "x".to_owned(),
                    },
                })
                .collect(),
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        };
        assert_eq!(
            too_many.validate().unwrap_err().kind,
            ValidationErrorKind::TooManyObjectives
        );
    }

    #[test]
    fn validation_rejects_duplicate_ids_and_invalid_soft_weights() {
        let duplicate: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [{"name": "x", "type": "int", }],
            "constraints": [
                {"id": "c", "expr": {"kind": "bool", "value": true}},
                {"id": "c", "expr": {"kind": "bool", "value": false}}
            ]
        }))
        .unwrap();
        assert_eq!(
            duplicate.validate().unwrap_err().kind,
            ValidationErrorKind::DuplicateConstraintId
        );

        let bad_weight: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [],
            "constraints": [
                {"soft": true, "weight": 0, "expr": {"kind": "bool", "value": true}}
            ]
        }))
        .unwrap();
        assert_eq!(
            bad_weight.validate().unwrap_err().kind,
            ValidationErrorKind::InvalidSoftWeight
        );

        let weight_without_soft: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [],
            "constraints": [
                {"weight": 2, "expr": {"kind": "bool", "value": true}}
            ]
        }))
        .unwrap();
        assert_eq!(
            weight_without_soft.validate().unwrap_err().kind,
            ValidationErrorKind::WeightWithoutSoft
        );

        let overflowing_group: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [],
            "constraints": [
                {
                    "group": "preferences",
                    "soft": true,
                    "weight": i64::MAX,
                    "expr": {"kind": "bool", "value": true}
                },
                {
                    "group": "preferences",
                    "soft": true,
                    "weight": i64::MAX,
                    "expr": {"kind": "bool", "value": false}
                }
            ]
        }))
        .unwrap();
        assert_eq!(
            overflowing_group.validate().unwrap_err().kind.to_string(),
            "soft_group_weight_overflow"
        );
    }

    #[test]
    fn soft_groups_repeat_while_diagnostic_ids_remain_unique() {
        let request: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [],
            "constraints": [
                {
                    "id": "prefer_a",
                    "group": "preferences",
                    "soft": true,
                    "weight": 2,
                    "expr": {"kind": "bool", "value": true}
                },
                {
                    "id": "prefer_b",
                    "group": "preferences",
                    "soft": true,
                    "expr": {"kind": "bool", "value": false}
                }
            ]
        }))
        .expect("soft groups must deserialize");

        assert_eq!(request.validate(), Ok(()));
        assert_eq!(request.constraints[0].id(), Some("prefer_a"));
        let serialized = serde_json::to_value(&request).unwrap();
        assert_eq!(serialized["constraints"][0]["group"], "preferences");
        assert_eq!(serialized["constraints"][1]["group"], "preferences");

        for (group, expected_kind) in [
            ("9bad", "invalid_constraint_group"),
            ("valid", "group_without_soft"),
        ] {
            let soft = group == "9bad";
            let invalid: SolveConstraintsRequest = serde_json::from_value(json!({
                "vars": [],
                "constraints": [{
                    "group": group,
                    "soft": soft,
                    "expr": {"kind": "bool", "value": true}
                }]
            }))
            .expect("group shape must deserialize before semantic validation");
            assert_eq!(
                invalid.validate().unwrap_err().kind.to_string(),
                expected_kind
            );
        }
    }

    #[test]
    fn max_solutions_defaults_to_16_and_validates_one_through_64() {
        let defaulted: SolveConstraintsRequest = serde_json::from_value(json!({
            "vars": [],
            "constraints": []
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&defaulted).unwrap()["max_solutions"],
            16
        );
        assert_eq!(defaulted.validate(), Ok(()));

        for value in [1, 64] {
            let request: SolveConstraintsRequest = serde_json::from_value(json!({
                "vars": [],
                "constraints": [],
                "max_solutions": value
            }))
            .unwrap();
            assert_eq!(request.validate(), Ok(()));
        }

        for value in [0, 65] {
            let request: SolveConstraintsRequest = serde_json::from_value(json!({
                "vars": [],
                "constraints": [],
                "max_solutions": value
            }))
            .expect("max_solutions shape must deserialize before semantic validation");
            let error = request.validate().unwrap_err();
            assert_eq!(error.kind.to_string(), "invalid_max_solutions");
            assert_eq!(error.path, "max_solutions");
        }
    }

    #[test]
    fn response_model_values_use_plain_json_scalars() {
        let response = SolveConstraintsResponse {
            status: SolveStatus::Sat,
            model: Some(BTreeMap::from([
                ("batch".to_owned(), ModelValue::Int(40)),
                ("mode".to_owned(), ModelValue::Enum("fast".to_owned())),
                ("use_cache".to_owned(), ModelValue::Bool(true)),
            ])),
            duration_ms: 12,
            solve_id: Some("sol_a1b2c3d4e5f67890".to_owned()),
            reason: None,
            smt: None,
            unsat_core: None,
            cached: false,
            session_id: None,
            optimization: None,
            solver_version: None,
        };
        let value = serde_json::to_value(&response).unwrap();

        assert_eq!(value["status"], "sat");
        assert_eq!(value["model"]["batch"], 40);
        assert_eq!(value["model"]["mode"], "fast");
        assert_eq!(value["model"]["use_cache"], true);
        assert_eq!(
            serde_json::from_value::<SolveConstraintsResponse>(value).unwrap(),
            response
        );
    }

    #[test]
    fn response_omits_absent_model_and_optional_solve_id() {
        let value = serde_json::to_value(SolveConstraintsResponse {
            status: SolveStatus::Unsat,
            model: None,
            duration_ms: 3,
            solve_id: None,
            reason: None,
            smt: None,
            unsat_core: None,
            cached: false,
            session_id: None,
            optimization: None,
            solver_version: None,
        })
        .unwrap();

        assert!(value.get("model").is_none());
        assert!(value.get("solve_id").is_none());
        assert!(value.get("optimization").is_none());
        assert!(value.get("solver_version").is_none());
        assert_eq!(value["reason"], Value::Null);
        assert_eq!(value["smt"], Value::Null);
    }

    #[test]
    fn exact_optimization_response_round_trips_and_validates() {
        let value = json!({
            "status": "sat",
            "model": {"x": 1},
            "duration_ms": 3,
            "reason": null,
            "optimization": {
                "priority": "pareto",
                "solutions": [{
                    "model": {"x": 1},
                    "objectives": [{
                        "op": "maximize",
                        "value": 1,
                        "bound": {"kind": "strict", "exact": "(+ 1.0 (* -1.0 epsilon))"}
                    }],
                    "soft_constraints": [{
                        "index": 0,
                        "id": "prefer_x",
                        "group": "preferences",
                        "weight": 2,
                        "satisfied": true
                    }],
                    "groups": [{
                        "group": "preferences",
                        "satisfied_weight": 2,
                        "violated_weight": 0
                    }]
                }],
                "termination": "complete"
            },
            "solver_version": "Z3 version 4.16.0 - 64 bit"
        });

        let response: SolveConstraintsResponse = serde_json::from_value(value.clone())
            .expect("additive optimization response must deserialize");
        assert_eq!(response.validate(), Ok(()));
        assert_eq!(serde_json::to_value(response).unwrap(), value);
    }

    #[test]
    fn response_validation_rejects_invalid_optimization_envelopes() {
        let base = json!({
            "status": "sat",
            "model": {"x": 1},
            "duration_ms": 1,
            "solve_id": null,
            "reason": null,
            "smt": null,
            "unsat_core": null,
            "cached": false,
            "session_id": null,
            "solver_version": null
        });

        let mut empty = base.clone();
        empty["optimization"] = json!({
            "priority": "lex",
            "solutions": [],
            "termination": "complete"
        });
        let error = serde_json::from_value::<SolveConstraintsResponse>(empty)
            .unwrap()
            .validate()
            .unwrap_err();
        assert_eq!(error.kind.to_string(), "response_optimization_mismatch");
        assert_eq!(error.path, "optimization.solutions");

        let mut non_sat = base.clone();
        non_sat["status"] = json!("unsat");
        non_sat["model"] = Value::Null;
        non_sat["optimization"] = json!({
            "priority": "lex",
            "solutions": [{"model": {}}],
            "termination": "complete"
        });
        let error = serde_json::from_value::<SolveConstraintsResponse>(non_sat)
            .unwrap()
            .validate()
            .unwrap_err();
        assert_eq!(error.kind.to_string(), "response_optimization_mismatch");
        assert_eq!(error.path, "optimization");

        let mut mismatched = base;
        mismatched["optimization"] = json!({
            "priority": "lex",
            "solutions": [{
                "model": {"x": 2},
                "objectives": [],
                "soft_constraints": [],
                "groups": []
            }],
            "termination": "complete"
        });
        let error = serde_json::from_value::<SolveConstraintsResponse>(mismatched)
            .unwrap()
            .validate()
            .unwrap_err();
        assert_eq!(error.kind.to_string(), "response_optimization_mismatch");
        assert_eq!(error.path, "optimization.solutions[0].model");
    }

    #[test]
    fn validation_accepts_surface_names_and_rejects_invalid_or_duplicate_names() {
        let valid = request(
            ["alpha", "A0", "_private", "snake_case"]
                .into_iter()
                .map(|name| Variable::Int {
                    name: name.to_owned(),
                })
                .collect(),
            vec![],
        );
        assert_eq!(valid.validate(), Ok(()));

        for name in ["", "9workers", "has-dash", "has space", "µ"] {
            let error = request(
                vec![Variable::Int {
                    name: name.to_owned(),
                }],
                vec![],
            )
            .validate()
            .unwrap_err();
            assert_eq!(error.kind, ValidationErrorKind::InvalidVariableName);
            assert_eq!(error.path, "vars[0].name");
        }

        let duplicate = request(
            vec![
                Variable::Bool {
                    name: "same".to_owned(),
                },
                Variable::Int {
                    name: "same".to_owned(),
                },
            ],
            vec![],
        )
        .validate()
        .unwrap_err();
        assert_eq!(duplicate.kind, ValidationErrorKind::DuplicateVariableName);
        assert_eq!(duplicate.path, "vars[1].name");
    }

    #[test]
    fn validation_enforces_request_caps_and_accepts_exact_limits() {
        let at_limits = SolveConstraintsRequest {
            vars: (0..MAX_VARIABLES)
                .map(|index| Variable::Int {
                    name: format!("v{index}"),
                })
                .collect(),
            constraints: vec![bool_literal(true); MAX_CONSTRAINTS]
                .into_iter()
                .map(Into::into)
                .collect(),
            objectives: vec![],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: MAX_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        };
        assert_eq!(at_limits.validate(), Ok(()));

        let too_many_vars = request(
            (0..=MAX_VARIABLES)
                .map(|index| Variable::Int {
                    name: format!("v{index}"),
                })
                .collect(),
            vec![],
        )
        .validate()
        .unwrap_err();
        assert_eq!(too_many_vars.kind, ValidationErrorKind::TooManyVariables);
        assert_eq!(too_many_vars.path, "vars");

        let too_many_constraints = request(vec![], vec![bool_literal(true); MAX_CONSTRAINTS + 1])
            .validate()
            .unwrap_err();
        assert_eq!(
            too_many_constraints.kind,
            ValidationErrorKind::TooManyConstraints
        );
        assert_eq!(too_many_constraints.path, "constraints");

        let timeout = SolveConstraintsRequest {
            objectives: vec![],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: MAX_TIMEOUT_MS + 1,
            ..request(vec![], vec![])
        }
        .validate()
        .unwrap_err();
        assert_eq!(timeout.kind, ValidationErrorKind::TimeoutTooLarge);
        assert_eq!(timeout.path, "timeout_ms");
    }

    #[test]
    fn validation_rejects_reversed_ranges_and_invalid_enum_domains() {
        let reversed = request(
            vec![Variable::IntRange {
                name: "workers".to_owned(),
                min: 16,
                max: 1,
            }],
            vec![],
        )
        .validate()
        .unwrap_err();
        assert_eq!(reversed.kind, ValidationErrorKind::InvalidIntegerRange);
        assert_eq!(reversed.path, "vars[0]");

        let empty = request(
            vec![Variable::Enum {
                name: "mode".to_owned(),
                values: vec![],
            }],
            vec![],
        )
        .validate()
        .unwrap_err();
        assert_eq!(empty.kind, ValidationErrorKind::EmptyEnum);
        assert_eq!(empty.path, "vars[0].values");

        let duplicate = request(
            vec![Variable::Enum {
                name: "mode".to_owned(),
                values: vec!["fast".to_owned(), "safe".to_owned(), "fast".to_owned()],
            }],
            vec![],
        )
        .validate()
        .unwrap_err();
        assert_eq!(duplicate.kind, ValidationErrorKind::DuplicateEnumLabel);
        assert_eq!(duplicate.path, "vars[0].values[2]");
    }

    #[test]
    fn validation_rejects_unknown_variable_and_invalid_enum_label_references() {
        let unknown = request(vec![], vec![var("missing")])
            .validate()
            .unwrap_err();
        assert_eq!(unknown.kind, ValidationErrorKind::UnknownVariable);
        assert_eq!(unknown.path, "constraints[0].name");

        let non_enum = request(
            vec![Variable::Int {
                name: "count".to_owned(),
            }],
            vec![op(
                ConstraintOp::Eq,
                vec![int(0), enum_label("count", "zero")],
            )],
        )
        .validate()
        .unwrap_err();
        assert_eq!(non_enum.kind, ValidationErrorKind::ExpectedEnumVariable);
        assert_eq!(non_enum.path, "constraints[0].args[1].var");

        let unknown_label = request(
            vec![Variable::Enum {
                name: "mode".to_owned(),
                values: vec!["fast".to_owned(), "safe".to_owned()],
            }],
            vec![op(
                ConstraintOp::Eq,
                vec![var("mode"), enum_label("mode", "debug")],
            )],
        )
        .validate()
        .unwrap_err();
        assert_eq!(unknown_label.kind, ValidationErrorKind::UnknownEnumLabel);
        assert_eq!(unknown_label.path, "constraints[0].args[1].label");
    }

    #[test]
    fn validation_enforces_every_operation_arity() {
        let invalid = [
            op(ConstraintOp::Eq, vec![int(1)]),
            op(ConstraintOp::Ne, vec![int(1), int(2), int(3)]),
            op(ConstraintOp::Lt, vec![]),
            op(ConstraintOp::Le, vec![int(1)]),
            op(ConstraintOp::Gt, vec![int(1), int(2), int(3)]),
            op(ConstraintOp::Ge, vec![int(1)]),
            op(ConstraintOp::Add, vec![int(1)]),
            op(ConstraintOp::Sub, vec![int(1)]),
            op(ConstraintOp::Mul, vec![]),
            op(ConstraintOp::And, vec![]),
            op(ConstraintOp::Or, vec![]),
            op(
                ConstraintOp::Not,
                vec![bool_literal(true), bool_literal(false)],
            ),
        ];

        for expression in invalid {
            let error = request(vec![], vec![expression]).validate().unwrap_err();
            assert_eq!(error.kind, ValidationErrorKind::WrongArity);
            assert_eq!(error.path, "constraints[0].args");
        }
    }

    #[test]
    fn validation_enforces_integer_and_boolean_type_rules() {
        let vars = vec![
            Variable::Int {
                name: "count".to_owned(),
            },
            Variable::Bool {
                name: "enabled".to_owned(),
            },
            Variable::Bool {
                name: "ready".to_owned(),
            },
        ];
        let valid = [
            op(ConstraintOp::Ge, vec![var("count"), int(1)]),
            op(
                ConstraintOp::Eq,
                vec![op(ConstraintOp::Add, vec![var("count"), int(1)]), int(2)],
            ),
            op(ConstraintOp::Eq, vec![var("enabled"), bool_literal(true)]),
            op(ConstraintOp::Ne, vec![bool_literal(false), var("enabled")]),
            op(
                ConstraintOp::And,
                vec![var("enabled"), op(ConstraintOp::Not, vec![var("ready")])],
            ),
        ];
        for expression in valid {
            assert_eq!(request(vars.clone(), vec![expression]).validate(), Ok(()));
        }

        let invalid = [
            op(ConstraintOp::Add, vec![var("count"), var("enabled")]),
            op(ConstraintOp::Lt, vec![var("enabled"), bool_literal(true)]),
            op(ConstraintOp::Eq, vec![var("enabled"), int(1)]),
            op(ConstraintOp::Ne, vec![int(1), bool_literal(false)]),
            op(ConstraintOp::And, vec![var("enabled"), int(1)]),
        ];
        for expression in invalid {
            let error = request(vars.clone(), vec![expression])
                .validate()
                .unwrap_err();
            assert_eq!(error.kind, ValidationErrorKind::TypeMismatch);
        }
    }

    #[test]
    fn validation_accepts_all_boolean_equality_operand_origins() {
        let vars = vec![
            Variable::Bool {
                name: "enabled".to_owned(),
            },
            Variable::Bool {
                name: "ready".to_owned(),
            },
        ];
        let cases = [
            ("variable/variable", var("enabled"), var("ready")),
            ("literal/literal", bool_literal(true), bool_literal(false)),
            (
                "compound/literal",
                op(ConstraintOp::Not, vec![var("enabled")]),
                bool_literal(true),
            ),
            (
                "compound/compound",
                op(ConstraintOp::Not, vec![var("enabled")]),
                op(ConstraintOp::And, vec![var("enabled"), var("ready")]),
            ),
        ];
        let mut rejected = Vec::new();

        for comparison in [ConstraintOp::Eq, ConstraintOp::Ne] {
            for (case, left, right) in &cases {
                let result = request(
                    vars.clone(),
                    vec![op(comparison, vec![left.clone(), right.clone()])],
                )
                .validate();
                if result.is_err() {
                    rejected.push(format!("{comparison} {case}"));
                }
            }
        }

        assert!(
            rejected.is_empty(),
            "same-sort Boolean equality rejected: {}",
            rejected.join(", ")
        );
    }

    #[test]
    fn validation_rejects_non_boolean_top_level_constraints() {
        let vars = vec![Variable::Int {
            name: "count".to_owned(),
        }];

        for expression in [
            var("count"),
            int(1),
            op(ConstraintOp::Add, vec![int(1), int(2)]),
        ] {
            let error = request(vars.clone(), vec![expression])
                .validate()
                .unwrap_err();
            assert_eq!(error.kind, ValidationErrorKind::TopLevelNotBoolean);
            assert_eq!(error.path, "constraints[0]");
        }
    }

    #[test]
    fn validation_allows_only_enum_equality_with_matching_domains() {
        let vars = vec![
            Variable::Enum {
                name: "primary".to_owned(),
                values: vec!["fast".to_owned(), "safe".to_owned()],
            },
            Variable::Enum {
                name: "reordered".to_owned(),
                values: vec!["safe".to_owned(), "fast".to_owned()],
            },
            Variable::Enum {
                name: "other".to_owned(),
                values: vec!["debug".to_owned(), "safe".to_owned()],
            },
        ];
        for expression in [
            op(
                ConstraintOp::Eq,
                vec![var("primary"), enum_label("primary", "fast")],
            ),
            op(
                ConstraintOp::Ne,
                vec![enum_label("reordered", "safe"), var("primary")],
            ),
            op(ConstraintOp::Eq, vec![var("primary"), var("reordered")]),
        ] {
            assert_eq!(request(vars.clone(), vec![expression]).validate(), Ok(()));
        }

        for expression in [
            op(ConstraintOp::Add, vec![var("primary"), int(1)]),
            op(
                ConstraintOp::Lt,
                vec![var("primary"), enum_label("primary", "safe")],
            ),
            op(ConstraintOp::Eq, vec![var("primary"), var("other")]),
        ] {
            let error = request(vars.clone(), vec![expression])
                .validate()
                .unwrap_err();
            assert_eq!(error.kind, ValidationErrorKind::TypeMismatch);
        }
    }

    #[test]
    fn validation_enforces_expression_nesting_depth() {
        fn nested_not(depth: usize) -> ConstraintExpr {
            (0..depth).fold(bool_literal(true), |expression, _| {
                op(ConstraintOp::Not, vec![expression])
            })
        }

        assert_eq!(
            request(vec![], vec![nested_not(MAX_EXPRESSION_DEPTH)]).validate(),
            Ok(())
        );

        let error = request(
            vec![],
            vec![nested_not(MAX_EXPRESSION_DEPTH.saturating_add(1))],
        )
        .validate()
        .unwrap_err();
        assert_eq!(error.kind, ValidationErrorKind::ExpressionTooDeep);
        assert!(error.path.starts_with("constraints[0].args[0]"));
    }

    #[test]
    fn response_validation_enforces_model_presence_by_status() {
        let sat_without_model = SolveConstraintsResponse {
            status: SolveStatus::Sat,
            model: None,
            duration_ms: 1,
            solve_id: None,
            reason: None,
            smt: None,
            unsat_core: None,
            cached: false,
            session_id: None,
            optimization: None,
            solver_version: None,
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            sat_without_model.kind,
            ValidationErrorKind::ResponseModelMismatch
        );
        assert_eq!(sat_without_model.path, "model");

        let unsat_with_model = SolveConstraintsResponse {
            status: SolveStatus::Unsat,
            model: Some(BTreeMap::new()),
            duration_ms: 1,
            solve_id: None,
            reason: None,
            smt: None,
            unsat_core: None,
            cached: false,
            session_id: None,
            optimization: None,
            solver_version: None,
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            unsat_with_model.kind,
            ValidationErrorKind::ResponseModelMismatch
        );

        let sat = SolveConstraintsResponse {
            status: SolveStatus::Sat,
            model: Some(BTreeMap::new()),
            duration_ms: 1,
            solve_id: None,
            reason: None,
            smt: None,
            unsat_core: None,
            cached: false,
            session_id: None,
            optimization: None,
            solver_version: None,
        };
        assert_eq!(sat.validate(), Ok(()));

        let ended = SolveConstraintsResponse {
            status: SolveStatus::Ended,
            model: None,
            duration_ms: 1,
            solve_id: None,
            reason: Some("session ended".to_owned()),
            smt: None,
            unsat_core: None,
            cached: false,
            session_id: Some("sess_0123456789abcdef".to_owned()),
            optimization: None,
            solver_version: None,
        };
        assert_eq!(ended.validate(), Ok(()));

        let ended_with_model = SolveConstraintsResponse {
            status: SolveStatus::Ended,
            model: Some(BTreeMap::new()),
            duration_ms: 1,
            solve_id: None,
            reason: Some("session ended".to_owned()),
            smt: None,
            unsat_core: None,
            cached: false,
            session_id: None,
            optimization: None,
            solver_version: None,
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            ended_with_model.kind,
            ValidationErrorKind::ResponseModelMismatch
        );
    }
}
