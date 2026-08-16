use spur_solver::rules::catalog::{
    RegistryError, RuleDefinition, RuleFamily, RuleProfile, RuleRegistry,
};

fn family(id: &str, profiles: &[&str]) -> RuleFamily {
    RuleFamily::new(id, format!("{id} family"), profiles.iter().copied())
}

fn profile(id: &str, family: &str, rules: &[&str]) -> RuleProfile {
    RuleProfile::new(id, family, format!("{id} profile"), rules.iter().copied())
}

fn rule(id: &str, family: &str, profile: &str) -> RuleDefinition {
    RuleDefinition::new(id, family, profile, "inside", format!("{id} rule"))
}

#[test]
fn registry_sorts_families_profiles_and_rules_for_stable_catalog_output() {
    let registry = RuleRegistry::new(
        1,
        vec![
            family("z_family", &["z_profile"]),
            family("a_family", &["a_profile"]),
        ],
        vec![
            profile("z_profile", "z_family", &["z.rule"]),
            profile("a_profile", "a_family", &["a.rule"]),
        ],
        vec![
            rule("z.rule", "z_family", "z_profile"),
            rule("a.rule", "a_family", "a_profile"),
        ],
    )
    .expect("valid registry");

    assert_eq!(registry.schema_version(), 1);
    assert_eq!(
        registry
            .families()
            .iter()
            .map(RuleFamily::id)
            .collect::<Vec<_>>(),
        ["a_family", "z_family"]
    );
    assert_eq!(
        registry
            .profiles()
            .iter()
            .map(RuleProfile::id)
            .collect::<Vec<_>>(),
        ["a_profile", "z_profile"]
    );
    assert_eq!(
        registry
            .rules()
            .iter()
            .map(RuleDefinition::id)
            .collect::<Vec<_>>(),
        ["a.rule", "z.rule"]
    );
}

#[test]
fn registry_rejects_duplicate_rule_ids() {
    let error = RuleRegistry::new(
        1,
        vec![family("design", &["layout"])],
        vec![profile("layout", "design", &["layout.inside"])],
        vec![
            rule("layout.inside", "design", "layout"),
            rule("layout.inside", "design", "layout"),
        ],
    )
    .expect_err("duplicate rule IDs must fail");

    assert_eq!(
        error,
        RegistryError::DuplicateRuleId {
            id: "layout.inside".to_owned()
        }
    );
}

#[test]
fn registry_rejects_family_profile_membership_that_does_not_resolve() {
    let error = RuleRegistry::new(
        1,
        vec![family("design", &["missing"])],
        vec![profile("layout", "design", &["layout.inside"])],
        vec![rule("layout.inside", "design", "layout")],
    )
    .expect_err("family profile members must resolve");

    assert_eq!(
        error,
        RegistryError::UnknownFamilyProfile {
            family_id: "design".to_owned(),
            profile_id: "missing".to_owned(),
        }
    );
}

#[test]
fn registry_rejects_profile_rule_membership_that_does_not_resolve() {
    let error = RuleRegistry::new(
        1,
        vec![family("design", &["layout"])],
        vec![profile("layout", "design", &["layout.missing"])],
        vec![rule("layout.inside", "design", "layout")],
    )
    .expect_err("profile rule members must resolve");

    assert_eq!(
        error,
        RegistryError::UnknownProfileRule {
            profile_id: "layout".to_owned(),
            rule_id: "layout.missing".to_owned(),
        }
    );
}

#[test]
fn registry_rejects_rules_owned_by_another_family_or_profile() {
    let error = RuleRegistry::new(
        1,
        vec![family("design", &["layout"])],
        vec![profile("layout", "design", &["media.aspect"])],
        vec![rule("media.aspect", "media", "media")],
    )
    .expect_err("profile membership must match rule ownership");

    assert_eq!(
        error,
        RegistryError::RuleOwnerMismatch {
            rule_id: "media.aspect".to_owned(),
            declared_family: "media".to_owned(),
            declared_profile: "media".to_owned(),
            listed_family: "design".to_owned(),
            listed_profile: "layout".to_owned(),
        }
    );
}
