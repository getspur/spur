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

fn minimize_skew_request(replicas: i64) -> Value {
    json!({
        "family": "resource",
        "mode": "synthesize",
        "rules": [
            {"rule_id": "placement.minimize_skew", "subjects": ["api"], "parameters": {}}
        ],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": replicas,
                    "requests": {},
                    "limits": {},
                    "domain_counts": {"zone-a": null, "zone-b": null}
                }
            },
            "pools": {},
            "quotas": {}
        },
        "unknowns": [
            {"subject": "api", "field": "domain_counts.zone-a", "min": 0, "max": replicas},
            {"subject": "api", "field": "domain_counts.zone-b", "min": 0, "max": replicas}
        ]
    })
}

#[tokio::test]
async fn minimize_skew_proves_one_for_three_replicas() {
    let result = run(
        &SolverService::new(),
        prepare(minimize_skew_request(3)).unwrap(),
    )
    .await
    .unwrap();
    let optimization = result.solver.optimization.unwrap();
    assert_eq!(optimization.termination, OptimizationTermination::Complete);
    assert_eq!(
        optimization.solutions[0].objectives[0].value,
        Some(ModelValue::Int(1))
    );
    assert_eq!(
        optimization.solutions[0].objectives[0].bound,
        ObjectiveBound::Finite {
            exact: "1".to_owned()
        }
    );
    assert!(result
        .assignments
        .iter()
        .any(|item| { item.field == "topology_skew" && item.value == ModelValue::Int(1) }));
}

#[test]
fn minimize_skew_rejects_verify_missing_bounds_duplicate_and_fewer_domains() {
    let mut verify = minimize_skew_request(3);
    verify["mode"] = json!("verify");
    verify["facts"]["workloads"]["api"]["domain_counts"] = json!({"zone-a": 2, "zone-b": 1});
    verify["unknowns"] = json!([]);

    let mut missing_domain_bound = minimize_skew_request(3);
    missing_domain_bound["unknowns"]
        .as_array_mut()
        .unwrap()
        .pop();

    let mut missing_replica_bound = minimize_skew_request(3);
    missing_replica_bound["facts"]["workloads"]["api"]["replicas"] = Value::Null;

    let mut duplicate = minimize_skew_request(3);
    let objective = duplicate["rules"][0].clone();
    duplicate["rules"].as_array_mut().unwrap().push(objective);

    let mut fewer_domains = minimize_skew_request(3);
    fewer_domains["facts"]["workloads"]["api"]["domain_counts"] = json!({"zone-a": null});
    fewer_domains["unknowns"] = json!([
        {"subject": "api", "field": "domain_counts.zone-a", "min": 0, "max": 3}
    ]);

    for request in [
        verify,
        missing_domain_bound,
        missing_replica_bound,
        duplicate,
        fewer_domains,
    ] {
        assert!(prepare(request).is_err());
    }
}

#[tokio::test]
async fn even_replicas_have_zero_optimum_and_negative_skew_is_unsat() {
    let prepared = prepare(minimize_skew_request(4)).unwrap();
    let result = run(&SolverService::new(), prepared).await.unwrap();
    let optimization = result.solver.optimization.unwrap();
    assert_eq!(optimization.termination, OptimizationTermination::Complete);
    assert_eq!(
        optimization.solutions[0].objectives[0].value,
        Some(ModelValue::Int(0))
    );
    assert_eq!(
        optimization.solutions[0].objectives[0].bound,
        ObjectiveBound::Finite {
            exact: "0".to_owned()
        }
    );

    let mut strict = prepare(minimize_skew_request(4)).unwrap();
    let objective = strict.request.objectives[0].expr.clone();
    strict
        .request
        .constraints
        .push(ConstraintItem::Declared(ConstraintDecl {
            id: Some("test_strict_better".to_owned()),
            group: None,
            soft: false,
            weight: None,
            expr: le(objective, int(-1)),
        }));
    let strict = SolverService::new()
        .solve_constraints(strict.request)
        .await
        .unwrap();
    assert_eq!(strict.status, SolveStatus::Unsat);
}

#[tokio::test]
async fn inconsistent_replica_conservation_is_infeasible() {
    let mut request = minimize_skew_request(3);
    for unknown in request["unknowns"].as_array_mut().unwrap() {
        unknown["max"] = json!(1);
    }

    let result = run(
        &SolverService::new(),
        prepare(request).expect("bounded request compiles"),
    )
    .await
    .unwrap();
    assert_eq!(result.solver.status, SolveStatus::Unsat);
}

#[test]
fn minimize_skew_keeps_composed_resource_rules_named_and_hard() {
    let mut request = minimize_skew_request(3);
    request["rules"] = json!([
        {"rule_id": "resource.request_within_limit", "subjects": ["api"]},
        {
            "rule_id": "resource.aggregate_capacity",
            "subjects": ["cluster", "api"],
            "parameters": {"resources": ["cpu"]}
        },
        {
            "rule_id": "resource.quota_capacity",
            "subjects": ["team", "api"],
            "parameters": {"resources": ["cpu"]}
        },
        {
            "rule_id": "placement.minimum_failure_domains",
            "subjects": ["api"],
            "parameters": {"minimum_domains": 2}
        },
        {
            "rule_id": "placement.topology_max_skew",
            "subjects": ["api"],
            "parameters": {"max_skew": 1}
        },
        {"rule_id": "placement.minimize_skew", "subjects": ["api"], "parameters": {}}
    ]);
    request["facts"]["workloads"]["api"]["requests"] = json!({"cpu": 1});
    request["facts"]["workloads"]["api"]["limits"] = json!({"cpu": 2});
    request["facts"]["pools"] = json!({"cluster": {"resources": {"cpu": 3}}});
    request["facts"]["quotas"] = json!({"team": {"resources": {"cpu": 3}}});

    let prepared = prepare(request).expect("composed request compiles");
    let request = serde_json::to_value(prepared.request).unwrap();
    let constraints = request["constraints"].as_array().unwrap();
    assert_eq!(constraints.len(), 6);
    assert!(constraints.iter().all(|constraint| {
        constraint["id"].as_str().is_some_and(|id| !id.is_empty()) && constraint["soft"] == false
    }));
    assert_eq!(request["objectives"].as_array().unwrap().len(), 1);
}
