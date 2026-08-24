//! Runtime projections over the canonical embedded rule manifest bundle.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::LazyLock,
};

use serde_json::{Map, Value};

use super::{
    catalog::{
        ExecutionKind, LlmEncoding, RegistryError, RuleAuthority, RuleDefinition, RuleExample,
        RuleExamples, RuleFamily, RuleGuidance, RuleProfile, RuleRegistry, RuleStrength,
        SolverEncoding,
    },
    manifest_format::{
        validate_manifest_bundle, AvailabilityV1, ConformanceVectorsV1, ExecutionKindV1,
        ManifestBundleV1, ManifestValidationError, NativeHandlerV1, NativeObjectValidatorV1,
        ParameterContractV1, ParameterKindV1, RuleManifestV1, RuleStrengthV1, SubjectCardinalityV1,
        SubjectContractV1,
    },
};

const EMBEDDED_RULE_MANIFESTS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/spur_rule_manifests_v1.json"));

static MANIFEST_DATA: LazyLock<ManifestData> = LazyLock::new(|| {
    load_manifest_data(EMBEDDED_RULE_MANIFESTS)
        .unwrap_or_else(|error| panic!("embedded rule manifest initialization failed: {error}"))
});

#[derive(Debug)]
struct ManifestData {
    bundle: ManifestBundleV1,
    registry: RuleRegistry,
    family_registries: BTreeMap<String, RuleRegistry>,
    executable_rule_ids: Vec<String>,
    family_executable_rule_ids: BTreeMap<String, Vec<String>>,
}

/// The manifest-owned request-shape data for one rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestRuleContract<'a> {
    /// Whether the manifest declares a hard constraint or an optimization objective.
    pub execution_kind: ExecutionKindV1,
    /// Accepted subject-list cardinality.
    pub subjects: &'a SubjectContractV1,
    /// Accepted parameter names, kinds, defaults, and static bounds.
    pub parameters: &'a [ParameterContractV1],
}

/// A manifest-validated rule binding ready for closed native dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedBinding {
    /// Whether the executable manifest declares a constraint or objective.
    pub execution_kind: ExecutionKindV1,
    /// Caller parameters after manifest defaults have been applied.
    pub parameters: Map<String, Value>,
    /// Exhaustive native handler selected by the executable manifest.
    pub handler: NativeHandlerV1,
}

/// Returns the process-wide catalog converted from the embedded manifest bundle.
#[must_use]
pub fn manifest_registry() -> &'static RuleRegistry {
    &MANIFEST_DATA.registry
}

/// Returns one family-owned catalog projection by exact family ID.
#[must_use]
pub fn manifest_family_registry(family_id: &str) -> Option<&'static RuleRegistry> {
    MANIFEST_DATA.family_registries.get(family_id)
}

/// Returns every executable manifest rule ID in stable order.
#[must_use]
pub fn manifest_executable_rule_ids() -> &'static [String] {
    &MANIFEST_DATA.executable_rule_ids
}

/// Returns one family's executable manifest rule IDs in stable order.
#[must_use]
pub fn manifest_family_executable_rule_ids(family_id: &str) -> Option<&'static [String]> {
    MANIFEST_DATA
        .family_executable_rule_ids
        .get(family_id)
        .map(Vec::as_slice)
}

/// Looks up the manifest-owned subject and parameter contract for one rule.
#[must_use]
pub fn manifest_rule_contract(rule_id: &str) -> Option<ManifestRuleContract<'static>> {
    rule_manifest(rule_id).map(|rule| ManifestRuleContract {
        execution_kind: rule.execution_kind,
        subjects: &rule.subjects,
        parameters: &rule.parameters,
    })
}

/// Looks up the closed native handler for one executable rule.
#[must_use]
pub fn manifest_rule_handler(rule_id: &str) -> Option<NativeHandlerV1> {
    let rule = rule_manifest(rule_id)?;
    rule.handler
}

