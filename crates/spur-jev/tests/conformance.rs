use std::{collections::BTreeMap, ffi::OsString, process::Command};

use serde::Deserialize;
use serde_json::Value;
use spur_jev::{
    compile::{compile_bprime, compile_family},
    gate::{DecisionReview, Gate},
    snapshot::{CatalogSnapshot, RuleCard},
    wire::Answer,
};
use spur_solver::{
    constraint_spec::parse_and_validate,
    rules::execute::{prepare, run},
    service::SolverService,
};

#[derive(Debug, Deserialize)]
struct RecordedFixture {
    cases: Vec<RecordedCase>,
}

#[derive(Debug, Deserialize)]
struct RecordedCase {
    case: String,
    answers: BTreeMap<String, RecordedAnswer>,
    #[serde(default)]
    gating_nouls: Vec<String>,
    #[serde(flatten)]
    input: RecordedInput,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RecordedAnswer {
    Choice { choice: String, confidence: f64 },
    Noul { noul: f64 },
}

impl RecordedAnswer {
    fn to_answer(&self) -> Answer {
        match self {
            Self::Choice { choice, confidence } => Answer::Choice {
                choice: choice.clone(),
                probabilities: BTreeMap::from([(choice.clone(), 1.0)]),
                confidence: *confidence,
            },
            Self::Noul { noul } => Answer::Noul { noul: *noul },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RecordedInput {
    Family { family: String, data: Value },
    Bprime { template: Value },
}

#[derive(Debug, Deserialize)]
struct Expected {
    status: String,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    diagnostic: Option<String>,
    #[serde(default)]
    unsat_core: Option<Vec<String>>,
    #[serde(default)]
    model: BTreeMap<String, Value>,
    #[serde(default)]
    termination: Option<String>,
    #[serde(default)]
    objective_value: Option<i64>,
    #[serde(default)]
    objective_bound_kind: Option<String>,
    #[serde(default)]
    objective_bound_exact: Option<String>,
    #[serde(default)]
    soft_satisfied: Vec<bool>,
}

impl RecordedCase {
    fn typed_answers(&self) -> BTreeMap<String, Answer> {
        self.answers
            .iter()
            .map(|(question, answer)| (question.clone(), answer.to_answer()))
            .collect()
    }
}

fn load_recorded_cases() -> Vec<RecordedCase> {
    let fixture: RecordedFixture =
        serde_json::from_str(include_str!("fixtures/recorded_cases.json"))
            .expect("recorded conformance fixture must deserialize");
    fixture.cases
}

fn load_recorded_case(name: &str) -> RecordedCase {
    load_recorded_cases()
        .into_iter()
        .find(|case| case.case == name)
        .unwrap_or_else(|| panic!("missing recorded case {name}"))
}

fn z3_available() -> bool {
    let binary = std::env::var_os("SPUR_Z3_BIN").unwrap_or_else(|| OsString::from("z3"));
    Command::new(binary)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

async fn solve_via_spur_solver(case: &RecordedCase) -> Value {
    let answers = case.typed_answers();
    let gating_nouls = case
        .gating_nouls
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let review = DecisionReview::from_answers(&answers, &gating_nouls);
    assert!(
        matches!(review.gate, Gate::Open),
        "{} gate unexpectedly blocked: {review:?}",
        case.case
    );

    match &case.input {
        RecordedInput::Family { family, data } => {
            let rule_id = match answers.get("route_rule") {
                Some(Answer::Choice { choice, .. }) => choice.clone(),
                other => panic!("{} missing route_rule choice: {other:?}", case.case),
            };
            let snapshot = CatalogSnapshot {
                language_version: 1,
                rules: vec![RuleCard {
                    rule_id,
                    family: family.clone(),
                    summary: "recorded fixture".to_owned(),
                }],
            };
            let request = compile_family(&answers, &snapshot, data)
                .unwrap_or_else(|error| panic!("{} family compile: {error}", case.case));
            let prepared = prepare(request)
                .unwrap_or_else(|error| panic!("{} family prepare: {error}", case.case));
            let response = run(&SolverService::new(), prepared)
                .await
                .unwrap_or_else(|error| panic!("{} family solve: {error}", case.case));
            serde_json::to_value(response).expect("family response must serialize")
        }
        RecordedInput::Bprime { template } => {
            let request = compile_bprime(&answers, template)
                .unwrap_or_else(|error| panic!("{} B-prime compile: {error}", case.case));
            let request = parse_and_validate(request)
                .unwrap_or_else(|error| panic!("{} B-prime validate: {error:?}", case.case));
            let response = SolverService::new()
                .solve_constraints(request)
                .await
                .unwrap_or_else(|error| panic!("{} B-prime solve: {error}", case.case));
            serde_json::to_value(response).expect("B-prime response must serialize")
        }
    }
}

fn assert_expected(case: &RecordedCase, observed: &Value) {
    assert_eq!(
        observed["status"], case.expected.status,
        "{} status",
        case.case
    );

    if let Some(expected) = &case.expected.outcome {
        assert_eq!(observed["outcome"], *expected, "{} outcome", case.case);
    }
    if let Some(expected) = &case.expected.diagnostic {
        assert_eq!(
            observed["rule_results"][0]["diagnostic"], *expected,
            "{} diagnostic",
            case.case
        );
    }
    if let Some(expected) = &case.expected.unsat_core {
        assert_eq!(
            observed["unsat_core"],
            Value::from(expected.clone()),
            "{} core",
            case.case
        );
    }
    for (name, expected) in &case.expected.model {
        assert_eq!(
            observed["model"][name], *expected,
            "{} model.{name}",
            case.case
        );
    }
    if let Some(expected) = &case.expected.termination {
        assert_eq!(
            observed["optimization"]["termination"], *expected,
            "{} optimization termination",
            case.case
        );
    }
    if let Some(expected) = case.expected.objective_value {
        assert_eq!(
            observed["optimization"]["solutions"][0]["objectives"][0]["value"], expected,
            "{} objective value",
            case.case
        );
    }
    if let Some(expected) = &case.expected.objective_bound_kind {
        assert_eq!(
            observed["optimization"]["solutions"][0]["objectives"][0]["bound"]["kind"], *expected,
            "{} objective bound kind",
            case.case
        );
    }
    if let Some(expected) = &case.expected.objective_bound_exact {
        assert_eq!(
            observed["optimization"]["solutions"][0]["objectives"][0]["bound"]["exact"], *expected,
            "{} objective bound",
            case.case
        );
    }
    if !case.expected.soft_satisfied.is_empty() {
        let actual = observed["optimization"]["solutions"][0]["soft_constraints"]
            .as_array()
            .expect("optimization must expose soft constraints")
            .iter()
            .map(|soft| soft["satisfied"].as_bool().expect("soft satisfied Boolean"))
            .collect::<Vec<_>>();
        assert_eq!(
            actual, case.expected.soft_satisfied,
            "{} soft constraints",
            case.case
        );
    }
}

#[tokio::test]
async fn b2_non_overlap_violation_fails_with_overlap_diagnostic() {
    if !z3_available() {
        eprintln!("skipping recorded conformance case: Z3 binary is unavailable");
        return;
    }

    let case = load_recorded_case("B2_non_overlap_violation");
    let outcome = solve_via_spur_solver(&case).await;
    assert_expected(&case, &outcome);
    assert_eq!(outcome["rule_results"][0]["diagnostic"], "design.overlap");
}

#[tokio::test]
async fn all_recorded_phase_two_cases_conform_offline() {
    if !z3_available() {
        eprintln!("skipping recorded conformance matrix: Z3 binary is unavailable");
        return;
    }

    let cases = load_recorded_cases();
    assert_eq!(
        cases.len(),
        11,
        "fixture must retain every recorded B/C/D case"
    );
    for case in cases {
        let outcome = solve_via_spur_solver(&case).await;
        assert_expected(&case, &outcome);
    }
}
