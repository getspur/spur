pub(crate) mod catalog;
pub mod delegation;
pub mod plan;
pub mod signals;

const WORKER_DENIED_TOOL_CALLS: &[&str] = &[
    "delegate_to_worker",
    "delegate_parallel",
    "check_delegation_status",
    "cancel_delegation",
    "list_available_workers",
    "update_issue",
    "create_issue",
    "add_dependency",
    "create_pr",
    "merge_plan",
    "resume_plan",
    "force_reclaim_plan",
    "submit_plan",
    "execute_epic",
    "get_reconciler_status",
    "preview_task_base",
    "plan_truncate_and_restart",
    "recover_orphaned_dispatch",
    "review_task",
    "submit_plan_mutation",
    "graph_triage",
    "graph_plan",
    "graph_insights",
    "graph_alerts",
    "graph_subgraph",
];

pub fn brain_tool_registry(
    delegation_deps: delegation::DelegationMcpDeps,
    signal_deps: signals::SignalMcpDeps,
) -> Result<spur_mcp::ToolRegistry, spur_mcp::ToolRegistryError> {
    let builder = spur_mcp::ToolRegistry::builder()
        .with(delegation::DelegationMcpModule::new(delegation_deps))?
        .with(catalog::ServerCatalogMcpModule)?
        .with(signals::SignalMcpModule::new(signal_deps))?
        .with_alias("code_search", "code_symbol_search")?;
    Ok(builder.build())
}

pub fn catalog_tool_registry() -> Result<spur_mcp::ToolRegistry, spur_mcp::ToolRegistryError> {
    spur_mcp::ToolRegistry::builder()
        .with(delegation::DelegationMcpModule::new(
            delegation::DelegationMcpDeps::catalog_only(),
        ))?
        .with(catalog::ServerCatalogMcpModule)?
        .with(signals::SignalMcpModule::new(signals::SignalMcpDeps {
            pm_service: None,
            event_sink: None,
            feature_gate: crate::server::community_feature_gate(),
        }))?
        .with_alias("code_search", "code_symbol_search")
        .map(spur_mcp::registry::ToolRegistryBuilder::build)
}

pub fn tools_list() -> Vec<spur_mcp::ToolDefinition> {
    catalog_tool_registry()
        .expect("core MCP tool registry must be valid")
        .list_tools()
}

pub fn worker_tool_registry() -> Result<spur_mcp::ToolRegistry, spur_mcp::ToolRegistryError> {
    let builder = spur_mcp::ToolRegistry::builder()
        .with(catalog::WorkerCatalogMcpModule)?
        .with_alias("code_search", "code_symbol_search")?
        .with_denied_tool_calls(WORKER_DENIED_TOOL_CALLS.iter().copied());
    Ok(builder.build())
}

pub fn worker_tools_list() -> Vec<spur_mcp::ToolDefinition> {
    worker_tool_registry()
        .expect("core worker MCP tool registry must be valid")
        .list_tools()
}
