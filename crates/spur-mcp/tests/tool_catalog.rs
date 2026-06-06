//! Tool catalog snapshot test.
//!
//! Guards INV-1 from the T1 contract-truthfulness spec: the set of tool
//! names exposed via `tools/list` must not drift silently. Any addition
//! or removal requires updating the `EXPECTED` list in this test in the
//! same commit.

use spur_mcp::tools::worker_tools_list;
use spur_mcp::{tools_list, ToolDefinition};

const EXPECTED: &[&str] = &[
    "delegate_to_worker",
    "delegate_parallel",
    "check_delegation_status",
    "fetch_outcome_artifact",
    "cancel_delegation",
    "list_available_workers",
    "get_issue",
    "list_issues",
    "update_issue",
    "create_issue",
    "add_dependency",
    "create_pr",
    "merge_plan",
    "resume_plan",
    "force_reclaim_plan",
    "graph_triage",
    "graph_plan",
    "graph_insights",
    "graph_alerts",
    "graph_subgraph",
    "code_resolve",
    "code_file_symbols",
    "code_symbol_info",
    "code_read_symbol",
    "code_callers",
    "code_callees",
    "code_symbol_search",
    "code_subgraph",
    "code_symbol_history",
    "doc_navigate",
    "submit_plan",
    "execute_epic",
    "get_plan_status",
    "get_reconciler_status",
    "get_task_diff",
    "preview_task_base",
    "plan_truncate_and_restart",
    "recover_orphaned_dispatch",
    "review_task",
    "submit_plan_mutation",
    "report_signal",
    "report_progress",
];

#[test]
fn tool_catalog_matches_expected() {
    let actual: Vec<String> = tools_list().iter().map(|t| t.name.clone()).collect();
    let expected: Vec<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        actual, expected,
        "tool_catalog drift detected; update EXPECTED in tests/tool_catalog.rs if intentional",
    );
}

#[test]
fn code_graph_tools_advertise_response_format_in_catalogs() {
    for (catalog_name, tools) in [
        ("tools_list", tools_list()),
        ("worker_tools_list", worker_tools_list()),
    ] {
        for tool_name in [
            "code_file_symbols",
            "code_callers",
            "code_callees",
            "code_subgraph",
        ] {
            assert_response_format_enum(
                catalog_name,
                &tools,
                tool_name,
                &["full", "compact", "table"],
            );
        }

        assert_response_format_enum(
            catalog_name,
            &tools,
            "code_read_symbol",
            &["full", "compact", "source"],
        );
    }
}

fn assert_response_format_enum(
    catalog_name: &str,
    tools: &[ToolDefinition],
    tool_name: &str,
    expected: &[&str],
) {
    let tool = tools
        .iter()
        .find(|tool| tool.name == tool_name)
        .unwrap_or_else(|| panic!("{catalog_name} missing {tool_name}"));
    let schema = &tool.input_schema["properties"]["response_format"];
    assert!(
        schema.is_object(),
        "{catalog_name}.{tool_name} must define response_format in input schema: {}",
        tool.input_schema
    );
    assert_eq!(
        schema["type"], "string",
        "{catalog_name}.{tool_name}.response_format must be a string schema"
    );
    let actual = schema["enum"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("{catalog_name}.{tool_name}.response_format must define enum values")
        })
        .iter()
        .map(|value| value.as_str().expect("enum entries are strings"))
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "{catalog_name}.{tool_name}.response_format enum drift"
    );
    assert!(
        schema["description"]
            .as_str()
            .is_some_and(|description| description.contains("Output shape")),
        "{catalog_name}.{tool_name}.response_format should explain the output shape"
    );
}
