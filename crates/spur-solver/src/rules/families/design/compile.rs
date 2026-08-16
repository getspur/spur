//! Lowering from validated design rules to the typed B-prime constraint IR.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    rules::{builtin_registry, CompiledRule, RuleSolveMode},
    types::{
        ConstraintDecl, ConstraintExpr, ConstraintItem, ConstraintOp, ObjectivePriority, SessionOp,
        SolveConstraintsRequest, Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS,
    },
};

use super::scene::{DesignField, DesignScene, DesignSceneError, DesignUnknown};

/// Maximum explicit rule bindings accepted by the design family.
pub const MAX_DESIGN_RULE_BINDINGS: usize = MAX_CONSTRAINTS;

/// Design-family name for the shared rule solve mode.
pub type DesignSolveMode = RuleSolveMode;

/// Design-family input before it is lowered to the generic solver request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignCompileRequest {
    /// Verification or synthesis semantics.
    pub mode: DesignSolveMode,
    /// Explicit rule applications in caller order.
    pub rules: Vec<DesignRuleBinding>,
    /// Typed scene facts.
    pub scene: DesignScene,
    /// Bounded geometry fields omitted from the scene.
    #[serde(default)]
    pub unknowns: Vec<DesignUnknown>,
    /// Existing solver wall-clock budget.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Persist the underlying solver result.
    #[serde(default)]
    pub persist: bool,
    /// Echo generated SMT from the generic backend.
    #[serde(default)]
    pub include_smt: bool,
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// One explicit rule application.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignRuleBinding {
    /// Stable catalog rule ID.
    pub rule_id: String,
    /// Ordered node IDs with rule-specific arity.
    pub subjects: Vec<String>,
    /// Typed optional parameters; unrelated fields are rejected by the compiler.
    #[serde(default)]
    pub parameters: DesignRuleParameters,
}

/// Closed parameter surface for the initial design rules.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignRuleParameters {
    /// Axis selected by `layout.axis_capacity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<DesignAxis>,
    /// Non-negative spacing between adjacent capacity items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<i64>,
    /// Non-negative inset before the first capacity item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inset_start: Option<i64>,
    /// Non-negative inset after the last capacity item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inset_end: Option<i64>,
    /// Non-negative inset for `layout.containment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<i64>,
    /// Non-negative separation for `layout.non_overlap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_gap: Option<i64>,
    /// Positive intrinsic width for `media.aspect_ratio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_width: Option<i64>,
    /// Positive intrinsic height for `media.aspect_ratio`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_height: Option<i64>,
}

/// One-dimensional extent selected by `layout.axis_capacity`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignAxis {
    /// Use rectangle widths.
    Horizontal,
    /// Use rectangle heights.
    Vertical,
}

/// Typed solver request plus model-to-scene metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledDesignRules {
    /// Request consumed by the shared [`crate::service::SolverService`].
    pub request: SolveConstraintsRequest,
    /// Identity-preserving predicates in caller binding order.
    pub rules: Vec<CompiledRule>,
    /// Stable mapping from backend variables to geometry paths.
    pub unknowns: Vec<CompiledDesignUnknown>,
}

/// One generated B-prime variable and its design-scene path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompiledDesignUnknown {
    /// Backend model key.
    pub variable: String,
    /// Scene node ID.
    pub node: String,
    /// Rectangle field.
    pub field: DesignField,
}

