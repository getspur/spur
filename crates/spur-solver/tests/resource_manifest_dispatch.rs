use serde_json::{json, Value};
use spur_solver::{
    rules::{execute::prepare, manifest::manifest_rule_handler, manifest_format::NativeHandlerV1},
    types::{ConstraintExpr, ConstraintOp, ObjectiveOp, ObjectivePriority},
};

fn request(rule: Value) -> Value {
    json!({
        "family": "resource",
        "mode": "verify",
        "rules": [rule],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": 3,
                    "requests": {"cpu": 500},
                    "limits": {"cpu": 1000},
                    "domain_counts": {"zone-a": 2, "zone-b": 1}
                }
            },
            "pools": {"cluster": {"resources": {"cpu": 1500}}},
            "quotas": {"team": {"resources": {"cpu": 1500}}}
        },
        "unknowns": []
    })
}

#[test]
fn minimize_skew_dispatches_one_hard_predicate_and_one_lex_objective() {
    assert_eq!(
        manifest_rule_handler("placement.minimize_skew"),
        Some(NativeHandlerV1::PlacementMinimizeSkew)
    );

    let prepared = prepare(json!({
        "family": "resource",
        "mode": "synthesize",
        "rules": [{
            "rule_id": "placement.minimize_skew",
            "subjects": ["api"],
            "parameters": {}
        }],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": 3,
                    "requests": {},
                    "limits": {},
                    "domain_counts": {"zone-a": null, "zone-b": null}
                }
            },
            "pools": {},
            "quotas": {}
        },
        "unknowns": [
            {"subject": "api", "field": "domain_counts.zone-a", "min": 0, "max": 3},
            {"subject": "api", "field": "domain_counts.zone-b", "min": 0, "max": 3}
        ]
    }))
    .expect("minimize-skew manifest handler must compile");

    let [constraint] = prepared.request.constraints.as_slice() else {
        panic!("minimize skew must generate one hard predicate");
    };
    assert_eq!(
        constraint.id(),
        Some("resource_rule_0_placement_minimize_skew")
    );
    assert!(!constraint.is_soft());

    let [objective] = prepared.request.objectives.as_slice() else {
        panic!("minimize skew must generate one objective");
    };
    assert_eq!(objective.op, ObjectiveOp::Minimize);
    assert_eq!(prepared.request.objective_priority, ObjectivePriority::Lex);
    assert!(prepared
        .request
        .constraints
        .iter()
        .all(|item| !item.is_soft()));
    assert!(prepared
        .projections
        .iter()
        .any(|projection| { projection.subject == "api" && projection.field == "topology_skew" }));
}

#[test]
fn all_resource_manifest_handlers_dispatch_to_existing_native_predicates() {
    let cases = [
        (
            json!({
                "rule_id": "resource.request_within_limit",
                "subjects": ["api"]
            }),
            NativeHandlerV1::ResourceRequestWithinLimit,
            ConstraintOp::Le,
        ),
        (
            json!({
                "rule_id": "resource.aggregate_capacity",
                "subjects": ["cluster", "api"],
                "parameters": {"resources": ["cpu"]}
            }),
            NativeHandlerV1::ResourceAggregateCapacity,
            ConstraintOp::Le,
        ),
        (
            json!({
                "rule_id": "resource.quota_capacity",
                "subjects": ["team", "api"],
                "parameters": {"resources": ["cpu"]}
            }),
            NativeHandlerV1::ResourceQuotaCapacity,
            ConstraintOp::Le,
        ),
        (
            json!({
                "rule_id": "placement.topology_max_skew",
                "subjects": ["api"]
            }),
            NativeHandlerV1::PlacementTopologyMaxSkew,
            ConstraintOp::And,
        ),
        (
            json!({
                "rule_id": "placement.minimum_failure_domains",
                "subjects": ["api"],
                "parameters": {"minimum_domains": 2}
            }),
            NativeHandlerV1::PlacementMinimumFailureDomains,
            ConstraintOp::And,
        ),
    ];

    for (rule, expected_handler, expected_op) in cases {
        let rule_id = rule["rule_id"].as_str().expect("rule ID").to_owned();
        assert_eq!(
            manifest_rule_handler(&rule_id),
            Some(expected_handler),
            "{rule_id} must select its closed native handler"
        );

        let prepared = prepare(request(rule)).expect("manifest handler must compile");
        let [constraint] = prepared.request.constraints.as_slice() else {
            panic!("one resource binding must generate one constraint");
        };
        let ConstraintExpr::Op { op, .. } = constraint.expr() else {
            panic!("native resource handlers must produce operator predicates");
        };
        assert_eq!(*op, expected_op, "{rule_id}");
    }
}

#[test]
fn manifest_contract_failure_precedes_resource_semantics_and_constraint_generation() {
    let mut input = request(json!({
        "rule_id": "placement.topology_max_skew",
        "subjects": ["api"],
        "parameters": {"resources": ["cpu"]}
    }));
    input["facts"]["workloads"]["api"]["replicas"] = json!(-1);

    let error = prepare(input)
        .expect_err("manifest contract must reject the binding")
        .to_string();
    assert!(
        error.contains("rule `placement.topology_max_skew` does not accept resources"),
        "unexpected contract diagnostic: {error}"
    );
    assert!(
        !error.contains("replicas must be non-negative"),
        "resource semantics ran before manifest validation: {error}"
    );
}
