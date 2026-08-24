use spur_solver::rules::{
    manifest_conformance_vectors, manifest_executable_rule_ids,
    manifest_family_executable_rule_ids, manifest_family_registry,
    manifest_format::{ExecutionKindV1, NativeHandlerV1, ParameterKindV1, SubjectCardinalityV1},
    manifest_registry, manifest_rule_contract, manifest_rule_handler,
};

#[test]
fn embedded_manifest_registry_contains_41_sorted_rules() {
    let rule_ids = manifest_registry()
        .rules()
        .iter()
        .map(|rule| rule.id())
        .collect::<Vec<_>>();

    assert_eq!(rule_ids.len(), 41);
    assert!(rule_ids.windows(2).all(|ids| ids[0] < ids[1]));
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
fn executable_rule_ids_cover_the_sorted_catalog() {
    let executable = manifest_executable_rule_ids();

    assert_eq!(manifest_registry().rules().len(), 41);
    assert_eq!(executable.len(), 41);
    assert!(executable.windows(2).all(|ids| ids[0] < ids[1]));
    assert!(executable.iter().any(|id| id == "rbac.minimum_privilege"));
    assert!(executable.iter().any(|id| id == "placement.minimize_skew"));
    assert_eq!(
        manifest_family_executable_rule_ids("policy").expect("policy executable manifest IDs"),
        [
            "rbac.dynamic_separation_of_duty",
            "rbac.minimum_privilege",
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

    let minimum_privilege =
        manifest_rule_contract("rbac.minimum_privilege").expect("minimum-privilege contract");
    assert_eq!(minimum_privilege.execution_kind, ExecutionKindV1::Objective);
    assert_eq!(
        manifest_rule_handler("rbac.minimum_privilege"),
        Some(NativeHandlerV1::RbacMinimumPrivilege)
    );
    let conformance = manifest_conformance_vectors("rbac.minimum_privilege")
        .expect("minimum-privilege conformance vectors");
    assert_eq!(conformance.valid.len(), 1);
    assert_eq!(conformance.invalid.len(), 1);
    assert!(manifest_rule_contract("missing").is_none());
}
