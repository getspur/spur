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

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
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
}

impl Variable {
    /// Returns the surface name shared by every variable variant.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Bool { name }
            | Self::Int { name }
            | Self::IntRange { name, .. }
            | Self::Enum { name, .. } => name,
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
    /// Integer multiplication.
    Mul,
    /// Boolean conjunction.
    And,
    /// Boolean disjunction.
    Or,
    /// Boolean negation.
    Not,
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
        }
    }

    const fn arity(self) -> Arity {
        match self {
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge | Self::Sub => {
                Arity::Exact(2)
            }
            Self::Add | Self::Mul => Arity::AtLeast(2),
            Self::And | Self::Or => Arity::AtLeast(1),
            Self::Not => Arity::Exact(1),
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
    /// Application of a closed B′ operation.
    Op {
        /// Operation to apply.
        op: ConstraintOp,
        /// Tagged child expressions.
        args: Vec<Self>,
    },
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
///     }],
///     timeout_ms: DEFAULT_TIMEOUT_MS,
///     persist: false,
/// };
///
/// assert!(request.validate().is_ok());
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SolveConstraintsRequest {
    /// Variables available to constraint expressions.
    pub vars: Vec<Variable>,
    /// Boolean expressions that every returned model must satisfy.
    pub constraints: Vec<ConstraintExpr>,
    /// Wall-clock budget in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Whether the service should persist the solve for later retrieval.
    #[serde(default)]
    pub persist: bool,
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
        for (constraint_index, constraint) in self.constraints.iter().enumerate() {
            let mut child_path = Vec::new();
            let sort =
                infer_expression(constraint, &variables, constraint_index, 0, &mut child_path)?;
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

        Ok(())
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
    #[serde(default)]
    pub persist: bool,
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
}

/// A scalar value returned for one surface variable.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ModelValue {
    /// Boolean model value.
    Bool(bool),
    /// Signed integer model value.
    Int(i64),
    /// Enum label, or an opaque SMT-LIB value from the raw solver path.
    Enum(String),
}

/// Surface-name-to-value mapping returned for a satisfiable solve.
pub type SolveModel = BTreeMap<String, ModelValue>;

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
    /// Optional generated SMT-LIB debug output.
    pub smt: Option<String>,
}

impl SolveConstraintsResponse {
    /// Checks invariants on a solver response envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationErrorKind::ResponseModelMismatch`] unless a model
    /// is present exactly when the response status is [`SolveStatus::Sat`].
    pub fn validate(&self) -> Result<(), ValidationError> {
        let model_is_valid = match self.status {
            SolveStatus::Sat => self.model.is_some(),
            SolveStatus::Unsat
            | SolveStatus::Unknown
            | SolveStatus::Timeout
            | SolveStatus::Error => self.model.is_none(),
        };
        if !model_is_valid {
            return Err(ValidationError::new(
                ValidationErrorKind::ResponseModelMismatch,
                "model",
                "model must be present if and only if status is sat",
            ));
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
    /// A response's model presence does not match its status.
    ResponseModelMismatch,
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
            Self::ResponseModelMismatch => "response_model_mismatch",
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
}

impl ExpressionSort {
    const fn description(self) -> &'static str {
        match self {
            Self::Int => "Int",
            Self::Bool(_) => "Bool",
            Self::Enum(_) => "Enum",
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
            })
        }
        ConstraintExpr::Int { .. } => Ok(ExpressionSort::Int),
        ConstraintExpr::Bool { .. } => Ok(ExpressionSort::Bool(BoolOrigin::Literal)),
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
            require_all_args(
                op,
                args,
                ExpressionClass::Int,
                variables,
                constraint_index,
                depth,
                child_path,
            )?;
            Ok(ExpressionSort::Bool(BoolOrigin::Compound))
        }
        ConstraintOp::Add | ConstraintOp::Sub | ConstraintOp::Mul => {
            require_all_args(
                op,
                args,
                ExpressionClass::Int,
                variables,
                constraint_index,
                depth,
                child_path,
            )?;
            Ok(ExpressionSort::Int)
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
    Int,
    Bool,
}

impl ExpressionClass {
    const fn accepts(self, sort: ExpressionSort) -> bool {
        match self {
            Self::Int => matches!(sort, ExpressionSort::Int),
            Self::Bool => matches!(sort, ExpressionSort::Bool(_)),
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Int => "Int",
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
        | (ExpressionSort::Bool(_), ExpressionSort::Bool(_)) => true,
        (ExpressionSort::Enum(left_domain), ExpressionSort::Enum(right_domain)) => {
            left_domain == right_domain
        }
        _ => false,
    }
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
        ConstraintExpr, ConstraintOp, ModelValue, SolveConstraintsRequest,
        SolveConstraintsResponse, SolveStatus, ValidationErrorKind, Variable, DEFAULT_TIMEOUT_MS,
        MAX_CONSTRAINTS, MAX_EXPRESSION_DEPTH, MAX_TIMEOUT_MS, MAX_VARIABLES,
    };

    fn request(vars: Vec<Variable>, constraints: Vec<ConstraintExpr>) -> SolveConstraintsRequest {
        SolveConstraintsRequest {
            vars,
            constraints,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
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
        assert_eq!(
            serde_json::from_value::<SolveConstraintsRequest>(
                serde_json::to_value(&request).unwrap()
            )
            .unwrap(),
            request
        );
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
        })
        .unwrap();

        assert!(value.get("model").is_none());
        assert!(value.get("solve_id").is_none());
        assert_eq!(value["reason"], Value::Null);
        assert_eq!(value["smt"], Value::Null);
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
            constraints: vec![bool_literal(true); MAX_CONSTRAINTS],
            timeout_ms: MAX_TIMEOUT_MS,
            persist: false,
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
        };
        assert_eq!(sat.validate(), Ok(()));
    }
}
