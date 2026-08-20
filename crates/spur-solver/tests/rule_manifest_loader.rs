use serde_json::Value;
use spur_solver::rules::{
    manifest_conformance_vectors, manifest_executable_rule_ids,
    manifest_family_executable_rule_ids, manifest_family_registry,
    manifest_format::{NativeHandlerV1, ParameterKindV1, SubjectCardinalityV1},
    manifest_registry, manifest_rule_contract, manifest_rule_handler,
};

const BUILTIN_RULE_CATALOG_V1: &str = include_str!("fixtures/builtin_rule_catalog_v1.json");

#[test]
fn embedded_manifest_registry_matches_frozen_catalog() {
    let expected: Value =
        serde_json::from_str(BUILTIN_RULE_CATALOG_V1).expect("valid frozen catalog fixture");
    let actual = serde_json::to_value(manifest_registry()).expect("serialize manifest registry");

    assert_eq!(actual, expected);
}

#[test]
fn family_registry_is_a_narrow_owned_projection() {
    let accessibility =
        manifest_family_registry("accessibility").expect("accessibility manifest registry");

    assert_eq!(
        accessibility
            .families()
            .iter()
            .map(|family| family.id())
            .collect::<Vec<_>>(),
        ["accessibility"]
    );
    assert!(accessibility
        .profiles()
        .iter()
        .all(|profile| profile.family() == "accessibility"));
    assert!(accessibility
        .rules()
        .iter()
        .all(|rule| rule.family() == "accessibility"));
    assert!(manifest_family_registry("missing").is_none());
}

#[test]
fn executable_rule_ids_exclude_catalog_only_rules() {
    let executable = manifest_executable_rule_ids();

    assert_eq!(executable.len(), 31);
    assert!(executable.windows(2).all(|ids| ids[0] < ids[1]));
    assert!(!executable.iter().any(|id| id == "rbac.minimum_privilege"));
    assert_eq!(
        manifest_family_executable_rule_ids("policy").expect("policy executable manifest IDs"),
        [
            "rbac.dynamic_separation_of_duty",
            "rbac.permission_reachable",
            "rbac.role_hierarchy_acyclic",
            "rbac.static_separation_of_duty",
        ]
    );
    assert!(manifest_family_executable_rule_ids("missing").is_none());
}

#[test]
fn contract_handler_and_conformance_lookups_are_independent() {
    let contract = manifest_rule_contract("a11y.target_size").expect("target-size contract");
    assert_eq!(
        contract.subjects.cardinality,
        SubjectCardinalityV1::Exact { count: 1 }
    );
    assert_eq!(
        contract
            .parameters
            .iter()
            .map(|parameter| (parameter.name.as_str(), parameter.kind))
            .collect::<Vec<_>>(),
        [
            ("minimum_width", ParameterKindV1::Integer),
            ("minimum_height", ParameterKindV1::Integer),
            ("exception", ParameterKindV1::NativeObject),
        ]
    );
    assert_eq!(
        manifest_rule_handler("a11y.target_size"),
        Some(NativeHandlerV1::A11yTargetSize)
    );

    let conformance =
        manifest_conformance_vectors("a11y.target_size").expect("target-size conformance vectors");
    assert_eq!(conformance.valid.len(), 1);
    assert_eq!(conformance.invalid.len(), 1);

    assert!(manifest_rule_handler("rbac.minimum_privilege").is_none());
    assert!(manifest_conformance_vectors("rbac.minimum_privilege").is_none());
    assert!(manifest_rule_contract("missing").is_none());
}
