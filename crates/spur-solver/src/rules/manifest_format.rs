//! Strict, versioned source DTOs for declarative solver rule manifests.
//!
//! This module deliberately contains no file discovery, generated-output,
//! registry-conversion, or native dispatch behavior. It is shared by the
//! library and the build script so both validate the same source format.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// The only manifest schema version accepted by these DTOs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SchemaVersionV1;

impl Serialize for SchemaVersionV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(1)
    }
}

impl<'de> Deserialize<'de> for SchemaVersionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == 1 {
            Ok(Self)
        } else {
            Err(de::Error::custom(format!(
                "unsupported rule manifest schema_version `{version}`; expected `1`"
            )))
        }
    }
}

/// One family source document and its owned profiles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FamilyManifestV1 {
    pub schema_version: SchemaVersionV1,
    pub id: String,
    pub family_version: u32,
    pub summary: String,
    pub profiles: Vec<ProfileManifestV1>,
}

/// One profile record owned by a family source document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileManifestV1 {
    pub id: String,
    pub profile_version: u32,
    pub summary: String,
}

/// One rule source document.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleManifestV1 {
    pub schema_version: SchemaVersionV1,
    pub id: String,
    pub rule_version: u32,
    pub family: String,
    pub profile: String,
    pub primitive: String,
    pub summary: String,
    pub availability: AvailabilityV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_reason: Option<String>,
    pub strength: RuleStrengthV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorities: Vec<RuleAuthorityV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    pub llm_encoding: LlmEncodingV1,
    pub solver_encoding: SolverEncodingV1,
    pub subjects: SubjectContractV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<ParameterContractV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<NativeHandlerV1>,
    pub examples: CatalogExamplesV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conformance: Option<ConformanceVectorsV1>,
}

/// Canonical in-memory bundle assembled from strict source documents.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestBundleV1 {
    pub schema_version: SchemaVersionV1,
    pub families: Vec<FamilyManifestV1>,
    pub rules: Vec<RuleManifestV1>,
}

/// Implementation readiness declared by a rule manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityV1 {
    Implemented,
    Experimental,
    CapabilityUnavailable,
}

/// Default semantic strength declared by a rule manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStrengthV1 {
    Hard,
    Soft,
    Advisory,
}

/// Static routing outcome for one valid rule manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestRouteV1 {
    Executable,
    CatalogOnly,
}

/// A normative or explanatory source for a rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleAuthorityV1 {
    pub kind: String,
    pub title: String,
    pub url: String,
}

/// Agent-facing instructions for recognizing and encoding a rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmEncodingV1 {
    pub effectiveness: String,
    pub problem_shapes: Vec<String>,
    pub encode_steps: Vec<String>,
    pub anti_patterns: Vec<String>,
    pub escalate_when: Vec<String>,
}

/// Solver-facing human guidance. This is descriptive data, not an expression DSL.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SolverEncodingV1 {
    pub theory: String,
    pub verification: String,
    pub synthesis: String,
    pub formula: Vec<String>,
}

/// Accepted subject-list cardinality.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubjectCardinalityV1 {
    Exact { count: usize },
    AtLeast { count: usize },
    Range { minimum: usize, maximum: usize },
}

/// Subject-list contract for one rule binding.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubjectContractV1 {
    pub cardinality: SubjectCardinalityV1,
}

/// Closed parameter value kinds supported by manifest contract validation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterKindV1 {
    Integer,
    Boolean,
    String,
    StringEnum,
    StringArray,
    NativeObject,
}

/// Static parameter contract for one accepted parameter name.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterContractV1 {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    pub kind: ParameterKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator: Option<NativeObjectValidatorV1>,
}

/// Closed selectors for native validation of structured parameter values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeObjectValidatorV1 {
    AccessibilityException,
}

impl NativeObjectValidatorV1 {
    pub const ALL: &'static [Self] = &[Self::AccessibilityException];
}

