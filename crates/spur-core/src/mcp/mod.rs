pub(crate) mod catalog;
pub mod context_service;
pub mod delegation;
pub mod plan;
pub mod signals;
pub mod worker;

pub use context_service::ContextServiceClient;
use spur_acp::config::ContextServiceConfig;

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
    "submit_loop",
    "execute_epic",
    "get_reconciler_status",
    "get_loop_status",
    "pause_loop",
    "resume_loop",
    "kill_loop",
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
    plan_deps: plan::PlanMcpDeps,
    signal_deps: signals::SignalMcpDeps,
    context_service_config: &ContextServiceConfig,
) -> Result<spur_mcp::ToolRegistry, spur_mcp::ToolRegistryError> {
    let mut builder = spur_mcp::ToolRegistry::builder()
        .with(delegation::DelegationMcpModule::new(delegation_deps))?
        .with(catalog::ServerCatalogMcpModule::prelude())?
        .with(plan::PlanMcpModule::management(plan_deps.clone()))?
        .with(catalog::ServerCatalogMcpModule::remainder())?
        .with(plan::PlanMcpModule::remainder(plan_deps))?
        .with(signals::SignalMcpModule::new(signal_deps))?
        .with_alias("code_search", "code_symbol_search")?;
    if let Some(context_service_client) = context_service_client(context_service_config) {
        builder = builder.with(context_service_client)?;
    }
    Ok(builder.build())
}

fn context_service_client(
    config: &ContextServiceConfig,
) -> Option<context_service::ContextServiceClient> {
    let base_url = std::env::var("SPUR_CONTEXT_SERVICE_URL")
        .ok()
        .and_then(non_empty_trimmed)
        .or_else(|| non_empty_trimmed(config.url.clone()))?;
    let bearer_token = std::env::var("SPUR_CONTEXT_SERVICE_TOKEN")
        .ok()
        .and_then(non_empty_trimmed)
        .or_else(|| config.token.clone().and_then(non_empty_trimmed));
    Some(context_service::ContextServiceClient::with_optional_token(
        base_url,
        bearer_token,
    ))
}

fn non_empty_trimmed(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

pub fn catalog_tool_registry() -> Result<spur_mcp::ToolRegistry, spur_mcp::ToolRegistryError> {
    spur_mcp::ToolRegistry::builder()
        .with(delegation::DelegationMcpModule::new(
            delegation::DelegationMcpDeps::catalog_only(),
        ))?
        .with(catalog::ServerCatalogMcpModule::prelude())?
        .with(plan::PlanMcpModule::management(
            plan::PlanMcpDeps::catalog_only(),
        ))?
        .with(catalog::ServerCatalogMcpModule::remainder())?
        .with(plan::PlanMcpModule::remainder(
            plan::PlanMcpDeps::catalog_only(),
        ))?
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
        .with(catalog::WorkerCatalogMcpModule::prelude())?
        .with(worker::WorkerReadMcpModule::plan(
            worker::WorkerReadMcpDeps::catalog_only(),
        ))?
        .with(catalog::WorkerCatalogMcpModule::remainder())?
        .with(worker::WorkerReadMcpModule::artifact(
            worker::WorkerReadMcpDeps::catalog_only(),
        ))?
        .with(signals::SignalMcpModule::new(signals::SignalMcpDeps {
            pm_service: None,
            event_sink: None,
            feature_gate: crate::server::community_feature_gate(),
        }))?
        .with_alias("code_search", "code_symbol_search")?
        .with_denied_tool_calls(WORKER_DENIED_TOOL_CALLS.iter().copied());
    Ok(builder.build())
}

pub fn worker_tools_list() -> Vec<spur_mcp::ToolDefinition> {
    worker_tool_registry()
        .expect("core worker MCP tool registry must be valid")
        .list_tools()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;
    use serde_json::json;
    use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext};

    #[tokio::test]
    async fn worker_registry_dispatches_allowed_read_tools_through_core_module() {
        let registry = worker_tool_registry().expect("worker registry");

        for (tool_name, args) in [
            ("get_plan_status", json!({})),
            ("get_task_diff", json!({})),
            ("fetch_outcome_artifact", json!({})),
        ] {
            let ctx = ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None);
            let err = match registry.call_tool(ctx, tool_name, args).await {
                Ok(_) => panic!("{tool_name} should reject missing required arguments"),
                Err(err) => err,
            };

            assert_eq!(
                err.code,
                ErrorCode(-32602),
                "{tool_name} should reach its real worker read handler"
            );
            assert!(
                !err.message.contains("must be dispatched"),
                "{tool_name} must not be a catalog-only placeholder: {}",
                err.message
            );
        }
    }

    #[tokio::test]
    async fn worker_registry_denies_brain_only_plan_tools_with_authorization_error() {
        let registry = worker_tool_registry().expect("worker registry");
        let listed: Vec<String> = registry
            .list_tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect();

        for tool_name in ["submit_plan", "review_task", "submit_plan_mutation"] {
            assert!(
                !listed.iter().any(|name| name == tool_name),
                "{tool_name} must not be advertised to workers"
            );

            let ctx = ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None);
            let err = match registry.call_tool(ctx, tool_name, json!({})).await {
                Ok(_) => panic!("brain-only worker call must be rejected: {tool_name}"),
                Err(err) => err,
            };
            assert_eq!(
                err.code,
                ErrorCode(-32001),
                "{tool_name} should fail authorization, not tool lookup"
            );
            assert!(
                err.message.contains("not authorized"),
                "{tool_name} denial should explain authorization: {}",
                err.message
            );
        }
    }
}
