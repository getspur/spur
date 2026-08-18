use serde_json::{json, Value};
use spur_solver::{encode::encode_solve_constraints, types::SolveConstraintsRequest};

fn encode_request(value: Value) -> String {
    let request: SolveConstraintsRequest =
        serde_json::from_value(value).expect("optimization request JSON must deserialize");
    encode_solve_constraints(&request).expect("optimization request must encode")
}

#[test]
fn diagnostic_soft_ids_do_not_create_z3_objective_groups() {
    let smt = encode_request(json!({
        "vars": [
            {"type":"bool","name":"a"},
            {"type":"bool","name":"b"}
        ],
        "constraints": [
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
        ]
    }));

    assert!(!smt.contains(":id prefer_a"), "unexpected SMT:\n{smt}");
    assert!(!smt.contains(":id prefer_b"), "unexpected SMT:\n{smt}");
}

#[test]
fn repeated_explicit_soft_group_uses_one_z3_group() {
    let smt = encode_request(json!({
        "vars": [
            {"type":"bool","name":"a"},
            {"type":"bool","name":"b"}
        ],
        "constraints": [
            {
                "id":"prefer_a",
                "group":"preferences",
                "soft":true,
                "weight":1,
                "expr":{"kind":"var","name":"a"}
            },
            {
                "id":"prefer_b",
                "group":"preferences",
                "soft":true,
                "weight":100,
                "expr":{"kind":"var","name":"b"}
            }
        ]
    }));

    assert_eq!(smt.matches(":id preferences").count(), 2, "SMT:\n{smt}");
    assert!(!smt.contains(":id prefer_a"), "unexpected SMT:\n{smt}");
    assert!(!smt.contains(":id prefer_b"), "unexpected SMT:\n{smt}");
}

#[test]
fn soft_only_request_emits_selected_priority() {
    let smt = encode_request(json!({
        "vars": [{"type":"bool","name":"preferred"}],
        "constraints": [{
            "id":"prefer_value",
            "soft":true,
            "weight":1,
            "expr":{"kind":"var","name":"preferred"}
        }],
        "objective_priority":"pareto"
    }));

    assert!(
        smt.contains("(set-option :opt.priority pareto)"),
        "SMT:\n{smt}"
    );
}

#[test]
fn optimization_cycle_retrieves_objectives_before_values() {
    let smt = encode_request(json!({
        "vars": [{"type":"int_range","name":"x","min":0,"max":3}],
        "constraints": [],
        "objectives": [{
            "op":"maximize",
            "expr":{"kind":"var","name":"x"}
        }]
    }));

    let check_sat = smt
        .find("(check-sat)")
        .expect("optimization cycle must check satisfiability");
    let get_objectives = smt
        .find("(get-objectives)")
        .expect("optimization cycle must retrieve exact objective bounds");
    let get_value = smt
        .find("(get-value")
        .expect("optimization cycle must retrieve model values");

    assert!(
        check_sat < get_objectives && get_objectives < get_value,
        "optimization responses must be requested positionally; SMT:\n{smt}"
    );
}
