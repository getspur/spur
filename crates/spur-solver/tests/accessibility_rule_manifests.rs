use std::collections::BTreeSet;

use serde_json::{json, Value};
use spur_solver::rules::{
    compiler::RuleFamilyCompiler,
    families::accessibility::{builtin_registry, COMPILER},
    manifest_format::{
        validate_manifest_bundle, FamilyManifestV1, ManifestBundleV1, RuleManifestV1,
        SchemaVersionV1,
    },
};

const FAMILY_YAML: &str = include_str!("../src/rules/families/accessibility/family.yaml");
const RULE_YAMLS: [&str; 4] = [
    include_str!("../src/rules/families/accessibility/rules/focus_not_obscured.yaml"),
    include_str!("../src/rules/families/accessibility/rules/reflow.yaml"),
    include_str!("../src/rules/families/accessibility/rules/target_size.yaml"),
    include_str!("../src/rules/families/accessibility/rules/text_contrast.yaml"),
];

fn parse_bundle() -> ManifestBundleV1 {
    let family = serde_yml::from_str::<FamilyManifestV1>(FAMILY_YAML)
        .expect("parse strict accessibility family manifest");
    let rules = RULE_YAMLS
        .into_iter()
        .map(|yaml| {
            serde_yml::from_str::<RuleManifestV1>(yaml)
                .expect("parse strict accessibility rule manifest")
        })
        .collect();

    ManifestBundleV1 {
        schema_version: SchemaVersionV1,
        families: vec![family],
        rules,
    }
}

fn public_rule_projection(rule: &RuleManifestV1) -> Value {
    json!({
        "id": rule.id,
        "family": rule.family,
        "profile": rule.profile,
        "rule_version": rule.rule_version,
        "primitive": rule.primitive,
        "summary": rule.summary,
        "availability": rule.availability,
        "execution_kind": rule.execution_kind,
        "default_strength": rule.strength,
        "authorities": rule.authorities,
        "requires": rule.requires,
        "llm_encoding": rule.llm_encoding,
        "solver_encoding": rule.solver_encoding,
        "examples": rule.examples,
    })
}

#[test]
fn accessibility_manifests_validate_and_preserve_the_public_catalog() {
    let bundle = parse_bundle();
    validate_manifest_bundle(&bundle).expect("validate accessibility manifest bundle");

    let expected_ids = [
        "a11y.focus_not_obscured",
        "a11y.reflow",
        "a11y.target_size",
        "a11y.text_contrast",
    ];
    assert_eq!(
        bundle
            .rules
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        expected_ids
    );
    assert_eq!(
        bundle
            .rules
            .iter()
            .map(|rule| rule.handler.expect("implemented hard rule handler"))
            .collect::<BTreeSet<_>>()
            .len(),
        expected_ids.len()
    );

    let catalog = builtin_registry();
    let family = &bundle.families[0];
    assert_eq!(
        serde_json::to_value(&catalog.families()[0]).expect("serialize catalog family"),
        json!({
            "id": family.id,
            "family_version": family.family_version,
            "summary": family.summary,
            "profiles": family
                .profiles
                .iter()
                .map(|profile| profile.id.as_str())
                .collect::<Vec<_>>(),
        })
    );

    let profile = &family.profiles[0];
    assert_eq!(
        serde_json::to_value(&catalog.profiles()[0]).expect("serialize catalog profile"),
        json!({
            "id": profile.id,
            "family": family.id,
            "profile_version": profile.profile_version,
            "summary": profile.summary,
            "rules": expected_ids,
        })
    );

    for rule in &bundle.rules {
        let catalog_rule = catalog.rule(&rule.id).expect("matching Rust catalog rule");
        assert_eq!(
            serde_json::to_value(catalog_rule).expect("serialize catalog rule"),
            public_rule_projection(rule),
            "public catalog drift for {}",
            rule.id
        );
    }
}

#[test]
fn accessibility_conformance_vectors_are_separate_and_executable() {
    let bundle = parse_bundle();

    for rule in bundle.rules {
        let conformance = rule
            .conformance
            .expect("implemented hard rule conformance vectors");
        assert!(!conformance.valid.is_empty(), "{} valid vector", rule.id);
        assert!(
            !conformance.invalid.is_empty(),
            "{} invalid vector",
            rule.id
        );

        for vector in conformance.valid.iter().chain(&conformance.invalid) {
            assert_eq!(vector.request["family"], "accessibility");
            assert_eq!(vector.request["rules"][0]["rule_id"], rule.id);
            COMPILER
                .compile(vector.request.clone())
                .unwrap_or_else(|error| {
                    panic!("{} vector `{}` must compile: {error}", rule.id, vector.name)
                });
        }
    }
}
