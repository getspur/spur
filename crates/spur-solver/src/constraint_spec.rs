//! Progressive discovery and validation for the generic B-prime language.

#![expect(
    clippy::result_large_err,
    reason = "authoring failures deliberately return complete path, repair, and example diagnostics"
)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::types::{
    ConstraintItem, ConstraintOp, SolveConstraintsRequest, ValidationError, MAX_BITVEC_WIDTH,
    MAX_CONSTRAINTS, MAX_EXPRESSION_DEPTH, MAX_OBJECTIVES, MAX_SOLUTIONS, MAX_TIMEOUT_MS,
    MAX_VARIABLES,
};

/// Current version of the generic constraint-language authoring contract.
pub const LANGUAGE_SCHEMA_VERSION: u64 = 1;

/// Stable variable tags accepted by [`crate::types::Variable`].
pub const VARIABLE_KINDS: [&str; 6] = ["bool", "int", "int_range", "enum", "real", "bit_vec"];

/// Stable expression tags accepted by [`crate::types::ConstraintExpr`].
pub const EXPRESSION_KINDS: [&str; 7] = ["var", "int", "bool", "enum_label", "real", "bv", "op"];

/// Detail section included in a language catalog response.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintSpecInclude {
    /// Contract and field summary only.
    #[default]
    Summary,
    /// Summary plus one valid JSON value.
    ValidExample,
    /// Summary plus one focused invalid JSON value and its repair.
    InvalidExample,
    /// Every available section.
    All,
}

/// A bounded progressive-disclosure query over the generic language.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConstraintSpecRequest {
    /// Request-level section: request, limits, or examples.
    pub section: Option<String>,
    /// Exact variable tag.
    pub variable: Option<String>,
    /// Exact expression tag.
    pub expression: Option<String>,
    /// Exact operator wire name.
    pub operator: Option<String>,
    /// Optional detail section, defaulting to summary.
    #[serde(default)]
    pub include: ConstraintSpecInclude,
}

impl ConstraintSpecRequest {
    fn selector(&self) -> Result<ConstraintSpecSelector<'_>, ConstraintSpecError> {
        let selectors = [
            self.section.as_deref().map(ConstraintSpecSelector::Section),
            self.variable
                .as_deref()
                .map(ConstraintSpecSelector::Variable),
            self.expression
                .as_deref()
                .map(ConstraintSpecSelector::Expression),
            self.operator
                .as_deref()
                .map(ConstraintSpecSelector::Operator),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

        match selectors.as_slice() {
            [] => Ok(ConstraintSpecSelector::Catalog),
            [selector] => Ok(*selector),
            _ => Err(ConstraintSpecError::AmbiguousSelector),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ConstraintSpecSelector<'a> {
    Catalog,
    Section(&'a str),
    Variable(&'a str),
    Expression(&'a str),
    Operator(&'a str),
}

impl ConstraintSpecSelector<'_> {
    const fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Section(_) => "section",
            Self::Variable(_) => "variable",
            Self::Expression(_) => "expression",
            Self::Operator(_) => "operator",
        }
    }

    const fn value(&self) -> Option<&str> {
        match self {
            Self::Catalog => None,
            Self::Section(value)
            | Self::Variable(value)
            | Self::Expression(value)
            | Self::Operator(value) => Some(*value),
        }
    }
}

/// Stable request/catalog errors returned by `solve_constraint_spec`.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ConstraintSpecError {
    /// More than one selector was provided.
    #[error("at most one selector may be provided: section, variable, expression, or operator")]
    AmbiguousSelector,
    /// The exact selected item does not exist.
    #[error("unknown {selector} `{value}`")]
    UnknownSelector {
        /// Selector field name.
        selector: &'static str,
        /// Rejected exact value.
        value: String,
    },
}

/// Stage that rejected an authoring request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticPhase {
    /// Raw JSON could not form the tagged request.
    Deserialize,
    /// The typed request violated an arity, sort, naming, or limit rule.
    Semantic,
    /// A progressive-disclosure query was invalid.
    Selector,
}

/// Path-aware repair information returned in invalid-parameter MCP error data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RequestDiagnostic {
    /// Stable machine-readable category.
    pub code: String,
    /// Validation stage.
    pub phase: DiagnosticPhase,
    /// JSON-like location; root is `$`.
    pub path: String,
    /// Concise explanation.
    pub message: String,
    /// Expected shape or value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Observed JSON type or selector value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<String>,
    /// Short repair instruction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Minimal valid replacement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
}

impl RequestDiagnostic {
    pub(crate) fn new(
        code: impl Into<String>,
        phase: DiagnosticPhase,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            phase,
            path: path.into(),
            message: message.into(),
            expected: None,
            found: None,
            hint: None,
            example: None,
        }
    }

    fn expected(mut self, value: impl Into<String>) -> Self {
        self.expected = Some(value.into());
        self
    }

    fn found(mut self, value: impl Into<String>) -> Self {
        self.found = Some(value.into());
        self
    }

    fn hint(mut self, value: impl Into<String>) -> Self {
        self.hint = Some(value.into());
        self
    }

    fn example(mut self, value: Value) -> Self {
        self.example = Some(value);
        self
    }
}

