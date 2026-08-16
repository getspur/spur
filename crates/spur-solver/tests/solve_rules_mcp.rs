use std::sync::Arc;

use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::{
    mcp::SolverMcpModule,
    rules::{execute::outcome_for, RuleOutcome, RuleSolveMode},
    service::SolverService,
    types::SolveStatus,
};

fn context() -> ToolCallContext<'static> {
    ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None)
}

fn live_registry() -> ToolRegistry {
    ToolRegistry::builder()
        .with(SolverMcpModule::new(Arc::new(SolverService::new())))
        .expect("register live solver module")
        .build()
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

fn containment_request(mode: &str, child_x: Value, unknowns: Value) -> Value {
    json!({
        "family": "design",
        "mode": mode,
        "rules": [{
            "rule_id": "layout.containment",
            "subjects": ["child", "panel"],
            "parameters": {"padding": 0}
        }],
        "scene": {
            "viewport": {"width": 390, "height": 844},
            "nodes": {
                "panel": {"rect": {"x": 0, "y": 0, "width": 320, "height": 200}},
                "child": {
                    "parent": "panel",
                    "rect": {"x": child_x, "y": 16, "width": 44, "height": 44}
                }
            }
        },
        "unknowns": unknowns,
        "timeout_ms": 30_000,
        "persist": false,
        "include_smt": false
    })
}

#[test]
fn solve_rules_schema_keeps_one_generic_family_execution_tool() {
    let tools = spur_solver::mcp::tool_definitions();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "solve_rules")
        .expect("solve_rules tool definition");

    assert_eq!(tool.input_schema["type"], "object");
    assert_eq!(
        tool.input_schema["required"],
        json!(["family", "mode", "rules", "scene"])
    );
    assert_eq!(tool.input_schema["additionalProperties"], false);
    assert_eq!(
        tool.input_schema["properties"]["family"]["enum"],
        json!(["design"])
    );
    assert_eq!(
        tool.input_schema["properties"]["mode"]["enum"],
        json!(["verify", "synthesize"])
    );
}

#[test]
fn every_solver_status_has_an_explicit_mode_specific_outcome() {
    for (status, verify, synthesize) in [
        (SolveStatus::Sat, RuleOutcome::Fail, RuleOutcome::Solution),
        (
            SolveStatus::Unsat,
            RuleOutcome::Pass,
            RuleOutcome::Infeasible,
        ),
        (
            SolveStatus::Unknown,
            RuleOutcome::Unknown,
            RuleOutcome::Unknown,
        ),
        (
            SolveStatus::Timeout,
            RuleOutcome::Timeout,
            RuleOutcome::Timeout,
        ),
        (SolveStatus::Error, RuleOutcome::Error, RuleOutcome::Error),
        (SolveStatus::Ended, RuleOutcome::Ended, RuleOutcome::Ended),
    ] {
        assert_eq!(outcome_for(RuleSolveMode::Verify, status), verify);
        assert_eq!(outcome_for(RuleSolveMode::Synthesize, status), synthesize);
    }
}

#[tokio::test]
async fn solve_rules_evaluates_verification_and_synthesis_with_z3() {
    let registry = live_registry();

    let valid = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                containment_request("verify", json!(16), json!([])),
            )
            .await,
    );
    assert_eq!(valid["family"], "design");
    assert_eq!(valid["mode"], "verify");
    assert_eq!(valid["status"], "unsat");
    assert_eq!(valid["outcome"], "pass");

    let invalid = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                containment_request("verify", json!(300), json!([])),
            )
            .await,
    );
    assert_eq!(invalid["status"], "sat");
    assert_eq!(invalid["outcome"], "fail");

    let solution = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                containment_request(
                    "synthesize",
                    Value::Null,
                    json!([{"node": "child", "field": "x", "min": 0, "max": 400}]),
                ),
            )
            .await,
    );
    assert_eq!(solution["status"], "sat");
    assert_eq!(solution["outcome"], "solution");
    assert_eq!(solution["assignments"].as_array().map(Vec::len), Some(1));
    assert_eq!(solution["assignments"][0]["node"], "child");
    assert_eq!(solution["assignments"][0]["field"], "x");
    assert!(solution["assignments"][0]["value"]
        .as_i64()
        .is_some_and(|x| (0..=276).contains(&x)));

    let infeasible = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                containment_request(
                    "synthesize",
                    Value::Null,
                    json!([{"node": "child", "field": "x", "min": 300, "max": 400}]),
                ),
            )
            .await,
    );
    assert_eq!(infeasible["status"], "unsat");
    assert_eq!(infeasible["outcome"], "infeasible");
}

#[tokio::test]
async fn solve_rules_rejects_unknown_families_and_invalid_design_facts() {
    let registry = live_registry();
    let unknown = registry
        .call_json_tool(
            context(),
            "solve_rules",
            json!({"family": "imaginary", "mode": "verify", "rules": [], "scene": {}}),
        )
        .await;
    let unknown_error = unknown.error.expect("unknown family error");
    assert_eq!(unknown_error.code, -32602);
    assert!(unknown_error
        .message
        .contains("unknown rule family `imaginary`"));

    let invalid = registry
        .call_json_tool(
            context(),
            "solve_rules",
            containment_request("verify", Value::Null, json!([])),
        )
        .await;
    let invalid_error = invalid.error.expect("missing geometry error");
    assert_eq!(invalid_error.code, -32602);
    assert!(invalid_error
        .message
        .contains("missing and has no unknown declaration"));
}
