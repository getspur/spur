use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::json;
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
fn core_brain_registry_preserves_compatibility_catalog() {
    let registry =
        spur_core::mcp::brain_tool_registry(catalog_deps()).expect("core-composed brain registry");
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
