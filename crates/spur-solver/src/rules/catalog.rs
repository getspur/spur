//! Generic, deterministic catalog types shared by every solver rule family.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

/// A top-level domain that owns one or more rule profiles.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleFamily {
    id: String,
    family_version: u32,
    summary: String,
    profiles: Vec<String>,
}

impl RuleFamily {
    /// Creates a version-one rule family.
    pub fn new<I, S>(id: impl Into<String>, summary: impl Into<String>, profiles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut profiles = profiles.into_iter().map(Into::into).collect::<Vec<_>>();
        profiles.sort();
        Self {
            id: id.into(),
            family_version: 1,
            summary: summary.into(),
            profiles,
        }
    }

    /// Returns the stable family ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns profile IDs owned by this family.
    #[must_use]
    pub fn profiles(&self) -> &[String] {
        &self.profiles
    }
}

/// A curated collection of related rules inside a family.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleProfile {
    id: String,
    family: String,
    profile_version: u32,
    summary: String,
    rules: Vec<String>,
}

impl RuleProfile {
    /// Creates a version-one rule profile.
    pub fn new<I, S>(
        id: impl Into<String>,
        family: impl Into<String>,
        summary: impl Into<String>,
        rules: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut rules = rules.into_iter().map(Into::into).collect::<Vec<_>>();
        rules.sort();
        Self {
            id: id.into(),
            family: family.into(),
            profile_version: 1,
            summary: summary.into(),
            rules,
        }
    }

    /// Returns the stable profile ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the owning family ID.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Returns rule IDs listed by this profile.
    #[must_use]
    pub fn rules(&self) -> &[String] {
        &self.rules
    }
}

/// One executable rule definition.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleDefinition {
    id: String,
    family: String,
    profile: String,
    rule_version: u32,
    primitive: String,
    summary: String,
    #[serde(flatten)]
    guidance: RuleGuidance,
}

impl RuleDefinition {
    /// Creates a version-one rule definition.
    pub fn new(
        id: impl Into<String>,
        family: impl Into<String>,
        profile: impl Into<String>,
        primitive: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            family: family.into(),
            profile: profile.into(),
            rule_version: 1,
            primitive: primitive.into(),
            summary: summary.into(),
            guidance: RuleGuidance::default(),
        }
    }

    /// Attaches catalog guidance and executable encoding metadata.
    #[must_use]
    pub fn with_guidance(mut self, guidance: RuleGuidance) -> Self {
        self.guidance = guidance;
        self
    }

    /// Returns the stable rule ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the owning family ID.
    #[must_use]
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Returns the owning profile ID.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Returns the primitive implemented by this rule.
    #[must_use]
    pub fn primitive(&self) -> &str {
        &self.primitive
    }
}

/// Implementation readiness of one family, profile, or rule.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// The rule is compiled and covered by tests.
    #[default]
    Implemented,
    /// The rule is available but its semantics may still change.
    Experimental,
    /// The registry documents the rule but the runtime cannot execute it.
    CapabilityUnavailable,
}

/// Whether violating a rule invalidates a model by default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStrength {
    /// The predicate must hold for a feasible model.
    #[default]
    Hard,
    /// The predicate is an optimization preference.
    Soft,
    /// The catalog can guide evaluation, but violation is not solver proof of invalidity.
    Advisory,
}

/// Whether a rule contributes feasibility constraints or optimization utility.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    /// The rule contributes a hard feasibility predicate.
    #[default]
    Constraint,
    /// The rule contributes an optimization objective after hard feasibility.
    Objective,
}

/// A normative or explanatory source for a rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleAuthority {
    kind: String,
    title: String,
    url: String,
}

impl RuleAuthority {
    /// Creates an authority reference.
    pub fn new(kind: impl Into<String>, title: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            title: title.into(),
            url: url.into(),
        }
    }
}

/// Agent-facing instructions for recognizing and encoding a rule.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct LlmEncoding {
    effectiveness: String,
    problem_shapes: Vec<String>,
    encode_steps: Vec<String>,
    anti_patterns: Vec<String>,
    escalate_when: Vec<String>,
}

impl LlmEncoding {
    /// Creates complete LLM encoding guidance.
    pub fn new(
        effectiveness: impl Into<String>,
        problem_shapes: impl IntoIterator<Item = impl Into<String>>,
        encode_steps: impl IntoIterator<Item = impl Into<String>>,
        anti_patterns: impl IntoIterator<Item = impl Into<String>>,
        escalate_when: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            effectiveness: effectiveness.into(),
            problem_shapes: strings(problem_shapes),
            encode_steps: strings(encode_steps),
            anti_patterns: strings(anti_patterns),
            escalate_when: strings(escalate_when),
        }
    }
}

