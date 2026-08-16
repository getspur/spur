use std::sync::Arc;

use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::{mcp::SolverMcpModule, service::SolverService};

fn context() -> ToolCallContext<'static> {
    ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None)
}

fn registry() -> ToolRegistry {
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

fn accessibility_request(width: Value, unknowns: Value) -> Value {
    json!({
        "family": "accessibility",
        "mode": if unknowns.as_array().is_some_and(Vec::is_empty) { "verify" } else { "synthesize" },
        "rules": [
            {"rule_id": "a11y.target_size", "subjects": ["save"], "parameters": {}},
            {"rule_id": "a11y.text_contrast", "subjects": ["label"], "parameters": {}}
        ],
        "scene": {
            "viewport": {"width": 320, "height": 568},
            "elements": {
                "save": {"rect": {"x": 0, "y": 0, "width": width, "height": 24}},
                "label": {"foreground_luminance": 17500, "background_luminance": 0}
            }
        },
        "unknowns": unknowns,
        "timeout_ms": 30000
    })
}

fn policy_request() -> Value {
    json!({
        "family": "policy",
        "mode": "verify",
        "rules": [
            {"rule_id": "rbac.permission_reachable", "subjects": ["alice", "read"]},
            {"rule_id": "rbac.role_hierarchy_acyclic", "subjects": []},
            {
                "rule_id": "rbac.static_separation_of_duty",
                "subjects": ["alice"],
                "parameters": {"roles": ["admin", "auditor"], "max_assigned": 1}
            },
            {
                "rule_id": "rbac.dynamic_separation_of_duty",
                "subjects": ["alice-session"],
                "parameters": {"roles": ["admin", "auditor"], "max_active": 1}
            }
        ],
        "facts": {
            "roles": {
                "admin": {"inherits": ["viewer"], "permissions": ["write"]},
                "viewer": {"inherits": [], "permissions": ["read"]},
                "auditor": {"inherits": [], "permissions": ["audit"]}
            },
            "principals": {"alice": {"roles": ["admin"]}},
            "sessions": {
                "alice-session": {"principal": "alice", "active_roles": ["admin"]}
            }
        },
        "unknowns": [],
        "timeout_ms": 30000
    })
}

fn resource_request(replicas: Value, unknowns: Value) -> Value {
    json!({
        "family": "resource",
        "mode": if unknowns.as_array().is_some_and(Vec::is_empty) { "verify" } else { "synthesize" },
        "rules": [
            {"rule_id": "resource.request_within_limit", "subjects": ["api"]},
            {
                "rule_id": "resource.aggregate_capacity",
                "subjects": ["cluster", "api"],
                "parameters": {"resources": ["cpu"]}
            },
            {
                "rule_id": "resource.quota_capacity",
                "subjects": ["team", "api"],
                "parameters": {"resources": ["cpu"]}
            },
            {
                "rule_id": "placement.topology_max_skew",
                "subjects": ["api"],
                "parameters": {"max_skew": 1}
            },
            {
                "rule_id": "placement.minimum_failure_domains",
                "subjects": ["api"],
                "parameters": {"minimum_domains": 2}
            }
        ],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": replicas,
                    "requests": {"cpu": 500},
                    "limits": {"cpu": 1000},
                    "domain_counts": {"zone-a": 2, "zone-b": 1}
                }
            },
            "pools": {"cluster": {"resources": {"cpu": 1500}}},
            "quotas": {"team": {"resources": {"cpu": 1500}}}
        },
        "unknowns": unknowns,
        "timeout_ms": 30000
    })
}

#[tokio::test]
async fn accessibility_verifies_boundaries_attributes_failures_and_synthesizes() {
    let registry = registry();

    let exact = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                accessibility_request(json!(24), json!([])),
            )
            .await,
    );
    assert_eq!(exact["status"], "sat");
    assert_eq!(exact["outcome"], "pass");

    let too_small = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                accessibility_request(json!(23), json!([])),
            )
            .await,
    );
    assert_eq!(too_small["status"], "unsat");
    assert_eq!(too_small["outcome"], "fail");
    assert_eq!(too_small["rule_results"][0]["rule_id"], "a11y.target_size");
    assert_eq!(too_small["rule_results"][0]["outcome"], "fail");
    assert_eq!(too_small["rule_results"][1]["outcome"], "pass");

    let solution = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                accessibility_request(
                    Value::Null,
                    json!([{"subject": "save", "field": "width", "min": 1, "max": 30}]),
                ),
            )
            .await,
    );
    assert_eq!(solution["status"], "sat");
    assert!(solution["assignments"][0]["value"]
        .as_i64()
        .is_some_and(|width| (24..=30).contains(&width)));
    assert_eq!(solution["assignments"][0]["node"], "save");
    assert_eq!(solution["assignments"][0]["field"], "width");
}