/// Looks up executable conformance vectors without exposing catalog examples as requests.
#[must_use]
pub fn manifest_conformance_vectors(rule_id: &str) -> Option<&'static ConformanceVectorsV1> {
    let rule = rule_manifest(rule_id)?;
    rule.conformance.as_ref()
}

pub(super) fn manifest_rule_violation_diagnostic(rule_id: &str) -> Option<&'static str> {
    rule_manifest(rule_id)?
        .examples
        .invalid
        .expected_diagnostic
        .as_deref()
}

/// Validates manifest-representable binding shape and applies declared defaults.
///
/// Fact-dependent, graph, geometry, cross-field, and solver semantics remain the
/// responsibility of the selected native family handler.
pub fn validate_binding_contract(
    rule_id: &str,
    subjects: &[String],
    parameters: &Map<String, Value>,
) -> Result<ValidatedBinding, String> {
    let rule =
        rule_manifest(rule_id).ok_or_else(|| format!("unknown manifest rule `{rule_id}`"))?;
    validate_binding_contract_for_rule(rule, subjects, parameters)
}

fn validate_binding_contract_for_rule(
    rule: &RuleManifestV1,
    subjects: &[String],
    parameters: &Map<String, Value>,
) -> Result<ValidatedBinding, String> {
    let rule_id = &rule.id;
    if !rule.is_executable() {
        return Err(format!("manifest rule `{rule_id}` is not executable"));
    }
    let Some(handler) = rule.handler else {
        return Err(format!("manifest rule `{rule_id}` is not executable"));
    };

    validate_subject_count(rule_id, &rule.subjects, subjects.len())?;

    for name in parameters.keys() {
        if !rule
            .parameters
            .iter()
            .any(|parameter| parameter.name == *name)
        {
            return Err(format!(
                "rule `{rule_id}` does not accept parameter `{name}`"
            ));
        }
    }

    let mut normalized = Map::new();
    for parameter in &rule.parameters {
        let value = match parameters.get(&parameter.name) {
            Some(value) => Some(value),
            None => parameter.default.as_ref(),
        };
        let Some(value) = value else {
            if parameter.required {
                return Err(format!(
                    "rule `{rule_id}` requires parameter `{}`",
                    parameter.name
                ));
            }
            continue;
        };

        validate_parameter_value(rule_id, parameter, value)?;
        normalized.insert(parameter.name.clone(), value.clone());
    }

    Ok(ValidatedBinding {
        execution_kind: rule.execution_kind,
        parameters: normalized,
        handler,
    })
}

fn validate_subject_count(
    rule_id: &str,
    contract: &SubjectContractV1,
    actual: usize,
) -> Result<(), String> {
    match contract.cardinality {
        SubjectCardinalityV1::Exact { count } if actual != count => Err(format!(
            "rule `{rule_id}` requires {count} subjects, got {actual}"
        )),
        SubjectCardinalityV1::AtLeast { count } if actual < count => Err(format!(
            "rule `{rule_id}` requires at least {count} subjects, got {actual}"
        )),
        SubjectCardinalityV1::Range { minimum, maximum }
            if !(minimum..=maximum).contains(&actual) =>
        {
            Err(format!(
                "rule `{rule_id}` requires between {minimum} and {maximum} subjects, got {actual}"
            ))
        }
        SubjectCardinalityV1::Exact { .. }
        | SubjectCardinalityV1::AtLeast { .. }
        | SubjectCardinalityV1::Range { .. } => Ok(()),
    }
}