/// Evaluates one read-only generic-language query without a solver service.
pub fn query(request: ConstraintSpecRequest) -> Result<Value, ConstraintSpecError> {
    let selector = request.selector()?;
    let mut response = Map::from_iter([
        (
            "language_schema_version".to_owned(),
            json!(LANGUAGE_SCHEMA_VERSION),
        ),
        (
            "query".to_owned(),
            json!({
                "selector": selector.name(),
                "value": selector.value(),
                "include": request.include,
            }),
        ),
        ("capability".to_owned(), json!({ "status": "implemented" })),
        (
            "next_tools".to_owned(),
            json!([
                "solve_constraint_spec",
                "solve_constraint_check",
                "solve_constraints"
            ]),
        ),
    ]);

    match selector {
        ConstraintSpecSelector::Catalog => {
            response.insert(
                "sections".to_owned(),
                json!([
                    {"name": "request", "summary": "Canonical request and wrapped constraint entry."},
                    {"name": "limits", "summary": "Enforced size, timeout, depth, and bit-width caps."},
                    {"name": "examples", "summary": "Complete valid and focused invalid requests."}
                ]),
            );
            response.insert(
                "variables".to_owned(),
                Value::Array(
                    VARIABLE_KINDS
                        .iter()
                        .map(|kind| {
                            variable_detail(kind, ConstraintSpecInclude::Summary)
                                .expect("known variable")
                        })
                        .collect(),
                ),
            );
            response.insert(
                "expressions".to_owned(),
                Value::Array(
                    EXPRESSION_KINDS
                        .iter()
                        .map(|kind| {
                            expression_detail(kind, ConstraintSpecInclude::Summary)
                                .expect("known expression")
                        })
                        .collect(),
                ),
            );
            response.insert(
                "operators".to_owned(),
                Value::Array(
                    ConstraintOp::ALL
                        .iter()
                        .map(|op| operator_card(*op))
                        .collect(),
                ),
            );
        }
        ConstraintSpecSelector::Section(name) => {
            let section = section_detail(name, request.include).ok_or_else(|| {
                ConstraintSpecError::UnknownSelector {
                    selector: "section",
                    value: name.to_owned(),
                }
            })?;
            response.insert("section".to_owned(), section);
        }
        ConstraintSpecSelector::Variable(kind) => {
            let variable = variable_detail(kind, request.include).ok_or_else(|| {
                ConstraintSpecError::UnknownSelector {
                    selector: "variable",
                    value: kind.to_owned(),
                }
            })?;
            response.insert("variable".to_owned(), variable);
        }
        ConstraintSpecSelector::Expression(kind) => {
            let expression = expression_detail(kind, request.include).ok_or_else(|| {
                ConstraintSpecError::UnknownSelector {
                    selector: "expression",
                    value: kind.to_owned(),
                }
            })?;
            response.insert("expression".to_owned(), expression);
        }
        ConstraintSpecSelector::Operator(name) => {
            let op = ConstraintOp::ALL
                .into_iter()
                .find(|candidate| candidate.as_str() == name)
                .ok_or_else(|| ConstraintSpecError::UnknownSelector {
                    selector: "operator",
                    value: name.to_owned(),
                })?;
            response.insert("operator".to_owned(), operator_detail(op));
        }
    }

    Ok(Value::Object(response))
}

/// Converts a selector error into the shared diagnostic contract.
#[must_use]
pub fn selector_diagnostic(error: &ConstraintSpecError) -> RequestDiagnostic {
    match error {
        ConstraintSpecError::AmbiguousSelector => RequestDiagnostic::new(
            "ambiguous_selector",
            DiagnosticPhase::Selector,
            "$",
            error.to_string(),
        )
        .expected("at most one of section, variable, expression, or operator"),
        ConstraintSpecError::UnknownSelector { selector, value } => RequestDiagnostic::new(
            "unknown_selector",
            DiagnosticPhase::Selector,
            *selector,
            error.to_string(),
        )
        .found(value.clone())
        .hint("Call solve_constraint_spec with no selector to list exact names."),
    }
}

/// Deserializes and semantically validates a generic request without running Z3.
pub fn parse_and_validate(args: Value) -> Result<SolveConstraintsRequest, RequestDiagnostic> {
    diagnose_request_shape(&args)?;
    let request = serde_json::from_value::<SolveConstraintsRequest>(args).map_err(|error| {
        RequestDiagnostic::new(
            "invalid_request_shape",
            DiagnosticPhase::Deserialize,
            "$",
            error.to_string(),
        )
        .hint("Query solve_constraint_spec for the exact selected variant before retrying.")
    })?;
    request
        .validate()
        .map_err(|error| semantic_diagnostic(&request, error))?;
    Ok(request)
}

/// Produces the deterministic validation-only success response.
pub fn check(args: Value) -> Result<Value, RequestDiagnostic> {
    let request = parse_and_validate(args)?;
    Ok(json!({
        "valid": true,
        "language_schema_version": LANGUAGE_SCHEMA_VERSION,
        "summary": {
            "variables": request.vars.len(),
            "constraints": request.constraints.len(),
            "objectives": request.objectives.len(),
            "has_soft_constraints": request.has_soft_constraints(),
            "is_optimization": request.has_soft_constraints() || request.has_objectives(),
        },
        "next_tools": ["solve_constraints"]
    }))
}

fn section_detail(name: &str, include: ConstraintSpecInclude) -> Option<Value> {
    let detail = match name {
        "request" => json!({
            "name": "request",
            "summary": "Declare vars and canonical wrapped constraints; objectives and execution controls are optional.",
            "required_fields": ["vars", "constraints"],
            "optional_fields": [
                "objectives", "objective_priority", "max_solutions", "timeout_ms",
                "persist", "include_smt", "use_cache", "session_id", "session_op"
            ],
            "constraint_entry": {
                "required_fields": ["expr"],
                "optional_fields": ["id", "group", "soft", "weight"],
                "notes": [
                    "Publish and author wrapped entries only.",
                    "Legacy bare expressions remain runtime-compatible but are not canonical.",
                    "group and weight are valid only for soft constraints."
                ]
            },
            "objective_entry": {
                "required_fields": ["op", "expr"],
                "op_values": ["minimize", "maximize"],
                "expr_sort": "Int, Real, or BitVec",
                "example": {
                    "op": "minimize",
                    "expr": {"kind": "var", "name": "workers"}
                }
            },
            "valid_example": canonical_request_example(),
            "invalid_example": {
                "value": {"vars": [], "constraints": [{"kind": "bool", "value": true, "id": "mixed"}]},
                "diagnostic": "Wrapper metadata requires expr; do not mix metadata with a bare expression."
            }
        }),
        "limits" => json!({
            "name": "limits",
            "summary": "Hard request limits enforced before Z3 execution.",
            "values": {
                "max_variables": MAX_VARIABLES,
                "max_constraints": MAX_CONSTRAINTS,
                "max_objectives": MAX_OBJECTIVES,
                "max_expression_depth": MAX_EXPRESSION_DEPTH,
                "max_bit_vec_width": MAX_BITVEC_WIDTH,
                "max_timeout_ms": MAX_TIMEOUT_MS,
                "max_solutions": MAX_SOLUTIONS
            }
        }),
        "examples" => json!({
            "name": "examples",
            "summary": "Load a complete valid request or one focused invalid request.",
            "valid_example": canonical_request_example(),
            "invalid_example": {
                "value": {
                    "vars": [{"type": "int_range", "name": "x", "max": 10}],
                    "constraints": []
                },
                "diagnostic": {
                    "code": "missing_variant_field",
                    "path": "vars[0].min",
                    "repair": "Add the inclusive lower bound min."
                }
            }
        }),
        _ => return None,
    };
    Some(project_examples(detail, include))
}

