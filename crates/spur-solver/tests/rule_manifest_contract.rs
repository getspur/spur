use serde_json::{json, Map, Value};
use spur_solver::rules::{
    manifest::{validate_binding_contract, ValidatedBinding},
    manifest_format::{ExecutionKindV1, NativeHandlerV1},
};

fn validate(
    rule_id: &str,
    subjects: &[&str],
    parameters: Value,
) -> Result<ValidatedBinding, String> {
    let subjects = subjects
        .iter()
        .map(|subject| (*subject).to_owned())
        .collect::<Vec<_>>();
    let parameters = parameters
        .as_object()
        .expect("test parameters must be an object");
    validate_binding_contract(rule_id, &subjects, parameters)
}

fn object(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect()
}

#[test]
fn defaults_are_applied_and_the_closed_handler_is_returned() {
    let binding = validate("a11y.target_size", &["button"], json!({}))
        .expect("target-size defaults must validate");

    assert_eq!(binding.handler, NativeHandlerV1::A11yTargetSize);
    assert_eq!(
        binding.parameters,
        object(&[("minimum_height", json!(24)), ("minimum_width", json!(24)),])
    );
}

#[test]
fn unknown_ids_are_rejected_and_objectives_return_closed_handlers() {
    let unknown = validate("a11y.missing", &[], json!({})).expect_err("unknown rule must fail");
    assert_eq!(unknown, "unknown manifest rule `a11y.missing`");

    for (rule_id, subject, handler) in [
        (
            "rbac.minimum_privilege",
            "alice",
            NativeHandlerV1::RbacMinimumPrivilege,
        ),
        (
            "placement.minimize_skew",
            "api",
            NativeHandlerV1::PlacementMinimizeSkew,
        ),
    ] {
        let binding = validate(rule_id, &[subject], json!({}))
            .unwrap_or_else(|error| panic!("{rule_id} objective contract must validate: {error}"));
        assert_eq!(binding.execution_kind, ExecutionKindV1::Objective);
        assert_eq!(binding.handler, handler);
        assert!(binding.parameters.is_empty());
    }
}

#[test]
fn subject_cardinality_is_checked_for_exact_and_at_least_contracts() {
    let exact = validate("a11y.focus_not_obscured", &["focused"], json!({}))
        .expect_err("exact subject count must fail");
    assert_eq!(
        exact,
        "rule `a11y.focus_not_obscured` requires 2 subjects, got 1"
    );

    let at_least = validate(
        "layout.axis_capacity",
        &["container"],
        json!({"axis": "horizontal"}),
    )
    .expect_err("minimum subject count must fail");
    assert_eq!(
        at_least,
        "rule `layout.axis_capacity` requires at least 2 subjects, got 1"
    );
}

#[test]
fn accepted_names_required_values_enums_and_types_are_checked() {
    let unknown = validate(
        "a11y.target_size",
        &["button"],
        json!({"minimum_width": 24, "surprise": true}),
    )
    .expect_err("unknown parameter must fail");
    assert_eq!(
        unknown,
        "rule `a11y.target_size` does not accept parameter `surprise`"
    );

    let missing = validate("layout.axis_capacity", &["container", "item"], json!({}))
        .expect_err("required parameter must fail");
    assert_eq!(
        missing,
        "rule `layout.axis_capacity` requires parameter `axis`"
    );

    let binding = validate(
        "layout.axis_capacity",
        &["container", "item"],
        json!({"axis": "vertical"}),
    )
    .expect("enum member and defaults must validate");
    assert_eq!(binding.handler, NativeHandlerV1::LayoutAxisCapacity);
    assert_eq!(
        binding.parameters,
        object(&[
            ("axis", json!("vertical")),
            ("gap", json!(0)),
            ("inset_end", json!(0)),
            ("inset_start", json!(0)),
        ])
    );

    for parameters in [json!({"axis": "diagonal"}), json!({"axis": true})] {
        assert!(validate("layout.axis_capacity", &["container", "item"], parameters,).is_err());
    }
}

#[test]
fn inclusive_integer_minimum_is_accepted_and_values_below_it_are_rejected() {
    let binding = validate(
        "a11y.target_size",
        &["button"],
        json!({"minimum_width": 1, "minimum_height": 1}),
    )
    .expect("inclusive integer minimum must validate");
    assert_eq!(binding.parameters["minimum_width"], json!(1));
    assert_eq!(binding.parameters["minimum_height"], json!(1));

    for parameters in [
        json!({"minimum_width": 0}),
        json!({"minimum_width": 1.5}),
        json!({"minimum_width": "1"}),
    ] {
        assert!(validate("a11y.target_size", &["button"], parameters).is_err());
    }
}

#[test]
fn string_array_type_and_inclusive_length_bounds_are_checked() {
    for length in [1, 64] {
        let roles = (0..length)
            .map(|index| Value::String(format!("role-{index}")))
            .collect::<Vec<_>>();
        let binding = validate(
            "rbac.dynamic_separation_of_duty",
            &["session"],
            json!({"roles": roles}),
        )
        .expect("inclusive array boundary must validate");
        assert_eq!(binding.parameters["max_active"], json!(1));
    }

    let too_many = (0..65)
        .map(|index| format!("role-{index}"))
        .collect::<Vec<_>>();
    for parameters in [
        json!({"roles": []}),
        json!({"roles": too_many}),
        json!({"roles": ["admin", 1]}),
        json!({"roles": "admin"}),
    ] {
        assert!(validate("rbac.dynamic_separation_of_duty", &["session"], parameters,).is_err());
    }
}

#[test]
fn accessibility_exception_native_object_has_a_closed_structure() {
    let binding = validate(
        "a11y.target_size",
        &["button"],
        json!({
            "exception": {
                "kind": "two_dimensional",
                "evidence": "family-native semantics decide applicability"
            }
        }),
    )
    .expect("structurally valid exceptions reach native semantics");
    assert_eq!(
        binding.parameters["exception"]["kind"],
        json!("two_dimensional")
    );

    for exception in [
        json!(null),
        json!([]),
        json!({"evidence": "missing kind"}),
        json!({"kind": "spacing"}),
        json!({"kind": "unsupported", "evidence": "x"}),
        json!({"kind": "spacing", "evidence": 1}),
        json!({"kind": "spacing", "evidence": "x", "extra": true}),
    ] {
        assert!(validate(
            "a11y.target_size",
            &["button"],
            json!({"exception": exception}),
        )
        .is_err());
    }
}
