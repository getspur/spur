//! Typed constraint encoding.
//!
//! Surface variable names are never emitted directly. After request
//! validation, each name is encoded as `v_<surface-name>`. This mapping is
//! injective for the accepted surface grammar, including names that already
//! begin with `v_`. The reverse map is therefore the declared-name whitelist
//! keyed by the emitted symbol; equivalently, strip [`SMT_IDENTIFIER_PREFIX`]
//! exactly once and verify the result is a variable declared by the request.
//! Enum labels use zero-based lexicographic order, giving equal label sets the
//! same SMT integer representation even when declarations list them differently.

use std::{collections::HashMap, error::Error, fmt};

use crate::types::{
    ConstraintExpr, ConstraintOp, SolveConstraintsRequest, ValidationError, Variable,
};

/// Prefix applied to every validated surface variable name in generated SMT.
pub const SMT_IDENTIFIER_PREFIX: &str = "v_";

/// Maximum generated SMT-LIB2 script size accepted by the typed encoder.
pub const MAX_GENERATED_SMT_BYTES: usize = 256 * 1024;

/// Failure while validating or serializing a typed solver request.
#[derive(Debug, Eq, PartialEq)]
pub enum EncodeError {
    /// The B′ request failed schema-independent semantic validation.
    InvalidRequest(ValidationError),
    /// The generated script would exceed [`MAX_GENERATED_SMT_BYTES`].
    GeneratedSmtTooLarge {
        /// Maximum permitted generated size.
        max_bytes: usize,
        /// Size after appending the fragment that crossed the limit.
        attempted_bytes: usize,
    },
    /// A validated request no longer satisfied an encoder invariant.
    InternalInvariant(&'static str),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(error) => write!(formatter, "invalid solve request: {error}"),
            Self::GeneratedSmtTooLarge {
                max_bytes,
                attempted_bytes,
            } => write!(
                formatter,
                "generated SMT-LIB2 size {attempted_bytes} exceeds maximum {max_bytes} bytes"
            ),
            Self::InternalInvariant(message) => {
                write!(
                    formatter,
                    "validated request violated encoder invariant: {message}"
                )
            }
        }
    }
}

impl Error for EncodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRequest(error) => Some(error),
            Self::GeneratedSmtTooLarge { .. } | Self::InternalInvariant(_) => None,
        }
    }
}

impl From<ValidationError> for EncodeError {
    fn from(error: ValidationError) -> Self {
        Self::InvalidRequest(error)
    }
}

/// Encodes a validated B′ request as a complete SMT-LIB2 model-finding script.
///
/// Declarations retain request order, followed by domain bounds, caller
/// constraints, `(check-sat)`, and one `(get-value ...)` query containing every
/// declared variable. An empty variable list omits `get-value`, whose SMT-LIB2
/// grammar requires at least one term.
///
/// # Errors
///
/// Returns [`EncodeError::InvalidRequest`] before emitting any SMT when the
/// request violates B′ validation rules. Returns
/// [`EncodeError::GeneratedSmtTooLarge`] when the script would exceed
/// [`MAX_GENERATED_SMT_BYTES`].
///
/// # Examples
///
/// ```
/// use spur_solver::{
///     encode::encode_solve_constraints,
///     types::{ConstraintExpr, ConstraintOp, SolveConstraintsRequest, Variable},
/// };
///
/// let request = SolveConstraintsRequest {
///     vars: vec![Variable::IntRange {
///         name: "workers".to_owned(),
///         min: 1,
///         max: 8,
///     }],
///     constraints: vec![ConstraintExpr::Op {
///         op: ConstraintOp::Ge,
///         args: vec![
///             ConstraintExpr::Var {
///                 name: "workers".to_owned(),
///             },
///             ConstraintExpr::Int { value: 3 },
///         ],
///     }],
///     timeout_ms: 30_000,
///     persist: false,
/// };
///
/// let smt = encode_solve_constraints(&request)?;
/// assert!(smt.contains("(declare-const v_workers Int)"));
/// assert!(smt.ends_with("(get-value (v_workers))\n"));
/// # Ok::<(), spur_solver::encode::EncodeError>(())
/// ```
pub fn encode_solve_constraints(request: &SolveConstraintsRequest) -> Result<String, EncodeError> {
    request.validate()?;

    let mut encoder = Encoder::new(request);
    encoder.output.push_line("(set-logic QF_NIA)")?;
    encoder
        .output
        .push_line("(set-option :produce-models true)")?;

    for variable in &request.vars {
        encoder.write_declaration(variable)?;
    }
    for variable in &request.vars {
        encoder.write_bounds(variable)?;
    }
    for constraint in &request.constraints {
        encoder.output.push("(assert ")?;
        encoder.write_expression(constraint)?;
        encoder.output.push_line(")")?;
    }

    encoder.output.push_line("(check-sat)")?;
    encoder.write_get_value()?;
    Ok(encoder.output.finish())
}

