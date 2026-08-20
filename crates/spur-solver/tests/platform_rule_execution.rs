use std::sync::Arc;

use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::{
    mcp::SolverMcpModule,
    rules::{
        execute::prepare, manifest::manifest_conformance_vectors,
        manifest_format::ConformanceVectorV1,
    },
    service::SolverService,
};

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

fn conformance_request(rule_id: &str, valid: bool) -> Value {
    let vectors = manifest_conformance_vectors(rule_id)
        .unwrap_or_else(|| panic!("missing conformance vectors for `{rule_id}`"));
    let cases: &[ConformanceVectorV1] = if valid {
        &vectors.valid
    } else {
        &vectors.invalid
    };
    cases
        .first()
        .unwrap_or_else(|| panic!("missing conformance case for `{rule_id}`"))
        .request
        .clone()
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

fn multi_subject_resource_request(rule_id: &str, parameters: Value) -> Value {
    json!({
        "family": "resource",
        "mode": "verify",
        "rules": [{
            "rule_id": rule_id,
            "subjects": ["good", "bad"],
            "parameters": parameters
        }],
        "facts": {
            "workloads": {
                "good": {
                    "replicas": 2,
                    "requests": {"cpu": 500},
                    "limits": {"cpu": 500},
                    "domain_counts": {"zone-a": 1, "zone-b": 1}
                },
                "bad": {
                    "replicas": 2,
                    "requests": {"cpu": 501},
                    "limits": {"cpu": 500},
                    "domain_counts": {"zone-a": 2, "zone-b": 0}
                }
            },
            "pools": {},
            "quotas": {}
        },
        "unknowns": [],
        "timeout_ms": 30000
    })
}

#[tokio::test]
async fn generic_family_legacy_accessibility_wire_behavior_is_unchanged() {
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

#[test]
fn generic_family_prepare_rejects_cross_family_rule_ownership() {
    let error = prepare(json!({
        "family": "configuration",
        "mode": "verify",
        "rules": [{
            "rule_id": "scheduling.assignment_exactly_once",
            "subjects": ["job"],
            "parameters": {}
        }],
        "facts": {
            "components": {},
            "selection_groups": {},
            "allowed_attribute_pairs": [],
            "version_orderings": {}
        },
        "unknowns": []
    }))
    .expect_err("a rule owned by another family must be rejected before solving");

    assert!(error
        .to_string()
        .contains("unsupported configuration rule `scheduling.assignment_exactly_once`"));
}

#[tokio::test]
async fn generic_family_configuration_verifies_and_projects_declared_synthesis_unknowns() {
    let registry = registry();
    let valid = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                conformance_request("configuration.requires_any", true),
            )
            .await,
    );
    assert_eq!(valid["status"], "sat");
    assert_eq!(valid["outcome"], "pass");

    let invalid = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                conformance_request("configuration.requires_any", false),
            )
            .await,
    );
    assert_eq!(invalid["status"], "unsat");
    assert_eq!(invalid["outcome"], "fail");
    assert_eq!(
        invalid["rule_results"][0]["rule_id"],
        "configuration.requires_any"
    );
    assert_eq!(
        invalid["rule_results"][0]["diagnostic"],
        "configuration.requires_any.violation"
    );

    let synthesized = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                json!({
                    "family": "configuration",
                    "mode": "synthesize",
                    "rules": [{
                        "rule_id": "configuration.requires_any",
                        "subjects": ["application", "postgres", "sqlite"],
                        "parameters": {}
                    }],
                    "facts": {
                        "components": {
                            "application": {"selected": true, "attributes": {}},
                            "postgres": {"selected": null, "attributes": {}},
                            "sqlite": {"selected": null, "attributes": {}}
                        },
                        "selection_groups": {},
                        "allowed_attribute_pairs": [],
                        "version_orderings": {}
                    },
                    "unknowns": [
                        {"kind": "component_selected", "component": "sqlite"},
                        {"kind": "component_selected", "component": "postgres"}
                    ]
                }),
            )
            .await,
    );
    assert_eq!(synthesized["status"], "sat");
    assert_eq!(synthesized["outcome"], "solution");
    let assignments = synthesized["assignments"]
        .as_array()
        .expect("configuration assignments");
    assert_eq!(
        assignments
            .iter()
            .map(|assignment| assignment["field"].as_str().expect("assignment field"))
            .collect::<Vec<_>>(),
        ["components.postgres.selected", "components.sqlite.selected"]
    );
    assert!(assignments
        .iter()
        .any(|assignment| assignment["value"] == true));
}