fn variable_detail(kind: &str, include: ConstraintSpecInclude) -> Option<Value> {
    let detail = match kind {
        "bool" => variant_detail(
            kind,
            "Unbounded Boolean variable.",
            &["type", "name"],
            &["min", "max", "values", "width"],
            json!({"type": "bool", "name": "enabled"}),
        ),
        "int" => variant_detail(
            kind,
            "Unbounded signed integer variable.",
            &["type", "name"],
            &["min", "max", "values", "width"],
            json!({"type": "int", "name": "count"}),
        ),
        "int_range" => variant_detail(
            kind,
            "Signed integer constrained to the inclusive range min..=max.",
            &["type", "name", "min", "max"],
            &["values", "width"],
            json!({"type": "int_range", "name": "workers", "min": 1, "max": 12}),
        ),
        "enum" => variant_detail(
            kind,
            "Finite enumeration with at least one unique string label.",
            &["type", "name", "values"],
            &["min", "max", "width"],
            json!({"type": "enum", "name": "mode", "values": ["safe", "fast"]}),
        ),
        "real" => variant_detail(
            kind,
            "Unbounded exact SMT Real variable.",
            &["type", "name"],
            &["min", "max", "values", "width"],
            json!({"type": "real", "name": "ratio"}),
        ),
        "bit_vec" => variant_detail(
            kind,
            "Fixed-width bit-vector; width is in 1..=64.",
            &["type", "name", "width"],
            &["min", "max", "values"],
            json!({"type": "bit_vec", "name": "mask", "width": 8}),
        ),
        _ => return None,
    };
    Some(project_examples(detail, include))
}

fn expression_detail(kind: &str, include: ConstraintSpecInclude) -> Option<Value> {
    let detail = match kind {
        "var" => expression_variant_detail(
            kind,
            "Reference a declared variable by name.",
            &["kind", "name"],
            &["value", "var", "label", "num", "den", "width", "op", "args"],
            json!({"kind": "var", "name": "workers"}),
            json!({
                "value": {"kind": "var", "var": "workers"},
                "diagnostic": "var references require name; var is used only by enum_label."
            }),
        ),
        "int" => expression_variant_detail(
            kind,
            "Signed 64-bit integer literal.",
            &["kind", "value"],
            &["name", "var", "label", "num", "den", "width", "op", "args"],
            json!({"kind": "int", "value": 4}),
            json!({"value": {"kind": "int", "value": true}, "diagnostic": "int value must be an integer."}),
        ),
        "bool" => expression_variant_detail(
            kind,
            "Boolean literal.",
            &["kind", "value"],
            &["name", "var", "label", "num", "den", "width", "op", "args"],
            json!({"kind": "bool", "value": true}),
            json!({"value": {"kind": "bool", "value": 1}, "diagnostic": "bool value must be true or false."}),
        ),
        "enum_label" => expression_variant_detail(
            kind,
            "Label from one declared enum variable's domain.",
            &["kind", "var", "label"],
            &["name", "value", "num", "den", "width", "op", "args"],
            json!({"kind": "enum_label", "var": "mode", "label": "safe"}),
            json!({"value": {"kind": "enum_label", "name": "mode", "label": "safe"}, "diagnostic": "enum_label identifies its enum declaration with var."}),
        ),
        "real" => expression_variant_detail(
            kind,
            "Exact rational num/den with a positive denominator.",
            &["kind", "num", "den"],
            &["name", "value", "var", "label", "width", "op", "args"],
            json!({"kind": "real", "num": 3, "den": 2}),
            json!({"value": {"kind": "real", "num": 3, "den": 0}, "diagnostic": "den must be positive."}),
        ),
        "bv" => expression_variant_detail(
            kind,
            "Unsigned bit-vector literal whose value fits the declared width.",
            &["kind", "width", "value"],
            &["name", "var", "label", "num", "den", "op", "args"],
            json!({"kind": "bv", "width": 8, "value": 15}),
            json!({"value": {"kind": "bv", "width": 4, "value": 16}, "diagnostic": "value must fit in width bits."}),
        ),
        "op" => expression_variant_detail(
            kind,
            "Apply one cataloged operator to tagged child expressions.",
            &["kind", "op", "args"],
            &["name", "value", "var", "label", "num", "den", "width"],
            json!({
                "kind": "op",
                "op": "ge",
                "args": [
                    {"kind": "var", "name": "workers"},
                    {"kind": "int", "value": 4}
                ]
            }),
            json!({"value": {"kind": "op", "op": "ge", "args": [{"kind": "int", "value": 4}]}, "diagnostic": "ge requires exactly two operands."}),
        ),
        _ => return None,
    };
    Some(project_examples(detail, include))
}

fn variant_detail(
    kind: &str,
    summary: &str,
    required_fields: &[&str],
    irrelevant_fields: &[&str],
    valid_example: Value,
) -> Value {
    json!({
        "kind": kind,
        "summary": summary,
        "required_fields": required_fields,
        "irrelevant_fields": irrelevant_fields,
        "valid_example": valid_example,
        "invalid_example": {
            "value": {"type": kind},
            "diagnostic": "Every variable variant requires name plus its kind-specific fields."
        }
    })
}

fn expression_variant_detail(
    kind: &str,
    summary: &str,
    required_fields: &[&str],
    irrelevant_fields: &[&str],
    valid_example: Value,
    invalid_example: Value,
) -> Value {
    json!({
        "kind": kind,
        "summary": summary,
        "required_fields": required_fields,
        "irrelevant_fields": irrelevant_fields,
        "valid_example": valid_example,
        "invalid_example": invalid_example
    })
}

fn project_examples(mut detail: Value, include: ConstraintSpecInclude) -> Value {
    let Some(object) = detail.as_object_mut() else {
        return detail;
    };
    match include {
        ConstraintSpecInclude::Summary => {
            object.remove("valid_example");
            object.remove("invalid_example");
        }
        ConstraintSpecInclude::ValidExample => {
            object.remove("invalid_example");
        }
        ConstraintSpecInclude::InvalidExample => {
            object.remove("valid_example");
        }
        ConstraintSpecInclude::All => {}
    }
    detail
}

fn operator_card(op: ConstraintOp) -> Value {
    let (minimum, maximum) = op.arity_bounds();
    json!({
        "name": op.as_str(),
        "arity": {"min": minimum, "max": maximum},
        "result_sort": operator_result_sort(op)
    })
}