/// Closed selectors for every implemented-hard native rule handler.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHandlerV1 {
    A11yFocusNotObscured,
    A11yReflow,
    A11yTargetSize,
    A11yTextContrast,
    LayoutAxisCapacity,
    LayoutContainment,
    LayoutNonOverlap,
    MediaAspectRatio,
    RbacDynamicSeparationOfDuty,
    RbacPermissionReachable,
    RbacRoleHierarchyAcyclic,
    RbacStaticSeparationOfDuty,
    PlacementMinimumFailureDomains,
    PlacementTopologyMaxSkew,
    ResourceAggregateCapacity,
    ResourceQuotaCapacity,
    ResourceRequestWithinLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeParameterModeV1 {
    Required,
    Defaulted,
    Optional,
}

impl NativeParameterModeV1 {
    const fn label(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Defaulted => "defaulted",
            Self::Optional => "optional",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeParameterAbiV1 {
    name: &'static str,
    mode: NativeParameterModeV1,
}

const fn native_parameter(name: &'static str, mode: NativeParameterModeV1) -> NativeParameterAbiV1 {
    NativeParameterAbiV1 { name, mode }
}

impl NativeHandlerV1 {
    pub const ALL: &'static [Self] = &[
        Self::A11yFocusNotObscured,
        Self::A11yReflow,
        Self::A11yTargetSize,
        Self::A11yTextContrast,
        Self::LayoutAxisCapacity,
        Self::LayoutContainment,
        Self::LayoutNonOverlap,
        Self::MediaAspectRatio,
        Self::RbacDynamicSeparationOfDuty,
        Self::RbacPermissionReachable,
        Self::RbacRoleHierarchyAcyclic,
        Self::RbacStaticSeparationOfDuty,
        Self::PlacementMinimumFailureDomains,
        Self::PlacementTopologyMaxSkew,
        Self::ResourceAggregateCapacity,
        Self::ResourceQuotaCapacity,
        Self::ResourceRequestWithinLimit,
    ];

    const fn family(self) -> &'static str {
        match self {
            Self::A11yFocusNotObscured
            | Self::A11yReflow
            | Self::A11yTargetSize
            | Self::A11yTextContrast => "accessibility",
            Self::LayoutAxisCapacity
            | Self::LayoutContainment
            | Self::LayoutNonOverlap
            | Self::MediaAspectRatio => "design",
            Self::RbacDynamicSeparationOfDuty
            | Self::RbacPermissionReachable
            | Self::RbacRoleHierarchyAcyclic
            | Self::RbacStaticSeparationOfDuty => "policy",
            Self::PlacementMinimumFailureDomains
            | Self::PlacementTopologyMaxSkew
            | Self::ResourceAggregateCapacity
            | Self::ResourceQuotaCapacity
            | Self::ResourceRequestWithinLimit => "resource",
        }
    }

    fn parameter_abi(self) -> Vec<NativeParameterAbiV1> {
        use NativeParameterModeV1::{Defaulted, Optional, Required};

        match self {
            Self::A11yFocusNotObscured
            | Self::RbacPermissionReachable
            | Self::RbacRoleHierarchyAcyclic => vec![],
            Self::A11yReflow => vec![native_parameter("exception", Optional)],
            Self::A11yTargetSize => vec![
                native_parameter("minimum_width", Defaulted),
                native_parameter("minimum_height", Defaulted),
                native_parameter("exception", Optional),
            ],
            Self::A11yTextContrast => vec![native_parameter("minimum_ratio_hundredths", Defaulted)],
            Self::LayoutAxisCapacity => vec![
                native_parameter("axis", Required),
                native_parameter("gap", Defaulted),
                native_parameter("inset_start", Defaulted),
                native_parameter("inset_end", Defaulted),
            ],
            Self::LayoutContainment => vec![native_parameter("padding", Defaulted)],
            Self::LayoutNonOverlap => vec![native_parameter("minimum_gap", Defaulted)],
            Self::MediaAspectRatio => vec![
                native_parameter("source_width", Required),
                native_parameter("source_height", Required),
            ],
            Self::RbacDynamicSeparationOfDuty => vec![
                native_parameter("roles", Required),
                native_parameter("max_active", Defaulted),
            ],
            Self::RbacStaticSeparationOfDuty => vec![
                native_parameter("roles", Required),
                native_parameter("max_assigned", Defaulted),
            ],
            Self::PlacementMinimumFailureDomains => {
                vec![native_parameter("minimum_domains", Defaulted)]
            }
            Self::PlacementTopologyMaxSkew => vec![native_parameter("max_skew", Defaulted)],
            Self::ResourceAggregateCapacity
            | Self::ResourceQuotaCapacity
            | Self::ResourceRequestWithinLimit => {
                vec![native_parameter("resources", Defaulted)]
            }
        }
    }
}

