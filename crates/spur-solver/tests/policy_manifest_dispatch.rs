use serde_json::{json, Value};
use spur_solver::rules::{
    execute::prepare, manifest::manifest_rule_handler, manifest_format::NativeHandlerV1,
};

fn policy_request(rule: Value) -> Value {
    json!({
        "family": "policy",
        "mode": "verify",
        "rules": [rule],
        "facts": {
            "roles": {
                "admin": {
                    "inherits": ["viewer"],
                    "permissions": ["write"]
                },
                "viewer": {
                    "inherits": [],
                    "permissions": ["read"]
                },
                "auditor": {
                    "inherits": [],
                    "permissions": ["audit"]
                }
            },
            "principals": {
                "alice": {"roles": ["admin"]}
            },
            "sessions": {
                "alice-session": {
                    "principal": "alice",
                    "active_roles": ["viewer"]
                }
            }
        },
        "unknowns": []
    })
}

#[test]
fn all_policy_manifest_handlers_dispatch_to_existing_native_predicates() {
    let cases = [
        (
            json!({
                "rule_id": "rbac.permission_reachable",
                "subjects": ["alice", "read"]
            }),
            NativeHandlerV1::RbacPermissionReachable,
        ),
        (
            json!({
                "rule_id": "rbac.role_hierarchy_acyclic",
                "subjects": []
            }),
            NativeHandlerV1::RbacRoleHierarchyAcyclic,
        ),
        (
            json!({
                "rule_id": "rbac.static_separation_of_duty",
                "subjects": ["alice"],
                "parameters": {"roles": ["admin", "auditor"]}
            }),
            NativeHandlerV1::RbacStaticSeparationOfDuty,
        ),
        (
            json!({
                "rule_id": "rbac.dynamic_separation_of_duty",
                "subjects": ["alice-session"],
                "parameters": {"roles": ["viewer", "auditor"]}
            }),
            NativeHandlerV1::RbacDynamicSeparationOfDuty,
        ),
    ];

    for (rule, expected_handler) in cases {
        let rule_id = rule["rule_id"].as_str().expect("rule ID").to_owned();
        assert_eq!(
            manifest_rule_handler(&rule_id),
            Some(expected_handler),
            "{rule_id} must select its closed native handler"
        );

        let compiled = prepare(policy_request(rule))
            .unwrap_or_else(|error| panic!("{rule_id} manifest handler must compile: {error}"));
        assert_eq!(compiled.rules.len(), 1);
        assert_eq!(compiled.rules[0].rule_id, rule_id);
    }
}

#[test]
fn manifest_contract_failure_precedes_policy_fact_and_constraint_generation() {
    let mut request = policy_request(json!({
        "rule_id": "rbac.permission_reachable",
        "subjects": ["alice"]
    }));
    request["facts"]["sessions"]["alice-session"]["principal"] = json!("missing");

    let error = prepare(request)
        .expect_err("binding contract must fail before invalid policy facts")
        .to_string();

    assert!(
        error.contains("rule `rbac.permission_reachable` requires 2 subjects, got 1"),
        "unexpected error: {error}"
    );
    assert!(!error.contains("references unknown principal"));
}

#[test]
fn minimum_privilege_remains_catalog_only_and_never_reaches_native_dispatch() {
    assert_eq!(manifest_rule_handler("rbac.minimum_privilege"), None);

    let error = prepare(policy_request(json!({
        "rule_id": "rbac.minimum_privilege",
        "subjects": []
    })))
    .expect_err("catalog-only policy rule must not compile")
    .to_string();

    assert!(
        error.contains("unsupported policy rule `rbac.minimum_privilege`"),
        "unexpected error: {error}"
    );
}
