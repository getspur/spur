use std::collections::BTreeMap;

use serde_json::{json, Value};
use spur_jev::{
    gate::GATE_THRESHOLD,
    repair::{
        apply_repair, build_repair_battery, CompileRuleAction, RepairDiagnostic, RepairError,
        REPAIR_DECISION,
    },
    wire::Answer,
};

const CANDIDATES: [&str; 4] = ["eq_wrap", "drop_constraint", "bool_var", "label_only"];

fn choice(value: &str, confidence: f64) -> Answer {
    Answer::Choice {
        choice: value.to_owned(),
        probabilities: BTreeMap::from([(value.to_owned(), 1.0)]),
        confidence,
    }
}

fn diagnostic() -> RepairDiagnostic {
    RepairDiagnostic {
        code: "top_level_not_boolean".to_owned(),
        path: "constraints[1].expr".to_owned(),
        message: "top-level constraint expression must have Boolean sort".to_owned(),
        found: Some("Enum".to_owned()),
    }
}

fn recorded_rejected_request() -> Value {
    let report: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/jev_poc_results/phase2_report.json"
    )))
    .expect("phase-2 report");
    report["suites"]["D"][0]["compiled"].clone()
}

#[test]
fn recorded_enum_diagnostic_repairs_to_eq_wrap() {
    let rejected_request = recorded_rejected_request();
    let rejected_expr = rejected_request["constraints"][1]["expr"].clone();
    let questions = build_repair_battery(&diagnostic(), &rejected_expr, &CANDIDATES);
    assert!(questions.contains_key(REPAIR_DECISION));

    let action = apply_repair(&choice("eq_wrap", 1.0)).expect("open repair decision");
    let repaired = action
        .recompile_bprime(&BTreeMap::new(), &rejected_request)
        .expect("recompile rejected request");
    let constraint = repaired["constraints"]
        .as_array()
        .expect("constraints")
        .iter()
        .find(|constraint| constraint["id"] == "tier_required")
        .expect("tier_required constraint");

    assert_eq!(constraint["expr"]["kind"], "op");
    assert_eq!(constraint["expr"]["op"], "eq");
    assert_eq!(constraint["expr"]["args"][1]["kind"], "enum_label");
}

#[test]
fn repair_battery_embeds_diagnostic_and_literal_candidate_descriptions() {
    let rejected_expr = json!({"kind": "enum_label", "var": "tier", "label": "large"});
    let questions = build_repair_battery(&diagnostic(), &rejected_expr, &CANDIDATES);
    let question = serde_json::to_value(&questions[REPAIR_DECISION]).expect("serialize question");

    assert_eq!(question["type"], "choice");
    assert_eq!(
        question["instructions"]["diagnostic"]["code"],
        "top_level_not_boolean"
    );
    assert_eq!(
        question["instructions"]["diagnostic"]["path"],
        "constraints[1].expr"
    );
    assert_eq!(
        question["instructions"]["diagnostic"]["message"],
        "top-level constraint expression must have Boolean sort"
    );
    assert_eq!(question["instructions"]["diagnostic"]["found"], "Enum");
    assert_eq!(question["instructions"]["rejected_expr"], rejected_expr);
    assert_eq!(
        question["criteria"]["eq_wrap"],
        "Wrap the enum-label assertion in eq(var, enum_label) so the top-level expression is Boolean."
    );
    assert_eq!(
        question["criteria"]["drop_constraint"],
        "Remove the rejected constraint from the request."
    );
    assert_eq!(
        question["criteria"]["bool_var"],
        "Replace the rejected expression with a Boolean variable reference."
    );
    assert_eq!(
        question["criteria"]["label_only"],
        "Keep the enum-label expression without adding a Boolean wrapper."
    );
}

#[test]
fn blocked_repair_returns_ambiguity_report_without_fallback() {
    let error = apply_repair(&choice("eq_wrap", GATE_THRESHOLD - 0.01))
        .expect_err("low-confidence repair must be blocked");
    let RepairError::Ambiguous(report) = error else {
        panic!("blocked repair must return its ambiguity report");
    };

    assert_eq!(report.weakest_link.question, REPAIR_DECISION);
    assert_eq!(report.weakest_link.value, GATE_THRESHOLD - 0.01);
    assert_eq!(report.threshold, GATE_THRESHOLD);
}

#[test]
fn every_candidate_maps_to_an_explicit_compile_rule_action() {
    for (candidate, expected) in [
        ("eq_wrap", CompileRuleAction::WrapEnumEquality),
        ("drop_constraint", CompileRuleAction::DropConstraint),
        ("bool_var", CompileRuleAction::ReplaceWithBooleanVariable),
        ("label_only", CompileRuleAction::KeepEnumLabelOnly),
    ] {
        assert_eq!(apply_repair(&choice(candidate, 1.0)), Ok(expected));
    }
}
