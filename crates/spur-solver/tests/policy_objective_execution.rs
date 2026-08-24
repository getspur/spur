use serde_json::{json, Value};
use spur_solver::{
    rules::{
        execute::{prepare, run},
        primitives::{int, le},
    },
    service::SolverService,
    types::{
        ConstraintDecl, ConstraintItem, ModelValue, ObjectiveBound, OptimizationTermination,
        SolveStatus,
    },
};

fn minimum_privilege_request() -> Value {
    json!({
        "family": "policy",
        "mode": "synthesize",
        "rules": [
            {"rule_id": "rbac.minimum_privilege", "subjects": ["alice"], "parameters": {}},
            {"rule_id": "rbac.permission_reachable", "subjects": ["alice", "read"], "parameters": {}},
            {"rule_id": "rbac.permission_reachable", "subjects": ["alice", "write"], "parameters": {}}
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
    })
}

#[tokio::test]
async fn minimum_privilege_proves_cost_three() {
    let result = run(
        &SolverService::new(),
        prepare(minimum_privilege_request()).unwrap(),
    )
    .await
    .unwrap();
    let optimization = result.solver.optimization.unwrap();

    assert_eq!(optimization.termination, OptimizationTermination::Complete);
    assert_eq!(
        optimization.solutions[0].objectives[0].value,
        Some(ModelValue::Int(3))
    );
    assert_eq!(
        optimization.solutions[0].objectives[0].bound,
        ObjectiveBound::Finite {
            exact: "3".to_owned()
        }
    );
}

#[test]
fn minimum_privilege_rejects_invalid_utility_and_bindings() {
    let mut cases = Vec::new();

    let mut verify = minimum_privilege_request();
    verify["mode"] = json!("verify");
    cases.push(("verify mode", verify));

    let mut empty_permissions = minimum_privilege_request();
    empty_permissions["facts"]["principals"]["alice"]["required_permissions"] = json!([]);
    cases.push(("empty required permissions", empty_permissions));

    let mut duplicate_permissions = minimum_privilege_request();
    duplicate_permissions["facts"]["principals"]["alice"]["required_permissions"] =
        json!(["read", "read"]);
    cases.push(("duplicate required permissions", duplicate_permissions));

    let mut missing_cost = minimum_privilege_request();
    missing_cost["facts"]["principals"]["alice"]["grant_costs"]
        .as_object_mut()
        .unwrap()
        .remove("writer");
    cases.push(("missing candidate cost", missing_cost));

    let mut empty_scoped_costs = minimum_privilege_request();
    empty_scoped_costs["rules"][0]["subjects"] = json!(["alice", "bob"]);
    empty_scoped_costs["rules"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "rule_id": "rbac.permission_reachable",
            "subjects": ["bob", "read"],
            "parameters": {}
        }));
    empty_scoped_costs["facts"]["principals"]["bob"] = json!({
        "roles": ["reader"],
        "required_permissions": ["read"],
        "grant_costs": {}
    });
    cases.push(("empty scoped grant costs", empty_scoped_costs));

    let mut non_positive_cost = minimum_privilege_request();
    non_positive_cost["facts"]["principals"]["alice"]["grant_costs"]["writer"] = json!(0);
    cases.push(("non-positive cost", non_positive_cost));

    let mut unknown_cost_role = minimum_privilege_request();
    unknown_cost_role["facts"]["principals"]["alice"]["grant_costs"]["missing"] = json!(1);
    cases.push(("unknown cost role", unknown_cost_role));

    let mut unknown_principal = minimum_privilege_request();
    unknown_principal["rules"][0]["subjects"] = json!(["missing"]);
    cases.push(("unknown objective principal", unknown_principal));

    let mut uncovered = minimum_privilege_request();
    uncovered["rules"]
        .as_array_mut()
        .unwrap()
        .retain(|rule| rule["subjects"] != json!(["alice", "write"]));
    cases.push(("missing reachability coverage", uncovered));

    let mut duplicate_objective = minimum_privilege_request();
    let objective = duplicate_objective["rules"][0].clone();
    duplicate_objective["rules"]
        .as_array_mut()
        .unwrap()
        .push(objective);
    cases.push(("duplicate objective", duplicate_objective));

    let mut no_candidate = minimum_privilege_request();
    no_candidate["unknowns"] = json!([]);
    cases.push(("no scoped candidate", no_candidate));

    for (name, request) in cases {
        assert!(prepare(request).is_err(), "{name} must fail before solving");
    }
}

#[tokio::test]
async fn cost_below_three_is_unsatisfiable() {
    let mut prepared = prepare(minimum_privilege_request()).unwrap();
    let objective = prepared.request.objectives[0].expr.clone();
    prepared
        .request
        .constraints
        .push(ConstraintItem::Declared(ConstraintDecl {
            id: Some("test_strict_better".to_owned()),
            group: None,
            soft: false,
            weight: None,
            expr: le(objective, int(2)),
        }));

    assert_eq!(
        SolverService::new()
            .solve_constraints(prepared.request)
            .await
            .unwrap()
            .status,
        SolveStatus::Unsat
    );
}
