use serde_json::{json, Value};
use spur_solver::{
    rules::{
        compiler::RuleFamilyCompiler, families::accessibility::COMPILER,
        manifest::manifest_rule_handler, manifest_format::NativeHandlerV1,
    },
    types::{ConstraintExpr, ConstraintOp},
};

fn request(rule_id: &str, subjects: &[&str], parameters: Value) -> Value {
    json!({
        "family": "accessibility",
        "mode": "verify",
        "rules": [{
            "rule_id": rule_id,
            "subjects": subjects,
            "parameters": parameters,
        }],
        "scene": {
            "viewport": {"width": 320, "height": 568},
            "elements": {
                "content": {"rect": {"x": 0, "y": 0, "width": 320, "height": 568}},
                "focused": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "obscurer": {"rect": {"x": 12, "y": 0, "width": 24, "height": 24}},
                "target": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "text": {"foreground_luminance": 17_500, "background_luminance": 0},
            },
        },
        "unknowns": [],
    })
}

#[test]
fn all_accessibility_manifest_handlers_dispatch_to_existing_native_predicates() {
    let cases = [
        (
            "a11y.focus_not_obscured",
            &["focused", "obscurer"][..],
            NativeHandlerV1::A11yFocusNotObscured,
            ConstraintOp::Not,
        ),
        (
            "a11y.reflow",
            &["content"][..],
            NativeHandlerV1::A11yReflow,
            ConstraintOp::Or,
        ),
        (
            "a11y.target_size",
            &["target"][..],
            NativeHandlerV1::A11yTargetSize,
            ConstraintOp::Or,
        ),
        (
            "a11y.text_contrast",
            &["text"][..],
            NativeHandlerV1::A11yTextContrast,
            ConstraintOp::Or,
        ),
    ];

    for (rule_id, subjects, expected_handler, expected_op) in cases {
        assert_eq!(
            manifest_rule_handler(rule_id),
            Some(expected_handler),
            "{rule_id} must select its closed native handler",
        );
        let compiled = COMPILER
            .compile(request(rule_id, subjects, json!({})))
            .unwrap_or_else(|error| panic!("{rule_id} manifest handler must compile: {error}"));
        let [constraint] = compiled.request.constraints.as_slice() else {
            panic!("one accessibility binding must generate one constraint");
        };
        let ConstraintExpr::Op { op, .. } = constraint.expr() else {
            panic!("native accessibility handlers must produce operator predicates");
        };
        assert_eq!(
            *op, expected_op,
            "unexpected native predicate for {rule_id}"
        );
    }
}

#[test]
fn manifest_contract_failure_precedes_scene_and_constraint_generation() {
    let mut input = request(
        "a11y.focus_not_obscured",
        &["focused", "obscurer"],
        json!({"minimum_width": 1}),
    );
    input["scene"]["viewport"]["width"] = json!(0);

    let error = COMPILER
        .compile(input)
        .expect_err("manifest contract must reject before semantic compilation")
        .to_string();

    assert!(
        error.contains("rule `a11y.focus_not_obscured` does not accept target-size parameters"),
        "unexpected error: {error}",
    );
    assert!(
        !error.contains("viewport dimensions"),
        "unexpected error: {error}"
    );
}