fn validate_parameter_value(
    rule_id: &str,
    parameter: &ParameterContractV1,
    value: &Value,
) -> Result<(), String> {
    let name = &parameter.name;
    match parameter.kind {
        ParameterKindV1::Integer => {
            let integer = value
                .as_i64()
                .ok_or_else(|| format!("rule `{rule_id}` parameter `{name}` must be an integer"))?;
            if let Some(minimum) = parameter.minimum {
                if integer < minimum {
                    return Err(format!(
                        "rule `{rule_id}` parameter `{name}` must be at least {minimum}"
                    ));
                }
            }
            if let Some(maximum) = parameter.maximum {
                if integer > maximum {
                    return Err(format!(
                        "rule `{rule_id}` parameter `{name}` must be at most {maximum}"
                    ));
                }
            }
        }
        ParameterKindV1::Boolean if !value.is_boolean() => {
            return Err(format!(
                "rule `{rule_id}` parameter `{name}` must be a boolean"
            ));
        }
        ParameterKindV1::String if !value.is_string() => {
            return Err(format!(
                "rule `{rule_id}` parameter `{name}` must be a string"
            ));
        }
        ParameterKindV1::StringEnum => {
            let string = value
                .as_str()
                .ok_or_else(|| format!("rule `{rule_id}` parameter `{name}` must be a string"))?;
            if !parameter.values.iter().any(|allowed| allowed == string) {
                return Err(format!(
                    "rule `{rule_id}` parameter `{name}` must be one of {:?}",
                    parameter.values
                ));
            }
        }
        ParameterKindV1::StringArray => {
            let values = value.as_array().ok_or_else(|| {
                format!("rule `{rule_id}` parameter `{name}` must be an array of strings")
            })?;
            if values.iter().any(|value| !value.is_string()) {
                return Err(format!(
                    "rule `{rule_id}` parameter `{name}` must be an array of strings"
                ));
            }
            if let Some(minimum) = parameter.min_items {
                if values.len() < minimum {
                    return Err(format!(
                        "rule `{rule_id}` parameter `{name}` must contain at least {minimum} items"
                    ));
                }
            }
            if let Some(maximum) = parameter.max_items {
                if values.len() > maximum {
                    return Err(format!(
                        "rule `{rule_id}` parameter `{name}` must contain at most {maximum} items"
                    ));
                }
            }
        }
        ParameterKindV1::NativeObject => {
            let validator = parameter.validator.ok_or_else(|| {
                format!("rule `{rule_id}` parameter `{name}` has no native object validator")
            })?;
            validate_native_object(rule_id, name, validator, value)?;
        }
        ParameterKindV1::Boolean | ParameterKindV1::String => {}
    }
    Ok(())
}

fn validate_native_object(
    rule_id: &str,
    parameter_name: &str,
    validator: NativeObjectValidatorV1,
    value: &Value,
) -> Result<(), String> {
    match validator {
        NativeObjectValidatorV1::AccessibilityException => {
            validate_accessibility_exception(rule_id, parameter_name, value)
        }
    }
}

fn validate_accessibility_exception(
    rule_id: &str,
    parameter_name: &str,
    value: &Value,
) -> Result<(), String> {
    let object = value.as_object().ok_or_else(|| {
        format!("rule `{rule_id}` parameter `{parameter_name}` must be an object")
    })?;
    for field in object.keys() {
        if !matches!(field.as_str(), "kind" | "evidence") {
            return Err(format!(
                "rule `{rule_id}` parameter `{parameter_name}` does not accept field `{field}`"
            ));
        }
    }

    let kind = object
        .get("kind")
        .ok_or_else(|| {
            format!("rule `{rule_id}` parameter `{parameter_name}` requires field `kind`")
        })?
        .as_str()
        .ok_or_else(|| {
            format!("rule `{rule_id}` parameter `{parameter_name}.kind` must be a string")
        })?;
    if !matches!(
        kind,
        "spacing" | "inline" | "equivalent" | "user_agent" | "essential" | "two_dimensional"
    ) {
        return Err(format!(
            "rule `{rule_id}` parameter `{parameter_name}.kind` is not a supported accessibility exception kind"
        ));
    }

    object
        .get("evidence")
        .ok_or_else(|| {
            format!("rule `{rule_id}` parameter `{parameter_name}` requires field `evidence`")
        })?
        .as_str()
        .ok_or_else(|| {
            format!("rule `{rule_id}` parameter `{parameter_name}.evidence` must be a string")
        })?;

    Ok(())
}

fn rule_manifest(rule_id: &str) -> Option<&'static RuleManifestV1> {
    MANIFEST_DATA
        .bundle
        .rules
        .binary_search_by_key(&rule_id, |rule| rule.id.as_str())
        .ok()
        .map(|index| &MANIFEST_DATA.bundle.rules[index])
}

