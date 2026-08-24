use serde_json::{json, Value};
use spur_solver::{
    rules::{execute::prepare, manifest::manifest_rule_handler, manifest_format::NativeHandlerV1},
    types::{ConstraintExpr, ConstraintItem, ObjectiveOp, ObjectivePriority},
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
fn minimum_privilege_dispatches_a_neutral_binding_and_typed_minimize_objective() {
    assert_eq!(
        manifest_rule_handler("rbac.minimum_privilege"),
        Some(NativeHandlerV1::RbacMinimumPrivilege)
    );

    let prepared = prepare(json!({
        "family": "policy",
        "mode": "synthesize",
        "rules": [
            {
                "rule_id": "rbac.minimum_privilege",
                "subjects": ["alice"],
                "parameters": {}
            },
            {
                "rule_id": "rbac.permission_reachable",
                "subjects": ["alice", "read"],
                "parameters": {}
            },
            {
                "rule_id": "rbac.permission_reachable",
                "subjects": ["alice", "write"],
                "parameters": {}
            }
        ],
        "facts": {
            "roles": {
                "reader": {"inherits": [], "permissions": ["read"]},
                "writer": {"inherits": [], "permissions": ["write"]},
                "admin": {"inherits": [], "permissions": ["read", "write"]}
            },
            "principals": {
                "alice": {
                    "roles": [],
                    "required_permissions": ["read", "write"],
                    "grant_costs": {"reader": 1, "writer": 2, "admin": 5}
                }
            },
            "sessions": {}
        },
        "unknowns": [
            {"kind": "principal_role", "principal": "alice", "role": "reader"},
            {"kind": "principal_role", "principal": "alice", "role": "writer"},
            {"kind": "principal_role", "principal": "alice", "role": "admin"}
        ]
    }))
    .expect("minimum privilege manifest handler must compile");

    assert_eq!(prepared.rules.len(), 3);
    assert_eq!(prepared.rules[0].rule_id, "rbac.minimum_privilege");
    assert_eq!(
        prepared.rules[0].predicate,
        ConstraintExpr::Bool { value: true }
    );
    assert_eq!(
        prepared
            .request
            .constraints
            .iter()
            .filter(|constraint| matches!(
                constraint,
                ConstraintItem::Declared(decl)
                    if decl.id.as_deref()
                        == Some("policy_rule_0_rbac_minimum_privilege")
                        && decl.group.is_none()
                        && !decl.soft
                        && decl.weight.is_none()
                        && matches!(&decl.expr, ConstraintExpr::Bool { value: true })
            ))
            .count(),
        1,
        "minimum privilege must contribute exactly one named hard-neutral binding"
    );
    assert!(
        prepared
            .request
            .constraints
            .iter()
            .all(|constraint| constraint.soft_weight().is_none()),
        "policy dispatch must not lower any rule to a soft constraint"
    );
    assert_eq!(prepared.request.objective_priority, ObjectivePriority::Lex);
    assert_eq!(prepared.request.objectives.len(), 1);
    assert_eq!(prepared.request.objectives[0].op, ObjectiveOp::Minimize);
}
