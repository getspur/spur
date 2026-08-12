//! Opt-in integration smoke tests against an operator-installed Z3 binary.
//!
//! Default test runs never launch Z3: every test in this target is ignored and
//! also requires `SPUR_TEST_Z3=1`. With `z3` on `PATH` (or `SPUR_Z3_BIN` set),
//! run the smoke target explicitly:
//!
//! ```text
//! SPUR_TEST_Z3=1 scripts/spur-cargo test -p spur-solver --test real_z3 -- --ignored
//! ```

use spur_solver::{
    service::SolverService,
    types::{
        ConstraintExpr, ConstraintOp, ModelValue, SolveConstraintsRequest, SolveStatus, Variable,
        DEFAULT_TIMEOUT_MS,
    },
};

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

fn real_z3_enabled() -> bool {
    std::env::var("SPUR_TEST_Z3").as_deref() == Ok("1")
}