fn load_manifest_data(source: &str) -> Result<ManifestData, ManifestLoadError> {
    let mut bundle =
        serde_json::from_str::<ManifestBundleV1>(source).map_err(ManifestLoadError::Deserialize)?;
    bundle
        .families
        .sort_by(|left, right| left.id.cmp(&right.id));
    for family in &mut bundle.families {
        family
            .profiles
            .sort_by(|left, right| left.id.cmp(&right.id));
    }
    bundle.rules.sort_by(|left, right| left.id.cmp(&right.id));

    validate_manifest_bundle(&bundle).map_err(ManifestLoadError::Validate)?;
    validate_catalog_versions(&bundle)?;
    validate_handler_coverage(&bundle)?;

    let registry = registry_from_bundle(&bundle, None)?;
    let mut family_registries = BTreeMap::new();
    let mut family_executable_rule_ids = BTreeMap::new();
    for family in &bundle.families {
        family_registries.insert(
            family.id.clone(),
            registry_from_bundle(&bundle, Some(&family.id))?,
        );
        family_executable_rule_ids.insert(
            family.id.clone(),
            bundle
                .rules
                .iter()
                .filter(|rule| rule.family == family.id && rule.handler.is_some())
                .map(|rule| rule.id.clone())
                .collect(),
        );
    }
    let executable_rule_ids = bundle
        .rules
        .iter()
        .filter(|rule| rule.handler.is_some())
        .map(|rule| rule.id.clone())
        .collect();

    Ok(ManifestData {
        bundle,
        registry,
        family_registries,
        executable_rule_ids,
        family_executable_rule_ids,
    })
}

fn validate_catalog_versions(bundle: &ManifestBundleV1) -> Result<(), ManifestLoadError> {
    for family in &bundle.families {
        if family.family_version != 1 {
            return conversion_error(format!(
                "family `{}` uses family_version `{}`; the runtime catalog supports only `1`",
                family.id, family.family_version
            ));
        }
        for profile in &family.profiles {
            if profile.profile_version != 1 {
                return conversion_error(format!(
                    "profile `{}` uses profile_version `{}`; the runtime catalog supports only `1`",
                    profile.id, profile.profile_version
                ));
            }
        }
    }
    for rule in &bundle.rules {
        if rule.rule_version != 1 {
            return conversion_error(format!(
                "rule `{}` uses rule_version `{}`; the runtime catalog supports only `1`",
                rule.id, rule.rule_version
            ));
        }
    }
    Ok(())
}

fn validate_handler_coverage(bundle: &ManifestBundleV1) -> Result<(), ManifestLoadError> {
    let actual = bundle
        .rules
        .iter()
        .filter_map(|rule| rule.handler)
        .collect::<BTreeSet<_>>();
    let expected = bundle
        .rules
        .iter()
        .filter(|rule| rule.is_executable())
        .filter_map(|rule| rule.handler)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return conversion_error(format!(
            "native handler coverage mismatch: expected {expected:?}, found {actual:?}"
        ));
    }
    Ok(())
}

fn registry_from_bundle(
    bundle: &ManifestBundleV1,
    family_filter: Option<&str>,
) -> Result<RuleRegistry, ManifestLoadError> {
    let selected_families = bundle
        .families
        .iter()
        .filter(|family| family_filter.is_none_or(|id| family.id == id));

    let mut families = Vec::new();
    let mut profiles = Vec::new();
    for family in selected_families {
        families.push(RuleFamily::new(
            family.id.clone(),
            family.summary.clone(),
            family.profiles.iter().map(|profile| profile.id.clone()),
        ));
        for profile in &family.profiles {
            profiles.push(RuleProfile::new(
                profile.id.clone(),
                family.id.clone(),
                profile.summary.clone(),
                bundle
                    .rules
                    .iter()
                    .filter(|rule| rule.family == family.id && rule.profile == profile.id)
                    .map(|rule| rule.id.clone()),
            ));
        }
    }

    let rules = bundle
        .rules
        .iter()
        .filter(|rule| family_filter.is_none_or(|id| rule.family == id))
        .map(convert_rule)
        .collect::<Result<Vec<_>, _>>()?;

    RuleRegistry::new(1, families, profiles, rules).map_err(ManifestLoadError::Registry)
}

