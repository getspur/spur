use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
};

use serde::de::DeserializeOwned;
use serde_json::Value;
use spur_solver::rules::{
    families::policy::builtin_registry,
    manifest_format::{
        validate_manifest_bundle, validate_rule_manifest, AvailabilityV1, ExecutionKindV1,
        FamilyManifestV1, ManifestBundleV1, ManifestRouteV1, NativeHandlerV1, RuleManifestV1,
        RuleStrengthV1, SchemaVersionV1,
    },
};

const RULE_FILES: &[&str] = &[
    "dynamic_separation_of_duty.yaml",
    "minimum_privilege.yaml",
    "permission_reachable.yaml",
    "role_hierarchy_acyclic.yaml",
    "static_separation_of_duty.yaml",
];

fn load_yaml<T: DeserializeOwned>(relative_path: &str) -> T {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative_path);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_yml::from_str(&source)
        .unwrap_or_else(|error| panic!("parse strict YAML {}: {error}", path.display()))
}

fn policy_manifests() -> (FamilyManifestV1, Vec<RuleManifestV1>) {
    let family = load_yaml("src/rules/families/policy/family.yaml");
    let mut rules = RULE_FILES
        .iter()
        .map(|name| load_yaml(&format!("src/rules/families/policy/rules/{name}")))
        .collect::<Vec<_>>();
    rules.sort_by(|left: &RuleManifestV1, right| left.id.cmp(&right.id));
    (family, rules)
}

#[test]
fn policy_manifests_preserve_exact_catalog_data_and_executable_routes() {
    let (family, rules) = policy_manifests();
    let expected_ids = vec![
        "rbac.dynamic_separation_of_duty",
        "rbac.minimum_privilege",
        "rbac.permission_reachable",
        "rbac.role_hierarchy_acyclic",
        "rbac.static_separation_of_duty",
    ];

    assert_eq!(family.id, "policy");
    assert_eq!(family.family_version, 1);
    assert_eq!(
        family.summary,
        "Finite RBAC reachability, hierarchy, and separation-of-duty rules."
    );
    assert_eq!(family.profiles.len(), 1);
    assert_eq!(family.profiles[0].id, "nist_rbac");
    assert_eq!(family.profiles[0].profile_version, 1);
    assert_eq!(
        family.profiles[0].summary,
        "Core, hierarchical, static-separation, and dynamic-separation RBAC constraints."
    );
    assert_eq!(
        rules
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        expected_ids
    );

    validate_manifest_bundle(&ManifestBundleV1 {
        schema_version: SchemaVersionV1,
        families: vec![family],
        rules: rules.clone(),
    })
    .expect("policy manifest bundle validates");

    let expected_handlers = BTreeMap::from([
        (
            "rbac.dynamic_separation_of_duty",
            NativeHandlerV1::RbacDynamicSeparationOfDuty,
        ),
        (
            "rbac.minimum_privilege",
            NativeHandlerV1::RbacMinimumPrivilege,
        ),
        (
            "rbac.permission_reachable",
            NativeHandlerV1::RbacPermissionReachable,
        ),
        (
            "rbac.role_hierarchy_acyclic",
            NativeHandlerV1::RbacRoleHierarchyAcyclic,
        ),
        (
            "rbac.static_separation_of_duty",
            NativeHandlerV1::RbacStaticSeparationOfDuty,
        ),
    ]);
    assert_eq!(expected_handlers.len(), 5);
    assert_eq!(
        expected_handlers
            .values()
            .copied()
            .collect::<BTreeSet<_>>()
            .len(),
        5,
        "policy routes must select unique native handlers"
    );

    for rule in &rules {
        let catalog_rule = builtin_registry()
            .rule(&rule.id)
            .unwrap_or_else(|| panic!("catalog rule {} exists", rule.id));
        let mut manifest_catalog = serde_json::to_value(rule)
            .expect("serialize rule manifest")
            .as_object()
            .expect("rule manifest object")
            .clone();
        manifest_catalog.remove("schema_version");
        manifest_catalog.remove("subjects");
        manifest_catalog.remove("parameters");
        manifest_catalog.remove("handler");
        manifest_catalog.remove("conformance");
        let strength = manifest_catalog
            .remove("strength")
            .expect("manifest strength");
        manifest_catalog.insert("default_strength".to_owned(), strength);
        assert_eq!(
            Value::Object(manifest_catalog),
            serde_json::to_value(catalog_rule).expect("serialize catalog rule"),
            "public catalog data drifted for {}",
            rule.id
        );

        if let Some(expected_handler) = expected_handlers.get(rule.id.as_str()) {
            assert_eq!(rule.handler, Some(*expected_handler));
            assert_eq!(
                validate_rule_manifest(rule),
                Ok(ManifestRouteV1::Executable)
            );
            let conformance = rule.conformance.as_ref().expect("executable conformance");
            assert!(!conformance.valid.is_empty());
            assert!(!conformance.invalid.is_empty());
        }
    }
}

#[test]
fn minimum_privilege_is_an_executable_objective_route() {
    let (_, rules) = policy_manifests();
    let rule = rules
        .iter()
        .find(|rule| rule.id == "rbac.minimum_privilege")
        .expect("minimum privilege manifest");

    assert_eq!(rule.availability, AvailabilityV1::Implemented);
    assert_eq!(rule.strength, RuleStrengthV1::Advisory);
    assert_eq!(rule.execution_kind, ExecutionKindV1::Objective);
    assert_eq!(rule.handler, Some(NativeHandlerV1::RbacMinimumPrivilege));
    let conformance = rule.conformance.as_ref().expect("objective conformance");
    assert!(!conformance.valid.is_empty());
    assert!(!conformance.invalid.is_empty());
    assert_eq!(
        validate_rule_manifest(rule),
        Ok(ManifestRouteV1::Executable)
    );
}
