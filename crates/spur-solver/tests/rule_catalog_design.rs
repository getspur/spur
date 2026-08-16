use spur_solver::rules::{catalog::RuleDefinition, families::design::builtin_registry};

#[test]
fn design_family_lists_geometric_integrity_rules_in_stable_order() {
    let registry = builtin_registry();

    assert_eq!(
        registry
            .families()
            .iter()
            .map(|family| family.id())
            .collect::<Vec<_>>(),
        ["design"]
    );
    assert_eq!(
        registry
            .profiles()
            .iter()
            .map(|profile| profile.id())
            .collect::<Vec<_>>(),
        ["geometric_integrity", "layout_capacity"]
    );
    assert_eq!(
        registry
            .rules()
            .iter()
            .map(RuleDefinition::id)
            .collect::<Vec<_>>(),
        [
            "layout.axis_capacity",
            "layout.containment",
            "layout.non_overlap",
            "media.aspect_ratio"
        ]
    );
}

#[test]
fn every_seed_rule_has_authority_examples_and_encoding_guidance() {
    for rule in builtin_registry().rules() {
        let value = serde_json::to_value(rule).expect("rule must serialize");

        assert_eq!(value["availability"], "implemented", "{}", rule.id());
        assert_eq!(value["default_strength"], "hard", "{}", rule.id());
        assert!(
            value["authorities"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "{}",
            rule.id()
        );
        assert!(value["examples"]["valid"].is_object(), "{}", rule.id());
        assert!(value["examples"]["invalid"].is_object(), "{}", rule.id());
        assert!(
            value["llm_encoding"]["encode_steps"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "{}",
            rule.id()
        );
        assert!(
            value["solver_encoding"]["formula"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "{}",
            rule.id()
        );
    }
}

#[test]
fn aspect_ratio_guidance_uses_cross_multiplication_instead_of_division() {
    let rule = builtin_registry()
        .rule("media.aspect_ratio")
        .expect("seed aspect-ratio rule");
    let value = serde_json::to_value(rule).expect("rule must serialize");
    let formulas = value["solver_encoding"]["formula"]
        .as_array()
        .expect("formula array");

    assert_eq!(
        formulas,
        &["render.width * source.height = render.height * source.width"]
    );
    assert!(formulas
        .iter()
        .all(|formula| !formula.as_str().unwrap_or_default().contains('/')));
}

#[test]
fn invalid_examples_name_stable_diagnostics() {
    let diagnostics = builtin_registry()
        .rules()
        .iter()
        .map(|rule| {
            let value = serde_json::to_value(rule).expect("rule must serialize");
            value["examples"]["invalid"]["expected_diagnostic"]
                .as_str()
                .expect("diagnostic")
                .to_owned()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        diagnostics,
        [
            "design.axis_capacity_exceeded",
            "design.outside_parent",
            "design.overlap",
            "design.aspect_ratio_mismatch"
        ]
    );
}

#[test]
fn axis_capacity_guidance_exposes_the_framework_neutral_equation() {
    let rule = builtin_registry()
        .rule("layout.axis_capacity")
        .expect("axis-capacity rule");
    let value = serde_json::to_value(rule).expect("rule must serialize");

    assert_eq!(rule.profile(), "layout_capacity");
    assert_eq!(rule.primitive(), "axis_capacity");
    assert_eq!(
        value["solver_encoding"]["formula"],
        serde_json::json!([
            "sum(item extents) + gap * (item count - 1) + inset_start + inset_end <= available extent"
        ])
    );
}