struct Encoder<'a> {
    request: &'a SolveConstraintsRequest,
    enum_domains: HashMap<&'a str, Vec<&'a str>>,
    output: SmtBuilder,
}

impl<'a> Encoder<'a> {
    fn new(request: &'a SolveConstraintsRequest) -> Self {
        let enum_domains = request
            .vars
            .iter()
            .filter_map(|variable| {
                let Variable::Enum { name, values } = variable else {
                    return None;
                };
                let mut labels: Vec<&str> = values.iter().map(String::as_str).collect();
                labels.sort_unstable();
                Some((name.as_str(), labels))
            })
            .collect();
        Self {
            request,
            enum_domains,
            output: SmtBuilder::new(),
        }
    }

    fn write_declaration(&mut self, variable: &Variable) -> Result<(), EncodeError> {
        self.output.push("(declare-const ")?;
        self.write_identifier(variable.name())?;
        match variable {
            Variable::Bool { .. } => self.output.push_line(" Bool)"),
            Variable::Int { .. } | Variable::IntRange { .. } | Variable::Enum { .. } => {
                self.output.push_line(" Int)")
            }
        }
    }

    fn write_bounds(&mut self, variable: &Variable) -> Result<(), EncodeError> {
        match variable {
            Variable::Bool { .. } | Variable::Int { .. } => Ok(()),
            Variable::IntRange { name, min, max } => {
                self.write_integer_bound(">=", name, *min)?;
                self.write_integer_bound("<=", name, *max)
            }
            Variable::Enum { name, values } => {
                self.write_unsigned_bound(">=", name, 0)?;
                self.write_unsigned_bound("<=", name, values.len().saturating_sub(1))
            }
        }
    }

    fn write_integer_bound(
        &mut self,
        operator: &'static str,
        name: &str,
        value: i64,
    ) -> Result<(), EncodeError> {
        self.output.push("(assert (")?;
        self.output.push(operator)?;
        self.output.push(" ")?;
        self.write_identifier(name)?;
        self.output.push(" ")?;
        self.write_integer(value)?;
        self.output.push_line("))")
    }

    fn write_unsigned_bound(
        &mut self,
        operator: &'static str,
        name: &str,
        value: usize,
    ) -> Result<(), EncodeError> {
        self.output.push("(assert (")?;
        self.output.push(operator)?;
        self.output.push(" ")?;
        self.write_identifier(name)?;
        self.output.push(" ")?;
        self.output.push_usize(value)?;
        self.output.push_line("))")
    }

    fn write_expression(&mut self, expression: &ConstraintExpr) -> Result<(), EncodeError> {
        match expression {
            ConstraintExpr::Var { name } => self.write_identifier(name),
            ConstraintExpr::Int { value } => self.write_integer(*value),
            ConstraintExpr::Bool { value } => {
                self.output.push(if *value { "true" } else { "false" })
            }
            ConstraintExpr::EnumLabel { var, label } => {
                let index = self.enum_label_index(var, label)?;
                self.output.push_usize(index)
            }
            ConstraintExpr::Op {
                op: ConstraintOp::Ne,
                args,
            } => {
                self.output.push("(not (=")?;
                self.write_arguments(args)?;
                self.output.push("))")
            }
            ConstraintExpr::Op { op, args } => {
                self.output.push("(")?;
                self.output.push(operation_token(*op))?;
                self.write_arguments(args)?;
                self.output.push(")")
            }
        }
    }

    fn write_arguments(&mut self, arguments: &[ConstraintExpr]) -> Result<(), EncodeError> {
        for argument in arguments {
            self.output.push(" ")?;
            self.write_expression(argument)?;
        }
        Ok(())
    }

