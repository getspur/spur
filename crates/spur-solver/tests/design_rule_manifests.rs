use std::collections::BTreeSet;

use serde_json::{json, Value};
use spur_solver::rules::{
    execute::prepare,
    families::design::builtin_registry,
    manifest_format::{
        validate_manifest_bundle, FamilyManifestV1, ManifestBundleV1, NativeHandlerV1,
        RuleManifestV1, SchemaVersionV1,
    },
};

const FAMILY_SOURCE: &str = include_str!("../src/rules/families/design/family.yaml");
const RULE_SOURCES: [&str; 4] = [
    include_str!("../src/rules/families/design/rules/axis_capacity.yaml"),
    include_str!("../src/rules/families/design/rules/containment.yaml"),
    include_str!("../src/rules/families/design/rules/non_overlap.yaml"),
    include_str!("../src/rules/families/design/rules/aspect_ratio.yaml"),
];

fn manifests() -> (FamilyManifestV1, Vec<RuleManifestV1>) {
    let family = serde_yml::from_str(FAMILY_SOURCE).expect("strict design family manifest");
    let rules = RULE_SOURCES
        .into_iter()
        .map(|source| serde_yml::from_str(source).expect("strict design rule manifest"))
        .collect();
    (family, rules)
}

#[test]
fn design_manifests_validate_with_exact_profile_ownership_and_handlers() {
    let (family, rules) = manifests();
    let bundle = ManifestBundleV1 {
        schema_version: SchemaVersionV1,
        families: vec![family.clone()],
        rules: rules.clone(),
    };

    validate_manifest_bundle(&bundle).expect("valid design manifest bundle");
    assert_eq!(family.id, "design");
    assert_eq!(
        family
            .profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>(),
        ["geometric_integrity", "layout_capacity"]
    );
    assert_eq!(
        rules
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        [
            "layout.axis_capacity",
            "layout.containment",
            "layout.non_overlap",
            "media.aspect_ratio",
        ]
    );
    assert_eq!(
        rules
            .iter()
            .map(|rule| rule.handler.expect("implemented-hard handler"))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            NativeHandlerV1::LayoutAxisCapacity,
            NativeHandlerV1::LayoutContainment,
            NativeHandlerV1::LayoutNonOverlap,
            NativeHandlerV1::MediaAspectRatio,
        ])
    );
}

#[test]
fn design_manifests_preserve_the_exact_public_catalog_projection() {
    let (family, rules) = manifests();
    let registry = builtin_registry();
    let profile_ids = family
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        json!({
            "id": &family.id,
            "family_version": family.family_version,
            "summary": &family.summary,
            "profiles": profile_ids,
        }),
        serde_json::to_value(registry.family("design").expect("design family"))
            .expect("serialize design family")
    );

    for profile in &family.profiles {
        let owned_rules = rules
            .iter()
            .filter(|rule| rule.profile == profile.id)
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            json!({
                "id": &profile.id,
                "family": &family.id,
                "profile_version": profile.profile_version,
                "summary": &profile.summary,
                "rules": owned_rules,
            }),
            serde_json::to_value(
                registry
                    .profile(&profile.id)
                    .unwrap_or_else(|| panic!("missing profile {}", profile.id)),
            )
            .expect("serialize design profile")
        );
    }

    for rule in &rules {
        assert_eq!(
            catalog_projection(rule),
            serde_json::to_value(
                registry
                    .rule(&rule.id)
                    .unwrap_or_else(|| panic!("missing rule {}", rule.id)),
            )
            .expect("serialize design rule"),
            "{} public catalog projection changed",
            rule.id
        );
    }
}

#[test]
fn design_conformance_vectors_are_separate_executable_requests() {
    let (_, rules) = manifests();

    for rule in rules {
        let conformance = rule
            .conformance
            .as_ref()
            .expect("implemented-hard conformance vectors");
        assert_eq!(conformance.valid.len(), 1, "{} valid vectors", rule.id);
        assert_eq!(conformance.invalid.len(), 1, "{} invalid vectors", rule.id);
        assert_ne!(
            conformance.valid[0].request, conformance.invalid[0].request,
            "{} conformance outcomes must use distinct requests",
            rule.id
        );

        for vector in conformance.valid.iter().chain(&conformance.invalid) {
            assert_eq!(vector.request["family"], "design", "{}", vector.name);
            assert!(vector.request["scene"].is_object(), "{}", vector.name);
            prepare(vector.request.clone())
                .unwrap_or_else(|error| panic!("{} must compile: {error}", vector.name));
        }
    }
}

fn catalog_projection(rule: &RuleManifestV1) -> Value {
    json!({
        "id": &rule.id,
        "family": &rule.family,
        "profile": &rule.profile,
        "rule_version": rule.rule_version,
        "primitive": &rule.primitive,
        "summary": &rule.summary,
        "availability": rule.availability,
        "execution_kind": rule.execution_kind,
        "default_strength": rule.strength,
        "authorities": &rule.authorities,
        "requires": &rule.requires,
        "llm_encoding": &rule.llm_encoding,
        "solver_encoding": &rule.solver_encoding,
        "examples": &rule.examples,
    })
}