#[tokio::test]
async fn generic_family_scheduling_reports_complete_optimum_and_hard_bound_failure() {
    let registry = registry();
    let optimum = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                conformance_request("scheduling.minimize_makespan", true),
            )
            .await,
    );
    assert_eq!(optimum["status"], "sat");
    assert_eq!(optimum["outcome"], "solution");
    assert_eq!(optimum["optimization"]["termination"], "complete");
    assert_eq!(
        optimum["optimization"]["solutions"][0]["objectives"][0]["value"],
        4
    );
    assert_eq!(
        optimum["optimization"]["solutions"][0]["objectives"][0]["bound"],
        json!({"kind": "finite", "exact": "4"})
    );
    assert!(optimum["assignments"]
        .as_array()
        .expect("decoded scheduling assignments")
        .iter()
        .any(|assignment| {
            assignment["node"] == "schedule"
                && assignment["field"] == "makespan"
                && assignment["value"] == 4
        }));

    let bounded = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                conformance_request("scheduling.minimize_makespan", false),
            )
            .await,
    );
    assert_eq!(bounded["status"], "unsat");
    assert_eq!(bounded["outcome"], "fail");
    assert_eq!(
        bounded["rule_results"][0]["diagnostic"],
        "scheduling.minimize_makespan.violation"
    );
}

#[tokio::test]
async fn generic_family_workflow_preserves_verification_and_bounded_witness_semantics() {
    let registry = registry();
    let legal = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                conformance_request("workflow.transition_allowed", true),
            )
            .await,
    );
    assert_eq!(legal["status"], "sat");
    assert_eq!(legal["outcome"], "pass");

    let illegal = result_json(
        &registry
            .call_json_tool(
                context(),
                "solve_rules",
                conformance_request("workflow.transition_allowed", false),
            )
            .await,
    );
    assert_eq!(illegal["status"], "unsat");
    assert_eq!(illegal["outcome"], "fail");
    assert_eq!(
        illegal["rule_results"][0]["diagnostic"],
        "workflow.transition_allowed.violation"
    );

    let witness_request = conformance_request("workflow.bounded_reachability", true);
    assert_eq!(witness_request["rules"][2]["parameters"]["bound"], 2);
    let witness = result_json(
        &registry
            .call_json_tool(context(), "solve_rules", witness_request)
            .await,
    );
    assert_eq!(witness["status"], "sat");
    assert_eq!(witness["outcome"], "solution");
    assert!(witness["assignments"]
        .as_array()
        .expect("bounded workflow witness")
        .iter()
        .any(|assignment| {
            assignment["field"] == "traces.unsafe_witness.states[2]"
                && assignment["value"] == "Rejected"
        }));

    let bounded_unsat_request = conformance_request("workflow.bounded_reachability", false);
    assert_eq!(
        bounded_unsat_request["rules"][2]["parameters"]["bound"], 1,
        "the negative result is scoped only to the declared bound"
    );
    let bounded_unsat = result_json(
        &registry
            .call_json_tool(context(), "solve_rules", bounded_unsat_request)
            .await,
    );
    assert_eq!(bounded_unsat["status"], "unsat");
    assert_eq!(bounded_unsat["outcome"], "infeasible");
    assert!(bounded_unsat.get("assignments").is_none());
    assert!(bounded_unsat.get("rule_results").is_none());
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

#[tokio::test]
async fn resource_rules_apply_to_every_declared_subject() {
    let registry = registry();
    for (rule_id, parameters) in [
        (
            "resource.request_within_limit",
            json!({"resources": ["cpu"]}),
        ),
        ("placement.topology_max_skew", json!({"max_skew": 1})),
        (
            "placement.minimum_failure_domains",
            json!({"minimum_domains": 2}),
        ),
    ] {
        let result = result_json(
            &registry
                .call_json_tool(
                    context(),
                    "solve_rules",
                    multi_subject_resource_request(rule_id, parameters),
                )
                .await,
        );
        assert_eq!(result["status"], "unsat", "{rule_id}");
        assert_eq!(result["outcome"], "fail", "{rule_id}");
        assert_eq!(result["rule_results"][0]["status"], "unsat", "{rule_id}");
    }
}