/// Solver-facing formula and proof strategy.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SolverEncoding {
    theory: String,
    verification: String,
    synthesis: String,
    formula: Vec<String>,
}

impl SolverEncoding {
    /// Creates a solver encoding description.
    pub fn new(
        theory: impl Into<String>,
        verification: impl Into<String>,
        synthesis: impl Into<String>,
        formula: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            theory: theory.into(),
            verification: verification.into(),
            synthesis: synthesis.into(),
            formula: strings(formula),
        }
    }
}

/// One valid or invalid rule fixture.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RuleExample {
    facts: Value,
    expectation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_diagnostic: Option<String>,
}

impl RuleExample {
    /// Creates a rule example.
    pub fn new(
        facts: Value,
        expectation: impl Into<String>,
        expected_diagnostic: Option<impl Into<String>>,
    ) -> Self {
        Self {
            facts,
            expectation: expectation.into(),
            expected_diagnostic: expected_diagnostic.map(Into::into),
        }
    }
}

/// Paired positive and counterexample fixtures.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RuleExamples {
    valid: RuleExample,
    invalid: RuleExample,
}

impl RuleExamples {
    /// Creates paired examples.
    #[must_use]
    pub const fn new(valid: RuleExample, invalid: RuleExample) -> Self {
        Self { valid, invalid }
    }
}

/// Detailed metadata returned by the catalog guide.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleGuidance {
    availability: Availability,
    #[serde(skip_serializing_if = "Option::is_none")]
    availability_reason: Option<String>,
    execution_kind: ExecutionKind,
    default_strength: RuleStrength,
    authorities: Vec<RuleAuthority>,
    requires: Vec<String>,
    llm_encoding: LlmEncoding,
    solver_encoding: SolverEncoding,
    examples: RuleExamples,
}

impl RuleGuidance {
    /// Creates complete guidance for an implemented hard rule.
    pub fn implemented_hard(
        authorities: Vec<RuleAuthority>,
        requires: impl IntoIterator<Item = impl Into<String>>,
        llm_encoding: LlmEncoding,
        solver_encoding: SolverEncoding,
        examples: RuleExamples,
    ) -> Self {
        Self {
            availability: Availability::Implemented,
            availability_reason: None,
            execution_kind: ExecutionKind::Constraint,
            default_strength: RuleStrength::Hard,
            authorities,
            requires: strings(requires),
            llm_encoding,
            solver_encoding,
            examples,
        }
    }

    /// Creates complete guidance for an implemented optimization objective.
    pub fn implemented_objective(
        default_strength: RuleStrength,
        authorities: Vec<RuleAuthority>,
        requires: impl IntoIterator<Item = impl Into<String>>,
        llm_encoding: LlmEncoding,
        solver_encoding: SolverEncoding,
        examples: RuleExamples,
    ) -> Self {
        Self {
            availability: Availability::Implemented,
            availability_reason: None,
            execution_kind: ExecutionKind::Objective,
            default_strength,
            authorities,
            requires: strings(requires),
            llm_encoding,
            solver_encoding,
            examples,
        }
    }

    /// Creates guidance for a documented rule that this runtime cannot prove.
    pub fn capability_unavailable(
        reason: impl Into<String>,
        execution_kind: ExecutionKind,
        default_strength: RuleStrength,
        authorities: Vec<RuleAuthority>,
        requires: impl IntoIterator<Item = impl Into<String>>,
        llm_encoding: LlmEncoding,
    ) -> Self {
        Self {
            availability: Availability::CapabilityUnavailable,
            availability_reason: Some(reason.into()),
            execution_kind,
            default_strength,
            authorities,
            requires: strings(requires),
            llm_encoding,
            solver_encoding: SolverEncoding::default(),
            examples: RuleExamples::default(),
        }
    }
}

impl Default for RuleGuidance {
    fn default() -> Self {
        Self {
            availability: Availability::Implemented,
            availability_reason: None,
            execution_kind: ExecutionKind::Constraint,
            default_strength: RuleStrength::Hard,
            authorities: Vec::new(),
            requires: Vec::new(),
            llm_encoding: LlmEncoding::default(),
            solver_encoding: SolverEncoding::default(),
            examples: RuleExamples::default(),
        }
    }
}

fn strings<I, S>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    items.into_iter().map(Into::into).collect()
}

/// A validated, deterministically ordered rule catalog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleRegistry {
    schema_version: u32,
    families: Vec<RuleFamily>,
    profiles: Vec<RuleProfile>,
    rules: Vec<RuleDefinition>,
}

