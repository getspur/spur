use std::time::Duration;

use serde_json::{json, Map, Value};
use spur_solver::{
    rules::{
        execute::{prepare, run},
        manifest::manifest_conformance_vectors,
        primitives::{int, le},
        RuleOutcome,
    },
    service::SolverService,
    types::{
        ConstraintDecl, ConstraintItem, ModelValue, ObjectiveBound, OptimizationTermination,
        SolveStatus,
    },
};

const OBJECTIVES: [(&str, i64); 2] = [
    ("rbac.minimum_privilege", 3),
    ("placement.minimize_skew", 1),
];

fn conformance_request(rule_id: &str, valid: bool) -> Value {
    let vectors = manifest_conformance_vectors(rule_id)
        .unwrap_or_else(|| panic!("missing conformance vectors for `{rule_id}`"));
    let vectors = if valid {
        &vectors.valid
    } else {
        &vectors.invalid
    };
    vectors
        .first()
        .unwrap_or_else(|| panic!("missing conformance vector for `{rule_id}`"))
        .request
        .clone()
}

#[tokio::test]
async fn objective_release_matrix_is_complete_and_ratcheted() {
    for (rule_id, expected) in OBJECTIVES {
        let request = conformance_request(rule_id, true);
        let prepared = prepare(request.clone()).unwrap_or_else(|error| {
            panic!("`{rule_id}` valid conformance request must prepare: {error}")
        });
        assert_eq!(prepared.request.objectives.len(), 1, "{rule_id}");

        let result = run(&SolverService::new(), prepared)
            .await
            .unwrap_or_else(|error| panic!("`{rule_id}` must solve: {error}"));
        assert_eq!(result.solver.status, SolveStatus::Sat, "{rule_id}");
        assert_eq!(result.outcome, RuleOutcome::Solution, "{rule_id}");
        assert!(result.rule_results.is_empty(), "{rule_id}");

        let optimization = result
            .solver
            .optimization
            .unwrap_or_else(|| panic!("`{rule_id}` must return optimization diagnostics"));
        assert_eq!(
            optimization.termination,
            OptimizationTermination::Complete,
            "{rule_id}"
        );
        assert_eq!(optimization.solutions.len(), 1, "{rule_id}");
        let solution = &optimization.solutions[0];
        assert_eq!(solution.objectives.len(), 1, "{rule_id}");
        assert_eq!(
            solution.objectives[0].value,
            Some(ModelValue::Int(expected)),
            "{rule_id}"
        );
        assert_eq!(
            solution.objectives[0].bound,
            ObjectiveBound::Finite {
                exact: expected.to_string(),
            },
            "{rule_id}"
        );
        assert!(solution.soft_constraints.is_empty(), "{rule_id}");
        assert!(solution.groups.is_empty(), "{rule_id}");

        let mut strict = prepare(request).unwrap_or_else(|error| {
            panic!("`{rule_id}` strict-better request must prepare: {error}")
        });
        let objective = strict.request.objectives[0].expr.clone();
        strict
            .request
            .constraints
            .push(ConstraintItem::Declared(ConstraintDecl {
                id: Some(format!("{}_strict_better", rule_id.replace('.', "_"))),
                group: None,
                soft: false,
                weight: None,
                expr: le(objective, int(expected - 1)),
            }));
        let strict = SolverService::new()
            .solve_constraints(strict.request)
            .await
            .unwrap_or_else(|error| panic!("`{rule_id}` strict-better proof must run: {error}"));
        assert_eq!(strict.status, SolveStatus::Unsat, "{rule_id}");
    }
}

#[test]
fn objective_preflight_rejects_verify_and_duplicates() {
    for (rule_id, _) in OBJECTIVES {
        let base = conformance_request(rule_id, true);

        let mut verify = base.clone();
        verify["mode"] = json!("verify");
        assert!(prepare(verify).is_err(), "{rule_id} verify mode");

        let mut duplicate = base;
        let binding = duplicate["rules"]
            .as_array()
            .expect("rules array")
            .iter()
            .find(|binding| binding["rule_id"] == rule_id)
            .unwrap_or_else(|| panic!("missing `{rule_id}` binding"))
            .clone();
        duplicate["rules"]
            .as_array_mut()
            .expect("rules array")
            .push(binding);
        assert!(prepare(duplicate).is_err(), "{rule_id} duplicate objective");
    }
}

#[tokio::test]
async fn hard_infeasibility_has_no_rule_attribution() {
    for (rule_id, _) in OBJECTIVES {
        let prepared = prepare(conformance_request(rule_id, false)).unwrap_or_else(|error| {
            panic!("`{rule_id}` invalid conformance request must prepare: {error}")
        });
        let result = run(&SolverService::new(), prepared)
            .await
            .unwrap_or_else(|error| panic!("`{rule_id}` infeasibility proof must run: {error}"));

        assert_eq!(result.solver.status, SolveStatus::Unsat, "{rule_id}");
        assert_eq!(result.outcome, RuleOutcome::Infeasible, "{rule_id}");
        assert!(result.rule_results.is_empty(), "{rule_id}");
    }
}

fn near_limit_resource_request() -> Value {
    let mut domain_counts = Map::new();
    let mut unknowns = Vec::new();
    for index in 0..32 {
        let domain = format!("zone-{index:02}");
        domain_counts.insert(domain.clone(), Value::Null);
        unknowns.push(json!({
            "subject": "api",
            "field": format!("domain_counts.{domain}"),
            "min": 0,
            "max": 64,
        }));
    }

    json!({
        "family": "resource",
        "mode": "synthesize",
        "rules": [
            {"rule_id": "placement.minimize_skew", "subjects": ["api"], "parameters": {}}
        ],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": 64,
                    "requests": {},
                    "limits": {},
                    "domain_counts": domain_counts,
                }
            },
            "pools": {},
            "quotas": {},
        },
        "unknowns": unknowns,
        "timeout_ms": 30_000,
    })
}

#[tokio::test]
async fn thirty_two_domain_placement_completes_with_one_finite_bound() {
    let prepared = prepare(near_limit_resource_request())
        .expect("32-domain bounded placement request must prepare");
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        run(&SolverService::new(), prepared),
    )
    .await
    .expect("32-domain bounded placement request must complete within 30 seconds")
    .expect("32-domain bounded placement request must solve");

    assert_eq!(result.solver.status, SolveStatus::Sat);
    assert_eq!(result.outcome, RuleOutcome::Solution);
    assert!(result.rule_results.is_empty());
    assert!(result.total_duration_ms <= 30_000);

    let optimization = result
        .solver
        .optimization
        .expect("bounded placement must return optimization diagnostics");
    assert_eq!(optimization.termination, OptimizationTermination::Complete);
    assert_eq!(optimization.solutions.len(), 1);
    assert_eq!(optimization.solutions[0].objectives.len(), 1);
    assert!(matches!(
        optimization.solutions[0].objectives[0].bound,
        ObjectiveBound::Finite { .. }
    ));
    assert!(optimization.solutions[0].soft_constraints.is_empty());
    assert!(optimization.solutions[0].groups.is_empty());
}
