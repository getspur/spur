use std::collections::BTreeMap;

use spur_solver::{
    rules::{
        builtin_registry,
        families::design::{
            compile::{
                compile, DesignCompileRequest, DesignRuleBinding, DesignRuleParameters,
                DesignSolveMode,
            },
            scene::{DesignNode, DesignRect, DesignScene, DesignSize},
        },
    },
    service::SolverService,
    types::SolveStatus,
};

#[derive(Clone)]
struct ConformanceCase {
    rule: DesignRuleBinding,
    valid: DesignScene,
    invalid: DesignScene,
}

fn rect(x: i64, y: i64, width: i64, height: i64) -> DesignNode {
    DesignNode {
        parent: None,
        rect: DesignRect {
            x: Some(x),
            y: Some(y),
            width: Some(width),
            height: Some(height),
        },
    }
}

fn scene(nodes: impl IntoIterator<Item = (&'static str, DesignNode)>) -> DesignScene {
    DesignScene {
        viewport: DesignSize {
            width: 390,
            height: 844,
        },
        nodes: nodes
            .into_iter()
            .map(|(id, node)| (id.to_owned(), node))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn binding(
    rule_id: &str,
    subjects: &[&str],
    parameters: DesignRuleParameters,
) -> DesignRuleBinding {
    DesignRuleBinding {
        rule_id: rule_id.to_owned(),
        subjects: subjects
            .iter()
            .map(|subject| (*subject).to_owned())
            .collect(),
        parameters,
    }
}

fn compile_verification(case: &ConformanceCase, scene: DesignScene) -> DesignCompileRequest {
    DesignCompileRequest {
        mode: DesignSolveMode::Verify,
        rules: vec![case.rule.clone()],
        scene,
        unknowns: Vec::new(),
        timeout_ms: 30_000,
        persist: false,
        include_smt: false,
    }
}

fn cases() -> Vec<ConformanceCase> {
    vec![
        ConformanceCase {
            rule: serde_json::from_value(serde_json::json!({
                "rule_id": "layout.axis_capacity",
                "subjects": ["container", "first", "second"],
                "parameters": {
                    "axis": "horizontal",
                    "gap": 20,
                    "inset_start": 10,
                    "inset_end": 10
                }
            }))
            .expect("axis-capacity conformance binding"),
            valid: scene([
                ("container", rect(0, 0, 100, 1)),
                ("first", rect(0, 0, 30, 1)),
                ("second", rect(0, 0, 30, 1)),
            ]),
            invalid: scene([
                ("container", rect(0, 0, 100, 1)),
                ("first", rect(0, 0, 30, 1)),
                ("second", rect(0, 0, 31, 1)),
            ]),
        },
        ConformanceCase {
            rule: binding(
                "layout.containment",
                &["child", "parent"],
                DesignRuleParameters::default(),
            ),
            valid: scene([
                ("parent", rect(0, 0, 320, 200)),
                ("child", rect(276, 16, 44, 44)),
            ]),
            invalid: scene([
                ("parent", rect(0, 0, 320, 200)),
                ("child", rect(277, 16, 44, 44)),
            ]),
        },
        ConformanceCase {
            rule: binding(
                "layout.non_overlap",
                &["first", "second"],
                DesignRuleParameters {
                    minimum_gap: Some(24),
                    ..DesignRuleParameters::default()
                },
            ),
            valid: scene([
                ("first", rect(16, 16, 44, 44)),
                ("second", rect(84, 16, 44, 44)),
            ]),
            invalid: scene([
                ("first", rect(16, 16, 44, 44)),
                ("second", rect(83, 16, 44, 44)),
            ]),
        },
        ConformanceCase {
            rule: binding(
                "media.aspect_ratio",
                &["media"],
                DesignRuleParameters {
                    source_width: Some(16),
                    source_height: Some(9),
                    ..DesignRuleParameters::default()
                },
            ),
            valid: scene([("media", rect(0, 0, 320, 180))]),
            invalid: scene([("media", rect(0, 0, 320, 181))]),
        },
    ]
}

#[tokio::test]
async fn every_implemented_design_rule_accepts_its_boundary_and_rejects_overflow() {
    let cases = cases();
    assert_eq!(
        cases
            .iter()
            .map(|case| case.rule.rule_id.as_str())
            .collect::<Vec<_>>(),
        builtin_registry()
            .rules()
            .iter()
            .map(|rule| rule.id())
            .collect::<Vec<_>>()
    );

    let service = SolverService::new();
    for case in cases {
        let valid = compile(compile_verification(&case, case.valid.clone()))
            .expect("valid conformance model must compile");
        assert_eq!(
            service
                .solve_constraints(valid.request)
                .await
                .expect("valid conformance model must solve")
                .status,
            SolveStatus::Sat,
            "{} exact boundary",
            case.rule.rule_id
        );

        let invalid = compile(compile_verification(&case, case.invalid.clone()))
            .expect("invalid conformance model must compile");
        assert_eq!(
            service
                .solve_constraints(invalid.request)
                .await
                .expect("invalid conformance model must solve")
                .status,
            SolveStatus::Unsat,
            "{} one-unit violation",
            case.rule.rule_id
        );
    }
}