fn operator_detail(op: ConstraintOp) -> Value {
    let mut detail = operator_card(op);
    let object = detail.as_object_mut().expect("operator card is an object");
    object.insert(
        "operand_sorts".to_owned(),
        json!(operator_operand_sorts(op)),
    );
    object.insert("valid_expression".to_owned(), operator_example(op));
    detail
}

const fn operator_operand_sorts(op: ConstraintOp) -> &'static str {
    match op {
        ConstraintOp::Eq | ConstraintOp::Ne => {
            "two operands of the same sort: Int, Bool, Real, same enum domain, or same-width BitVec"
        }
        ConstraintOp::Lt | ConstraintOp::Le | ConstraintOp::Gt | ConstraintOp::Ge => {
            "exactly two homogeneous Int or Real operands"
        }
        ConstraintOp::Add | ConstraintOp::Sub | ConstraintOp::Mul => {
            "homogeneous Int or homogeneous Real operands"
        }
        ConstraintOp::And | ConstraintOp::Or | ConstraintOp::Not => "Bool operands",
        ConstraintOp::BvNot => "one BitVec operand",
        ConstraintOp::BvAnd
        | ConstraintOp::BvOr
        | ConstraintOp::BvXor
        | ConstraintOp::BvAdd
        | ConstraintOp::BvSub
        | ConstraintOp::BvMul
        | ConstraintOp::BvUlt
        | ConstraintOp::BvUle
        | ConstraintOp::BvUgt
        | ConstraintOp::BvUge => "exactly two same-width BitVec operands",
    }
}

const fn operator_result_sort(op: ConstraintOp) -> &'static str {
    match op {
        ConstraintOp::Eq
        | ConstraintOp::Ne
        | ConstraintOp::Lt
        | ConstraintOp::Le
        | ConstraintOp::Gt
        | ConstraintOp::Ge
        | ConstraintOp::And
        | ConstraintOp::Or
        | ConstraintOp::Not
        | ConstraintOp::BvUlt
        | ConstraintOp::BvUle
        | ConstraintOp::BvUgt
        | ConstraintOp::BvUge => "Bool",
        ConstraintOp::Add | ConstraintOp::Sub | ConstraintOp::Mul => {
            "same numeric sort as operands"
        }
        ConstraintOp::BvAnd
        | ConstraintOp::BvOr
        | ConstraintOp::BvXor
        | ConstraintOp::BvNot
        | ConstraintOp::BvAdd
        | ConstraintOp::BvSub
        | ConstraintOp::BvMul => "same BitVec width as operands",
    }
}

fn operator_example(op: ConstraintOp) -> Value {
    let integer_var = json!({"kind": "var", "name": "x"});
    let integer = json!({"kind": "int", "value": 1});
    let boolean_a = json!({"kind": "var", "name": "enabled"});
    let boolean_b = json!({"kind": "bool", "value": true});
    let bit_vec_a = json!({"kind": "bv", "width": 8, "value": 15});
    let bit_vec_b = json!({"kind": "bv", "width": 8, "value": 3});
    let args = match op {
        ConstraintOp::Eq
        | ConstraintOp::Ne
        | ConstraintOp::Lt
        | ConstraintOp::Le
        | ConstraintOp::Gt
        | ConstraintOp::Ge
        | ConstraintOp::Add
        | ConstraintOp::Sub
        | ConstraintOp::Mul => vec![integer_var, integer],
        ConstraintOp::And | ConstraintOp::Or => vec![boolean_a, boolean_b],
        ConstraintOp::Not => vec![boolean_a],
        ConstraintOp::BvNot => vec![bit_vec_a],
        ConstraintOp::BvAnd
        | ConstraintOp::BvOr
        | ConstraintOp::BvXor
        | ConstraintOp::BvAdd
        | ConstraintOp::BvSub
        | ConstraintOp::BvMul
        | ConstraintOp::BvUlt
        | ConstraintOp::BvUle
        | ConstraintOp::BvUgt
        | ConstraintOp::BvUge => vec![bit_vec_a, bit_vec_b],
    };
    json!({"kind": "op", "op": op.as_str(), "args": args})
}

fn canonical_request_example() -> Value {
    json!({
        "vars": [
            {"type": "int_range", "name": "workers", "min": 1, "max": 12},
            {"type": "bool", "name": "expedited"}
        ],
        "constraints": [
            {
                "id": "workers_at_least_four",
                "expr": {
                    "kind": "op",
                    "op": "ge",
                    "args": [
                        {"kind": "var", "name": "workers"},
                        {"kind": "int", "value": 4}
                    ]
                }
            }
        ]
    })
}

fn diagnose_request_shape(value: &Value) -> Result<(), RequestDiagnostic> {
    let root = expect_object(value, "$")?;
    ensure_known_fields(
        root,
        "$",
        &[
            "vars",
            "constraints",
            "objectives",
            "objective_priority",
            "max_solutions",
            "timeout_ms",
            "persist",
            "include_smt",
            "use_cache",
            "session_id",
            "session_op",
        ],
    )?;

    let vars = required_field(root, "$", "vars", "array of tagged variable declarations")?;
    let vars = expect_array(vars, "vars")?;
    for (index, variable) in vars.iter().enumerate() {
        diagnose_variable(variable, &format!("vars[{index}]"))?;
    }

    let constraints = required_field(root, "$", "constraints", "array of wrapped constraints")?;
    let constraints = expect_array(constraints, "constraints")?;
    for (index, constraint) in constraints.iter().enumerate() {
        diagnose_constraint(constraint, index)?;
    }

    if let Some(objectives) = root.get("objectives") {
        let objectives = expect_array(objectives, "objectives")?;
        for (index, objective) in objectives.iter().enumerate() {
            diagnose_objective(objective, index)?;
        }
    }

    if let Some(value) = root.get("objective_priority") {
        let priority = expect_string(value, "objective_priority")?;
        if !["lex", "pareto", "box"].contains(&priority) {
            return Err(RequestDiagnostic::new(
                "unknown_objective_priority",
                DiagnosticPhase::Deserialize,
                "objective_priority",
                format!("unknown objective priority {priority:?}"),
            )
            .expected("lex | pareto | box")
            .found(priority)
            .hint("Use lex for ordered objectives, pareto for a frontier, or box for independent optima."));
        }
    }
    if let Some(value) = root.get("max_solutions") {
        expect_u64(value, "max_solutions")?;
    }
    if let Some(value) = root.get("timeout_ms") {
        expect_u64(value, "timeout_ms")?;
    }
    for field in ["persist", "include_smt", "use_cache"] {
        if let Some(value) = root.get(field) {
            expect_bool(value, field)?;
        }
    }
    if let Some(value) = root.get("session_id") {
        if !value.is_null() {
            expect_string(value, "session_id")?;
        }
    }
    if let Some(value) = root.get("session_op") {
        let session_op = expect_string(value, "session_op")?;
        if !["none", "begin", "push", "pop", "end"].contains(&session_op) {
            return Err(RequestDiagnostic::new(
                "unknown_session_op",
                DiagnosticPhase::Deserialize,
                "session_op",
                format!("unknown session operation {session_op:?}"),
            )
            .expected("none | begin | push | pop | end")
            .found(session_op)
            .hint(
                "Use none for a stateless solve, or begin/push/pop/end for an incremental session.",
            ));
        }
    }

    Ok(())
}

