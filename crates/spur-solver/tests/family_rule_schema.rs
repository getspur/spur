use serde_json::Value;
use spur_solver::rules::{families, manifest_family_executable_rule_ids};

const ACCESSIBILITY_COMPILE: &str = include_str!("../src/rules/families/accessibility/compile.rs");
const POLICY_COMPILE: &str = include_str!("../src/rules/families/policy/compile.rs");
const RESOURCE_COMPILE: &str = include_str!("../src/rules/families/resource/compile.rs");

#[test]
fn family_rule_enums_equal_manifest_executable_ids() {
    for family in ["accessibility", "design", "policy", "resource"] {
        let expected = manifest_family_executable_rule_ids(family)
            .unwrap_or_else(|| panic!("missing manifest executable IDs for {family}"));
        let actual = schema_rule_ids(family);

        assert_eq!(actual, expected, "{family} compiler rule enum drifted");
    }

    assert!(!schema_rule_ids("policy")
        .iter()
        .any(|rule_id| rule_id == "rbac.minimum_privilege"));
}

#[test]
fn affected_schema_builders_do_not_hard_code_rule_ids() {
    for (family, source) in [
        ("accessibility", ACCESSIBILITY_COMPILE),
        ("policy", POLICY_COMPILE),
        ("resource", RESOURCE_COMPILE),
    ] {
        let schema_source = source
            .rsplit_once("fn input_schema() -> Value")
            .unwrap_or_else(|| panic!("missing {family} input_schema"))
            .1;

        assert!(
            schema_source.contains("manifest_family_executable_rule_ids"),
            "{family} input_schema must source rule IDs from manifests"
        );
        for rule_id in manifest_family_executable_rule_ids(family)
            .unwrap_or_else(|| panic!("missing manifest executable IDs for {family}"))
        {
            assert!(
                !schema_source.contains(&format!("\"{rule_id}\"")),
                "{family} input_schema hard-codes manifest rule ID {rule_id}"
            );
        }
    }
}

fn schema_rule_ids(family: &str) -> Vec<String> {
    let schema = families::compiler(family)
        .unwrap_or_else(|| panic!("missing compiler for {family}"))
        .input_schema();

    schema
        .pointer("/properties/rules/items/properties/rule_id/enum")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing rule enum in {family} compiler schema"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("non-string rule ID in {family} compiler schema"))
                .to_owned()
        })
        .collect()
}
