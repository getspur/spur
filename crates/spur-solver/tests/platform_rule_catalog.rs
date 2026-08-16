use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::{mcp::SolverMcpModule, rules::builtin_registry};

fn registry() -> ToolRegistry {
    ToolRegistry::builder()
        .with(SolverMcpModule::catalog_only())
        .expect("register solver catalog module")
        .build()
}

fn context() -> ToolCallContext<'static> {
    ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None)
}

fn result_json(response: &spur_mcp::response::JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "unexpected MCP error: {:?}",
        response.error
    );
    let text = response
        .result
        .as_ref()
        .and_then(|result| result["content"][0]["text"].as_str())
        .expect("MCP JSON text result");
    serde_json::from_str(text).expect("parse MCP JSON text")
}

#[test]
fn builtin_registry_composes_all_platform_families_in_stable_order() {
    assert_eq!(
        builtin_registry()
            .families()
            .iter()
            .map(|family| family.id())
            .collect::<Vec<_>>(),
        vec!["accessibility", "design", "policy", "resource"]
    );

    for rule_id in [
        "a11y.focus_not_obscured",
        "a11y.reflow",
        "a11y.target_size",
        "a11y.text_contrast",
        "rbac.dynamic_separation_of_duty",
        "rbac.permission_reachable",
        "rbac.role_hierarchy_acyclic",
        "rbac.static_separation_of_duty",
        "resource.aggregate_capacity",
        "resource.quota_capacity",
        "resource.request_within_limit",
        "placement.minimum_failure_domains",
        "placement.topology_max_skew",
    ] {
        assert!(
            builtin_registry().rule(rule_id).is_some(),
            "missing implemented rule {rule_id}"
        );
    }
}

#[test]
fn solve_rules_schema_is_closed_and_family_discriminated() {
    let tools = spur_solver::mcp::tool_definitions();
    let schema = &tools
        .iter()
        .find(|tool| tool.name == "solve_rules")
        .expect("solve_rules tool definition")
        .input_schema;

    let branches = schema["oneOf"].as_array().expect("family schema branches");
    assert_eq!(branches.len(), 4);
    assert_eq!(
        branches
            .iter()
            .map(|branch| branch["properties"]["family"]["const"]
                .as_str()
                .expect("family const"))
            .collect::<Vec<_>>(),
        vec!["accessibility", "design", "policy", "resource"]
    );
    assert!(branches
        .iter()
        .all(|branch| branch["additionalProperties"] == false));
}

#[tokio::test]
async fn rule_spec_lists_platform_families_and_exposes_authority_and_capability() {
    let list = result_json(
        &registry()
            .call_json_tool(context(), "solve_rule_spec", json!({}))
            .await,
    );
    assert_eq!(list["families"].as_array().map(Vec::len), Some(4));
    assert_eq!(list["families"][0]["id"], "accessibility");
    assert_eq!(list["families"][1]["id"], "design");
    assert_eq!(list["families"][2]["id"], "policy");
    assert_eq!(list["families"][3]["id"], "resource");

    let target_size = result_json(
        &registry()
            .call_json_tool(
                context(),
                "solve_rule_spec",
                json!({"rule_id": "a11y.target_size", "include": "all"}),
            )
            .await,
    );
    assert_eq!(target_size["rule"]["availability"], "implemented");
    assert_eq!(target_size["rule"]["default_strength"], "hard");
    assert_eq!(
        target_size["rule"]["authorities"][0]["url"],
        "https://www.w3.org/TR/WCAG22/"
    );
    assert!(target_size["rule"]["requires"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == "exception.evidence")));

    let least_privilege = result_json(
        &registry()
            .call_json_tool(
                context(),
                "solve_rule_spec",
                json!({"rule_id": "rbac.minimum_privilege"}),
            )
            .await,
    );
    assert_eq!(
        least_privilege["rule"]["availability"],
        "capability_unavailable"
    );
    assert_eq!(least_privilege["rule"]["default_strength"], "advisory");
}