fn diagnose_variable(value: &Value, path: &str) -> Result<(), RequestDiagnostic> {
    let object = expect_object(value, path)?;
    let kind_value = required_field(object, path, "type", "variable kind string")?;
    let kind = expect_string(kind_value, &format!("{path}.type"))?;
    if !VARIABLE_KINDS.contains(&kind) {
        return Err(RequestDiagnostic::new(
            "unknown_variable_kind",
            DiagnosticPhase::Deserialize,
            format!("{path}.type"),
            format!("unknown variable type {kind:?}"),
        )
        .expected(VARIABLE_KINDS.join(" | "))
        .found(kind));
    }
    expect_string(
        required_field(object, path, "name", "surface identifier")?,
        &format!("{path}.name"),
    )?;

    let (required, allowed): (&[&str], &[&str]) = match kind {
        "int_range" => (&["min", "max"], &["type", "name", "min", "max"]),
        "enum" => (&["values"], &["type", "name", "values"]),
        "bit_vec" => (&["width"], &["type", "name", "width"]),
        _ => (&[], &["type", "name"]),
    };
    ensure_known_fields(object, path, allowed)?;
    for field in required {
        if !object.contains_key(*field) {
            return Err(RequestDiagnostic::new(
                "missing_variant_field",
                DiagnosticPhase::Deserialize,
                format!("{path}.{field}"),
                format!("type={kind} requires field {field}"),
            )
            .expected(format!("{field}: kind-specific value"))
            .hint(format!(
                "Query solve_constraint_spec with variable={kind} for a valid declaration."
            )));
        }
    }

    match kind {
        "int_range" => {
            expect_i64(&object["min"], &format!("{path}.min"))?;
            expect_i64(&object["max"], &format!("{path}.max"))?;
        }
        "enum" => {
            let values = expect_array(&object["values"], &format!("{path}.values"))?;
            for (index, value) in values.iter().enumerate() {
                expect_string(value, &format!("{path}.values[{index}]"))?;
            }
        }
        "bit_vec" => {
            expect_u64(&object["width"], &format!("{path}.width"))?;
        }
        _ => {}
    }
    Ok(())
}

fn diagnose_constraint(value: &Value, index: usize) -> Result<(), RequestDiagnostic> {
    let path = format!("constraints[{index}]");
    let object = expect_object(value, &path)?;
    let has_wrapper_metadata = ["id", "group", "soft", "weight"]
        .iter()
        .any(|field| object.contains_key(*field));

    if let Some(expr) = object.get("expr") {
        ensure_known_fields(object, &path, &["id", "group", "soft", "weight", "expr"])?;
        for field in ["id", "group"] {
            if let Some(value) = object.get(field) {
                if !value.is_null() {
                    expect_string(value, &format!("{path}.{field}"))?;
                }
            }
        }
        if let Some(value) = object.get("soft") {
            expect_bool(value, &format!("{path}.soft"))?;
        }
        if let Some(value) = object.get("weight") {
            if !value.is_null() {
                expect_i64(value, &format!("{path}.weight"))?;
            }
        }
        return diagnose_expression(expr, &format!("{path}.expr"));
    }

    if has_wrapper_metadata {
        return Err(RequestDiagnostic::new(
            "mixed_constraint_forms",
            DiagnosticPhase::Deserialize,
            format!("{path}.expr"),
            "constraint metadata requires a wrapped expr field",
        )
        .expected("expr: tagged ConstraintExpr")
        .hint("Move kind/op/args under expr; publish wrapped constraints only.")
        .example(json!({
            "id": "rule_name",
            "expr": {"kind": "bool", "value": true}
        })));
    }

    // Runtime compatibility: legacy bare expressions remain accepted.
    diagnose_expression(value, &path)
}

fn diagnose_objective(value: &Value, index: usize) -> Result<(), RequestDiagnostic> {
    let path = format!("objectives[{index}]");
    let object = expect_object(value, &path)?;
    ensure_known_fields(object, &path, &["op", "expr"])?;
    let op = expect_string(
        required_field(object, &path, "op", "maximize or minimize")?,
        &format!("{path}.op"),
    )?;
    if !matches!(op, "maximize" | "minimize") {
        return Err(RequestDiagnostic::new(
            "unknown_objective_op",
            DiagnosticPhase::Deserialize,
            format!("{path}.op"),
            format!("unknown objective op {op:?}"),
        )
        .expected("maximize | minimize")
        .found(op));
    }
    let expr = required_field(object, &path, "expr", "tagged numeric expression")?;
    diagnose_expression(expr, &format!("{path}.expr"))
}

