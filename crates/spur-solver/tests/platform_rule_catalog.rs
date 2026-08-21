use std::collections::BTreeSet;

use serde_json::{json, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::{
    mcp::SolverMcpModule,
    rules::{builtin_registry, families, manifest_executable_rule_ids},
};

const EXPECTED_FAMILY_IDS: [&str; 8] = [
    "accessibility",
    "configuration",
    "data_integrity",
    "design",
    "policy",
    "resource",
    "scheduling",
    "workflow",
];

const EXPECTED_EXECUTABLE_RULE_IDS: [&str; 39] = [
    "a11y.focus_not_obscured",
    "a11y.reflow",
    "a11y.target_size",
    "a11y.text_contrast",
    "configuration.attribute_allowed_pair",
    "configuration.excludes",
    "configuration.requires_any",
    "configuration.selection_cardinality",
    "configuration.version_interval",
    "data_integrity.aggregate_balance",
    "data_integrity.cardinality",
    "data_integrity.conditional_required",
    "data_integrity.foreign_key",
    "data_integrity.mutually_consistent",
    "data_integrity.temporal_consistency",
    "data_integrity.unique",
    "data_integrity.value_range",
    "layout.axis_capacity",
    "layout.containment",
    "layout.non_overlap",
    "media.aspect_ratio",
    "placement.minimum_failure_domains",
    "placement.topology_max_skew",
    "rbac.dynamic_separation_of_duty",
    "rbac.permission_reachable",
    "rbac.role_hierarchy_acyclic",
    "rbac.static_separation_of_duty",
    "resource.aggregate_capacity",
    "resource.quota_capacity",
    "resource.request_within_limit",
    "scheduling.assignment_exactly_once",
    "scheduling.cumulative_capacity",
    "scheduling.minimize_makespan",
    "scheduling.placement_allowed",
    "scheduling.precedence_finish_start",
    "workflow.bounded_reachability",
    "workflow.initial_state_allowed",
    "workflow.safety_invariant",
    "workflow.transition_allowed",
];

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
fn generic_family_registry_composes_all_platform_families_in_stable_order() {
    assert_eq!(
        builtin_registry()
            .families()
            .iter()
            .map(|family| family.id())
            .collect::<Vec<_>>(),
        EXPECTED_FAMILY_IDS
    );
    assert_eq!(
        families::compilers()
            .iter()
            .map(|compiler| compiler.id())
            .collect::<Vec<_>>(),
        EXPECTED_FAMILY_IDS,
        "every discoverable family must have one executable compiler in the same stable order"
    );

    assert_eq!(
        manifest_executable_rule_ids()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        EXPECTED_EXECUTABLE_RULE_IDS
    );
    assert!(!manifest_executable_rule_ids()
        .iter()
        .any(|rule_id| rule_id == "rbac.minimum_privilege"));

    for rule_id in EXPECTED_EXECUTABLE_RULE_IDS {
        assert!(
            builtin_registry().rule(rule_id).is_some(),
            "missing implemented rule {rule_id}"
        );
    }
}

#[test]
fn generic_family_solve_rules_schema_has_exact_global_enums() {
    let tools = spur_solver::mcp::tool_definitions();
    let schema = &tools
        .iter()
        .find(|tool| tool.name == "solve_rules")
        .expect("solve_rules tool definition")
        .input_schema;

    assert_eq!(
        schema["properties"]["family"]["enum"],
        json!(EXPECTED_FAMILY_IDS)
    );
    assert_eq!(
        schema["properties"]["rules"]["items"]["properties"]["rule_id"]["enum"],
        json!(EXPECTED_EXECUTABLE_RULE_IDS.as_slice())
    );
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["family", "mode", "rules"]));
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn generic_family_bedrock_schema_remains_a_simple_object_without_unions() {
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
async fn generic_family_rule_spec_progressively_discovers_all_rules() {
    let list = result_json(
        &registry()
            .call_json_tool(context(), "solve_rule_spec", json!({}))
            .await,
    );
    assert_eq!(
        list["families"]
            .as_array()
            .expect("family cards")
            .iter()
            .map(|family| family["id"].as_str().expect("family ID"))
            .collect::<Vec<_>>(),
        EXPECTED_FAMILY_IDS
    );

    let mut discovered_executable_rule_ids = BTreeSet::new();
    let mut discovered_advisory_rule_ids = BTreeSet::new();
    for family_id in EXPECTED_FAMILY_IDS {
        let family = result_json(
            &registry()
                .call_json_tool(context(), "solve_rule_spec", json!({"family": family_id}))
                .await,
        );
        assert_eq!(family["family"]["id"], family_id);
        for profile in family["profiles"].as_array().expect("family profiles") {
            let profile_id = profile["id"].as_str().expect("profile ID");
            let profile = result_json(
                &registry()
                    .call_json_tool(context(), "solve_rule_spec", json!({"profile": profile_id}))
                    .await,
            );
            for rule in profile["rules"].as_array().expect("profile rules") {
                assert_eq!(rule["family"], family_id);
                let rule_id = rule["id"].as_str().expect("discovered rule ID");
                let discovered_rule_ids = if rule["availability"] == "implemented" {
                    &mut discovered_executable_rule_ids
                } else {
                    &mut discovered_advisory_rule_ids
                };
                assert!(
                    discovered_rule_ids.insert(rule_id.to_owned()),
                    "rule IDs must appear in exactly one profile"
                );
            }
        }
    }
    assert_eq!(
        discovered_executable_rule_ids
            .into_iter()
            .collect::<Vec<_>>(),
        EXPECTED_EXECUTABLE_RULE_IDS
    );
    assert_eq!(
        discovered_advisory_rule_ids.into_iter().collect::<Vec<_>>(),
        ["rbac.minimum_privilege"]
    );

    for rule_id in EXPECTED_EXECUTABLE_RULE_IDS {
        let direct = result_json(
            &registry()
                .call_json_tool(context(), "solve_rule_spec", json!({"rule_id": rule_id}))
                .await,
        );
        assert_eq!(direct["rule"]["id"], rule_id);
        assert_eq!(direct["rule"]["availability"], "implemented");
    }

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