    fn write_integer(&mut self, value: i64) -> Result<(), EncodeError> {
        if value.is_negative() {
            self.output.push("(- ")?;
            self.output.push_u64(value.unsigned_abs())?;
            self.output.push(")")
        } else {
            self.output.push_u64(value.unsigned_abs())
        }
    }

    fn write_identifier(&mut self, surface_name: &str) -> Result<(), EncodeError> {
        self.output.push(SMT_IDENTIFIER_PREFIX)?;
        self.output.push(surface_name)
    }

    fn enum_label_index(&self, variable_name: &str, label: &str) -> Result<usize, EncodeError> {
        self.enum_domains
            .get(variable_name)
            .and_then(|labels| labels.binary_search(&label).ok())
            .ok_or(EncodeError::InternalInvariant(
                "enum label was not present in its validated declaration",
            ))
    }

    fn write_get_value(&mut self) -> Result<(), EncodeError> {
        if self.request.vars.is_empty() {
            return Ok(());
        }

        self.output.push("(get-value (")?;
        for (index, variable) in self.request.vars.iter().enumerate() {
            if index > 0 {
                self.output.push(" ")?;
            }
            self.write_identifier(variable.name())?;
        }
        self.output.push_line("))")
    }
}

const fn operation_token(operation: ConstraintOp) -> &'static str {
    match operation {
        ConstraintOp::Eq | ConstraintOp::Ne => "=",
        ConstraintOp::Lt => "<",
        ConstraintOp::Le => "<=",
        ConstraintOp::Gt => ">",
        ConstraintOp::Ge => ">=",
        ConstraintOp::Add => "+",
        ConstraintOp::Sub => "-",
        ConstraintOp::Mul => "*",
        ConstraintOp::And => "and",
        ConstraintOp::Or => "or",
        ConstraintOp::Not => "not",
    }
}

struct SmtBuilder {
    output: String,
}

impl SmtBuilder {
    fn new() -> Self {
        Self {
            output: String::with_capacity(4 * 1024),
        }
    }

    fn push(&mut self, fragment: &str) -> Result<(), EncodeError> {
        let attempted_bytes = self.output.len().saturating_add(fragment.len());
        if attempted_bytes > MAX_GENERATED_SMT_BYTES {
            return Err(EncodeError::GeneratedSmtTooLarge {
                max_bytes: MAX_GENERATED_SMT_BYTES,
                attempted_bytes,
            });
        }
        self.output.push_str(fragment);
        Ok(())
    }

    fn push_line(&mut self, fragment: &str) -> Result<(), EncodeError> {
        self.push(fragment)?;
        self.push("\n")
    }

    fn push_u64(&mut self, value: u64) -> Result<(), EncodeError> {
        self.push(&value.to_string())
    }

    fn push_usize(&mut self, value: usize) -> Result<(), EncodeError> {
        self.push(&value.to_string())
    }

