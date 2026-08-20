//! Runtime projections over the canonical embedded rule manifest bundle.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::LazyLock,
};

use super::{
    catalog::{
        LlmEncoding, RegistryError, RuleAuthority, RuleDefinition, RuleExample, RuleExamples,
        RuleFamily, RuleGuidance, RuleProfile, RuleRegistry, RuleStrength, SolverEncoding,
    },
    manifest_format::{
        validate_manifest_bundle, AvailabilityV1, ConformanceVectorsV1, ManifestBundleV1,
        ManifestValidationError, NativeHandlerV1, ParameterContractV1, RuleManifestV1,
        RuleStrengthV1, SubjectContractV1,
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
    /// Accepted subject-list cardinality.
    pub subjects: &'a SubjectContractV1,
    /// Accepted parameter names, kinds, defaults, and static bounds.
    pub parameters: &'a [ParameterContractV1],
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
    let expected = NativeHandlerV1::ALL
        .iter()
        .copied()
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

    let guidance = match (rule.availability, rule.strength) {
        (AvailabilityV1::Implemented, RuleStrengthV1::Hard) => RuleGuidance::implemented_hard(
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
        ),
        (AvailabilityV1::CapabilityUnavailable, strength) => {
            ensure_unavailable_projection_is_lossless(rule)?;
            let Some(reason) = rule.availability_reason.clone() else {
                return conversion_error(format!(
                    "catalog-only rule `{}` is missing its validated availability reason",
                    rule.id
                ));
            };
            RuleGuidance::capability_unavailable(
                reason,
                convert_strength(strength),
                authorities,
                rule.requires.clone(),
                llm_encoding,
            )
        }
        (availability, strength) => {
            return conversion_error(format!(
                "rule `{}` uses unsupported catalog projection `{availability:?}/{strength:?}`",
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
}