/// One public catalog example.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogExampleV1 {
    pub facts: Value,
    pub expectation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_diagnostic: Option<String>,
}

/// Public valid and invalid examples projected by `solve_rule_spec`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogExamplesV1 {
    pub valid: CatalogExampleV1,
    pub invalid: CatalogExampleV1,
}

/// One executable family request used as a conformance vector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceVectorV1 {
    pub name: String,
    pub request: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_diagnostic: Option<String>,
}

/// Separate valid and invalid executable conformance vectors.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceVectorsV1 {
    pub valid: Vec<ConformanceVectorV1>,
    pub invalid: Vec<ConformanceVectorV1>,
}

/// A deterministic source-manifest validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestValidationError {
    InvalidField {
        path: String,
        message: String,
    },
    DuplicateId {
        kind: &'static str,
        id: String,
    },
    UnknownFamily {
        rule_id: String,
        family_id: String,
    },
    UnknownProfile {
        rule_id: String,
        profile_id: String,
    },
    ProfileFamilyMismatch {
        rule_id: String,
        declared_family: String,
        profile_id: String,
        profile_family: String,
    },
    DuplicateHandler {
        handler: NativeHandlerV1,
        first_rule: String,
        second_rule: String,
    },
    InvalidRouting {
        rule_id: String,
        implemented_hard: bool,
        handler_present: bool,
    },
    InvalidNativeHandlerContract {
        rule_id: String,
        handler: NativeHandlerV1,
        message: String,
    },
}

impl fmt::Display for ManifestValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField { path, message } => {
                write!(formatter, "invalid manifest field `{path}`: {message}")
            }
            Self::DuplicateId { kind, id } => write!(formatter, "duplicate {kind} id `{id}`"),
            Self::UnknownFamily { rule_id, family_id } => write!(
                formatter,
                "rule `{rule_id}` names unknown family `{family_id}`"
            ),
            Self::UnknownProfile {
                rule_id,
                profile_id,
            } => write!(
                formatter,
                "rule `{rule_id}` names unknown profile `{profile_id}`"
            ),
            Self::ProfileFamilyMismatch {
                rule_id,
                declared_family,
                profile_id,
                profile_family,
            } => write!(
                formatter,
                "rule `{rule_id}` declares family `{declared_family}`, but profile `{profile_id}` belongs to `{profile_family}`"
            ),
            Self::DuplicateHandler {
                handler,
                first_rule,
                second_rule,
            } => write!(
                formatter,
                "native handler `{handler:?}` is assigned to both `{first_rule}` and `{second_rule}`"
            ),
            Self::InvalidRouting {
                rule_id,
                implemented_hard,
                handler_present,
            } => write!(
                formatter,
                "rule `{rule_id}` has invalid routing: implemented_hard={implemented_hard}, handler_present={handler_present}"
            ),
            Self::InvalidNativeHandlerContract {
                rule_id,
                handler,
                message,
            } => write!(
                formatter,
                "rule `{rule_id}` has invalid native handler `{handler:?}` contract: {message}"
            ),
        }
    }
}

impl std::error::Error for ManifestValidationError {}

