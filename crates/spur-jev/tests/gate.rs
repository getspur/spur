use std::collections::BTreeMap;

use spur_jev::{
    gate::{DecisionReview, Gate, GATE_THRESHOLD},
    provenance::JevProvenance,
    wire::{Answer, JevResponse, Usage},
};

fn choice(choice: &str, confidence: f64) -> Answer {
    Answer::Choice {
        choice: choice.to_owned(),
        probabilities: BTreeMap::from([(choice.to_owned(), 1.0)]),
        confidence,
    }
}

#[test]
fn weakest_link_blocks_recorded_vague_case() {
    let review = DecisionReview::from_confidences(&[
        ("route_rule", 0.98),
        ("solve_mode", 0.35),
        ("data_complete", 0.32),
    ]);

    assert_eq!(review.weakest, 0.32);
    assert_eq!(review.weakest_link.question, "data_complete");
    assert_eq!(review.weakest_link.value, 0.32);
    assert_eq!(GATE_THRESHOLD, 0.60);

    let Gate::Blocked(report) = &review.gate else {
        panic!("recorded case 5 must be blocked");
    };
    assert_eq!(report.per_decision, review.per_decision);
    assert_eq!(report.weakest_link, review.weakest_link);
    assert_eq!(report.threshold, GATE_THRESHOLD);
}

#[test]
fn every_choice_and_only_named_gating_nouls_feed_the_gate() {
    let answers = BTreeMap::from([
        (
            "route_rule".to_owned(),
            choice("scheduling.precedence_finish_start", 0.98),
        ),
        ("solve_mode".to_owned(), choice("verify", 0.97)),
        ("predecessor".to_owned(), choice("a", 0.05)),
        ("data_complete".to_owned(), Answer::Noul { noul: 0.74 }),
        ("meta_self_report".to_owned(), Answer::Noul { noul: 0.01 }),
    ]);

    let review = DecisionReview::from_answers(&answers, &["data_complete"]);

    assert_eq!(review.weakest, 0.05);
    assert_eq!(review.weakest_link.question, "predecessor");
    assert!(!review.per_decision.contains_key("meta_self_report"));
    assert!(matches!(review.gate, Gate::Blocked(_)));
}

#[test]
fn every_recorded_phase_two_decision_set_opens() {
    let report: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/jev_poc_results/phase2_report.json"
    )))
    .expect("phase-2 report");

    for row in report["suites"]["BC"].as_array().expect("BC rows") {
        let review = DecisionReview::from_confidences(&[
            (
                "route_rule",
                row["route_conf"].as_f64().expect("route confidence"),
            ),
            (
                "solve_mode",
                row["mode_conf"].as_f64().expect("mode confidence"),
            ),
            (
                "data_complete",
                row["data_complete"].as_f64().expect("data completeness"),
            ),
        ]);
        assert_eq!(review.weakest, row["weakest"].as_f64().expect("weakest"));
        assert!(matches!(review.gate, Gate::Open), "{}", row["case"]);
    }

    for row in report["suites"]["D"].as_array().expect("D rows") {
        let confidences = row["confidences"].as_object().expect("confidences");
        let owned = confidences
            .iter()
            .map(|(question, confidence)| {
                (
                    question.clone(),
                    confidence.as_f64().expect("numeric confidence"),
                )
            })
            .collect::<Vec<_>>();
        let borrowed = owned
            .iter()
            .map(|(question, confidence)| (question.as_str(), *confidence))
            .collect::<Vec<_>>();
        let review = DecisionReview::from_confidences(&borrowed);
        assert_eq!(review.weakest, row["weakest"].as_f64().expect("weakest"));
        assert!(matches!(review.gate, Gate::Open), "{}", row["case"]);
    }
}

#[test]
fn provenance_records_served_model_answers_and_gate_without_key_material() {
    let response = JevResponse {
        model: "jev-1.13.0".to_owned(),
        answers: BTreeMap::from([
            ("route_rule".to_owned(), choice("layout.non_overlap", 0.98)),
            ("data_complete".to_owned(), Answer::Noul { noul: 0.32 }),
        ]),
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
        },
    };
    let review = DecisionReview::from_answers(&response.answers, &["data_complete"]);

    let provenance = JevProvenance::from_response(&response, &review);
    let value = serde_json::to_value(&provenance).expect("serialize provenance");

    assert_eq!(value["served_model_version"], "jev-1.13.0");
    assert_eq!(
        value["answers"],
        serde_json::to_value(&response.answers).unwrap()
    );
    assert_eq!(value["confidences"]["route_rule"], 0.98);
    assert_eq!(value["confidences"]["data_complete"], 0.32);
    assert_eq!(value["weakest"], 0.32);
    assert_eq!(value["gate"]["status"], "blocked");
    assert!(value.get("api_key").is_none());
    assert!(value.get("usage").is_none());
}
