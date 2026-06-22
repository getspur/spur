//! Infrastructure-owned `spur-mcp` catalog snapshot.
//!
//! The orchestration, PM, graph, analyst, delegation, signal, plan, and
//! worker-read catalogs are composed by `spur-core`. `spur-mcp` owns only the
//! registry and transport infrastructure, so its default catalogs must stay
//! empty after the core extraction.

const CORE_OWNED_TOOL_NAMES: &[&str] = &[
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
    "knowledge_context_pack",
    "knowledge_context_pack_2",
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
fn spur_mcp_default_brain_catalog_contains_only_infra_owned_tools() {
    let actual: Vec<String> = spur_mcp::tools_list()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    assert_eq!(
        actual,
        Vec::<String>::new(),
        "spur-mcp must not advertise core-owned brain tools from its default catalog",
    );
}

#[test]
fn spur_mcp_default_worker_catalog_contains_only_infra_owned_tools() {
    let actual: Vec<String> = spur_mcp::tools::worker_tools_list()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    assert_eq!(
        actual,
        Vec::<String>::new(),
        "spur-mcp must not advertise core-owned worker tools from its default catalog",
    );
}

#[test]
fn core_owned_tools_do_not_leak_back_into_spur_mcp_catalogs() {
    let brain_names: Vec<String> = spur_mcp::tools_list()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    let worker_names: Vec<String> = spur_mcp::tools::worker_tools_list()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    for tool_name in CORE_OWNED_TOOL_NAMES {
        assert!(
            !brain_names.iter().any(|name| name == tool_name),
            "{tool_name} must be owned by spur-core, not spur-mcp's brain catalog",
        );
        assert!(
            !worker_names.iter().any(|name| name == tool_name),
            "{tool_name} must be owned by spur-core, not spur-mcp's worker catalog",
        );
    }
}