#[tokio::test]
async fn accessibility_requires_evidence_for_typed_exceptions() {
    let registry = registry();
    let accepted = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                json!({
                    "family": "accessibility",
                    "mode": "verify",
                    "rules": [{
                        "rule_id": "a11y.target_size",
                        "subjects": ["inline-link"],
                        "parameters": {"exception": {"kind": "inline", "evidence": "wcag-review:link-17"}}
                    }],
                    "scene": {
                        "viewport": {"width": 320, "height": 568},
                        "elements": {
                            "inline-link": {"rect": {"x": 0, "y": 0, "width": 12, "height": 16}}
                        }
                    },
                    "unknowns": []
                }),
            )
            .await,
    );
    assert_eq!(accepted["status"], "sat");
    assert_eq!(accepted["outcome"], "pass");

    let response = registry
        .call_json_tool(
            context(),
            "solve_rules",
            json!({
                "family": "accessibility",
                "mode": "verify",
                "rules": [{
                    "rule_id": "a11y.target_size",
                    "subjects": ["inline-link"],
                    "parameters": {"exception": {"kind": "inline", "evidence": ""}}
                }],
                "scene": {
                    "viewport": {"width": 320, "height": 568},
                    "elements": {
                        "inline-link": {"rect": {"x": 0, "y": 0, "width": 12, "height": 16}}
                    }
                },
                "unknowns": []
            }),
        )
        .await;
    let error = response.error.expect("empty evidence must be rejected");
    assert_eq!(error.code, -32602);
    assert!(error
        .message
        .contains("exception evidence must not be empty"));
}

#[tokio::test]
async fn advisory_unavailable_rules_cannot_masquerade_as_solver_proof() {
    let response = registry()
        .call_json_tool(
            context(),
            "solve_rules",
            json!({
                "family": "policy",
                "mode": "verify",
                "rules": [{"rule_id": "rbac.minimum_privilege", "subjects": ["alice"]}],
                "facts": {
                    "roles": {},
                    "principals": {"alice": {"roles": []}},
                    "sessions": {}
                },
                "unknowns": []
            }),
        )
        .await;
    let error = response.error.expect("unavailable rule must be rejected");
    assert_eq!(error.code, -32602);
    assert!(error
        .message
        .contains("unsupported policy rule `rbac.minimum_privilege`"));
}

#[tokio::test]
async fn policy_verifies_rbac_rules_rejects_cycles_and_synthesizes_membership() {
    let registry = registry();
    let valid = result_json(
        &registry
            .call_json_tool(context(), "solve_rules", policy_request())
            .await,
    );
    assert_eq!(valid["status"], "sat");
    assert_eq!(valid["outcome"], "pass");

    let mut cyclic = policy_request();
    cyclic["facts"]["roles"]["viewer"]["inherits"] = json!(["admin"]);
    let invalid = result_json(
        &registry
            .call_json_tool(context(), "solve_rules", cyclic)
            .await,
    );
    assert_eq!(invalid["status"], "unsat");
    assert_eq!(invalid["outcome"], "fail");
    assert_eq!(
        invalid["rule_results"][1]["rule_id"],
        "rbac.role_hierarchy_acyclic"
    );
    assert_eq!(invalid["rule_results"][1]["outcome"], "fail");

    let synthesized = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                json!({
                    "family": "policy",
                    "mode": "synthesize",
                    "rules": [{"rule_id": "rbac.permission_reachable", "subjects": ["alice", "write"]}],
                    "facts": {
                        "roles": {"writer": {"inherits": [], "permissions": ["write"]}},
                        "principals": {"alice": {"roles": []}},
                        "sessions": {}
                    },
                    "unknowns": [{"kind": "principal_role", "principal": "alice", "role": "writer"}]
                }),
            )
            .await,
    );
    assert_eq!(synthesized["status"], "sat");
    assert_eq!(synthesized["assignments"][0]["node"], "alice");
    assert_eq!(synthesized["assignments"][0]["field"], "roles.writer");
    assert_eq!(synthesized["assignments"][0]["value"], 1);
}

#[tokio::test]
async fn resource_verifies_capacity_and_placement_and_synthesizes_replicas() {
    let registry = registry();
    let exact = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                resource_request(json!(3), json!([])),
            )
            .await,
    );
    assert_eq!(exact["status"], "sat");
    assert_eq!(exact["outcome"], "pass");

    let overflow = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                resource_request(json!(4), json!([])),
            )
            .await,
    );
    assert_eq!(overflow["status"], "unsat");
    assert_eq!(overflow["outcome"], "fail");
    assert_eq!(
        overflow["rule_results"][1]["rule_id"],
        "resource.aggregate_capacity"
    );
    assert_eq!(overflow["rule_results"][1]["outcome"], "fail");
    assert_eq!(overflow["rule_results"][2]["outcome"], "fail");

    let solution = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                resource_request(
                    Value::Null,
                    json!([{"subject": "api", "field": "replicas", "min": 1, "max": 4}]),
                ),
            )
            .await,
    );
    assert_eq!(solution["status"], "sat");
    assert!(solution["assignments"][0]["value"]
        .as_i64()
        .is_some_and(|replicas| (1..=3).contains(&replicas)));
}