fn diagnose_expression(value: &Value, path: &str) -> Result<(), RequestDiagnostic> {
    let object = expect_object(value, path)?;
    let kind_value = required_field(object, path, "kind", "expression kind string")?;
    let kind = expect_string(kind_value, &format!("{path}.kind"))?;
    if !EXPRESSION_KINDS.contains(&kind) {
        return Err(RequestDiagnostic::new(
            "unknown_expression_kind",
            DiagnosticPhase::Deserialize,
            format!("{path}.kind"),
            format!("unknown expression kind {kind:?}"),
        )
        .expected(EXPRESSION_KINDS.join(" | "))
        .found(kind));
    }

    let (required, allowed): (&[&str], &[&str]) = match kind {
        "var" => (&["name"], &["kind", "name"]),
        "int" | "bool" => (&["value"], &["kind", "value"]),
        "enum_label" => (&["var", "label"], &["kind", "var", "label"]),
        "real" => (&["num", "den"], &["kind", "num", "den"]),
        "bv" => (&["width", "value"], &["kind", "width", "value"]),
        "op" => (&["op", "args"], &["kind", "op", "args"]),
        _ => unreachable!("expression kind checked above"),
    };

    for field in required {
        if !object.contains_key(*field) {
            let mut diagnostic = RequestDiagnostic::new(
                "missing_variant_field",
                DiagnosticPhase::Deserialize,
                format!("{path}.{field}"),
                format!("kind={kind} requires field {field}"),
            )
            .expected(format!("{field}: kind-specific value"));
            if kind == "var" && *field == "name" && object.contains_key("var") {
                diagnostic = diagnostic
                    .found("var")
                    .hint("Use name for a variable reference; var is used by enum_label.")
                    .example(json!({"kind": "var", "name": "x"}));
            }
            return Err(diagnostic);
        }
    }
    ensure_known_fields(object, path, allowed)?;

    match kind {
        "var" => {
            expect_string(&object["name"], &format!("{path}.name"))?;
        }
        "int" => {
            expect_i64(&object["value"], &format!("{path}.value"))?;
        }
        "bool" => {
            if !object["value"].is_boolean() {
                return Err(
                    type_diagnostic(&format!("{path}.value"), "boolean", &object["value"])
                        .hint("kind=bool requires a JSON boolean, not 0/1 or a string.")
                        .example(json!({"kind": "bool", "value": true})),
                );
            }
        }
        "enum_label" => {
            expect_string(&object["var"], &format!("{path}.var"))?;
            expect_string(&object["label"], &format!("{path}.label"))?;
        }
        "real" => {
            expect_i64(&object["num"], &format!("{path}.num"))?;
            expect_i64(&object["den"], &format!("{path}.den"))?;
        }
        "bv" => {
            expect_u64(&object["width"], &format!("{path}.width"))?;
            expect_u64(&object["value"], &format!("{path}.value"))?;
        }
        "op" => {
            let op = expect_string(&object["op"], &format!("{path}.op"))?;
            if !ConstraintOp::ALL
                .iter()
                .any(|candidate| candidate.as_str() == op)
            {
                return Err(RequestDiagnostic::new(
                    "unknown_operator",
                    DiagnosticPhase::Deserialize,
                    format!("{path}.op"),
                    format!("unknown constraint operator {op:?}"),
                )
                .expected(
                    ConstraintOp::ALL
                        .iter()
                        .map(|candidate| candidate.as_str())
                        .collect::<Vec<_>>()
                        .join(" | "),
                )
                .found(op)
                .hint(
                    "Query solve_constraint_spec with an operator selector for arity and sorts.",
                ));
            }
            let args = expect_array(&object["args"], &format!("{path}.args"))?;
            for (index, argument) in args.iter().enumerate() {
                diagnose_expression(argument, &format!("{path}.args[{index}]"))?;
            }
        }
        _ => unreachable!("expression kind checked above"),
    }
    Ok(())
}

fn semantic_diagnostic(
    request: &SolveConstraintsRequest,
    error: ValidationError,
) -> RequestDiagnostic {
    let path = canonical_semantic_path(request, &error.path);
    RequestDiagnostic::new(
        error.kind.to_string(),
        DiagnosticPhase::Semantic,
        path,
        error.message,
    )
    .hint("Query solve_constraint_spec for the selected operator or variant contract.")
}

fn canonical_semantic_path(request: &SolveConstraintsRequest, path: &str) -> String {
    let Some(index_start) = path.strip_prefix("constraints[") else {
        return path.to_owned();
    };
    let Some(close_index) = index_start.find(']') else {
        return path.to_owned();
    };
    let Ok(index) = index_start[..close_index].parse::<usize>() else {
        return path.to_owned();
    };
    if !matches!(
        request.constraints.get(index),
        Some(ConstraintItem::Declared(_))
    ) {
        return path.to_owned();
    }
    let suffix = &index_start[close_index + 1..];
    if [".id", ".group", ".soft", ".weight"]
        .iter()
        .any(|field| suffix == *field || suffix.starts_with(&format!("{field}.")))
        || suffix.starts_with(".expr")
    {
        path.to_owned()
    } else {
        format!("constraints[{index}].expr{suffix}")
    }
}

fn required_field<'a>(
    object: &'a Map<String, Value>,
    path: &str,
    field: &str,
    expected: &str,
) -> Result<&'a Value, RequestDiagnostic> {
    object.get(field).ok_or_else(|| {
        RequestDiagnostic::new(
            "missing_required_field",
            DiagnosticPhase::Deserialize,
            if path == "$" {
                field.to_owned()
            } else {
                format!("{path}.{field}")
            },
            format!("missing required field {field}"),
        )
        .expected(expected)
    })
}

fn ensure_known_fields(
    object: &Map<String, Value>,
    path: &str,
    allowed: &[&str],
) -> Result<(), RequestDiagnostic> {
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(RequestDiagnostic::new(
            "irrelevant_variant_field",
            DiagnosticPhase::Deserialize,
            if path == "$" {
                field.clone()
            } else {
                format!("{path}.{field}")
            },
            format!("field {field:?} is not valid for the selected shape"),
        )
        .expected(allowed.join(", "))
        .found(field.clone())
        .hint("Remove the field or query solve_constraint_spec for the selected variant."));
    }
    Ok(())
}

fn expect_object<'a>(
    value: &'a Value,
    path: &str,
) -> Result<&'a Map<String, Value>, RequestDiagnostic> {
    value
        .as_object()
        .ok_or_else(|| type_diagnostic(path, "object", value))
}

fn expect_array<'a>(value: &'a Value, path: &str) -> Result<&'a [Value], RequestDiagnostic> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| type_diagnostic(path, "array", value))
}

fn expect_string<'a>(value: &'a Value, path: &str) -> Result<&'a str, RequestDiagnostic> {
    value
        .as_str()
        .ok_or_else(|| type_diagnostic(path, "string", value))
}

fn expect_bool(value: &Value, path: &str) -> Result<bool, RequestDiagnostic> {
    value
        .as_bool()
        .ok_or_else(|| type_diagnostic(path, "boolean", value))
}

fn expect_i64(value: &Value, path: &str) -> Result<i64, RequestDiagnostic> {
    value
        .as_i64()
        .ok_or_else(|| type_diagnostic(path, "signed integer", value))
}

fn expect_u64(value: &Value, path: &str) -> Result<u64, RequestDiagnostic> {
    value
        .as_u64()
        .ok_or_else(|| type_diagnostic(path, "non-negative integer", value))
}

