//! Accessibility fact validation and lowering to typed solver constraints.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{FamilyCompilation, FamilyCompileError, ModelProjection, RuleFamilyCompiler},
        manifest_family_executable_rule_ids,
        primitives::{add, and, boolean, ge, int, le, mul, not, or, request, var},
        CompiledRule, RuleSolveMode,
    },
    types::{Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS, MAX_VARIABLES},
};

use super::builtin_registry;

const DEFAULT_TARGET_SIZE: i64 = 24;
const DEFAULT_CONTRAST_HUNDREDTHS: i64 = 450;
const LUMINANCE_SCALE: i64 = 100_000;
const LUMINANCE_OFFSET: i64 = 5_000;

/// Accessibility compiler registered behind `solve_rules`.
pub static COMPILER: AccessibilityCompiler = AccessibilityCompiler;

/// Stateless accessibility family compiler.
pub struct AccessibilityCompiler;

impl RuleFamilyCompiler for AccessibilityCompiler {
    fn id(&self) -> &'static str {
        "accessibility"
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError> {
        compile(input).map_err(|message| FamilyCompileError::new(self.id(), message))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityRequest {
    #[serde(rename = "family")]
    _family: String,
    mode: RuleSolveMode,
    rules: Vec<AccessibilityRuleBinding>,
    scene: AccessibilityScene,
    #[serde(default)]
    unknowns: Vec<AccessibilityUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityRuleBinding {
    rule_id: String,
    subjects: Vec<String>,
    #[serde(default)]
    parameters: AccessibilityParameters,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityParameters {
    minimum_width: Option<i64>,
    minimum_height: Option<i64>,
    minimum_ratio_hundredths: Option<i64>,
    exception: Option<AccessibilityException>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityException {
    kind: AccessibilityExceptionKind,
    evidence: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AccessibilityExceptionKind {
    Spacing,
    Inline,
    Equivalent,
    UserAgent,
    Essential,
    TwoDimensional,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityScene {
    viewport: AccessibilitySize,
    elements: BTreeMap<String, AccessibilityElement>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilitySize {
    width: i64,
    height: i64,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityElement {
    rect: Option<AccessibilityRect>,
    foreground_luminance: Option<i64>,
    background_luminance: Option<i64>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityRect {
    x: Option<i64>,
    y: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityUnknown {
    subject: String,
    field: AccessibilityField,
    min: i64,
    max: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum AccessibilityField {
    X,
    Y,
    Width,
    Height,
    ForegroundLuminance,
    BackgroundLuminance,
}

impl AccessibilityField {
    fn path(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Width => "width",
            Self::Height => "height",
            Self::ForegroundLuminance => "foreground_luminance",
            Self::BackgroundLuminance => "background_luminance",
        }
    }
}

fn compile(input: Value) -> Result<FamilyCompilation, String> {
    let input: AccessibilityRequest =
        serde_json::from_value(input).map_err(|error| error.to_string())?;
    if input.rules.is_empty() {
        return Err("at least one accessibility rule binding is required".to_owned());
    }
    if input.rules.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "request has {} accessibility rule bindings; maximum is {MAX_CONSTRAINTS}",
            input.rules.len()
        ));
    }
    if input.mode == RuleSolveMode::Verify && !input.unknowns.is_empty() {
        return Err(
            "verification requires a complete model; remove unknown declarations".to_owned(),
        );
    }
    validate_scene(&input.scene, &input.unknowns)?;
    let resolver = AccessibilityResolver::new(input.scene, &input.unknowns);
    let rules = input
        .rules
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            compile_binding(binding, &resolver)
                .map(|predicate| CompiledRule::new(binding.rule_id.clone(), index, predicate))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let solver_request = request(
        "accessibility",
        resolver.variables,
        &rules,
        input.timeout_ms,
        input.persist,
        input.include_smt,
    );
    solver_request
        .validate()
        .map_err(|error| format!("compiled accessibility rules are invalid: {error}"))?;

    Ok(FamilyCompilation {
        mode: input.mode,
        request: solver_request,
        rules,
        projections: resolver.projections,
    })
}

fn validate_scene(
    scene: &AccessibilityScene,
    unknowns: &[AccessibilityUnknown],
) -> Result<(), String> {
    if scene.viewport.width <= 0 || scene.viewport.height <= 0 {
        return Err("accessibility viewport dimensions must be positive".to_owned());
    }
    if scene.elements.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "accessibility scene has too many elements; maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if unknowns.len() > MAX_VARIABLES {
        return Err(format!(
            "accessibility request has too many unknowns; maximum is {MAX_VARIABLES}"
        ));
    }

    let mut paths = BTreeSet::new();
    for unknown in unknowns {
        if !scene.elements.contains_key(&unknown.subject) {
            return Err(format!(
                "unknown accessibility subject `{}`",
                unknown.subject
            ));
        }
        if unknown.min > unknown.max {
            return Err(format!(
                "unknown {}.{} has min greater than max",
                unknown.subject,
                unknown.field.path()
            ));
        }
        if matches!(
            unknown.field,
            AccessibilityField::Width | AccessibilityField::Height
        ) && unknown.min <= 0
        {
            return Err(format!(
                "unknown {}.{} must have a positive lower bound",
                unknown.subject,
                unknown.field.path()
            ));
        }
        if matches!(
            unknown.field,
            AccessibilityField::ForegroundLuminance | AccessibilityField::BackgroundLuminance
        ) && (unknown.min < 0 || unknown.max > LUMINANCE_SCALE)
        {
            return Err(format!(
                "unknown {}.{} must stay within 0..={LUMINANCE_SCALE}",
                unknown.subject,
                unknown.field.path()
            ));
        }
        if accessibility_value(&scene.elements[&unknown.subject], unknown.field).is_some() {
            return Err(format!(
                "{}.{} is already fixed",
                unknown.subject,
                unknown.field.path()
            ));
        }
        if !paths.insert((unknown.subject.clone(), unknown.field)) {
            return Err(format!(
                "duplicate accessibility unknown {}.{}",
                unknown.subject,
                unknown.field.path()
            ));
        }
    }

    for (subject, element) in &scene.elements {
        for (field, value) in [
            (
                AccessibilityField::ForegroundLuminance,
                element.foreground_luminance,
            ),
            (
                AccessibilityField::BackgroundLuminance,
                element.background_luminance,
            ),
        ] {
            if value.is_some_and(|value| !(0..=LUMINANCE_SCALE).contains(&value)) {
                return Err(format!(
                    "{subject}.{} must stay within 0..={LUMINANCE_SCALE}",
                    field.path()
                ));
            }
        }
        if let Some(rect) = &element.rect {
            for (field, value) in [
                (AccessibilityField::Width, rect.width),
                (AccessibilityField::Height, rect.height),
            ] {
                if value.is_some_and(|value| value <= 0) {
                    return Err(format!("{subject}.{} must be positive", field.path()));
                }
            }
        }
    }
    Ok(())
}

fn compile_binding(
    binding: &AccessibilityRuleBinding,
    resolver: &AccessibilityResolver,
) -> Result<crate::types::ConstraintExpr, String> {
    if builtin_registry().rule(&binding.rule_id).is_none() {
        return Err(format!("unknown accessibility rule `{}`", binding.rule_id));
    }
    match binding.rule_id.as_str() {
        "a11y.target_size" => {
            require_subjects(binding, resolver, 1)?;
            reject_ratio(binding)?;
            let minimum_width = positive(
                "minimum_width",
                binding
                    .parameters
                    .minimum_width
                    .unwrap_or(DEFAULT_TARGET_SIZE),
            )?;
            let minimum_height = positive(
                "minimum_height",
                binding
                    .parameters
                    .minimum_height
                    .unwrap_or(DEFAULT_TARGET_SIZE),
            )?;
            let exception = exception_applies(
                binding,
                &[
                    AccessibilityExceptionKind::Spacing,
                    AccessibilityExceptionKind::Inline,
                    AccessibilityExceptionKind::Equivalent,
                    AccessibilityExceptionKind::UserAgent,
                    AccessibilityExceptionKind::Essential,
                ],
            )?;
            let subject = &binding.subjects[0];
            Ok(or(vec![
                boolean(exception),
                and(vec![
                    ge(
                        resolver.field(subject, AccessibilityField::Width)?,
                        int(minimum_width),
                    ),
                    ge(
                        resolver.field(subject, AccessibilityField::Height)?,
                        int(minimum_height),
                    ),
                ]),
            ]))
        }
        "a11y.focus_not_obscured" => {
            require_subjects(binding, resolver, 2)?;
            reject_all_parameters(binding)?;
            let focused = &binding.subjects[0];
            let obscurer = &binding.subjects[1];
            Ok(not(and(vec![
                le(
                    resolver.field(obscurer, AccessibilityField::X)?,
                    resolver.field(focused, AccessibilityField::X)?,
                ),
                le(
                    resolver.field(obscurer, AccessibilityField::Y)?,
                    resolver.field(focused, AccessibilityField::Y)?,
                ),
                le(
                    add(vec![
                        resolver.field(focused, AccessibilityField::X)?,
                        resolver.field(focused, AccessibilityField::Width)?,
                    ]),
                    add(vec![
                        resolver.field(obscurer, AccessibilityField::X)?,
                        resolver.field(obscurer, AccessibilityField::Width)?,
                    ]),
                ),
                le(
                    add(vec![
                        resolver.field(focused, AccessibilityField::Y)?,
                        resolver.field(focused, AccessibilityField::Height)?,
                    ]),
                    add(vec![
                        resolver.field(obscurer, AccessibilityField::Y)?,
                        resolver.field(obscurer, AccessibilityField::Height)?,
                    ]),
                ),
            ])))
        }
        "a11y.reflow" => {
            require_subjects(binding, resolver, 1)?;
            reject_target_parameters(binding)?;
            reject_ratio(binding)?;
            let exception =
                exception_applies(binding, &[AccessibilityExceptionKind::TwoDimensional])?;
            let subject = &binding.subjects[0];
            let x = resolver.field(subject, AccessibilityField::X)?;
            Ok(or(vec![
                boolean(exception),
                and(vec![
                    ge(x.clone(), int(0)),
                    le(
                        add(vec![x, resolver.field(subject, AccessibilityField::Width)?]),
                        int(resolver.scene.viewport.width),
                    ),
                ]),
            ]))
        }
        "a11y.text_contrast" => {
            require_subjects(binding, resolver, 1)?;
            reject_target_parameters(binding)?;
            reject_exception(binding)?;
            let ratio = positive(
                "minimum_ratio_hundredths",
                binding
                    .parameters
                    .minimum_ratio_hundredths
                    .unwrap_or(DEFAULT_CONTRAST_HUNDREDTHS),
            )?;
            let foreground = resolver.field(
                &binding.subjects[0],
                AccessibilityField::ForegroundLuminance,
            )?;
            let background = resolver.field(
                &binding.subjects[0],
                AccessibilityField::BackgroundLuminance,
            )?;
            Ok(or(vec![
                contrast_branch(foreground.clone(), background.clone(), ratio),
                contrast_branch(background, foreground, ratio),
            ]))
        }
        _ => Err(format!("unknown accessibility rule `{}`", binding.rule_id)),
    }
}

fn contrast_branch(
    lighter: crate::types::ConstraintExpr,
    darker: crate::types::ConstraintExpr,
    ratio: i64,
) -> crate::types::ConstraintExpr {
    and(vec![
        ge(lighter.clone(), darker.clone()),
        ge(
            mul(vec![add(vec![lighter, int(LUMINANCE_OFFSET)]), int(100)]),
            mul(vec![add(vec![darker, int(LUMINANCE_OFFSET)]), int(ratio)]),
        ),
    ])
}

fn require_subjects(
    binding: &AccessibilityRuleBinding,
    resolver: &AccessibilityResolver,
    expected: usize,
) -> Result<(), String> {
    if binding.subjects.len() != expected {
        return Err(format!(
            "rule `{}` requires {expected} subjects, got {}",
            binding.rule_id,
            binding.subjects.len()
        ));
    }
    for subject in &binding.subjects {
        if !resolver.scene.elements.contains_key(subject) {
            return Err(format!(
                "rule `{}` references unknown subject `{subject}`",
                binding.rule_id
            ));
        }
    }
    Ok(())
}

fn positive(parameter: &str, value: i64) -> Result<i64, String> {
    if value <= 0 {
        return Err(format!("parameter `{parameter}` must be positive"));
    }
    Ok(value)
}

fn exception_applies(
    binding: &AccessibilityRuleBinding,
    accepted: &[AccessibilityExceptionKind],
) -> Result<bool, String> {
    let Some(exception) = &binding.parameters.exception else {
        return Ok(false);
    };
    if exception.evidence.trim().is_empty() {
        return Err("exception evidence must not be empty".to_owned());
    }
    if !accepted
        .iter()
        .any(|accepted| std::mem::discriminant(accepted) == std::mem::discriminant(&exception.kind))
    {
        return Err(format!(
            "rule `{}` does not accept this exception kind",
            binding.rule_id
        ));
    }
    Ok(true)
}

fn reject_target_parameters(binding: &AccessibilityRuleBinding) -> Result<(), String> {
    if binding.parameters.minimum_width.is_some() || binding.parameters.minimum_height.is_some() {
        return Err(format!(
            "rule `{}` does not accept target-size parameters",
            binding.rule_id
        ));
    }
    Ok(())
}

fn reject_ratio(binding: &AccessibilityRuleBinding) -> Result<(), String> {
    if binding.parameters.minimum_ratio_hundredths.is_some() {
        return Err(format!(
            "rule `{}` does not accept `minimum_ratio_hundredths`",
            binding.rule_id
        ));
    }
    Ok(())
}

fn reject_exception(binding: &AccessibilityRuleBinding) -> Result<(), String> {
    if binding.parameters.exception.is_some() {
        return Err(format!(
            "rule `{}` does not accept an exception",
            binding.rule_id
        ));
    }
    Ok(())
}

fn reject_all_parameters(binding: &AccessibilityRuleBinding) -> Result<(), String> {
    reject_target_parameters(binding)?;
    reject_ratio(binding)?;
    reject_exception(binding)
}

struct AccessibilityResolver {
    scene: AccessibilityScene,
    paths: BTreeMap<(String, AccessibilityField), String>,
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
}

fn accessibility_value(element: &AccessibilityElement, field: AccessibilityField) -> Option<i64> {
    match field {
        AccessibilityField::X => element.rect.as_ref()?.x,
        AccessibilityField::Y => element.rect.as_ref()?.y,
        AccessibilityField::Width => element.rect.as_ref()?.width,
        AccessibilityField::Height => element.rect.as_ref()?.height,
        AccessibilityField::ForegroundLuminance => element.foreground_luminance,
        AccessibilityField::BackgroundLuminance => element.background_luminance,
    }
}

impl AccessibilityResolver {
    fn new(scene: AccessibilityScene, unknowns: &[AccessibilityUnknown]) -> Self {
        let mut sorted = unknowns.iter().collect::<Vec<_>>();
        sorted
            .sort_by(|left, right| (&left.subject, left.field).cmp(&(&right.subject, right.field)));
        let mut paths = BTreeMap::new();
        let mut variables = Vec::new();
        let mut projections = Vec::new();
        for (index, unknown) in sorted.into_iter().enumerate() {
            let variable = format!("accessibility_u_{index}");
            paths.insert((unknown.subject.clone(), unknown.field), variable.clone());
            variables.push(Variable::IntRange {
                name: variable.clone(),
                min: unknown.min,
                max: unknown.max,
            });
            projections.push(ModelProjection {
                variable,
                subject: unknown.subject.clone(),
                field: unknown.field.path().to_owned(),
            });
        }
        Self {
            scene,
            paths,
            variables,
            projections,
        }
    }

    fn field(
        &self,
        subject: &str,
        field: AccessibilityField,
    ) -> Result<crate::types::ConstraintExpr, String> {
        let element = self
            .scene
            .elements
            .get(subject)
            .ok_or_else(|| format!("unknown accessibility subject `{subject}`"))?;
        let value = accessibility_value(element, field);
        if let Some(value) = value {
            return Ok(int(value));
        }
        self.paths
            .get(&(subject.to_owned(), field))
            .cloned()
            .map(var)
            .ok_or_else(|| {
                format!(
                    "{}.{} is missing and has no unknown declaration",
                    subject,
                    field.path()
                )
            })
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn input_schema() -> Value {
    let rule_ids = manifest_family_executable_rule_ids("accessibility")
        .expect("accessibility manifest executable rule IDs");
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "accessibility"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": rule_ids},
                        "subjects": {"type": "array", "maxItems": 2, "items": {"type": "string"}},
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "minimum_width": {"type": "integer", "minimum": 1},
                                "minimum_height": {"type": "integer", "minimum": 1},
                                "minimum_ratio_hundredths": {"type": "integer", "minimum": 1},
                                "exception": {
                                    "type": "object",
                                    "properties": {
                                        "kind": {"type": "string", "enum": ["spacing", "inline", "equivalent", "user_agent", "essential", "two_dimensional"]},
                                        "evidence": {"type": "string", "minLength": 1}
                                    },
                                    "required": ["kind", "evidence"],
                                    "additionalProperties": false
                                }
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["rule_id", "subjects"],
                    "additionalProperties": false
                }
            },
            "scene": {
                "type": "object",
                "properties": {
                    "viewport": {
                        "type": "object",
                        "properties": {"width": {"type": "integer", "minimum": 1}, "height": {"type": "integer", "minimum": 1}},
                        "required": ["width", "height"],
                        "additionalProperties": false
                    },
                    "elements": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "rect": {
                                    "type": "object",
                                    "properties": {
                                        "x": {"type": ["integer", "null"]}, "y": {"type": ["integer", "null"]},
                                        "width": {"type": ["integer", "null"]}, "height": {"type": ["integer", "null"]}
                                    },
                                    "additionalProperties": false
                                },
                                "foreground_luminance": {"type": ["integer", "null"], "minimum": 0, "maximum": LUMINANCE_SCALE},
                                "background_luminance": {"type": ["integer", "null"], "minimum": 0, "maximum": LUMINANCE_SCALE}
                            },
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["viewport", "elements"],
                "additionalProperties": false
            },
            "unknowns": {
                "type": "array", "maxItems": MAX_VARIABLES, "default": [],
                "items": {
                    "type": "object",
                    "properties": {
                        "subject": {"type": "string"},
                        "field": {"type": "string", "enum": ["x", "y", "width", "height", "foreground_luminance", "background_luminance"]},
                        "min": {"type": "integer"}, "max": {"type": "integer"}
                    },
                    "required": ["subject", "field", "min", "max"],
                    "additionalProperties": false
                }
            },
            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS},
            "persist": {"type": "boolean", "default": false},
            "include_smt": {"type": "boolean", "default": false}
        },
        "required": ["family", "mode", "rules", "scene"],
        "additionalProperties": false
    })
}
