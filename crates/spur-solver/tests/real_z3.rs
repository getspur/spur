//! Opt-in integration smoke tests against an operator-installed Z3 binary.
//!
//! Default test runs never launch Z3: every test in this target is ignored and
//! also requires `SPUR_TEST_Z3=1`. With `z3` on `PATH` (or `SPUR_Z3_BIN` set),
//! run the smoke target explicitly:
//!
//! ```text
//! SPUR_REMOTE=0 SPUR_TEST_Z3=1 scripts/spur-cargo test -p spur-solver --test real_z3 -- --ignored
//! ```

use std::collections::{BTreeMap, BTreeSet};

use spur_solver::{
    service::SolverService,
    types::{
        ConstraintDecl, ConstraintExpr, ConstraintItem, ConstraintOp, ModelValue, Objective,
        ObjectiveOp, SolveConstraintsRequest, SolveConstraintsResponse, SolveSmtRequest,
        SolveStatus, Variable, DEFAULT_MAX_SOLUTIONS, DEFAULT_TIMEOUT_MS,
    },
};

fn request_from_json(value: serde_json::Value) -> SolveConstraintsRequest {
    serde_json::from_value(value).expect("real-Z3 optimization request JSON must deserialize")
}

fn model_bool(model: &BTreeMap<String, ModelValue>, name: &str) -> bool {
    match model.get(name) {
        Some(ModelValue::Bool(value)) => *value,
        other => panic!("expected Boolean {name} in real-Z3 model, got {other:?}"),
    }
}

fn model_int(model: &BTreeMap<String, ModelValue>, name: &str) -> i64 {
    match model.get(name) {
        Some(ModelValue::Int(value)) => *value,
        other => panic!("expected integer {name} in real-Z3 model, got {other:?}"),
    }
}

fn optimization_json(response: &SolveConstraintsResponse) -> serde_json::Value {
    serde_json::to_value(response)
        .expect("solver response must serialize")
        .get("optimization")
        .cloned()
        .expect("optimization request must return an optimization envelope")
}

fn optimization_model_set(response: &SolveConstraintsResponse) -> BTreeSet<(i64, i64)> {
    optimization_json(response)
        .get("solutions")
        .and_then(serde_json::Value::as_array)
        .expect("optimization envelope must contain solutions")
        .iter()
        .map(|solution| {
            let model: BTreeMap<String, ModelValue> = serde_json::from_value(
                solution
                    .get("model")
                    .cloned()
                    .expect("optimization solution must contain a model"),
            )
            .expect("optimization solution model must deserialize");
            (model_int(&model, "x"), model_int(&model, "y"))
        })
        .collect()
}

fn assert_complete_optimization(response: &SolveConstraintsResponse) {
    assert_eq!(
        optimization_json(response)
            .get("termination")
            .and_then(serde_json::Value::as_str),
        Some("complete")
    );
}