fn convert_rule(rule: &RuleManifestV1) -> Result<RuleDefinition, ManifestLoadError> {
    let authorities = rule
        .authorities
        .iter()
        .map(|authority| {
            RuleAuthority::new(
                authority.kind.clone(),
                authority.title.clone(),
                authority.url.clone(),
            )
        })
        .collect::<Vec<_>>();
    let llm_encoding = LlmEncoding::new(
        rule.llm_encoding.effectiveness.clone(),
        rule.llm_encoding.problem_shapes.clone(),
        rule.llm_encoding.encode_steps.clone(),
        rule.llm_encoding.anti_patterns.clone(),
        rule.llm_encoding.escalate_when.clone(),
    );

    let guidance = match (rule.availability, rule.execution_kind, rule.strength) {
        (AvailabilityV1::Implemented, ExecutionKindV1::Constraint, RuleStrengthV1::Hard) => {
            RuleGuidance::implemented_hard(
                authorities,
                rule.requires.clone(),
                llm_encoding,
                SolverEncoding::new(
                    rule.solver_encoding.theory.clone(),
                    rule.solver_encoding.verification.clone(),
                    rule.solver_encoding.synthesis.clone(),
                    rule.solver_encoding.formula.clone(),
                ),
                convert_examples(rule),
            )
        }
        (AvailabilityV1::Implemented, ExecutionKindV1::Objective, strength) => {
            RuleGuidance::implemented_objective(
                convert_strength(strength),
                authorities,
                rule.requires.clone(),
                llm_encoding,
                SolverEncoding::new(
                    rule.solver_encoding.theory.clone(),
                    rule.solver_encoding.verification.clone(),
                    rule.solver_encoding.synthesis.clone(),
                    rule.solver_encoding.formula.clone(),
                ),
                convert_examples(rule),
            )
        }
        (AvailabilityV1::CapabilityUnavailable, execution_kind, strength) => {
            ensure_unavailable_projection_is_lossless(rule)?;
            let Some(reason) = rule.availability_reason.clone() else {
                return conversion_error(format!(
                    "catalog-only rule `{}` is missing its validated availability reason",
                    rule.id
                ));
            };
            RuleGuidance::capability_unavailable(
                reason,
                convert_execution_kind(execution_kind),
                convert_strength(strength),
                authorities,
                rule.requires.clone(),
                llm_encoding,
            )
        }
        (availability, execution_kind, strength) => {
            return conversion_error(format!(
                "rule `{}` uses unsupported catalog projection `{availability:?}/{execution_kind:?}/{strength:?}`",
                rule.id
            ));
        }
    };

    Ok(RuleDefinition::new(
        rule.id.clone(),
        rule.family.clone(),
        rule.profile.clone(),
        rule.primitive.clone(),
        rule.summary.clone(),
    )
    .with_guidance(guidance))
}

fn convert_examples(rule: &RuleManifestV1) -> RuleExamples {
    RuleExamples::new(
        RuleExample::new(
            rule.examples.valid.facts.clone(),
            rule.examples.valid.expectation.clone(),
            rule.examples.valid.expected_diagnostic.clone(),
        ),
        RuleExample::new(
            rule.examples.invalid.facts.clone(),
            rule.examples.invalid.expectation.clone(),
            rule.examples.invalid.expected_diagnostic.clone(),
        ),
    )
}

