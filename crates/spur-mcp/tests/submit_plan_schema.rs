//! submit_plan schema shape tests.
//!
//! Guards that the new persist_as_epic fields are advertised with the
//! right types and descriptions. Negative-input behavior is tested in
//! tests/submit_plan_persist.rs.

use spur_mcp::tools_list;

fn submit_plan_def() -> serde_json::Value {
    tools_list()
        .into_iter()
        .find(|t| t.name == "submit_plan")
        .expect("submit_plan must be in tool catalog")
        .input_schema
}

#[test]
fn schema_advertises_persist_as_epic() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("persist_as_epic"))
        .expect("persist_as_epic must be advertised");
    assert_eq!(
        prop.get("type").and_then(|v| v.as_str()),
        Some("boolean"),
        "persist_as_epic must be boolean"
    );
}

#[test]
fn schema_advertises_epic_title_as_string() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("epic_title"))
        .expect("epic_title must be advertised");
    assert_eq!(
        prop.get("type").and_then(|v| v.as_str()),
        Some("string"),
        "epic_title must be string"
    );
}

#[test]
fn schema_advertises_epic_body_as_string() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("epic_body"))
        .expect("epic_body must be advertised");
    assert_eq!(
        prop.get("type").and_then(|v| v.as_str()),
        Some("string"),
        "epic_body must be string"
    );
}

#[test]
fn persist_fields_are_not_required() {
    let schema = submit_plan_def();
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(
        !required.contains(&"persist_as_epic"),
        "persist_as_epic must remain optional"
    );
    assert!(
        !required.contains(&"epic_title"),
        "epic_title is only required when persist_as_epic is true (enforced in handler)"
    );
}

#[test]
fn persist_as_epic_without_title_is_documented_as_handler_error() {
    // Schema-level test only: epic_title is optional at schema level.
    // Handler-level rejection lives in submit_plan_persist.rs once the
    // handler branch is implemented (Task 6).
    let schema = submit_plan_def();
    // Documented via description text containing "Required when".
    let desc = schema
        .get("properties")
        .and_then(|p| p.get("epic_title"))
        .and_then(|p| p.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        desc.to_lowercase().contains("required when"),
        "epic_title description must document its conditional-required semantics; got: {desc}",
    );
}

#[test]
fn schema_advertises_base_oneof() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("base"))
        .expect("base must be advertised");
    let one_of = prop
        .get("oneOf")
        .and_then(|v| v.as_array())
        .expect("base must be a oneOf union");
    assert_eq!(
        one_of.len(),
        3,
        "base must list repo_main / branch / commit"
    );
    let kinds: Vec<&str> = one_of
        .iter()
        .filter_map(|variant| {
            variant
                .get("properties")
                .and_then(|p| p.get("kind"))
                .and_then(|k| k.get("const"))
                .and_then(|c| c.as_str())
        })
        .collect();
    assert!(kinds.contains(&"repo_main"));
    assert!(kinds.contains(&"branch"));
    assert!(kinds.contains(&"commit"));
}

#[test]
fn schema_base_field_is_optional() {
    let schema = submit_plan_def();
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(!required.contains(&"base"), "base must not be required");
}