/// Validates one family document without reading any external state.
pub fn validate_family_manifest(family: &FamilyManifestV1) -> Result<(), ManifestValidationError> {
    require_text("family.id", &family.id)?;
    require_text("family.summary", &family.summary)?;
    require_version("family.family_version", family.family_version)?;
    if family.profiles.is_empty() {
        return invalid("family.profiles", "must contain at least one profile");
    }
    let mut profile_ids = BTreeSet::new();
    for profile in &family.profiles {
        require_text("profile.id", &profile.id)?;
        require_text("profile.summary", &profile.summary)?;
        require_version("profile.profile_version", profile.profile_version)?;
        if !profile_ids.insert(profile.id.clone()) {
            return Err(ManifestValidationError::DuplicateId {
                kind: "profile",
                id: profile.id.clone(),
            });
        }
    }
    Ok(())
}

/// Validates one rule document and returns its only legal routing outcome.
pub fn validate_rule_manifest(
    rule: &RuleManifestV1,
) -> Result<ManifestRouteV1, ManifestValidationError> {
    require_text("rule.id", &rule.id)?;
    require_text("rule.family", &rule.family)?;
    require_text("rule.profile", &rule.profile)?;
    require_text("rule.primitive", &rule.primitive)?;
    require_text("rule.summary", &rule.summary)?;
    require_version("rule.rule_version", rule.rule_version)?;

    match (rule.availability, rule.availability_reason.as_deref()) {
        (AvailabilityV1::CapabilityUnavailable, Some(reason)) if !reason.trim().is_empty() => {}
        (AvailabilityV1::CapabilityUnavailable, _) => {
            return invalid(
                "rule.availability_reason",
                "is required for capability_unavailable rules",
            );
        }
        (AvailabilityV1::Implemented, Some(_)) => {
            return invalid(
                "rule.availability_reason",
                "must be absent for implemented rules",
            );
        }
        (_, Some(reason)) if reason.trim().is_empty() => {
            return invalid("rule.availability_reason", "must not be empty");
        }
        _ => {}
    }

    validate_subject_contract(&rule.subjects)?;
    let mut parameter_names = BTreeSet::new();
    for parameter in &rule.parameters {
        if !parameter_names.insert(parameter.name.clone()) {
            return Err(ManifestValidationError::DuplicateId {
                kind: "parameter",
                id: parameter.name.clone(),
            });
        }
        validate_parameter_contract(parameter)?;
    }

    let implemented_hard =
        rule.availability == AvailabilityV1::Implemented && rule.strength == RuleStrengthV1::Hard;
    let handler_present = rule.handler.is_some();
    if implemented_hard != handler_present {
        return Err(ManifestValidationError::InvalidRouting {
            rule_id: rule.id.clone(),
            implemented_hard,
            handler_present,
        });
    }
    if implemented_hard {
        let Some(conformance) = &rule.conformance else {
            return invalid("rule.conformance", "is required for implemented-hard rules");
        };
        validate_conformance_vectors(conformance)?;
        validate_violation_diagnostics(rule, conformance)?;
        Ok(ManifestRouteV1::Executable)
    } else {
        if rule.conformance.is_some() {
            return invalid("rule.conformance", "must be absent for catalog-only rules");
        }
        Ok(ManifestRouteV1::CatalogOnly)
    }
}

fn validate_native_handler_family(
    rule: &RuleManifestV1,
    handler: NativeHandlerV1,
) -> Result<(), ManifestValidationError> {
    if rule.family != handler.family() {
        return Err(ManifestValidationError::InvalidNativeHandlerContract {
            rule_id: rule.id.clone(),
            handler,
            message: format!(
                "handler family `{}` does not match declared rule family `{}`",
                handler.family(),
                rule.family
            ),
        });
    }
    Ok(())
}

