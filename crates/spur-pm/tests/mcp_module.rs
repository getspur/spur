use std::sync::Arc;

use serde_json::json;
use spur_pm::mcp::{McpHandlerError, PmMcpDeps, PmMcpModule};
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, PmService};
use tempfile::TempDir;

fn attach_beads_workspace(repo: &std::path::Path, w: &TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    w.copy_db_to(&beads_dir);
}

async fn pm_service_fixture(repo: &std::path::Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    )
}

async fn create_issue(pm: &PmService, title: &str, body: &str) -> String {
    pm.create_issue(IssueCreate {
        title: title.to_string(),
        description: Some(body.to_string()),
        issue_type: Some("task".to_string()),
        ..Default::default()
    })
    .await
    .expect("create issue")
}

#[test]
fn pm_mcp_module_advertises_pm_and_issue_graph_tools() {
    let module = PmMcpModule::new(PmMcpDeps::default());
    let tools = module.tools();
    let actual: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(
        actual,
        [
            "get_issue",
            "list_issues",
            "update_issue",
            "create_issue",
            "add_dependency",
            "create_pr",
            "graph_triage",
            "graph_plan",
            "graph_insights",
            "graph_alerts",
            "graph_subgraph",
        ]
    );
}

#[tokio::test]
async fn pm_mcp_module_get_issue_returns_same_text_content_shape_without_server() {
    let dir = TempDir::new().expect("tempdir");
    attach_beads_workspace(dir.path(), &TestBeadsWorkspace::init());

    let pm = pm_service_fixture(dir.path()).await;
    let issue_id = create_issue(&pm, "Issue title", "test issue body").await;
    let module = PmMcpModule::new(PmMcpDeps {
        pm_service: Some(Arc::clone(&pm)),
        ..Default::default()
    });

    let response = module
        .call("get_issue", json!({ "id": issue_id.clone() }))
        .await
        .expect("get_issue succeeds");

    assert_eq!(response["content"][0]["type"], "text");
    let text = response["content"][0]["text"]
        .as_str()
        .expect("text content");
    let value: serde_json::Value = serde_json::from_str(text).expect("pretty issue json");
    assert_eq!(value["id"], issue_id);
    assert_eq!(value["body"], "test issue body");
}

#[tokio::test]
async fn get_issue_reports_only_effective_parent_child_blockers() {
    let dir = TempDir::new().expect("tempdir");
    attach_beads_workspace(dir.path(), &TestBeadsWorkspace::init());

    let pm = pm_service_fixture(dir.path()).await;
    let blocker = create_issue(&pm, "Blocker", "blocks parent").await;
    let parent = create_issue(&pm, "Parent", "parent issue").await;
    let child = pm
        .create_issue(IssueCreate {
            title: "Child".to_string(),
            description: Some("child issue".to_string()),
            issue_type: Some("task".to_string()),
            parent: Some(parent.clone()),
            ..Default::default()
        })
        .await
        .expect("create child issue");
    let module = PmMcpModule::new(PmMcpDeps {
        pm_service: Some(Arc::clone(&pm)),
        ..Default::default()
    });

    let ready_response = module
        .call("get_issue", json!({ "id": child.clone() }))
        .await
        .expect("get ready child");
    let ready_issue: serde_json::Value = serde_json::from_str(
        ready_response["content"][0]["text"]
            .as_str()
            .expect("ready child text"),
    )
    .expect("ready child json");
    assert!(ready_issue.get("blocked_by").is_none());

    pm.add_dependency(&parent, &blocker)
        .await
        .expect("block parent");
    let blocked_response = module
        .call("get_issue", json!({ "id": child }))
        .await
        .expect("get inherited-blocked child");
    let blocked_issue: serde_json::Value = serde_json::from_str(
        blocked_response["content"][0]["text"]
            .as_str()
            .expect("blocked child text"),
    )
    .expect("blocked child json");
    assert_eq!(blocked_issue["blocked_by"], json!([parent]));
}

#[tokio::test]
async fn get_issue_parent_blocking_stops_after_fifty_hops() {
    let dir = TempDir::new().expect("tempdir");
    attach_beads_workspace(dir.path(), &TestBeadsWorkspace::init());

    let pm = pm_service_fixture(dir.path()).await;
    let blocker = create_issue(&pm, "Blocker", "blocks root").await;
    let root = create_issue(&pm, "Blocked root", "root issue").await;
    let mut chain = vec![root.clone()];

    for hop in 1..=51 {
        let child = pm
            .create_issue(IssueCreate {
                title: format!("Child at hop {hop}"),
                description: Some(format!("child at hop {hop}")),
                issue_type: Some("task".to_string()),
                parent: chain.last().cloned(),
                ..Default::default()
            })
            .await
            .expect("create child issue");
        chain.push(child);
    }
    pm.add_dependency(&root, &blocker)
        .await
        .expect("block root");

    let module = PmMcpModule::new(PmMcpDeps {
        pm_service: Some(Arc::clone(&pm)),
        ..Default::default()
    });
    let at_limit_response = module
        .call("get_issue", json!({ "id": chain[50].clone() }))
        .await
        .expect("get 50-hop child");
    let at_limit: serde_json::Value = serde_json::from_str(
        at_limit_response["content"][0]["text"]
            .as_str()
            .expect("50-hop child text"),
    )
    .expect("50-hop child json");
    assert_eq!(at_limit["blocked_by"], json!([chain[49]]));

    let beyond_limit_response = module
        .call("get_issue", json!({ "id": chain[51].clone() }))
        .await
        .expect("get 51-hop child");
    let beyond_limit: serde_json::Value = serde_json::from_str(
        beyond_limit_response["content"][0]["text"]
            .as_str()
            .expect("51-hop child text"),
    )
    .expect("51-hop child json");
    assert!(beyond_limit.get("blocked_by").is_none());
}

#[tokio::test]
async fn pm_mcp_module_update_issue_missing_id_is_invalid_params() {
    let dir = TempDir::new().expect("tempdir");
    attach_beads_workspace(dir.path(), &TestBeadsWorkspace::init());

    let pm = pm_service_fixture(dir.path()).await;
    let module = PmMcpModule::new(PmMcpDeps {
        pm_service: Some(pm),
        ..Default::default()
    });

    let err = module
        .call("update_issue", json!({ "comment": "x" }))
        .await
        .expect_err("missing id should fail");

    assert!(matches!(err, McpHandlerError::InvalidParams(_)));
}
