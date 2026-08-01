use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::json;
use spur_acp::config::ContextServiceConfig;
use spur_core::mcp::plan::{PlanMcpDeps, PlanMcpModule};
use spur_core::mcp::signals::{SignalMcpDeps, SignalMcpModule};
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_mcp::ToolModule;

const EXPECTED_BRAIN_TOOLS: &[&str] = &[
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
    "local_project_add",
    "local_project_list",
    "local_project_remove",
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
    "query",
    "submit_plan",
    "spur_loop_doctor",
    "submit_loop",
    "execute_epic",
    "get_plan_status",
    "get_loop_status",
    "get_reconciler_status",
    "get_task_diff",
    "preview_task_base",
    "plan_truncate_and_restart",
    "recover_orphaned_dispatch",
    "pause_loop",
    "resume_loop",
    "kill_loop",
    "set_loop_autonomy",
    "review_task",
    "submit_plan_mutation",
    "report_signal",
    "report_progress",
    "solve_constraints",
    "solve_smt",
    "get_solve_result",
    "external_catalog",
    "external_code_search",
    "external_code_read",
    "external_code_callers",
    "external_code_callees",
    "external_knowledge_context",
    "external_index",
    "external_index_status",
    "skill_search",
    "skill_read",
];

fn pro_feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features = BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_string()]);
    gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
    gate
}

fn catalog_deps() -> SignalMcpDeps {
    SignalMcpDeps {
        pm_service: None,
        event_sink: None,
        feature_gate: pro_feature_gate(),
    }
}

fn plan_deps() -> PlanMcpDeps {
    PlanMcpDeps::catalog_only()
}

#[test]
fn signal_module_advertises_only_worker_signal_tools() {
    let module = SignalMcpModule::new(catalog_deps());
    let tools = module.tools();
    let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();

    assert_eq!(names, vec!["report_signal", "report_progress"]);

    let report_signal = tools
        .iter()
        .find(|tool| tool.name == "report_signal")
        .expect("report_signal definition");
    assert_eq!(
        report_signal.input_schema.get("required"),
        Some(&json!(["task_id", "signal"]))
    );

    let report_progress = tools
        .iter()
        .find(|tool| tool.name == "report_progress")
        .expect("report_progress definition");
    assert_eq!(
        report_progress.input_schema.get("required"),
        Some(&json!(["message"]))
    );
}

#[test]
fn plan_module_advertises_exact_plan_review_reconciler_tools() {
    let module = PlanMcpModule::new(plan_deps());
    let names: Vec<String> = module.tools().into_iter().map(|tool| tool.name).collect();
    let expected: Vec<String> = [
        "merge_plan",
        "resume_plan",
        "force_reclaim_plan",
        "submit_plan",
        "spur_loop_doctor",
        "submit_loop",
        "execute_epic",
        "get_plan_status",
        "get_loop_status",
        "get_reconciler_status",
        "get_task_diff",
        "preview_task_base",
        "plan_truncate_and_restart",
        "recover_orphaned_dispatch",
        "pause_loop",
        "resume_loop",
        "kill_loop",
        "set_loop_autonomy",
        "review_task",
        "submit_plan_mutation",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert_eq!(names, expected, "plan module catalog order must not drift");
}

#[test]
fn core_brain_registry_preserves_compatibility_catalog() {
    let registry = spur_core::mcp::brain_tool_registry(
        spur_core::mcp::delegation::DelegationMcpDeps::catalog_only(),
        plan_deps(),
        catalog_deps(),
        &ContextServiceConfig::default(),
    )
    .expect("core-composed brain registry");
    let names: Vec<String> = registry
        .list_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    let expected: Vec<String> = EXPECTED_BRAIN_TOOLS
        .iter()
        .map(|tool| tool.to_string())
        .collect();

    assert_eq!(
        names, expected,
        "brain catalog order and size must not drift"
    );
}

#[tokio::test]
async fn core_brain_registry_dispatches_solver_tools() {
    let registry = spur_core::mcp::brain_tool_registry(
        spur_core::mcp::delegation::DelegationMcpDeps::catalog_only(),
        plan_deps(),
        catalog_deps(),
        &ContextServiceConfig::default(),
    )
    .expect("core-composed brain registry");

    for tool_name in ["solve_constraints", "solve_smt", "get_solve_result"] {
        let context = spur_mcp::ToolCallContext::new(
            spur_mcp::ServerKind::Brain,
            spur_mcp::ToolAuthority::Brain,
            None,
            None,
        );
        let error = match registry.call_tool(context, tool_name, json!({})).await {
            Ok(_) => panic!("missing solver arguments must be rejected: {tool_name}"),
            Err(error) => error,
        };

        assert_eq!(
            error.code,
            spur_mcp::ErrorCode(-32602),
            "{tool_name} must reach the live solver module"
        );
    }
}

#[test]
fn core_brain_registry_omits_external_tools_when_context_service_url_empty() {
    let context_service = ContextServiceConfig {
        url: String::new(),
        ..ContextServiceConfig::default()
    };
    let registry = spur_core::mcp::brain_tool_registry(
        spur_core::mcp::delegation::DelegationMcpDeps::catalog_only(),
        plan_deps(),
        catalog_deps(),
        &context_service,
    )
    .expect("core-composed brain registry");
    let names: Vec<String> = registry
        .list_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    assert!(
        names.iter().all(|name| !name.starts_with("external_")),
        "empty context-service URL must disable external tools: {names:?}"
    );
}