impl RuleRegistry {
    /// Validates and constructs a stable rule registry.
    pub fn new(
        schema_version: u32,
        mut families: Vec<RuleFamily>,
        mut profiles: Vec<RuleProfile>,
        mut rules: Vec<RuleDefinition>,
    ) -> Result<Self, RegistryError> {
        families.sort_by(|left, right| left.id.cmp(&right.id));
        profiles.sort_by(|left, right| left.id.cmp(&right.id));
        rules.sort_by(|left, right| left.id.cmp(&right.id));

        reject_duplicate_ids(&families, RuleFamily::id, |id| {
            RegistryError::DuplicateFamilyId { id }
        })?;
        reject_duplicate_ids(&profiles, RuleProfile::id, |id| {
            RegistryError::DuplicateProfileId { id }
        })?;
        reject_duplicate_ids(&rules, RuleDefinition::id, |id| {
            RegistryError::DuplicateRuleId { id }
        })?;

        let profiles_by_id = profiles
            .iter()
            .map(|profile| (profile.id(), profile))
            .collect::<BTreeMap<_, _>>();
        let rules_by_id = rules
            .iter()
            .map(|rule| (rule.id(), rule))
            .collect::<BTreeMap<_, _>>();

        for family in &families {
            for profile_id in family.profiles() {
                let Some(profile) = profiles_by_id.get(profile_id.as_str()) else {
                    return Err(RegistryError::UnknownFamilyProfile {
                        family_id: family.id.clone(),
                        profile_id: profile_id.clone(),
                    });
                };
                if profile.family() != family.id() {
                    return Err(RegistryError::ProfileFamilyMismatch {
                        profile_id: profile.id.clone(),
                        declared_family: profile.family.clone(),
                        listed_family: family.id.clone(),
                    });
                }
            }
        }

        for profile in &profiles {
            for rule_id in profile.rules() {
                let Some(rule) = rules_by_id.get(rule_id.as_str()) else {
                    return Err(RegistryError::UnknownProfileRule {
                        profile_id: profile.id.clone(),
                        rule_id: rule_id.clone(),
                    });
                };
                if rule.family() != profile.family() || rule.profile() != profile.id() {
                    return Err(RegistryError::RuleOwnerMismatch {
                        rule_id: rule.id.clone(),
                        declared_family: rule.family.clone(),
                        declared_profile: rule.profile.clone(),
                        listed_family: profile.family.clone(),
                        listed_profile: profile.id.clone(),
                    });
                }
            }
        }

        Ok(Self {
            schema_version,
            families,
            profiles,
            rules,
        })
    }

    /// Merges validated family-owned registries into one deterministic catalog.
    pub fn merge(
        schema_version: u32,
        registries: impl IntoIterator<Item = &'static Self>,
    ) -> Result<Self, RegistryError> {
        let mut families = Vec::new();
        let mut profiles = Vec::new();
        let mut rules = Vec::new();
        for registry in registries {
            families.extend_from_slice(registry.families());
            profiles.extend_from_slice(registry.profiles());
            rules.extend_from_slice(registry.rules());
        }
        Self::new(schema_version, families, profiles, rules)
    }

    /// Returns the registry schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns families in stable ID order.
    #[must_use]
    pub fn families(&self) -> &[RuleFamily] {
        &self.families
    }

    /// Returns profiles in stable ID order.
    #[must_use]
    pub fn profiles(&self) -> &[RuleProfile] {
        &self.profiles
    }

    /// Returns rules in stable ID order.
    #[must_use]
    pub fn rules(&self) -> &[RuleDefinition] {
        &self.rules
    }

    /// Looks up one family by exact ID.
    #[must_use]
    pub fn family(&self, id: &str) -> Option<&RuleFamily> {
        self.families
            .binary_search_by_key(&id, |family| family.id())
            .ok()
            .map(|index| &self.families[index])
    }

    /// Looks up one profile by exact ID.
    #[must_use]
    pub fn profile(&self, id: &str) -> Option<&RuleProfile> {
        self.profiles
            .binary_search_by_key(&id, |profile| profile.id())
            .ok()
            .map(|index| &self.profiles[index])
    }

    /// Looks up one rule by exact ID.
    #[must_use]
    pub fn rule(&self, id: &str) -> Option<&RuleDefinition> {
        self.rules
            .binary_search_by_key(&id, |rule| rule.id())
            .ok()
            .map(|index| &self.rules[index])
    }
}

fn reject_duplicate_ids<T, F, E>(items: &[T], id: F, error: E) -> Result<(), RegistryError>
where
    F: Fn(&T) -> &str,
    E: Fn(String) -> RegistryError,
{
    if let Some([first, _]) = items.windows(2).find(|pair| id(&pair[0]) == id(&pair[1])) {
        return Err(error(id(first).to_owned()));
    }
    Ok(())
}