fn validate_native_handler_parameter_contract(
    rule: &RuleManifestV1,
    handler: NativeHandlerV1,
) -> Result<(), ManifestValidationError> {
    let expected = handler.parameter_abi();
    for abi in &expected {
        let Some(parameter) = rule
            .parameters
            .iter()
            .find(|parameter| parameter.name == abi.name)
        else {
            return Err(ManifestValidationError::InvalidNativeHandlerContract {
                rule_id: rule.id.clone(),
                handler,
                message: format!(
                    "parameter `{}` is missing; native handler requires it to be {}",
                    abi.name,
                    abi.mode.label()
                ),
            });
        };
        let actual = if parameter.required {
            NativeParameterModeV1::Required
        } else if parameter.default.is_some() {
            NativeParameterModeV1::Defaulted
        } else {
            NativeParameterModeV1::Optional
        };
        if actual != abi.mode {
            return Err(ManifestValidationError::InvalidNativeHandlerContract {
                rule_id: rule.id.clone(),
                handler,
                message: format!(
                    "parameter `{}` must be {}, found {}",
                    abi.name,
                    abi.mode.label(),
                    actual.label()
                ),
            });
        }
    }
    if let Some(parameter) = rule.parameters.iter().find(|parameter| {
        !expected
            .iter()
            .any(|abi| abi.name == parameter.name.as_str())
    }) {
        return Err(ManifestValidationError::InvalidNativeHandlerContract {
            rule_id: rule.id.clone(),
            handler,
            message: format!(
                "parameter `{}` is not accepted by the native handler ABI",
                parameter.name
            ),
        });
    }
    Ok(())
}

fn validate_violation_diagnostics(
    rule: &RuleManifestV1,
    conformance: &ConformanceVectorsV1,
) -> Result<(), ManifestValidationError> {
    let Some(expected) = rule
        .examples
        .invalid
        .expected_diagnostic
        .as_deref()
        .filter(|diagnostic| !diagnostic.trim().is_empty())
    else {
        return invalid(
            "rule.examples.invalid.expected_diagnostic",
            "is required for implemented-hard rules",
        );
    };
    for (index, vector) in conformance.invalid.iter().enumerate() {
        match vector.expected_diagnostic.as_deref() {
            Some(actual) if actual == expected => {}
            Some(actual) => {
                return invalid(
                    format!("rule.conformance.invalid[{index}].expected_diagnostic"),
                    format!("must match `{expected}`, found `{actual}`"),
                );
            }
            None => {
                return invalid(
                    format!("rule.conformance.invalid[{index}].expected_diagnostic"),
                    format!("is required and must match `{expected}`"),
                );
            }
        }
    }
    Ok(())
}