fn ensure_unavailable_projection_is_lossless(
    rule: &RuleManifestV1,
) -> Result<(), ManifestLoadError> {
    let solver = &rule.solver_encoding;
    let examples = &rule.examples;
    let empty = solver.theory.is_empty()
        && solver.verification.is_empty()
        && solver.synthesis.is_empty()
        && solver.formula.is_empty()
        && examples.valid.facts.is_null()
        && examples.valid.expectation.is_empty()
        && examples.valid.expected_diagnostic.is_none()
        && examples.invalid.facts.is_null()
        && examples.invalid.expectation.is_empty()
        && examples.invalid.expected_diagnostic.is_none();
    if !empty {
        return conversion_error(format!(
            "catalog-only rule `{}` contains solver guidance or public examples that the existing catalog cannot represent losslessly",
            rule.id
        ));
    }
    Ok(())
}

const fn convert_strength(strength: RuleStrengthV1) -> RuleStrength {
    match strength {
        RuleStrengthV1::Hard => RuleStrength::Hard,
        RuleStrengthV1::Soft => RuleStrength::Soft,
        RuleStrengthV1::Advisory => RuleStrength::Advisory,
    }
}

const fn convert_execution_kind(execution_kind: ExecutionKindV1) -> ExecutionKind {
    match execution_kind {
        ExecutionKindV1::Constraint => ExecutionKind::Constraint,
        ExecutionKindV1::Objective => ExecutionKind::Objective,
    }
}

fn conversion_error<T>(message: String) -> Result<T, ManifestLoadError> {
    Err(ManifestLoadError::Conversion(message))
}

#[derive(Debug)]
enum ManifestLoadError {
    Deserialize(serde_json::Error),
    Validate(ManifestValidationError),
    Conversion(String),
    Registry(RegistryError),
}

impl fmt::Display for ManifestLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deserialize(error) => {
                write!(formatter, "embedded rule manifest JSON is invalid: {error}")
            }
            Self::Validate(error) => {
                write!(
                    formatter,
                    "embedded rule manifest bundle is invalid: {error}"
                )
            }
            Self::Conversion(message) => write!(
                formatter,
                "embedded rule manifest catalog conversion failed: {message}"
            ),
            Self::Registry(error) => write!(
                formatter,
                "embedded rule manifest registry is invalid: {error}"
            ),
        }
    }
}