fn type_diagnostic(path: &str, expected: &str, value: &Value) -> RequestDiagnostic {
    RequestDiagnostic::new(
        "wrong_json_type",
        DiagnosticPhase::Deserialize,
        path,
        format!("expected {expected}, found {}", json_type(value)),
    )
    .expected(expected)
    .found(json_type(value))
}

const fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{
        check, parse_and_validate, query, selector_diagnostic, ConstraintSpecError,
        ConstraintSpecInclude, ConstraintSpecRequest, DiagnosticPhase, EXPRESSION_KINDS,
        LANGUAGE_SCHEMA_VERSION, VARIABLE_KINDS,
    };
    use crate::types::{ConstraintExpr, ConstraintOp, Variable};

    #[test]
    fn catalog_covers_every_published_variant_and_operator() {
        let response = query(ConstraintSpecRequest::default()).expect("catalog summary");

        assert_eq!(response["language_schema_version"], LANGUAGE_SCHEMA_VERSION);
        assert_eq!(
            names(&response["variables"], "kind"),
            VARIABLE_KINDS.to_vec()
        );
        assert_eq!(
            names(&response["expressions"], "kind"),
            EXPRESSION_KINDS.to_vec()
        );
        assert_eq!(
            names(&response["operators"], "name"),
            ConstraintOp::ALL
                .iter()
                .map(|op| op.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_variant_valid_example_deserializes() {
        for kind in VARIABLE_KINDS {
            let response = query(ConstraintSpecRequest {
                variable: Some(kind.to_owned()),
                include: ConstraintSpecInclude::ValidExample,
                ..ConstraintSpecRequest::default()
            })
            .expect("variable detail");
            serde_json::from_value::<Variable>(response["variable"]["valid_example"].clone())
                .unwrap_or_else(|error| panic!("invalid {kind} example: {error}"));
        }

        for kind in EXPRESSION_KINDS {
            let response = query(ConstraintSpecRequest {
                expression: Some(kind.to_owned()),
                include: ConstraintSpecInclude::ValidExample,
                ..ConstraintSpecRequest::default()
            })
            .expect("expression detail");
            serde_json::from_value::<ConstraintExpr>(
                response["expression"]["valid_example"].clone(),
            )
            .unwrap_or_else(|error| panic!("invalid {kind} example: {error}"));
        }
    }

    #[test]
    fn every_operator_example_validates_in_a_complete_request() {
        for op in ConstraintOp::ALL {
            let response = query(ConstraintSpecRequest {
                operator: Some(op.as_str().to_owned()),
                ..ConstraintSpecRequest::default()
            })
            .expect("operator detail");
            let expression = response["operator"]["valid_expression"].clone();
            let result_sort = response["operator"]["result_sort"]
                .as_str()
                .expect("operator result sort");
            let (constraints, objectives) = if result_sort == "Bool" {
                (json!([{"expr": expression}]), json!([]))
            } else {
                (json!([]), json!([{"op": "minimize", "expr": expression}]))
            };
            let request = json!({
                "vars": [
                    {"type": "int", "name": "x"},
                    {"type": "bool", "name": "enabled"}
                ],
                "constraints": constraints,
                "objectives": objectives
            });

            parse_and_validate(request)
                .unwrap_or_else(|error| panic!("invalid {} example: {error:?}", op.as_str()));
        }
    }

    #[test]
    fn canonical_request_preflights_without_a_solver() {
        let response = query(ConstraintSpecRequest {
            section: Some("request".to_owned()),
            include: ConstraintSpecInclude::ValidExample,
            ..ConstraintSpecRequest::default()
        })
        .expect("request detail");
        let checked = check(response["section"]["valid_example"].clone()).expect("valid request");

        assert_eq!(checked["valid"], true);
        assert_eq!(checked["summary"]["variables"], 2);
        assert_eq!(checked["summary"]["constraints"], 1);
        assert_eq!(checked["summary"]["is_optimization"], false);
        assert_eq!(checked["next_tools"], json!(["solve_constraints"]));
    }

    #[test]
    fn common_union_mistakes_return_repairable_paths() {
        for (args, code, path) in [
            (
                json!({
                    "vars": [{"type": "int_range", "name": "x", "max": 10}],
                    "constraints": []
                }),
                "missing_variant_field",
                "vars[0].min",
            ),
            (
                json!({
                    "vars": [{"type": "int_range", "name": "x", "min": 0, "max": 10}],
                    "constraints": [{
                        "id": "minimum",
                        "expr": {"kind": "op", "op": "ge", "args": [
                            {"kind": "var", "var": "x"},
                            {"kind": "int", "value": 3}
                        ]}
                    }]
                }),
                "missing_variant_field",
                "constraints[0].expr.args[0].name",
            ),
            (
                json!({
                    "vars": [],
                    "constraints": [{"id": "mixed", "kind": "bool", "value": true}]
                }),
                "mixed_constraint_forms",
                "constraints[0].expr",
            ),
            (
                json!({
                    "vars": [],
                    "constraints": [{"expr": {"kind": "bool", "value": 1}}]
                }),
                "wrong_json_type",
                "constraints[0].expr.value",
            ),
        ] {
            let error = parse_and_validate(args).expect_err("fixture must fail");
            assert_eq!(error.code, code);
            assert_eq!(error.phase, DiagnosticPhase::Deserialize);
            assert_eq!(error.path, path);
        }
    }

    #[test]
    fn execution_controls_and_wrapper_metadata_report_exact_paths() {
        let base = json!({
            "vars": [],
            "constraints": [{"expr": {"kind": "bool", "value": true}}]
        });
        for (field, value, path) in [
            ("objective_priority", json!(7), "objective_priority"),
            ("max_solutions", json!("1"), "max_solutions"),
            ("timeout_ms", json!("100"), "timeout_ms"),
            ("persist", json!("true"), "persist"),
            ("include_smt", json!("false"), "include_smt"),
            ("use_cache", json!(1), "use_cache"),
            ("session_id", json!(7), "session_id"),
            ("session_op", json!(true), "session_op"),
        ] {
            let mut request = base.clone();
            request[field] = value;
            let error = parse_and_validate(request).expect_err("wrong type must fail");
            assert_eq!(error.code, "wrong_json_type", "field {field}");
            assert_eq!(error.phase, DiagnosticPhase::Deserialize, "field {field}");
            assert_eq!(error.path, path, "field {field}");
        }

        for (field, value, path) in [
            ("id", json!(1), "constraints[0].id"),
            ("group", json!(1), "constraints[0].group"),
            ("soft", json!("true"), "constraints[0].soft"),
            ("weight", json!("1"), "constraints[0].weight"),
        ] {
            let mut request = base.clone();
            request["constraints"][0][field] = value;
            let error = parse_and_validate(request).expect_err("wrong type must fail");
            assert_eq!(error.code, "wrong_json_type", "field {field}");
            assert_eq!(error.phase, DiagnosticPhase::Deserialize, "field {field}");
            assert_eq!(error.path, path, "field {field}");
        }
    }

    #[test]
    fn execution_control_values_report_exact_paths() {
        for (field, value, code) in [
            (
                "objective_priority",
                json!("fastest"),
                "unknown_objective_priority",
            ),
            ("session_op", json!("restart"), "unknown_session_op"),
        ] {
            let mut request = json!({"vars": [], "constraints": []});
            request[field] = value;
            let error = parse_and_validate(request).expect_err("unknown value must fail");
            assert_eq!(error.code, code, "field {field}");
            assert_eq!(error.phase, DiagnosticPhase::Deserialize, "field {field}");
            assert_eq!(error.path, field, "field {field}");
            assert!(error.hint.is_some(), "field {field}");
        }
    }

    #[test]
    fn negative_authoring_matrix_has_stable_repairable_diagnostics() {
        let fixtures = vec![
            (
                json!({
                    "vars": [],
                    "constraints": [{"expr": {
                        "kind": "op", "op": "implies", "args": []
                    }}]
                }),
                "unknown_operator",
                DiagnosticPhase::Deserialize,
                "constraints[0].expr.op",
                "Query solve_constraint_spec with an operator selector for arity and sorts.",
            ),
            (
                json!({
                    "vars": [],
                    "constraints": [{"expr": {
                        "kind": "op", "op": "eq", "args": [
                            {"kind": "int", "value": 1},
                            {"kind": "bool", "value": true}
                        ]
                    }}]
                }),
                "type_mismatch",
                DiagnosticPhase::Semantic,
                "constraints[0].expr",
                "Query solve_constraint_spec for the selected operator or variant contract.",
            ),
            (
                json!({
                    "vars": [],
                    "constraints": [{"expr": {"kind": "var", "name": "missing"}}]
                }),
                "unknown_variable",
                DiagnosticPhase::Semantic,
                "constraints[0].expr.name",
                "Query solve_constraint_spec for the selected operator or variant contract.",
            ),
            (
                json!({
                    "vars": [{"type": "enum", "name": "mode", "values": ["safe"]}],
                    "constraints": [{"expr": {
                        "kind": "enum_label", "var": "mode", "label": "turbo"
                    }}]
                }),
                "unknown_enum_label",
                DiagnosticPhase::Semantic,
                "constraints[0].expr.label",
                "Query solve_constraint_spec for the selected operator or variant contract.",
            ),
            (
                json!({
                    "vars": [],
                    "constraints": [{
                        "group": "preferences",
                        "expr": {"kind": "bool", "value": true}
                    }]
                }),
                "group_without_soft",
                DiagnosticPhase::Semantic,
                "constraints[0].group",
                "Query solve_constraint_spec for the selected operator or variant contract.",
            ),
            (
                json!({
                    "vars": [],
                    "constraints": [{
                        "weight": 2,
                        "expr": {"kind": "bool", "value": true}
                    }]
                }),
                "weight_without_soft",
                DiagnosticPhase::Semantic,
                "constraints[0].weight",
                "Query solve_constraint_spec for the selected operator or variant contract.",
            ),
        ];

        for (request, code, phase, path, hint) in fixtures {
            let error = parse_and_validate(request).expect_err("fixture must fail");
            assert_eq!(error.code, code);
            assert_eq!(error.phase, phase);
            assert_eq!(error.path, path);
            assert_eq!(error.hint.as_deref(), Some(hint));
        }
    }

    #[test]
    fn boolean_type_error_includes_an_actionable_hint() {
        let error = parse_and_validate(json!({
            "vars": [],
            "constraints": [{"expr": {"kind": "bool", "value": 1}}]
        }))
        .expect_err("wrong bool type must fail");

        assert_eq!(error.code, "wrong_json_type");
        assert_eq!(error.path, "constraints[0].expr.value");
        assert_eq!(
            error.hint.as_deref(),
            Some("kind=bool requires a JSON boolean, not 0/1 or a string.")
        );
    }

    #[test]
    fn semantic_errors_preserve_kind_and_canonical_expr_path() {
        let error = parse_and_validate(json!({
            "vars": [],
            "constraints": [{
                "id": "bad_arity",
                "expr": {"kind": "op", "op": "ge", "args": [{"kind": "int", "value": 1}]}
            }]
        }))
        .expect_err("wrong arity must fail");

        assert_eq!(error.code, "wrong_arity");
        assert_eq!(error.phase, DiagnosticPhase::Semantic);
        assert_eq!(error.path, "constraints[0].expr.args");
    }

    #[test]
    fn selector_failures_are_stable_and_repairable() {
        let ambiguous = query(ConstraintSpecRequest {
            variable: Some("int_range".to_owned()),
            expression: Some("var".to_owned()),
            ..ConstraintSpecRequest::default()
        })
        .expect_err("multiple selectors must fail");
        assert_eq!(ambiguous, ConstraintSpecError::AmbiguousSelector);
        let diagnostic = selector_diagnostic(&ambiguous);
        assert_eq!(diagnostic.code, "ambiguous_selector");
        assert_eq!(diagnostic.phase, DiagnosticPhase::Selector);
        assert_eq!(diagnostic.path, "$");

        let unknown = query(ConstraintSpecRequest {
            operator: Some("implies".to_owned()),
            ..ConstraintSpecRequest::default()
        })
        .expect_err("unknown operator must fail");
        let diagnostic = selector_diagnostic(&unknown);
        assert_eq!(diagnostic.code, "unknown_selector");
        assert_eq!(diagnostic.path, "operator");
        assert_eq!(diagnostic.found.as_deref(), Some("implies"));
        assert!(diagnostic.hint.is_some());
    }

    fn names<'a>(value: &'a Value, field: &str) -> Vec<&'a str> {
        value
            .as_array()
            .expect("catalog cards")
            .iter()
            .map(|card| card[field].as_str().expect("card name"))
            .collect()
    }
}
