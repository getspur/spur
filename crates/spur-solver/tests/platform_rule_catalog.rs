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
fn solve_rules_schema_is_bedrock_compatible_and_family_discriminated() {
    let tools = spur_solver::mcp::tool_definitions();
    let schema = &tools
        .iter()
        .find(|tool| tool.name == "solve_rules")
        .expect("solve_rules tool definition")
        .input_schema;

    assert_eq!(
        schema["properties"]["family"]["enum"],
        json!(["accessibility", "design", "policy", "resource"])
    );
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["family", "mode", "rules"]));
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn bedrock_requires_simple_object_for_every_solver_tool_schema() {
    fn find_unsupported_keyword(value: &Value, path: &str) -> Option<String> {
        match value {
            Value::Object(map) => {
                for keyword in ["oneOf", "allOf"] {
                    if map.contains_key(keyword) {
                        return Some(format!("{path}.{keyword}"));
                    }
                }
                map.iter().find_map(|(key, value)| {
                    find_unsupported_keyword(value, &format!("{path}.{key}"))
                })
            }
            Value::Array(values) => values.iter().enumerate().find_map(|(index, value)| {
                find_unsupported_keyword(value, &format!("{path}[{index}]"))
            }),
            _ => None,
        }
    }

    for tool in spur_solver::mcp::tool_definitions() {
        assert_eq!(
            tool.input_schema["type"], "object",
            "Bedrock requires a top-level object schema for {}",
            tool.name
        );
        assert_eq!(
            find_unsupported_keyword(&tool.input_schema, "$"),
            None,
            "Bedrock rejects oneOf/allOf in {}",
            tool.name
        );
    }
}

#[test]
fn topology_guidance_distinguishes_normalized_model_from_scheduler_semantics() {
    let rule = builtin_registry()
        .rule("placement.topology_max_skew")
        .expect("topology rule");
    let guidance = serde_json::to_value(rule).expect("serialize topology guidance");

    assert_eq!(guidance["authorities"][0]["kind"], "derived_reference");
    assert!(guidance["solver_encoding"]["formula"]
        .as_array()
        .is_some_and(|items| items
            .iter()
            .any(|item| item == "sum count(domain) = workload.replicas")));
    assert!(guidance["llm_encoding"]["anti_patterns"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item
            .as_str()
            .is_some_and(|item| item.contains("scheduler global-min")))));
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