/// Compiles validated design facts and rule bindings to B-prime.
pub fn compile(input: DesignCompileRequest) -> Result<CompiledDesignRules, DesignCompileError> {
    if input.rules.is_empty() {
        return Err(DesignCompileError::NoRules);
    }
    if input.rules.len() > MAX_DESIGN_RULE_BINDINGS {
        return Err(DesignCompileError::TooManyRuleBindings {
            count: input.rules.len(),
            max: MAX_DESIGN_RULE_BINDINGS,
        });
    }
    if input.mode == DesignSolveMode::Verify && !input.unknowns.is_empty() {
        return Err(DesignCompileError::VerificationUnknowns {
            count: input.unknowns.len(),
        });
    }
    input.scene.validate(&input.unknowns)?;

    let resolver = GeometryResolver::new(&input.scene, &input.unknowns);
    let rules = input
        .rules
        .iter()
        .enumerate()
        .map(|(binding_index, binding)| {
            compile_binding(binding, &input.scene, &resolver).map(|predicate| {
                CompiledRule::new(binding.rule_id.clone(), binding_index, predicate)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let constraints = rules
        .iter()
        .map(|rule| declared(&rule.constraint_id("design"), rule.predicate.clone()))
        .collect();

    let request = SolveConstraintsRequest {
        vars: resolver.variables,
        constraints,
        objectives: Vec::new(),
        objective_priority: ObjectivePriority::Lex,
        timeout_ms: input.timeout_ms,
        persist: input.persist,
        include_smt: input.include_smt,
        use_cache: true,
        session_id: None,
        session_op: SessionOp::None,
    };
    request
        .validate()
        .map_err(|error| DesignCompileError::InvalidSolverRequest {
            message: error.to_string(),
        })?;

    Ok(CompiledDesignRules {
        request,
        rules,
        unknowns: resolver.unknowns,
    })
}

fn compile_binding(
    binding: &DesignRuleBinding,
    scene: &DesignScene,
    resolver: &GeometryResolver,
) -> Result<ConstraintExpr, DesignCompileError> {
    if builtin_registry().rule(&binding.rule_id).is_none() {
        return Err(DesignCompileError::UnknownRule {
            rule_id: binding.rule_id.clone(),
        });
    }

    match binding.rule_id.as_str() {
        "layout.axis_capacity" => {
            validate_minimum_subjects(binding, scene, 2)?;
            reject_parameter(binding, "padding", binding.parameters.padding)?;
            reject_parameter(binding, "minimum_gap", binding.parameters.minimum_gap)?;
            reject_parameter(binding, "source_width", binding.parameters.source_width)?;
            reject_parameter(binding, "source_height", binding.parameters.source_height)?;
            let axis =
                binding
                    .parameters
                    .axis
                    .ok_or_else(|| DesignCompileError::MissingParameter {
                        rule_id: binding.rule_id.clone(),
                        parameter: "axis",
                    })?;
            let gap = non_negative_parameter(
                &binding.rule_id,
                "gap",
                binding.parameters.gap.unwrap_or(0),
            )?;
            let inset_start = non_negative_parameter(
                &binding.rule_id,
                "inset_start",
                binding.parameters.inset_start.unwrap_or(0),
            )?;
            let inset_end = non_negative_parameter(
                &binding.rule_id,
                "inset_end",
                binding.parameters.inset_end.unwrap_or(0),
            )?;
            Ok(axis_capacity(
                resolver,
                &binding.subjects,
                axis,
                gap,
                inset_start,
                inset_end,
            ))
        }
        "layout.containment" => {
            validate_subjects(binding, scene, 2)?;
            reject_capacity_parameters(binding)?;
            reject_parameter(binding, "minimum_gap", binding.parameters.minimum_gap)?;
            reject_parameter(binding, "source_width", binding.parameters.source_width)?;
            reject_parameter(binding, "source_height", binding.parameters.source_height)?;
            let padding = non_negative_parameter(
                &binding.rule_id,
                "padding",
                binding.parameters.padding.unwrap_or(0),
            )?;
            Ok(containment(
                resolver,
                &binding.subjects[0],
                &binding.subjects[1],
                padding,
            ))
        }
        "layout.non_overlap" => {
            validate_subjects(binding, scene, 2)?;
            reject_capacity_parameters(binding)?;
            reject_parameter(binding, "padding", binding.parameters.padding)?;
            reject_parameter(binding, "source_width", binding.parameters.source_width)?;
            reject_parameter(binding, "source_height", binding.parameters.source_height)?;
            let minimum_gap = non_negative_parameter(
                &binding.rule_id,
                "minimum_gap",
                binding.parameters.minimum_gap.unwrap_or(0),
            )?;
            Ok(non_overlap(
                resolver,
                &binding.subjects[0],
                &binding.subjects[1],
                minimum_gap,
            ))
        }
        "media.aspect_ratio" => {
            validate_subjects(binding, scene, 1)?;
            reject_capacity_parameters(binding)?;
            reject_parameter(binding, "padding", binding.parameters.padding)?;
            reject_parameter(binding, "minimum_gap", binding.parameters.minimum_gap)?;
            let source_width = positive_required_parameter(
                &binding.rule_id,
                "source_width",
                binding.parameters.source_width,
            )?;
            let source_height = positive_required_parameter(
                &binding.rule_id,
                "source_height",
                binding.parameters.source_height,
            )?;
            Ok(aspect_ratio(
                resolver,
                &binding.subjects[0],
                source_width,
                source_height,
            ))
        }
        _ => Err(DesignCompileError::UnknownRule {
            rule_id: binding.rule_id.clone(),
        }),
    }
}

fn validate_minimum_subjects(
    binding: &DesignRuleBinding,
    scene: &DesignScene,
    minimum: usize,
) -> Result<(), DesignCompileError> {
    if binding.subjects.len() < minimum {
        return Err(DesignCompileError::TooFewSubjects {
            rule_id: binding.rule_id.clone(),
            minimum,
            actual: binding.subjects.len(),
        });
    }
    validate_subject_ids(binding, scene)
}

fn validate_subjects(
    binding: &DesignRuleBinding,
    scene: &DesignScene,
    expected: usize,
) -> Result<(), DesignCompileError> {
    if binding.subjects.len() != expected {
        return Err(DesignCompileError::InvalidSubjectArity {
            rule_id: binding.rule_id.clone(),
            expected,
            actual: binding.subjects.len(),
        });
    }
    validate_subject_ids(binding, scene)
}

fn validate_subject_ids(
    binding: &DesignRuleBinding,
    scene: &DesignScene,
) -> Result<(), DesignCompileError> {
    for subject in &binding.subjects {
        if !scene.nodes.contains_key(subject) {
            return Err(DesignCompileError::UnknownSubject {
                rule_id: binding.rule_id.clone(),
                node: subject.clone(),
            });
        }
    }
    Ok(())
}

fn reject_capacity_parameters(binding: &DesignRuleBinding) -> Result<(), DesignCompileError> {
    if binding.parameters.axis.is_some() {
        return Err(DesignCompileError::UnexpectedParameter {
            rule_id: binding.rule_id.clone(),
            parameter: "axis",
        });
    }
    reject_parameter(binding, "gap", binding.parameters.gap)?;
    reject_parameter(binding, "inset_start", binding.parameters.inset_start)?;
    reject_parameter(binding, "inset_end", binding.parameters.inset_end)
}

fn reject_parameter(
    binding: &DesignRuleBinding,
    parameter: &'static str,
    value: Option<i64>,
) -> Result<(), DesignCompileError> {
    if value.is_some() {
        return Err(DesignCompileError::UnexpectedParameter {
            rule_id: binding.rule_id.clone(),
            parameter,
        });
    }
    Ok(())
}

fn non_negative_parameter(
    rule_id: &str,
    parameter: &'static str,
    value: i64,
) -> Result<i64, DesignCompileError> {
    if value < 0 {
        return Err(DesignCompileError::NegativeParameter {
            rule_id: rule_id.to_owned(),
            parameter,
            value,
        });
    }
    Ok(value)
}

fn positive_required_parameter(
    rule_id: &str,
    parameter: &'static str,
    value: Option<i64>,
) -> Result<i64, DesignCompileError> {
    let value = value.ok_or_else(|| DesignCompileError::MissingParameter {
        rule_id: rule_id.to_owned(),
        parameter,
    })?;
    if value <= 0 {
        return Err(DesignCompileError::NonPositiveParameter {
            rule_id: rule_id.to_owned(),
            parameter,
            value,
        });
    }
    Ok(value)
}

fn containment(
    resolver: &GeometryResolver,
    child: &str,
    parent: &str,
    padding: i64,
) -> ConstraintExpr {
    operation(
        ConstraintOp::And,
        vec![
            le(
                add(vec![resolver.field(parent, DesignField::X), int(padding)]),
                resolver.field(child, DesignField::X),
            ),
            le(
                add(vec![resolver.field(parent, DesignField::Y), int(padding)]),
                resolver.field(child, DesignField::Y),
            ),
            le(
                add(vec![
                    resolver.field(child, DesignField::X),
                    resolver.field(child, DesignField::Width),
                    int(padding),
                ]),
                add(vec![
                    resolver.field(parent, DesignField::X),
                    resolver.field(parent, DesignField::Width),
                ]),
            ),
            le(
                add(vec![
                    resolver.field(child, DesignField::Y),
                    resolver.field(child, DesignField::Height),
                    int(padding),
                ]),
                add(vec![
                    resolver.field(parent, DesignField::Y),
                    resolver.field(parent, DesignField::Height),
                ]),
            ),
        ],
    )
}

fn axis_capacity(
    resolver: &GeometryResolver,
    subjects: &[String],
    axis: DesignAxis,
    gap: i64,
    inset_start: i64,
    inset_end: i64,
) -> ConstraintExpr {
    let extent = match axis {
        DesignAxis::Horizontal => DesignField::Width,
        DesignAxis::Vertical => DesignField::Height,
    };
    let mut used = vec![int(inset_start)];
    for (index, item) in subjects[1..].iter().enumerate() {
        used.push(resolver.field(item, extent));
        if index + 1 < subjects.len() - 1 {
            used.push(int(gap));
        }
    }
    used.push(int(inset_end));

    le(add(used), resolver.field(&subjects[0], extent))
}

fn non_overlap(
    resolver: &GeometryResolver,
    first: &str,
    second: &str,
    minimum_gap: i64,
) -> ConstraintExpr {
    operation(
        ConstraintOp::Or,
        vec![
            before_axis(
                resolver,
                first,
                second,
                DesignField::X,
                DesignField::Width,
                minimum_gap,
            ),
            before_axis(
                resolver,
                second,
                first,
                DesignField::X,
                DesignField::Width,
                minimum_gap,
            ),
            before_axis(
                resolver,
                first,
                second,
                DesignField::Y,
                DesignField::Height,
                minimum_gap,
            ),
            before_axis(
                resolver,
                second,
                first,
                DesignField::Y,
                DesignField::Height,
                minimum_gap,
            ),
        ],
    )
}

fn before_axis(
    resolver: &GeometryResolver,
    first: &str,
    second: &str,
    position: DesignField,
    length: DesignField,
    gap: i64,
) -> ConstraintExpr {
    le(
        add(vec![
            resolver.field(first, position),
            resolver.field(first, length),
            int(gap),
        ]),
        resolver.field(second, position),
    )
}

fn aspect_ratio(
    resolver: &GeometryResolver,
    render: &str,
    source_width: i64,
    source_height: i64,
) -> ConstraintExpr {
    operation(
        ConstraintOp::Eq,
        vec![
            operation(
                ConstraintOp::Mul,
                vec![
                    resolver.field(render, DesignField::Width),
                    int(source_height),
                ],
            ),
            operation(
                ConstraintOp::Mul,
                vec![
                    resolver.field(render, DesignField::Height),
                    int(source_width),
                ],
            ),
        ],
    )
}

fn le(left: ConstraintExpr, right: ConstraintExpr) -> ConstraintExpr {
    operation(ConstraintOp::Le, vec![left, right])
}

fn add(args: Vec<ConstraintExpr>) -> ConstraintExpr {
    operation(ConstraintOp::Add, args)
}

fn operation(op: ConstraintOp, args: Vec<ConstraintExpr>) -> ConstraintExpr {
    ConstraintExpr::Op { op, args }
}

const fn int(value: i64) -> ConstraintExpr {
    ConstraintExpr::Int { value }
}

fn declared(id: &str, expr: ConstraintExpr) -> ConstraintItem {
    ConstraintItem::Declared(ConstraintDecl {
        id: Some(id.to_owned()),
        soft: false,
        weight: None,
        expr,
    })
}

struct GeometryResolver {
    scene: DesignScene,
    paths: BTreeMap<(String, DesignField), String>,
    variables: Vec<Variable>,
    unknowns: Vec<CompiledDesignUnknown>,
}

impl GeometryResolver {
    fn new(scene: &DesignScene, unknowns: &[DesignUnknown]) -> Self {
        let mut sorted = unknowns.iter().collect::<Vec<_>>();
        sorted.sort_by(|left, right| (&left.node, left.field).cmp(&(&right.node, right.field)));

        let mut paths = BTreeMap::new();
        let mut variables = Vec::with_capacity(sorted.len());
        let mut compiled_unknowns = Vec::with_capacity(sorted.len());
        for (index, unknown) in sorted.into_iter().enumerate() {
            let variable = format!("design_u_{index}");
            paths.insert((unknown.node.clone(), unknown.field), variable.clone());
            variables.push(Variable::IntRange {
                name: variable.clone(),
                min: unknown.min,
                max: unknown.max,
            });
            compiled_unknowns.push(CompiledDesignUnknown {
                variable,
                node: unknown.node.clone(),
                field: unknown.field,
            });
        }

        Self {
            scene: scene.clone(),
            paths,
            variables,
            unknowns: compiled_unknowns,
        }
    }

    fn field(&self, node: &str, field: DesignField) -> ConstraintExpr {
        if let Some(value) = self.scene.nodes[node].rect.value(field) {
            return int(value);
        }
        ConstraintExpr::Var {
            name: self.paths[&(node.to_owned(), field)].clone(),
        }
    }
}

/// Deterministic design compiler failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DesignCompileError {
    /// No predicate was selected.
    #[error("at least one design rule binding is required")]
    NoRules,
    /// Binding count exceeds the generic constraint budget.
    #[error("request has {count} design rule bindings; maximum is {max}")]
    TooManyRuleBindings { count: usize, max: usize },
    /// Verification accepts only complete supplied models.
    #[error("verification requires a complete model; remove {count} unknown declaration")]
    VerificationUnknowns { count: usize },
    /// Scene validation failed.
    #[error(transparent)]
    InvalidScene(#[from] DesignSceneError),
    /// The rule ID is absent from the built-in registry.
    #[error("unknown design rule `{rule_id}`")]
    UnknownRule { rule_id: String },
    /// A rule received the wrong number of ordered subjects.
    #[error("rule `{rule_id}` requires {expected} subjects, got {actual}")]
    InvalidSubjectArity {
        rule_id: String,
        expected: usize,
        actual: usize,
    },
    /// A variadic rule received fewer subjects than its minimum.
    #[error("rule `{rule_id}` requires at least {minimum} subjects, got {actual}")]
    TooFewSubjects {
        rule_id: String,
        minimum: usize,
        actual: usize,
    },
    /// A subject ID does not resolve in the scene.
    #[error("rule `{rule_id}` references unknown subject `{node}`")]
    UnknownSubject { rule_id: String, node: String },
    /// A parameter is not accepted by the selected rule.
    #[error("rule `{rule_id}` does not accept parameter `{parameter}`")]
    UnexpectedParameter {
        rule_id: String,
        parameter: &'static str,
    },
    /// A non-negative parameter was negative.
    #[error("rule `{rule_id}` parameter `{parameter}` must be non-negative, got {value}")]
    NegativeParameter {
        rule_id: String,
        parameter: &'static str,
        value: i64,
    },
    /// A required parameter was omitted.
    #[error("rule `{rule_id}` requires parameter `{parameter}`")]
    MissingParameter {
        rule_id: String,
        parameter: &'static str,
    },
    /// A required dimension-like parameter was not positive.
    #[error("rule `{rule_id}` parameter `{parameter}` must be positive, got {value}")]
    NonPositiveParameter {
        rule_id: String,
        parameter: &'static str,
        value: i64,
    },
    /// The generated generic request violated a backend invariant.
    #[error("compiled design rules produced an invalid solver request: {message}")]
    InvalidSolverRequest { message: String },
}
