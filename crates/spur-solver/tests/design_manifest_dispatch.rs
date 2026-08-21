use std::collections::BTreeMap;

use spur_solver::{
    rules::{
        families::design::{
            compile::{
                compile, DesignCompileError, DesignCompileRequest, DesignRuleBinding,
                DesignRuleParameters, DesignSolveMode,
            },
            scene::{DesignNode, DesignRect, DesignScene, DesignSize},
        },
        manifest::manifest_rule_handler,
        manifest_format::NativeHandlerV1,
    },
    types::{ConstraintExpr, ConstraintOp},
};

fn rect(x: i64, y: i64, width: i64, height: i64) -> DesignRect {
    DesignRect {
        x: Some(x),
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
                    rect: rect(0, 0, 320, 200),
                },
            ),
            (
                "child".to_owned(),
                DesignNode {
                    parent: Some("panel".to_owned()),
                    rect: rect(16, 16, 44, 44),
                },
            ),
            (
                "second".to_owned(),
                DesignNode {
                    parent: None,
                    rect: rect(84, 16, 44, 44),
                },
            ),
            (
                "media".to_owned(),
                DesignNode {
                    parent: None,
                    rect: rect(0, 240, 320, 180),
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

fn request(rule: DesignRuleBinding) -> DesignCompileRequest {
    DesignCompileRequest {
        mode: DesignSolveMode::Synthesize,
        rules: vec![rule],
        scene: scene(),
        unknowns: Vec::new(),
        timeout_ms: 5_000,
        persist: false,
        include_smt: false,
    }
}

#[test]
fn all_design_manifest_handlers_dispatch_to_existing_native_predicates() {
    let cases = [
        (
            binding(
                "layout.axis_capacity",
                &["panel", "child", "second"],
                DesignRuleParameters {
                    axis: Some(
                        spur_solver::rules::families::design::compile::DesignAxis::Horizontal,
                    ),
                    gap: Some(20),
                    inset_start: Some(10),
                    inset_end: Some(10),
                    ..DesignRuleParameters::default()
                },
            ),
            NativeHandlerV1::LayoutAxisCapacity,
            ConstraintOp::Le,
        ),
        (
            binding(
                "layout.containment",
                &["child", "panel"],
                DesignRuleParameters {
                    padding: Some(8),
                    ..DesignRuleParameters::default()
                },
            ),
            NativeHandlerV1::LayoutContainment,
            ConstraintOp::And,
        ),
        (
            binding(
                "layout.non_overlap",
                &["child", "second"],
                DesignRuleParameters {
                    minimum_gap: Some(24),
                    ..DesignRuleParameters::default()
                },
            ),
            NativeHandlerV1::LayoutNonOverlap,
            ConstraintOp::Or,
        ),
        (
            binding(
                "media.aspect_ratio",
                &["media"],
                DesignRuleParameters {
                    source_width: Some(16),
                    source_height: Some(9),
                    ..DesignRuleParameters::default()
                },
            ),
            NativeHandlerV1::MediaAspectRatio,
            ConstraintOp::Eq,
        ),
    ];

    for (rule, expected_handler, expected_op) in cases {
        assert_eq!(
            manifest_rule_handler(&rule.rule_id),
            Some(expected_handler),
            "{} must select its closed native handler",
            rule.rule_id
        );
        let compiled = compile(request(rule)).expect("manifest handler must compile");
        let [constraint] = compiled.request.constraints.as_slice() else {
            panic!("one design binding must generate one constraint");
        };
        let ConstraintExpr::Op { op, .. } = constraint.expr() else {
            panic!("native design handlers must produce operator predicates");
        };
        assert_eq!(*op, expected_op);
    }
}

#[test]
fn manifest_contract_failure_precedes_scene_and_constraint_generation() {
    let mut input = request(binding(
        "media.aspect_ratio",
        &["media"],
        DesignRuleParameters {
            padding: Some(1),
            source_width: Some(16),
            source_height: Some(9),
            ..DesignRuleParameters::default()
        },
    ));
    input.scene.viewport.width = 0;

    assert_eq!(
        compile(input),
        Err(DesignCompileError::UnexpectedParameter {
            rule_id: "media.aspect_ratio".to_owned(),
            parameter: "padding",
        })
    );
}
