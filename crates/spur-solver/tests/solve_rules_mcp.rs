use std::sync::Arc;

use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::{
    mcp::SolverMcpModule,
    rules::{execute::outcome_for, manifest_executable_rule_ids, RuleOutcome, RuleSolveMode},
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

fn mixed_verification_request() -> Value {
    json!({
        "family": "design",
        "mode": "verify",
        "rules": [
            {
                "rule_id": "layout.containment",
                "subjects": ["child", "panel"],
                "parameters": {"padding": 0}
            },
            {
                "rule_id": "media.aspect_ratio",
                "subjects": ["media"],
                "parameters": {"source_width": 16, "source_height": 9}
            }
        ],
        "scene": {
            "viewport": {"width": 390, "height": 844},
            "nodes": {
                "panel": {"rect": {"x": 0, "y": 0, "width": 320, "height": 200}},
                "child": {
                    "parent": "panel",
                    "rect": {"x": 300, "y": 16, "width": 44, "height": 44}
                },
                "media": {"rect": {"x": 0, "y": 240, "width": 320, "height": 180}}
            }
        },
        "unknowns": [],
        "timeout_ms": 30_000,
        "persist": false,
        "include_smt": false
    })
}

fn axis_capacity_request(axis: &str, second_extent: i64) -> Value {
    let (container_width, container_height) = (100, 100);
    let (first_width, first_height, second_width, second_height) = match axis {
        "horizontal" => (30, 1, second_extent, 1),
        "vertical" => (1, 30, 1, second_extent),
        other => panic!("unexpected test axis {other}"),
    };
    json!({
        "family": "design",
        "mode": "verify",
        "rules": [{
            "rule_id": "layout.axis_capacity",
            "subjects": ["container", "first", "second"],
            "parameters": {
                "axis": axis,
                "gap": 20,
                "inset_start": 10,
                "inset_end": 10
            }
        }],
        "scene": {
            "viewport": {"width": 100, "height": 100},
            "nodes": {
                "container": {"rect": {
                    "x": 0, "y": 0,
                    "width": container_width, "height": container_height
                }},
                "first": {"rect": {
                    "x": 0, "y": 0,
                    "width": first_width, "height": first_height
                }},
                "second": {"rect": {
                    "x": 0, "y": 0,
                    "width": second_width, "height": second_height
                }}
            }
        },
        "unknowns": [],
        "timeout_ms": 30_000,
        "persist": false,
        "include_smt": false
    })
}

#[test]
fn solve_rules_schema_keeps_one_bedrock_compatible_family_execution_tool() {
    let tools = spur_solver::mcp::tool_definitions();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "solve_rules")
        .expect("solve_rules tool definition");

    let schema = &tool.input_schema;
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["family", "mode", "rules"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["family"]["enum"],
        json!([
            "accessibility",
            "configuration",
            "data_integrity",
            "design",
            "policy",
            "resource",
            "scheduling",
            "workflow"
        ])
    );
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        json!(["verify", "synthesize"])
    );
    let rule_ids = schema["properties"]["rules"]["items"]["properties"]["rule_id"]["enum"]
        .as_array()
        .expect("platform rule ids")
        .iter()
        .map(|rule_id| rule_id.as_str().expect("string rule id"))
        .collect::<Vec<_>>();
    let manifest_rule_ids = manifest_executable_rule_ids()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();

    assert_eq!(rule_ids, manifest_rule_ids);
    assert_eq!(rule_ids.len(), 41);
    for expected_rule_id in manifest_executable_rule_ids() {
        assert_eq!(
            rule_ids
                .iter()
                .filter(|rule_id| **rule_id == expected_rule_id)
                .count(),
            1,
            "{expected_rule_id} must appear exactly once"
        );
    }
    assert!(rule_ids.contains(&"rbac.minimum_privilege"));
    assert!(rule_ids.contains(&"placement.minimize_skew"));
}

#[test]
fn every_solver_status_has_an_explicit_mode_specific_outcome() {
    for (status, verify, synthesize) in [
        (SolveStatus::Sat, RuleOutcome::Pass, RuleOutcome::Solution),
        (
            SolveStatus::Unsat,
            RuleOutcome::Fail,
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
    assert_eq!(valid["status"], "sat");
    assert_eq!(valid["outcome"], "pass");
    assert_eq!(valid["rule_results"][0]["rule_id"], "layout.containment");
    assert_eq!(valid["rule_results"][0]["binding_index"], 0);
    assert_eq!(valid["rule_results"][0]["status"], "sat");
    assert_eq!(valid["rule_results"][0]["outcome"], "pass");

    let invalid = result_json(
        &registry
            .call_json_tool(context(), "solve_rules", mixed_verification_request())
            .await,
    );
    assert_eq!(invalid["status"], "unsat");
    assert_eq!(invalid["outcome"], "fail");
    assert_eq!(invalid["rule_results"].as_array().map(Vec::len), Some(2));
    assert_eq!(invalid["rule_results"][0]["rule_id"], "layout.containment");
    assert_eq!(invalid["rule_results"][0]["status"], "unsat");
    assert_eq!(invalid["rule_results"][0]["outcome"], "fail");
    assert_eq!(invalid["rule_results"][1]["rule_id"], "media.aspect_ratio");
    assert_eq!(invalid["rule_results"][1]["status"], "sat");
    assert_eq!(invalid["rule_results"][1]["outcome"], "pass");

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

#[tokio::test]
async fn solve_rules_verifies_axis_capacity_exact_fit_and_one_unit_overflow() {
    let registry = live_registry();
    for axis in ["horizontal", "vertical"] {
        let exact = result_json(
            &registry
                .call_json_tool(context(), "solve_rules", axis_capacity_request(axis, 30))
                .await,
        );
        assert_eq!(exact["status"], "sat", "{axis} exact fit");
        assert_eq!(exact["outcome"], "pass", "{axis} exact fit");

        let overflow = result_json(
            &registry
                .call_json_tool(context(), "solve_rules", axis_capacity_request(axis, 31))
                .await,
        );
        assert_eq!(overflow["status"], "unsat", "{axis} overflow");
        assert_eq!(overflow["outcome"], "fail", "{axis} overflow");
        assert_eq!(
            overflow["rule_results"][0]["rule_id"],
            "layout.axis_capacity"
        );
    }
}