    fn finish(self) -> String {
        self.output
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{
        ConstraintExpr, ConstraintOp, SolveConstraintsRequest, ValidationErrorKind, Variable,
        DEFAULT_TIMEOUT_MS,
    };

    use super::{
        encode_solve_constraints, EncodeError, MAX_GENERATED_SMT_BYTES, SMT_IDENTIFIER_PREFIX,
    };

    #[test]
    fn encodes_validated_request_as_canonical_smt_lib() {
        let request = SolveConstraintsRequest {
            vars: vec![
                Variable::IntRange {
                    name: "workers".to_owned(),
                    min: -2,
                    max: 16,
                },
                Variable::Bool {
                    name: "use_cache".to_owned(),
                },
                Variable::Enum {
                    name: "mode".to_owned(),
                    values: vec!["fast".to_owned(), "safe".to_owned(), "debug".to_owned()],
                },
                Variable::Int {
                    name: "batch".to_owned(),
                },
            ],
            constraints: vec![
                op(
                    ConstraintOp::Ge,
                    vec![variable("workers"), ConstraintExpr::Int { value: 4 }],
                ),
                op(
                    ConstraintOp::Eq,
                    vec![variable("use_cache"), ConstraintExpr::Bool { value: true }],
                ),
                op(
                    ConstraintOp::Eq,
                    vec![
                        variable("mode"),
                        ConstraintExpr::EnumLabel {
                            var: "mode".to_owned(),
                            label: "safe".to_owned(),
                        },
                    ],
                ),
                op(
                    ConstraintOp::Le,
                    vec![
                        op(
                            ConstraintOp::Mul,
                            vec![
                                variable("workers"),
                                op(
                                    ConstraintOp::Add,
                                    vec![
                                        ConstraintExpr::Int { value: 48 },
                                        op(
                                            ConstraintOp::Mul,
                                            vec![
                                                ConstraintExpr::Int { value: 2 },
                                                variable("batch"),
                                            ],
                                        ),
                                    ],
                                ),
                            ],
                        ),
                        ConstraintExpr::Int { value: 512 },
                    ],
                ),
                op(
                    ConstraintOp::Ne,
                    vec![variable("batch"), ConstraintExpr::Int { value: -5 }],
                ),
            ],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let smt = encode_solve_constraints(&request).expect("valid request should encode");

        assert_eq!(
            smt,
            "\
(set-logic QF_NIA)
(set-option :produce-models true)
(declare-const v_workers Int)
(declare-const v_use_cache Bool)
(declare-const v_mode Int)
(declare-const v_batch Int)
(assert (>= v_workers (- 2)))
(assert (<= v_workers 16))
(assert (>= v_mode 0))
(assert (<= v_mode 2))
(assert (>= v_workers 4))
(assert (= v_use_cache true))
(assert (= v_mode 2))
(assert (<= (* v_workers (+ 48 (* 2 v_batch))) 512))
(assert (not (= v_batch (- 5))))
(check-sat)
(get-value (v_workers v_use_cache v_mode v_batch))
"
        );
    }

    #[test]
    fn declares_non_linear_integer_logic_for_unrestricted_multiplication() {
        let request = SolveConstraintsRequest {
            vars: vec![
                Variable::Int {
                    name: "left".to_owned(),
                },
                Variable::Int {
                    name: "right".to_owned(),
                },
            ],
            constraints: vec![op(
                ConstraintOp::Eq,
                vec![
                    op(ConstraintOp::Mul, vec![variable("left"), variable("right")]),
                    ConstraintExpr::Int { value: 12 },
                ],
            )],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let smt = encode_solve_constraints(&request).expect("valid request should encode");

        assert!(smt.starts_with("(set-logic QF_NIA)\n"));
    }

    #[test]
    fn mangling_is_unique_reversible_and_enum_labels_are_not_serialized() {
        let hostile_label = "safe) (exit) (assert false";
        let request = SolveConstraintsRequest {
            vars: vec![
                Variable::Enum {
                    name: "assert".to_owned(),
                    values: vec!["safe".to_owned(), hostile_label.to_owned()],
                },
                Variable::Int {
                    name: "v_assert".to_owned(),
                },
            ],
            constraints: vec![op(
                ConstraintOp::Eq,
                vec![
                    variable("assert"),
                    ConstraintExpr::EnumLabel {
                        var: "assert".to_owned(),
                        label: hostile_label.to_owned(),
                    },
                ],
            )],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let smt = encode_solve_constraints(&request).expect("valid request should encode");

        assert!(smt.contains("(declare-const v_assert Int)"));
        assert!(smt.contains("(declare-const v_v_assert Int)"));
        assert!(smt.contains("(assert (= v_assert 1))"));
        assert!(!smt.contains(hostile_label));

        for (symbol, surface) in [("v_assert", "assert"), ("v_v_assert", "v_assert")] {
            assert_eq!(symbol.strip_prefix(SMT_IDENTIFIER_PREFIX), Some(surface));
        }
    }

    #[test]
    fn equal_enum_sets_share_canonical_indices_despite_declaration_order() {
        let request = SolveConstraintsRequest {
            vars: vec![
                Variable::Enum {
                    name: "primary".to_owned(),
                    values: vec!["red".to_owned(), "blue".to_owned()],
                },
                Variable::Enum {
                    name: "secondary".to_owned(),
                    values: vec!["blue".to_owned(), "red".to_owned()],
                },
            ],
            constraints: vec![
                op(
                    ConstraintOp::Eq,
                    vec![variable("primary"), variable("secondary")],
                ),
                op(
                    ConstraintOp::Eq,
                    vec![
                        variable("primary"),
                        ConstraintExpr::EnumLabel {
                            var: "primary".to_owned(),
                            label: "red".to_owned(),
                        },
                    ],
                ),
                op(
                    ConstraintOp::Eq,
                    vec![
                        variable("secondary"),
                        ConstraintExpr::EnumLabel {
                            var: "secondary".to_owned(),
                            label: "red".to_owned(),
                        },
                    ],
                ),
            ],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let smt = encode_solve_constraints(&request).expect("valid request should encode");

        assert!(smt.contains("(assert (= v_primary v_secondary))"));
        assert!(smt.contains("(assert (= v_primary 1))"));
        assert!(smt.contains("(assert (= v_secondary 1))"));
    }

    #[test]
    fn rejects_hostile_identifier_before_smt_serialization() {
        let request = SolveConstraintsRequest {
            vars: vec![Variable::Int {
                name: "x) (exit".to_owned(),
            }],
            constraints: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let error = encode_solve_constraints(&request).expect_err("hostile name must be rejected");

        assert!(matches!(
            error,
            EncodeError::InvalidRequest(error)
                if error.kind == ValidationErrorKind::InvalidVariableName
        ));
    }

    #[test]
    fn encodes_every_closed_operation_token() {
        let request = SolveConstraintsRequest {
            vars: vec![
                Variable::Int {
                    name: "x".to_owned(),
                },
                Variable::Int {
                    name: "y".to_owned(),
                },
                Variable::Bool {
                    name: "flag".to_owned(),
                },
            ],
            constraints: vec![
                op(ConstraintOp::Lt, vec![variable("x"), variable("y")]),
                op(ConstraintOp::Gt, vec![variable("y"), variable("x")]),
                op(
                    ConstraintOp::Eq,
                    vec![
                        variable("x"),
                        op(
                            ConstraintOp::Sub,
                            vec![variable("y"), ConstraintExpr::Int { value: 1 }],
                        ),
                    ],
                ),
                op(
                    ConstraintOp::And,
                    vec![
                        variable("flag"),
                        op(ConstraintOp::Not, vec![variable("flag")]),
                    ],
                ),
                op(
                    ConstraintOp::Or,
                    vec![
                        ConstraintExpr::Bool { value: false },
                        ConstraintExpr::Bool { value: true },
                    ],
                ),
            ],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let smt = encode_solve_constraints(&request).expect("valid request should encode");

        for fragment in [
            "(assert (< v_x v_y))",
            "(assert (> v_y v_x))",
            "(assert (= v_x (- v_y 1)))",
            "(assert (and v_flag (not v_flag)))",
            "(assert (or false true))",
        ] {
            assert!(smt.contains(fragment), "missing fragment: {fragment}");
        }
    }

    #[test]
    fn encodes_minimum_i64_without_overflow() {
        let request = SolveConstraintsRequest {
            vars: vec![Variable::IntRange {
                name: "minimum".to_owned(),
                min: i64::MIN,
                max: i64::MIN,
            }],
            constraints: vec![op(
                ConstraintOp::Eq,
                vec![variable("minimum"), ConstraintExpr::Int { value: i64::MIN }],
            )],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let smt = encode_solve_constraints(&request).expect("valid request should encode");

        assert!(smt.contains("(- 9223372036854775808)"));
    }

    #[test]
    fn omits_get_value_for_an_empty_surface_variable_set() {
        let request = SolveConstraintsRequest {
            vars: Vec::new(),
            constraints: vec![ConstraintExpr::Bool { value: true }],
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let smt = encode_solve_constraints(&request).expect("valid request should encode");

        assert_eq!(
            smt,
            "\
(set-logic QF_NIA)
(set-option :produce-models true)
(assert true)
(check-sat)
"
        );
    }

    #[test]
    fn rejects_generated_smt_over_the_size_cap() {
        let request = SolveConstraintsRequest {
            vars: vec![Variable::Int {
                name: "a".repeat(MAX_GENERATED_SMT_BYTES),
            }],
            constraints: Vec::new(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
        };

        let error = encode_solve_constraints(&request).expect_err("oversized SMT must fail");

        assert!(matches!(
            error,
            EncodeError::GeneratedSmtTooLarge {
                max_bytes: MAX_GENERATED_SMT_BYTES,
                attempted_bytes,
            } if attempted_bytes > MAX_GENERATED_SMT_BYTES
        ));
    }

    fn variable(name: &str) -> ConstraintExpr {
        ConstraintExpr::Var {
            name: name.to_owned(),
        }
    }

    fn op(op: ConstraintOp, args: Vec<ConstraintExpr>) -> ConstraintExpr {
        ConstraintExpr::Op { op, args }
    }
}