fn assert_first_objective_bound_kind(response: &SolveConstraintsResponse, expected: &str) {
    let optimization = optimization_json(response);
    let bound_kind = optimization
        .get("solutions")
        .and_then(serde_json::Value::as_array)
        .and_then(|solutions| solutions.first())
        .and_then(|solution| solution.get("objectives"))
        .and_then(serde_json::Value::as_array)
        .and_then(|objectives| objectives.first())
        .and_then(|objective| objective.get("bound"))
        .and_then(|bound| bound.get("kind"))
        .and_then(serde_json::Value::as_str);

    assert_eq!(bound_kind, Some(expected), "response: {response:?}");
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn trivial_sat_model_satisfies_declared_constraints() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(SolveConstraintsRequest {
            vars: vec![Variable::IntRange {
                name: "value".to_owned(),
                min: 1,
                max: 10,
            }],
            constraints: vec![ConstraintExpr::Op {
                op: ConstraintOp::Ge,
                args: vec![
                    ConstraintExpr::Var {
                        name: "value".to_owned(),
                    },
                    ConstraintExpr::Int { value: 4 },
                ],
            }]
            .into_iter()
            .map(Into::into)
            .collect(),
            objectives: vec![],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        })
        .await
        .expect("enabled real Z3 solve should start");

    assert_eq!(
        response.status,
        SolveStatus::Sat,
        "unexpected real Z3 response: {response:?}"
    );
    let model = response.model.expect("sat must include a model");
    let value = match model.get("value") {
        Some(ModelValue::Int(value)) => value,
        other => panic!("expected integer value in real Z3 model, got {other:?}"),
    };
    assert!(
        (4..=10).contains(value),
        "model value {value} must satisfy value >= 4 and value <= 10"
    );
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn unsat_protocol_has_no_model() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(SolveConstraintsRequest {
            vars: vec![Variable::Int {
                name: "value".to_owned(),
            }],
            constraints: vec![ConstraintExpr::Bool { value: false }]
                .into_iter()
                .map(Into::into)
                .collect(),
            objectives: vec![],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        })
        .await
        .expect("enabled real Z3 solve should start");

    assert_eq!(
        response.status,
        SolveStatus::Unsat,
        "unexpected real Z3 response: {response:?}"
    );
    assert!(response.model.is_none());
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn named_hard_conflict_returns_unsat_core() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(SolveConstraintsRequest {
            vars: vec![Variable::IntRange {
                name: "x".to_owned(),
                min: 0,
                max: 10,
            }],
            constraints: vec![
                ConstraintItem::Declared(ConstraintDecl {
                    id: Some("lower".to_owned()),
                    group: None,
                    soft: false,
                    weight: None,
                    expr: ConstraintExpr::Op {
                        op: ConstraintOp::Ge,
                        args: vec![
                            ConstraintExpr::Var {
                                name: "x".to_owned(),
                            },
                            ConstraintExpr::Int { value: 5 },
                        ],
                    },
                }),
                ConstraintItem::Declared(ConstraintDecl {
                    id: Some("upper".to_owned()),
                    group: None,
                    soft: false,
                    weight: None,
                    expr: ConstraintExpr::Op {
                        op: ConstraintOp::Le,
                        args: vec![
                            ConstraintExpr::Var {
                                name: "x".to_owned(),
                            },
                            ConstraintExpr::Int { value: 3 },
                        ],
                    },
                }),
            ],
            objectives: vec![],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        })
        .await
        .expect("enabled real Z3 solve should start");

    assert_eq!(response.status, SolveStatus::Unsat, "{response:?}");
    assert!(response.model.is_none());
    let core = response
        .unsat_core
        .expect("named hard conflict should return unsat_core");
    assert!(core.contains(&"lower".to_owned()), "core={core:?}");
    assert!(core.contains(&"upper".to_owned()), "core={core:?}");
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn soft_preference_and_maximize_prefer_high_values() {
    if !real_z3_enabled() {
        return;
    }

    let soft = SolverService::new()
        .solve_constraints(SolveConstraintsRequest {
            vars: vec![Variable::IntRange {
                name: "sidebar".to_owned(),
                min: 200,
                max: 480,
            }],
            constraints: vec![
                ConstraintItem::Declared(ConstraintDecl {
                    id: Some("max_sidebar".to_owned()),
                    group: None,
                    soft: false,
                    weight: None,
                    expr: ConstraintExpr::Op {
                        op: ConstraintOp::Le,
                        args: vec![
                            ConstraintExpr::Var {
                                name: "sidebar".to_owned(),
                            },
                            ConstraintExpr::Int { value: 400 },
                        ],
                    },
                }),
                ConstraintItem::Declared(ConstraintDecl {
                    id: Some("prefer_wide".to_owned()),
                    group: None,
                    soft: true,
                    weight: Some(5),
                    expr: ConstraintExpr::Op {
                        op: ConstraintOp::Ge,
                        args: vec![
                            ConstraintExpr::Var {
                                name: "sidebar".to_owned(),
                            },
                            ConstraintExpr::Int { value: 320 },
                        ],
                    },
                }),
            ],
            objectives: vec![],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        })
        .await
        .expect("soft solve should start");

    assert_eq!(soft.status, SolveStatus::Sat, "{soft:?}");
    let soft_sidebar = match soft.model.as_ref().and_then(|m| m.get("sidebar")) {
        Some(ModelValue::Int(v)) => *v,
        other => panic!("expected sidebar int, got {other:?}"),
    };
    assert!(
        soft_sidebar >= 320,
        "soft preference should push sidebar >= 320, got {soft_sidebar}"
    );
    assert!(soft.unsat_core.is_none());

    let optimized = SolverService::new()
        .solve_constraints(SolveConstraintsRequest {
            vars: vec![Variable::IntRange {
                name: "batch".to_owned(),
                min: 8,
                max: 128,
            }],
            constraints: vec![ConstraintExpr::Op {
                op: ConstraintOp::Le,
                args: vec![
                    ConstraintExpr::Op {
                        op: ConstraintOp::Mul,
                        args: vec![
                            ConstraintExpr::Int { value: 4 },
                            ConstraintExpr::Op {
                                op: ConstraintOp::Add,
                                args: vec![
                                    ConstraintExpr::Int { value: 48 },
                                    ConstraintExpr::Op {
                                        op: ConstraintOp::Mul,
                                        args: vec![
                                            ConstraintExpr::Int { value: 2 },
                                            ConstraintExpr::Var {
                                                name: "batch".to_owned(),
                                            },
                                        ],
                                    },
                                ],
                            },
                        ],
                    },
                    ConstraintExpr::Int { value: 512 },
                ],
            }
            .into()],
            objectives: vec![Objective {
                op: ObjectiveOp::Maximize,
                expr: ConstraintExpr::Var {
                    name: "batch".to_owned(),
                },
            }],
            objective_priority: Default::default(),
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            use_cache: true,
            session_id: None,
            session_op: Default::default(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: true,
        })
        .await
        .expect("maximize solve should start");

    assert_eq!(optimized.status, SolveStatus::Sat, "{optimized:?}");
    let batch = match optimized.model.as_ref().and_then(|m| m.get("batch")) {
        Some(ModelValue::Int(v)) => *v,
        other => panic!("expected batch int, got {other:?}"),
    };
    // workers fixed at 4: 4 * (48 + 2*batch) <= 512 → batch <= 40
    assert_eq!(batch, 40, "maximize batch under budget should yield 40");
    let smt = optimized.smt.expect("include_smt should echo script");
    assert!(smt.contains("(maximize v_batch)"));
    assert!(!smt.contains("produce-unsat-cores"));
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn anonymous_weighted_soft_constraints_use_one_maxsmt_objective() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(request_from_json(serde_json::json!({
            "vars": [
                {"type":"bool","name":"a"},
                {"type":"bool","name":"b"}
            ],
            "constraints": [
                {
                    "kind":"op",
                    "op":"not",
                    "args":[{
                        "kind":"op",
                        "op":"and",
                        "args":[
                            {"kind":"var","name":"a"},
                            {"kind":"var","name":"b"}
                        ]
                    }]
                },
                {
                    "id":"prefer_a",
                    "soft":true,
                    "weight":1,
                    "expr":{"kind":"var","name":"a"}
                },
                {
                    "id":"prefer_b",
                    "soft":true,
                    "weight":100,
                    "expr":{"kind":"var","name":"b"}
                }
            ],
            "use_cache":false
        })))
        .await
        .expect("weighted MaxSMT solve should start");

    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    let model = response.model.as_ref().expect("sat must include a model");
    assert!(!model_bool(model, "a"), "response: {response:?}");
    assert!(model_bool(model, "b"), "response: {response:?}");
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn unconstrained_real_maximum_reports_infinite_bound() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(request_from_json(serde_json::json!({
            "vars": [{"type":"real","name":"x"}],
            "constraints": [],
            "objectives": [{
                "op":"maximize",
                "expr":{"kind":"var","name":"x"}
            }],
            "use_cache":false
        })))
        .await
        .expect("unbounded optimization should start");

    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    assert_first_objective_bound_kind(&response, "infinite");
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn open_real_maximum_reports_strict_bound() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(request_from_json(serde_json::json!({
            "vars": [{"type":"real","name":"x"}],
            "constraints": [{
                "kind":"op",
                "op":"lt",
                "args":[
                    {"kind":"var","name":"x"},
                    {"kind":"real","num":1,"den":1}
                ]
            }],
            "objectives": [{
                "op":"maximize",
                "expr":{"kind":"var","name":"x"}
            }],
            "use_cache":false
        })))
        .await
        .expect("strict optimization should start");

    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    assert_first_objective_bound_kind(&response, "strict");
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn compound_real_objective_accepts_z3_normalization() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(request_from_json(serde_json::json!({
            "vars": [{"type":"real","name":"x"}],
            "constraints": [{
                "kind":"op",
                "op":"eq",
                "args":[
                    {"kind":"var","name":"x"},
                    {"kind":"real","num":1,"den":2}
                ]
            }],
            "objectives": [{
                "op":"maximize",
                "expr":{
                    "kind":"op",
                    "op":"add",
                    "args":[
                        {"kind":"var","name":"x"},
                        {"kind":"real","num":1,"den":2}
                    ]
                }
            }],
            "use_cache":false
        })))
        .await
        .expect("compound Real optimization should start");

    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    assert_first_objective_bound_kind(&response, "finite");
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn single_objective_box_completes_after_z3_restarts() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(request_from_json(serde_json::json!({
            "vars": [{"type":"int_range","name":"x","min":0,"max":3}],
            "constraints": [],
            "objectives": [{
                "op":"maximize",
                "expr":{"kind":"var","name":"x"}
            }],
            "objective_priority":"box",
            "use_cache":false
        })))
        .await
        .expect("single-objective box optimization should start");

    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    assert_complete_optimization(&response);
    assert_eq!(
        response
            .optimization
            .as_ref()
            .expect("optimization payload")
            .solutions
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn raw_unsat_core_named_objectives_is_preserved() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_smt(SolveSmtRequest {
            smt_lib: "(set-option :produce-unsat-cores true)\n\
                      (assert (! false :named objectives))\n\
                      (check-sat)\n\
                      (get-unsat-core)\n"
                .to_owned(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            persist: false,
            include_smt: false,
        })
        .await
        .expect("raw unsat-core solve should start");

    assert_eq!(response.status, SolveStatus::Unsat, "{response:?}");
    assert_eq!(response.unsat_core, Some(vec!["objectives".to_owned()]));
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn pareto_enumeration_returns_the_complete_bounded_frontier() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(frontier_request("pareto"))
        .await
        .expect("Pareto optimization should start");

    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    assert_eq!(
        optimization_model_set(&response),
        BTreeSet::from([(0, 3), (1, 2), (2, 1), (3, 0)]),
        "response: {response:?}"
    );
    assert_complete_optimization(&response);
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn box_enumeration_returns_one_model_per_independent_objective() {
    if !real_z3_enabled() {
        return;
    }

    let response = SolverService::new()
        .solve_constraints(frontier_request("box"))
        .await
        .expect("box optimization should start");

    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    assert_eq!(
        optimization_model_set(&response),
        BTreeSet::from([(0, 3), (3, 0)]),
        "response: {response:?}"
    );
    assert_complete_optimization(&response);
}

fn frontier_request(priority: &str) -> SolveConstraintsRequest {
    request_from_json(serde_json::json!({
        "vars": [
            {"type":"int_range","name":"x","min":0,"max":3},
            {"type":"int_range","name":"y","min":0,"max":3}
        ],
        "constraints": [{
            "kind":"op",
            "op":"le",
            "args":[
                {
                    "kind":"op",
                    "op":"add",
                    "args":[
                        {"kind":"var","name":"x"},
                        {"kind":"var","name":"y"}
                    ]
                },
                {"kind":"int","value":3}
            ]
        }],
        "objectives": [
            {"op":"maximize","expr":{"kind":"var","name":"x"}},
            {"op":"maximize","expr":{"kind":"var","name":"y"}}
        ],
        "objective_priority":priority,
        "use_cache":false
    }))
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn skill_example_1_budget_json_is_sat() {
    if !real_z3_enabled() {
        return;
    }

    let request: SolveConstraintsRequest = serde_json::from_value(serde_json::json!({
        "vars": [
            {"name":"workers","type":"int_range","min":1,"max":16},
            {"name":"batch","type":"int_range","min":8,"max":128}
        ],
        "constraints": [
            {"kind":"op","op":"ge","args":[{"kind":"var","name":"workers"},{"kind":"int","value":4}]},
            {"kind":"op","op":"le","args":[
              {"kind":"op","op":"mul","args":[
                {"kind":"var","name":"workers"},
                {"kind":"op","op":"add","args":[
                  {"kind":"int","value":48},
                  {"kind":"op","op":"mul","args":[{"kind":"int","value":2},{"kind":"var","name":"batch"}]}
                ]}
              ]},
              {"kind":"int","value":512}
            ]}
        ],
        "timeout_ms": 30000
    }))
    .expect("skill example 1 JSON must deserialize");

    let response = SolverService::new()
        .solve_constraints(request)
        .await
        .expect("skill example 1 should start");
    assert_eq!(response.status, SolveStatus::Sat, "{response:?}");
    let model = response.model.expect("sat must include a model");
    let workers = match model.get("workers") {
        Some(ModelValue::Int(value)) => *value,
        other => panic!("expected workers int, got {other:?}"),
    };
    let batch = match model.get("batch") {
        Some(ModelValue::Int(value)) => *value,
        other => panic!("expected batch int, got {other:?}"),
    };
    assert!(workers >= 4 && workers <= 16, "workers={workers}");
    assert!((8..=128).contains(&batch), "batch={batch}");
    assert!(
        workers * (48 + 2 * batch) <= 512,
        "model violates budget: workers={workers} batch={batch}"
    );
}

#[tokio::test]
#[ignore = "requires SPUR_TEST_Z3=1 and an installed Z3 binary"]
async fn skill_example_2_overflow_json_is_unsat() {
    if !real_z3_enabled() {
        return;
    }

    let request: SolveConstraintsRequest = serde_json::from_value(serde_json::json!({
        "vars": [
            {"name":"pos","type":"int_range","min":0,"max":512},
            {"name":"len","type":"int_range","min":0,"max":512}
        ],
        "constraints": [
            {"kind":"op","op":"gt","args":[
              {"kind":"op","op":"add","args":[{"kind":"var","name":"pos"},{"kind":"var","name":"len"}]},
              {"kind":"int","value":1024}
            ]}
        ]
    }))
    .expect("skill example 2 JSON must deserialize");

    let response = SolverService::new()
        .solve_constraints(request)
        .await
        .expect("skill example 2 should start");
    assert_eq!(response.status, SolveStatus::Unsat, "{response:?}");
    assert!(response.model.is_none(), "{response:?}");
}

fn real_z3_enabled() -> bool {
    std::env::var("SPUR_TEST_Z3").as_deref() == Ok("1")
}
