use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use serde_json::{json, Value};
use spur_solver::{
    rules::{
        execute::{prepare, run},
        families::resource::builtin_registry,
        manifest_format::{
            validate_manifest_bundle, FamilyManifestV1, ManifestBundleV1, RuleManifestV1,
            SchemaVersionV1,
        },
    },
    service::SolverService,
    types::SolveStatus,
};

const RULE_FILES: [&str; 5] = [
    "aggregate_capacity.yaml",
    "minimum_failure_domains.yaml",
    "quota_capacity.yaml",
    "request_within_limit.yaml",
    "topology_max_skew.yaml",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/rules/families/resource")
}

fn load_manifests() -> (FamilyManifestV1, Vec<RuleManifestV1>) {
    let root = manifest_dir();
    let family_source =
        fs::read_to_string(root.join("family.yaml")).expect("resource family manifest must exist");
    let family = serde_yml::from_str(&family_source).expect("resource family manifest must parse");
    let rules = RULE_FILES
        .iter()
        .map(|file| {
            let source =
                fs::read_to_string(root.join("rules").join(file)).unwrap_or_else(|error| {
                    panic!("resource rule manifest {file} must exist: {error}")
                });
            serde_yml::from_str(&source)
                .unwrap_or_else(|error| panic!("resource rule manifest {file} must parse: {error}"))
        })
        .collect();
    (family, rules)
}

fn public_rule_json(rule: &RuleManifestV1) -> Value {
    let mut value = serde_json::to_value(rule).expect("serialize rule manifest");
    let object = value.as_object_mut().expect("rule manifest object");
    object.remove("schema_version");
    object.remove("subjects");
    object.remove("parameters");
    object.remove("handler");
    object.remove("conformance");
    let strength = object.remove("strength").expect("rule strength");
    object.insert("default_strength".to_owned(), strength);
    value
}

#[test]
fn resource_manifests_validate_with_exact_ids_owners_handlers_and_public_catalog_data() {
    let (family, rules) = load_manifests();
    validate_manifest_bundle(&ManifestBundleV1 {
        schema_version: SchemaVersionV1,
        families: vec![family.clone()],
        rules: rules.clone(),
    })
    .expect("resource manifest bundle must validate");

    let expected_owners = BTreeMap::from([
        (
            "capacity",
            BTreeSet::from([
                "resource.aggregate_capacity",
                "resource.quota_capacity",
                "resource.request_within_limit",
            ]),
        ),
        (
            "topology_placement",
            BTreeSet::from([
                "placement.minimum_failure_domains",
                "placement.topology_max_skew",
            ]),
        ),
    ]);
    let actual_owners = family
        .profiles
        .iter()
        .map(|profile| {
            (
                profile.id.as_str(),
                rules
                    .iter()
                    .filter(|rule| rule.profile == profile.id)
                    .map(|rule| rule.id.as_str())
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual_owners, expected_owners);

    let handlers = rules
        .iter()
        .map(|rule| rule.handler.expect("implemented rule handler"))
        .collect::<BTreeSet<_>>();
    assert_eq!(handlers.len(), rules.len(), "handlers must be unique");

    let registry = builtin_registry();
    let family_json = json!({
        "id": family.id,
        "family_version": family.family_version,
        "summary": family.summary,
        "profiles": family.profiles.iter().map(|profile| &profile.id).collect::<Vec<_>>(),
    });
    assert_eq!(
        family_json,
        serde_json::to_value(registry.family("resource").expect("resource family"))
            .expect("serialize resource family")
    );

    for profile in &family.profiles {
        let rule_ids = rules
            .iter()
            .filter(|rule| rule.profile == profile.id)
            .map(|rule| &rule.id)
            .collect::<Vec<_>>();
        let profile_json = json!({
            "id": profile.id,
            "family": family.id,
            "profile_version": profile.profile_version,
            "summary": profile.summary,
            "rules": rule_ids,
        });
        assert_eq!(
            profile_json,
            serde_json::to_value(registry.profile(&profile.id).expect("resource profile"))
                .expect("serialize resource profile")
        );
    }

    for rule in &rules {
        assert_eq!(
            public_rule_json(rule),
            serde_json::to_value(registry.rule(&rule.id).expect("resource rule"))
                .expect("serialize resource rule"),
            "public catalog data changed for {}",
            rule.id
        );
    }
}

#[tokio::test]
async fn resource_manifest_conformance_vectors_execute() {
    let (_, rules) = load_manifests();
    let service = SolverService::new();

    for rule in rules {
        let conformance = rule.conformance.expect("implemented rule conformance");
        for vector in conformance.valid {
            let result = run(
                &service,
                prepare(vector.request).unwrap_or_else(|error| {
                    panic!("{} valid vector must compile: {error}", rule.id)
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("{} valid vector must run: {error}", rule.id));
            assert_eq!(result.solver.status, SolveStatus::Sat, "{} valid", rule.id);
        }
        for vector in conformance.invalid {
            assert_eq!(
                vector.expected_diagnostic.as_deref(),
                Some(format!("{}.violation", rule.id).as_str())
            );
            let result = run(
                &service,
                prepare(vector.request).unwrap_or_else(|error| {
                    panic!("{} invalid vector must compile: {error}", rule.id)
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("{} invalid vector must run: {error}", rule.id));
            assert_eq!(
                result.solver.status,
                SolveStatus::Unsat,
                "{} invalid",
                rule.id
            );
            assert_eq!(result.rule_results.len(), 1, "{} attribution", rule.id);
            assert_eq!(result.rule_results[0].rule_id, rule.id);
        }
    }
}