/// Structural errors in a code-owned rule registry.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RegistryError {
    /// Two families use the same ID.
    #[error("duplicate rule family ID `{id}`")]
    DuplicateFamilyId { id: String },
    /// Two profiles use the same ID.
    #[error("duplicate rule profile ID `{id}`")]
    DuplicateProfileId { id: String },
    /// Two rules use the same ID.
    #[error("duplicate rule ID `{id}`")]
    DuplicateRuleId { id: String },
    /// A family lists a profile that does not exist.
    #[error("family `{family_id}` lists unknown profile `{profile_id}`")]
    UnknownFamilyProfile {
        family_id: String,
        profile_id: String,
    },
    /// A profile is listed by a family other than the one it declares.
    #[error(
        "profile `{profile_id}` declares family `{declared_family}` but is listed by `{listed_family}`"
    )]
    ProfileFamilyMismatch {
        profile_id: String,
        declared_family: String,
        listed_family: String,
    },
    /// A profile lists a rule that does not exist.
    #[error("profile `{profile_id}` lists unknown rule `{rule_id}`")]
    UnknownProfileRule { profile_id: String, rule_id: String },
    /// A rule is listed under a family or profile it does not declare.
    #[error(
        "rule `{rule_id}` declares `{declared_family}/{declared_profile}` but is listed by `{listed_family}/{listed_profile}`"
    )]
    RuleOwnerMismatch {
        rule_id: String,
        declared_family: String,
        declared_profile: String,
        listed_family: String,
        listed_profile: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        LlmEncoding, RuleAuthority, RuleExample, RuleExamples, RuleGuidance, RuleStrength,
        SolverEncoding,
    };

    #[test]
    fn objective_guidance_serializes_its_execution_kind() {
        let guidance = RuleGuidance::implemented_objective(
            RuleStrength::Advisory,
            vec![],
            Vec::<String>::new(),
            LlmEncoding::default(),
            SolverEncoding::default(),
            RuleExamples::default(),
        );

        let value = serde_json::to_value(guidance).expect("serialize objective guidance");

        assert_eq!(value["execution_kind"], "objective");
        assert_eq!(value["default_strength"], "advisory");
    }

    #[test]
    fn objective_guidance_preserves_solver_examples_and_explicit_utility_requirements() {
        let solver_encoding = SolverEncoding::new(
            "QF_LIA with typed optimization",
            "verification is unsupported",
            "minimize caller-declared grant cost",
            ["cost = sum selected(role) * grant_cost(role)"],
        );
        let examples = RuleExamples::new(
            RuleExample::new(
                json!({"required_permissions": ["read"], "grant_costs": {"reader": 3}}),
                "optimized cost 3 after complete termination",
                None::<String>,
            ),
            RuleExample::new(
                json!({"required_permissions": [], "grant_costs": {}}),
                "reject missing explicit utility",
                None::<String>,
            ),
        );
        let guidance = RuleGuidance::implemented_objective(
            RuleStrength::Advisory,
            vec![RuleAuthority::new(
                "standard",
                "Caller-owned least privilege",
                "https://example.invalid/least-privilege",
            )],
            [
                "principals[].required_permissions",
                "principals[].grant_costs",
            ],
            LlmEncoding::new(
                "high",
                ["Minimize declared grant utility after hard authorization constraints."],
                ["Require explicit permissions and positive finite grant costs."],
                ["Do not infer utility from role names."],
                ["Escalate missing utility facts."],
            ),
            solver_encoding,
            examples,
        );

        let value = serde_json::to_value(guidance).expect("serialize objective guidance");

        assert_eq!(value["execution_kind"], "objective");
        assert_eq!(value["default_strength"], "advisory");
        assert_eq!(
            value["requires"],
            json!([
                "principals[].required_permissions",
                "principals[].grant_costs"
            ])
        );
        assert_eq!(
            value["solver_encoding"],
            json!({
                "theory": "QF_LIA with typed optimization",
                "verification": "verification is unsupported",
                "synthesis": "minimize caller-declared grant cost",
                "formula": ["cost = sum selected(role) * grant_cost(role)"]
            })
        );
        assert_eq!(
            value["examples"],
            json!({
                "valid": {
                    "facts": {
                        "required_permissions": ["read"],
                        "grant_costs": {"reader": 3}
                    },
                    "expectation": "optimized cost 3 after complete termination"
                },
                "invalid": {
                    "facts": {"required_permissions": [], "grant_costs": {}},
                    "expectation": "reject missing explicit utility"
                }
            })
        );
    }
}
