use std::collections::BTreeMap;

use serde_json::{json, Value};
use spur_solver::rules::families::design::{
    compile::{
        compile, DesignCompileError, DesignCompileRequest, DesignRuleBinding, DesignRuleParameters,
        DesignSolveMode,
    },
    scene::{DesignField, DesignNode, DesignRect, DesignScene, DesignSize, DesignUnknown},
};
use spur_solver::types::{ConstraintOp, Variable};

fn rect(x: Option<i64>, y: i64, width: i64, height: i64) -> DesignRect {
    DesignRect {
        x,
        y: Some(y),
        width: Some(width),
        height: Some(height),
    }
}

fn scene() -> DesignScene {
    DesignScene {
        viewport: DesignSize {
            width: 390,
            height: 844,
        },
        nodes: BTreeMap::from([
            (
                "panel".to_owned(),
                DesignNode {
                    parent: None,
                    rect: rect(Some(0), 0, 320, 200),
                },
            ),
            (
                "child".to_owned(),
                DesignNode {
                    parent: Some("panel".to_owned()),
                    rect: rect(Some(16), 16, 44, 44),
                },
            ),
            (
                "second".to_owned(),
                DesignNode {
                    parent: None,
                    rect: rect(Some(84), 16, 44, 44),
                },
            ),
            (
                "media".to_owned(),
                DesignNode {
                    parent: None,
                    rect: rect(Some(0), 240, 320, 180),
                },
            ),
        ]),
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

fn request(mode: DesignSolveMode, rules: Vec<DesignRuleBinding>) -> DesignCompileRequest {
    DesignCompileRequest {
        mode,
        rules,
        scene: scene(),
        unknowns: Vec::new(),
        timeout_ms: 5_000,
        persist: true,
        include_smt: true,
    }
}

#[test]
fn synthesis_compiles_all_seed_rules_to_named_b_prime_predicates() {
    let compiled = compile(request(
        DesignSolveMode::Synthesize,
        vec![
            binding(
                "layout.containment",
                &["child", "panel"],
                DesignRuleParameters {
                    padding: Some(8),
                    ..DesignRuleParameters::default()
                },
            ),
            binding(
                "layout.non_overlap",
                &["child", "second"],
                DesignRuleParameters {
                    minimum_gap: Some(24),
                    ..DesignRuleParameters::default()
                },
            ),
            binding(
                "media.aspect_ratio",
                &["media"],
                DesignRuleParameters {
                    source_width: Some(16),
                    source_height: Some(9),
                    ..DesignRuleParameters::default()
                },
            ),
        ],
    ))
    .expect("compile seed rules");

    compiled.request.validate().expect("valid B-prime request");
    assert!(compiled.request.vars.is_empty());
    assert_eq!(compiled.request.constraints.len(), 3);
    assert_eq!(
        compiled
            .request
            .constraints
            .iter()
            .map(|constraint| constraint.id())
            .collect::<Vec<_>>(),
        [
            Some("design_rule_0_layout_containment"),
            Some("design_rule_1_layout_non_overlap"),
            Some("design_rule_2_media_aspect_ratio")
        ]
    );

    let value = serde_json::to_value(&compiled.request).expect("serialize request");
    let containment = &value["constraints"][0]["expr"];
    assert_eq!(containment["op"], "and");
    assert_eq!(containment["args"].as_array().map(Vec::len), Some(4));
    assert_eq!(
        containment["args"][0],
        json!({
            "kind": "op",
            "op": "le",
            "args": [
                {"kind": "op", "op": "add", "args": [
                    {"kind": "int", "value": 0},
                    {"kind": "int", "value": 8}
                ]},
                {"kind": "int", "value": 16}
            ]
        })
    );

    let disjoint = &value["constraints"][1]["expr"];
    assert_eq!(disjoint["op"], "or");
    assert_eq!(disjoint["args"].as_array().map(Vec::len), Some(4));

    let aspect_ratio = &value["constraints"][2]["expr"];
    assert_eq!(aspect_ratio["op"], "eq");
    assert_eq!(aspect_ratio["args"][0]["op"], "mul");
    assert_eq!(aspect_ratio["args"][1]["op"], "mul");
    assert_eq!(compiled.request.timeout_ms, 5_000);
    assert!(compiled.request.persist);
    assert!(compiled.request.include_smt);
}

#[test]
fn verification_compiles_direct_identity_preserving_rule_predicates() {
    let compiled = compile(request(
        DesignSolveMode::Verify,
        vec![
            binding(
                "layout.containment",
                &["child", "panel"],
                DesignRuleParameters::default(),
            ),
            binding(
                "layout.non_overlap",
                &["child", "second"],
                DesignRuleParameters::default(),
            ),
        ],
    ))
    .expect("compile verification query");

    assert_eq!(compiled.request.constraints.len(), 2);
    assert_eq!(
        compiled
            .request
            .constraints
            .iter()
            .map(|constraint| constraint.id())
            .collect::<Vec<_>>(),
        [
            Some("design_rule_0_layout_containment"),
            Some("design_rule_1_layout_non_overlap")
        ]
    );
    let expr = compiled.request.constraints[0].expr();
    let spur_solver::types::ConstraintExpr::Op {
        op: ConstraintOp::And,
        args,
    } = expr
    else {
        panic!("verification must assert the containment predicate directly");
    };
    assert_eq!(args.len(), 4);
    assert!(matches!(
        compiled.request.constraints[1].expr(),
        spur_solver::types::ConstraintExpr::Op {
            op: ConstraintOp::Or,
            ..
        }
    ));
    compiled.request.validate().expect("valid B-prime request");
}

#[test]
fn verification_rejects_declared_unknowns() {
    let mut input = request(
        DesignSolveMode::Verify,
        vec![binding(
            "layout.containment",
            &["child", "panel"],
            DesignRuleParameters::default(),
        )],
    );
    input.scene.nodes.get_mut("child").unwrap().rect.x = None;
    input.unknowns = vec![DesignUnknown {
        node: "child".to_owned(),
        field: DesignField::X,
        min: 0,
        max: 276,
    }];

    let error = compile(input).expect_err("verify must require a complete model");
    assert_eq!(
        error.to_string(),
        "verification requires a complete model; remove 1 unknown declaration"
    );
}

#[test]
fn bounded_unknowns_use_deterministic_variables_and_model_paths() {
    let mut input = request(
        DesignSolveMode::Synthesize,
        vec![binding(
            "layout.containment",
            &["child", "panel"],
            DesignRuleParameters::default(),
        )],
    );
    input.scene.nodes.get_mut("child").unwrap().rect.x = None;
    input.unknowns = vec![DesignUnknown {
        node: "child".to_owned(),
        field: DesignField::X,
        min: 0,
        max: 276,
    }];

    let compiled = compile(input).expect("compile bounded unknown");
    assert_eq!(
        compiled.request.vars,
        [Variable::IntRange {
            name: "design_u_0".to_owned(),
            min: 0,
            max: 276,
        }]
    );
    assert_eq!(compiled.unknowns[0].variable, "design_u_0");
    assert_eq!(compiled.unknowns[0].node, "child");
    assert_eq!(compiled.unknowns[0].field, DesignField::X);

    let value = serde_json::to_value(&compiled.request).expect("serialize request");
    assert!(contains_value(
        &value["constraints"],
        &json!({"kind": "var", "name": "design_u_0"})
    ));
}

#[test]
fn axis_capacity_compiles_horizontal_and_vertical_extent_sums() {
    let horizontal = serde_json::from_value::<DesignRuleBinding>(json!({
        "rule_id": "layout.axis_capacity",
        "subjects": ["panel", "child", "second"],
        "parameters": {
            "axis": "horizontal",
            "gap": 20,
            "inset_start": 10,
            "inset_end": 10
        }
    }))
    .expect("horizontal axis-capacity binding");
    let vertical = serde_json::from_value::<DesignRuleBinding>(json!({
        "rule_id": "layout.axis_capacity",
        "subjects": ["panel", "child", "second"],
        "parameters": {
            "axis": "vertical",
            "gap": 20,
            "inset_start": 10,
            "inset_end": 10
        }
    }))
    .expect("vertical axis-capacity binding");

    let horizontal = compile(request(DesignSolveMode::Verify, vec![horizontal]))
        .expect("compile horizontal capacity");
    let vertical = compile(request(DesignSolveMode::Verify, vec![vertical]))
        .expect("compile vertical capacity");
    let horizontal = serde_json::to_value(horizontal.request.constraints[0].expr())
        .expect("serialize horizontal predicate");
    let vertical = serde_json::to_value(vertical.request.constraints[0].expr())
        .expect("serialize vertical predicate");

    assert_eq!(horizontal["op"], "le");
    assert_eq!(horizontal["args"][0]["op"], "add");
    assert!(contains_value(
        &horizontal["args"][0],
        &json!({"kind": "int", "value": 44})
    ));
    assert_eq!(horizontal["args"][1], json!({"kind": "int", "value": 320}));
    assert_eq!(vertical["op"], "le");
    assert_eq!(vertical["args"][1], json!({"kind": "int", "value": 200}));
}

#[test]
fn compiler_rejects_unknown_rules_subjects_arities_and_parameters() {
    assert_eq!(
        compile(request(
            DesignSolveMode::Synthesize,
            vec![binding(
                "layout.missing",
                &["child"],
                DesignRuleParameters::default(),
            )],
        )),
        Err(DesignCompileError::UnknownRule {
            rule_id: "layout.missing".to_owned(),
        })
    );
    assert_eq!(
        compile(request(
            DesignSolveMode::Synthesize,
            vec![binding(
                "layout.containment",
                &["child"],
                DesignRuleParameters::default(),
            )],
        )),
        Err(DesignCompileError::InvalidSubjectArity {
            rule_id: "layout.containment".to_owned(),
            expected: 2,
            actual: 1,
        })
    );
    assert_eq!(
        compile(request(
            DesignSolveMode::Synthesize,
            vec![binding(
                "layout.non_overlap",
                &["child", "missing"],
                DesignRuleParameters::default(),
            )],
        )),
        Err(DesignCompileError::UnknownSubject {
            rule_id: "layout.non_overlap".to_owned(),
            node: "missing".to_owned(),
        })
    );
    assert_eq!(
        compile(request(
            DesignSolveMode::Synthesize,
            vec![binding(
                "media.aspect_ratio",
                &["media"],
                DesignRuleParameters {
                    padding: Some(1),
                    source_width: Some(16),
                    source_height: Some(9),
                    ..DesignRuleParameters::default()
                },
            )],
        )),
        Err(DesignCompileError::UnexpectedParameter {
            rule_id: "media.aspect_ratio".to_owned(),
            parameter: "padding",
        })
    );
}

fn contains_value(value: &Value, needle: &Value) -> bool {
    value == needle
        || value
            .as_array()
            .is_some_and(|items| items.iter().any(|item| contains_value(item, needle)))
        || value
            .as_object()
            .is_some_and(|items| items.values().any(|item| contains_value(item, needle)))
}
