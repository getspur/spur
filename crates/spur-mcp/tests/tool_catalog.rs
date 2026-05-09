//! Tool catalog snapshot test.
//!
//! Guards INV-1 from the T1 contract-truthfulness spec: the set of tool
//! names exposed via `tools/list` must not drift silently. Any addition
//! or removal requires updating the `EXPECTED` list in this test in the
//! same commit.

use spur_mcp::tools_list;

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