/// Validates bundle-wide identity, ownership, routing, and handler uniqueness.
pub fn validate_manifest_bundle(bundle: &ManifestBundleV1) -> Result<(), ManifestValidationError> {
    let mut family_ids = BTreeSet::new();
    let mut profile_owners = BTreeMap::new();
    for family in &bundle.families {
        validate_family_manifest(family)?;
        if !family_ids.insert(family.id.clone()) {
            return Err(ManifestValidationError::DuplicateId {
                kind: "family",
                id: family.id.clone(),
            });
        }
        for profile in &family.profiles {
            if profile_owners
                .insert(profile.id.clone(), family.id.clone())
                .is_some()
            {
                return Err(ManifestValidationError::DuplicateId {
                    kind: "profile",
                    id: profile.id.clone(),
                });
            }
        }
    }

    let mut rule_ids = BTreeSet::new();
    let mut handler_owners = BTreeMap::new();
    for rule in &bundle.rules {
        validate_rule_manifest(rule)?;
        if !rule_ids.insert(rule.id.clone()) {
            return Err(ManifestValidationError::DuplicateId {
                kind: "rule",
                id: rule.id.clone(),
            });
        }
        if !family_ids.contains(&rule.family) {
            return Err(ManifestValidationError::UnknownFamily {
                rule_id: rule.id.clone(),
                family_id: rule.family.clone(),
            });
        }
        let Some(profile_family) = profile_owners.get(&rule.profile) else {
            return Err(ManifestValidationError::UnknownProfile {
                rule_id: rule.id.clone(),
                profile_id: rule.profile.clone(),
            });
        };
        if profile_family != &rule.family {
            return Err(ManifestValidationError::ProfileFamilyMismatch {
                rule_id: rule.id.clone(),
                declared_family: rule.family.clone(),
                profile_id: rule.profile.clone(),
                profile_family: profile_family.clone(),
            });
        }
        if let Some(handler) = rule.handler {
            validate_native_handler_family(rule, handler)?;
            validate_native_handler_parameter_contract(rule, handler)?;
            if let Some(first_rule) = handler_owners.insert(handler, rule.id.clone()) {
                return Err(ManifestValidationError::DuplicateHandler {
                    handler,
                    first_rule,
                    second_rule: rule.id.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_subject_contract(subjects: &SubjectContractV1) -> Result<(), ManifestValidationError> {
    if let SubjectCardinalityV1::Range { minimum, maximum } = subjects.cardinality {
        if minimum > maximum {
            return invalid(
                "rule.subjects.cardinality",
                "range minimum must not exceed maximum",
            );
        }
    }
    Ok(())
}

fn validate_parameter_contract(
    parameter: &ParameterContractV1,
) -> Result<(), ManifestValidationError> {
    require_text("rule.parameters[].name", &parameter.name)?;
    if parameter.required && parameter.default.is_some() {
        return invalid_parameter(parameter, "required parameters cannot declare a default");
    }

    match parameter.kind {
        ParameterKindV1::Integer => {
            reject_irrelevant_parameter_fields(
                parameter,
                parameter.values.is_empty()
                    && parameter.min_items.is_none()
                    && parameter.max_items.is_none()
                    && parameter.validator.is_none(),
            )?;
            if parameter
                .minimum
                .zip(parameter.maximum)
                .is_some_and(|(minimum, maximum)| minimum > maximum)
            {
                return invalid_parameter(parameter, "minimum must not exceed maximum");
            }
            if let Some(default) = &parameter.default {
                let Some(default) = default.as_i64() else {
                    return invalid_parameter(parameter, "default must be an integer");
                };
                if parameter.minimum.is_some_and(|minimum| default < minimum)
                    || parameter.maximum.is_some_and(|maximum| default > maximum)
                {
                    return invalid_parameter(
                        parameter,
                        "default must be within the inclusive integer bounds",
                    );
                }
            }
        }
        ParameterKindV1::Boolean => {
            reject_scalar_constraints(parameter)?;
            if parameter
                .default
                .as_ref()
                .is_some_and(|value| !value.is_boolean())
            {
                return invalid_parameter(parameter, "default must be a boolean");
            }
        }
        ParameterKindV1::String => {
            reject_scalar_constraints(parameter)?;
            if parameter
                .default
                .as_ref()
                .is_some_and(|value| !value.is_string())
            {
                return invalid_parameter(parameter, "default must be a string");
            }
        }
        ParameterKindV1::StringEnum => {
            reject_irrelevant_parameter_fields(
                parameter,
                parameter.minimum.is_none()
                    && parameter.maximum.is_none()
                    && parameter.min_items.is_none()
                    && parameter.max_items.is_none()
                    && parameter.validator.is_none(),
            )?;
            if parameter.values.is_empty() {
                return invalid_parameter(parameter, "values must not be empty");
            }
            let mut values = BTreeSet::new();
            for value in &parameter.values {
                if value.trim().is_empty() || !values.insert(value.as_str()) {
                    return invalid_parameter(parameter, "values must be non-empty and unique");
                }
            }
            if let Some(default) = &parameter.default {
                let Some(default) = default.as_str() else {
                    return invalid_parameter(parameter, "default must be a string");
                };
                if !values.contains(default) {
                    return invalid_parameter(parameter, "default must be one of values");
                }
            }
        }
        ParameterKindV1::StringArray => {
            reject_irrelevant_parameter_fields(
                parameter,
                parameter.minimum.is_none()
                    && parameter.maximum.is_none()
                    && parameter.values.is_empty()
                    && parameter.validator.is_none(),
            )?;
            if parameter
                .min_items
                .zip(parameter.max_items)
                .is_some_and(|(minimum, maximum)| minimum > maximum)
            {
                return invalid_parameter(parameter, "min_items must not exceed max_items");
            }
            if let Some(default) = &parameter.default {
                let Some(default) = default.as_array() else {
                    return invalid_parameter(parameter, "default must be a string array");
                };
                if default.iter().any(|value| !value.is_string()) {
                    return invalid_parameter(parameter, "default must be a string array");
                }
                if parameter
                    .min_items
                    .is_some_and(|minimum| default.len() < minimum)
                    || parameter
                        .max_items
                        .is_some_and(|maximum| default.len() > maximum)
                {
                    return invalid_parameter(parameter, "default violates array length bounds");
                }
            }
        }
        ParameterKindV1::NativeObject => {
            reject_irrelevant_parameter_fields(
                parameter,
                parameter.minimum.is_none()
                    && parameter.maximum.is_none()
                    && parameter.values.is_empty()
                    && parameter.min_items.is_none()
                    && parameter.max_items.is_none(),
            )?;
            if parameter.validator.is_none() {
                return invalid_parameter(parameter, "validator is required for native_object");
            }
            if parameter
                .default
                .as_ref()
                .is_some_and(|value| !value.is_object())
            {
                return invalid_parameter(parameter, "default must be an object");
            }
        }
    }
    Ok(())
}

fn reject_scalar_constraints(
    parameter: &ParameterContractV1,
) -> Result<(), ManifestValidationError> {
    reject_irrelevant_parameter_fields(
        parameter,
        parameter.minimum.is_none()
            && parameter.maximum.is_none()
            && parameter.values.is_empty()
            && parameter.min_items.is_none()
            && parameter.max_items.is_none()
            && parameter.validator.is_none(),
    )
}

fn reject_irrelevant_parameter_fields(
    parameter: &ParameterContractV1,
    fields_are_relevant: bool,
) -> Result<(), ManifestValidationError> {
    if fields_are_relevant {
        Ok(())
    } else {
        invalid_parameter(
            parameter,
            "contains constraints that are not valid for its parameter kind",
        )
    }
}

fn validate_conformance_vectors(
    conformance: &ConformanceVectorsV1,
) -> Result<(), ManifestValidationError> {
    if conformance.valid.is_empty() {
        return invalid("rule.conformance.valid", "must contain at least one vector");
    }
    if conformance.invalid.is_empty() {
        return invalid(
            "rule.conformance.invalid",
            "must contain at least one vector",
        );
    }
    let mut names = BTreeSet::new();
    for (kind, vectors) in [
        ("valid", conformance.valid.as_slice()),
        ("invalid", conformance.invalid.as_slice()),
    ] {
        for vector in vectors {
            if vector.name.trim().is_empty() {
                return invalid(
                    format!("rule.conformance.{kind}[].name"),
                    "must not be empty",
                );
            }
            if !names.insert(&vector.name) {
                return invalid(
                    format!("rule.conformance.{kind}[].name"),
                    "must be unique within the vector set",
                );
            }
            if !vector.request.is_object() {
                return invalid(
                    format!("rule.conformance.{kind}[].request"),
                    "must be an object",
                );
            }
        }
    }
    Ok(())
}

fn require_text(path: impl Into<String>, value: &str) -> Result<(), ManifestValidationError> {
    if value.trim().is_empty() {
        invalid(path, "must not be empty")
    } else {
        Ok(())
    }
}

fn require_version(path: impl Into<String>, version: u32) -> Result<(), ManifestValidationError> {
    if version == 0 {
        invalid(path, "must be greater than zero")
    } else {
        Ok(())
    }
}

fn invalid<T>(
    path: impl Into<String>,
    message: impl Into<String>,
) -> Result<T, ManifestValidationError> {
    Err(ManifestValidationError::InvalidField {
        path: path.into(),
        message: message.into(),
    })
}

fn invalid_parameter<T>(
    parameter: &ParameterContractV1,
    message: impl Into<String>,
) -> Result<T, ManifestValidationError> {
    invalid(format!("rule.parameters.{}", parameter.name), message)
}
