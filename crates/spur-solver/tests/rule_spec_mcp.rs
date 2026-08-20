use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::mcp::SolverMcpModule;

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
fn rule_spec_schema_exposes_one_bounded_guide_tool() {
    let tools = spur_solver::mcp::tool_definitions();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "solve_rule_spec")
        .expect("solve_rule_spec tool definition");

    assert_eq!(tool.input_schema["type"], "object");
    assert_eq!(tool.input_schema["additionalProperties"], false);
    assert_eq!(
        tool.input_schema["properties"]["include"]["enum"],
        json!([
            "summary",
            "valid_example",
            "invalid_example",
            "llm_encoding",
            "solver_encoding",
            "all"
        ])
    );
    assert_eq!(
        tool.input_schema["properties"]["include"]["default"],
        "summary"
    );
    assert!(tool.input_schema.get("allOf").is_none());
    assert!(tool.input_schema["description"]
        .as_str()
        .is_some_and(|description| description.contains("at most one")));
}

#[tokio::test]
async fn empty_rule_spec_request_lists_all_bounded_family_cards_without_a_live_solver() {
    let response = registry()
        .call_json_tool(context(), "solve_rule_spec", json!({}))
        .await;
    let result = result_json(&response);

    assert_eq!(result["registry_schema_version"], 1);
    assert_eq!(result["query"]["selector"], "catalog");
    assert_eq!(result["query"]["include"], "summary");
    assert_eq!(
        result["families"]
            .as_array()
            .expect("family cards")
            .iter()
            .map(|family| family["id"].as_str().expect("family ID"))
            .collect::<Vec<_>>(),
        [
            "accessibility",
            "configuration",
            "design",
            "policy",
            "resource",
            "scheduling",
            "workflow",
        ]
    );
    assert_eq!(
        result["families"][2]["profiles"],
        json!(["geometric_integrity", "layout_capacity"])
    );
    assert!(result.get("rules").is_none());
    assert_eq!(
        result["next_tools"],
        json!(["solve_rule_spec", "solve_rules"])
    );
}

#[tokio::test]
async fn exact_rule_query_returns_requested_guidance_without_unrequested_sections() {
    let solver_response = registry()
        .call_json_tool(
            context(),
            "solve_rule_spec",
            json!({
                "rule_id": "media.aspect_ratio",
                "include": "solver_encoding"
            }),
        )
        .await;
    let solver_result = result_json(&solver_response);

    assert_eq!(solver_result["query"]["selector"], "rule_id");
    assert_eq!(solver_result["rule"]["id"], "media.aspect_ratio");
    assert_eq!(solver_result["rule"]["family"], "design");
    assert_eq!(solver_result["rule"]["profile"], "geometric_integrity");
    assert_eq!(solver_result["rule"]["availability"], "implemented");
    assert_eq!(solver_result["rule"]["solver_encoding"]["theory"], "QF_NIA");
    assert!(solver_result["rule"].get("authorities").is_some());
    assert!(solver_result["rule"].get("examples").is_none());
    assert!(solver_result["rule"].get("llm_encoding").is_none());

    let invalid_response = registry()
        .call_json_tool(
            context(),
            "solve_rule_spec",
            json!({
                "rule_id": "layout.containment",
                "include": "invalid_example"
            }),
        )
        .await;
    let invalid_result = result_json(&invalid_response);
    assert_eq!(
        invalid_result["rule"]["invalid_example"]["expected_diagnostic"],
        "design.outside_parent"
    );
    assert!(invalid_result["rule"].get("valid_example").is_none());
}

#[tokio::test]
async fn primitive_query_returns_stably_sorted_matching_rule_cards() {
    let response = registry()
        .call_json_tool(
            context(),
            "solve_rule_spec",
            json!({ "primitive": "disjoint" }),
        )
        .await;
    let result = result_json(&response);

    assert_eq!(result["query"]["selector"], "primitive");
    assert_eq!(result["rules"].as_array().map(Vec::len), Some(1));
    assert_eq!(result["rules"][0]["id"], "layout.non_overlap");
    assert!(result["rules"][0].get("solver_encoding").is_none());
}

#[tokio::test]
async fn rule_spec_rejects_ambiguous_and_unknown_selectors_as_invalid_params() {
    let ambiguous = registry()
        .call_json_tool(
            context(),
            "solve_rule_spec",
            json!({ "family": "design", "rule_id": "layout.containment" }),
        )
        .await;
    let ambiguous_error = ambiguous.error.expect("ambiguous selector error");
    assert_eq!(ambiguous_error.code, -32602);
    assert!(ambiguous_error.message.contains("at most one selector"));

    let unknown = registry()
        .call_json_tool(
            context(),
            "solve_rule_spec",
            json!({ "rule_id": "layout.missing" }),
        )
        .await;
    let unknown_error = unknown.error.expect("unknown selector error");
    assert_eq!(unknown_error.code, -32602);
    assert!(unknown_error
        .message
        .contains("unknown rule_id `layout.missing`"));
}
