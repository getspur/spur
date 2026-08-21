use serde_json::Value;
use spur_solver::rules::builtin_registry;

const BUILTIN_RULE_CATALOG_V1: &str = include_str!("fixtures/builtin_rule_catalog_v1.json");

fn fixture() -> Value {
    serde_json::from_str(BUILTIN_RULE_CATALOG_V1).expect("valid built-in rule catalog fixture")
}

fn ids<'a>(catalog: &'a Value, section: &str) -> Vec<&'a str> {
    catalog[section]
        .as_array()
        .unwrap_or_else(|| panic!("{section} array"))
        .iter()
        .map(|entry| {
            entry["id"]
                .as_str()
                .unwrap_or_else(|| panic!("{section} entry ID"))
        })
        .collect()
}

fn assert_strictly_sorted(ids: &[&str], section: &str) {
    assert!(
        ids.windows(2).all(|pair| pair[0] < pair[1]),
        "{section} must remain in stable ID order: {ids:?}"
    );
}

#[test]
fn builtin_registry_matches_frozen_catalog_v1() {
    let expected = fixture();
    let actual = serde_json::to_value(builtin_registry()).expect("serialize built-in registry");

    assert_eq!(actual, expected);
}

#[test]
fn frozen_catalog_contains_every_rule_in_stable_order() {
    let catalog = fixture();

    assert_strictly_sorted(&ids(&catalog, "families"), "families");
    assert_strictly_sorted(&ids(&catalog, "profiles"), "profiles");

    let rule_ids = ids(&catalog, "rules");
    assert_strictly_sorted(&rule_ids, "rules");
    assert_eq!(
        rule_ids.len(),
        40,
        "fixture must contain every built-in rule"
    );
    assert!(
        rule_ids.contains(&"rbac.minimum_privilege"),
        "fixture must include the catalog-only RBAC rule"
    );
}