impl std::error::Error for ManifestLoadError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::manifest_format::{
        ExecutionKindV1, NativeObjectValidatorV1, ParameterContractV1, ParameterKindV1,
        RuleStrengthV1, SubjectCardinalityV1, SubjectContractV1,
    };

    #[test]
    fn existing_binding_contract_defaults_to_constraint() {
        let contract = super::manifest_rule_contract("a11y.target_size").expect("manifest rule");

        assert_eq!(contract.execution_kind, ExecutionKindV1::Constraint);
    }

    #[test]
    fn objective_binding_contract_uses_the_manifest_executable_route() {
        let mut objective = super::rule_manifest("scheduling.minimize_makespan")
            .expect("manifest rule")
            .clone();
        objective.execution_kind = ExecutionKindV1::Objective;
        objective.strength = RuleStrengthV1::Advisory;

        let binding = super::validate_binding_contract_for_rule(
            &objective,
            &["a".to_owned(), "b".to_owned(), "c".to_owned()],
            &serde_json::Map::new(),
        )
        .expect("manifest-declared objective is executable");

        assert_eq!(binding.execution_kind, ExecutionKindV1::Objective);
    }

    #[test]
    fn strict_json_failures_are_deterministic() {
        let source = super::EMBEDDED_RULE_MANIFESTS
            .strip_suffix('}')
            .expect("embedded manifest JSON object");
        let malformed = format!(r#"{source},"unknown":true}}"#);

        let first = super::load_manifest_data(&malformed)
            .expect_err("unknown fields must fail")
            .to_string();
        let second = super::load_manifest_data(&malformed)
            .expect_err("unknown fields must fail repeatedly")
            .to_string();

        assert_eq!(first, second);
        assert!(first.contains("embedded rule manifest JSON is invalid"));
        assert!(first.contains("unknown field `unknown`"));
    }

    #[test]
    fn unrepresentable_catalog_versions_fail_deterministically() {
        let unsupported = super::EMBEDDED_RULE_MANIFESTS.replacen(
            r#""family_version":1"#,
            r#""family_version":2"#,
            1,
        );

        let error = super::load_manifest_data(&unsupported)
            .expect_err("the existing catalog only represents version one")
            .to_string();

        assert_eq!(
            error,
            "embedded rule manifest catalog conversion failed: family `accessibility` uses family_version `2`; the runtime catalog supports only `1`"
        );
    }

    #[test]
    fn every_parameter_kind_and_inclusive_bound_is_validated() {
        let mut integer = parameter("integer", ParameterKindV1::Integer);
        integer.minimum = Some(1);
        integer.maximum = Some(3);
        for value in [json!(1), json!(3)] {
            super::validate_parameter_value("test.rule", &integer, &value)
                .expect("inclusive integer boundary");
        }
        for value in [json!(0), json!(4), json!(1.5), json!("1")] {
            assert!(super::validate_parameter_value("test.rule", &integer, &value).is_err());
        }

        let boolean = parameter("boolean", ParameterKindV1::Boolean);
        super::validate_parameter_value("test.rule", &boolean, &json!(true))
            .expect("boolean value");
        assert!(super::validate_parameter_value("test.rule", &boolean, &json!("true")).is_err());

        let string = parameter("string", ParameterKindV1::String);
        super::validate_parameter_value("test.rule", &string, &json!("value"))
            .expect("string value");
        assert!(super::validate_parameter_value("test.rule", &string, &json!(true)).is_err());

        let mut enumeration = parameter("enum", ParameterKindV1::StringEnum);
        enumeration.values = vec!["first".to_owned(), "second".to_owned()];
        super::validate_parameter_value("test.rule", &enumeration, &json!("first"))
            .expect("enum member");
        assert!(
            super::validate_parameter_value("test.rule", &enumeration, &json!("third")).is_err()
        );

        let mut array = parameter("array", ParameterKindV1::StringArray);
        array.min_items = Some(1);
        array.max_items = Some(2);
        for value in [json!(["one"]), json!(["one", "two"])] {
            super::validate_parameter_value("test.rule", &array, &value)
                .expect("inclusive array boundary");
        }
        for value in [json!([]), json!(["one", "two", "three"]), json!([1])] {
            assert!(super::validate_parameter_value("test.rule", &array, &value).is_err());
        }

        let mut native = parameter("native", ParameterKindV1::NativeObject);
        native.validator = Some(NativeObjectValidatorV1::AccessibilityException);
        super::validate_parameter_value(
            "test.rule",
            &native,
            &json!({"kind": "spacing", "evidence": "documented"}),
        )
        .expect("native accessibility exception");
        assert!(
            super::validate_parameter_value("test.rule", &native, &json!({"kind": "spacing"}),)
                .is_err()
        );
    }

    #[test]
    fn every_subject_cardinality_variant_uses_inclusive_boundaries() {
        let exact = SubjectContractV1 {
            cardinality: SubjectCardinalityV1::Exact { count: 2 },
        };
        assert!(super::validate_subject_count("test.rule", &exact, 2).is_ok());
        assert!(super::validate_subject_count("test.rule", &exact, 1).is_err());

        let at_least = SubjectContractV1 {
            cardinality: SubjectCardinalityV1::AtLeast { count: 2 },
        };
        assert!(super::validate_subject_count("test.rule", &at_least, 2).is_ok());
        assert!(super::validate_subject_count("test.rule", &at_least, 1).is_err());

        let range = SubjectContractV1 {
            cardinality: SubjectCardinalityV1::Range {
                minimum: 2,
                maximum: 4,
            },
        };
        for count in [2, 4] {
            super::validate_subject_count("test.rule", &range, count)
                .expect("inclusive subject boundary");
        }
        for count in [1, 5] {
            assert!(super::validate_subject_count("test.rule", &range, count).is_err());
        }
    }

    fn parameter(name: &str, kind: ParameterKindV1) -> ParameterContractV1 {
        ParameterContractV1 {
            name: name.to_owned(),
            required: false,
            default: None,
            kind,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: None,
        }
    }
}
