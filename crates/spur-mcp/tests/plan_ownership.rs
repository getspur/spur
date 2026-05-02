use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::{labels, PlanTask};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_mcp::tools_list;
use tempfile::TempDir;

mod common;

#[test]
fn resume_plan_appears_in_tools_list() {
    let schema = tools_list()
        .into_iter()
        .find(|tool| tool.name == "resume_plan")
        .expect("resume_plan must be in tool catalog")
        .input_schema;

    let required: Vec<&str> = schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|values| values.iter().filter_map(|value| value.as_str()).collect())
        .unwrap_or_default();

    assert!(
        required.contains(&"plan_id"),
        "resume_plan must require plan_id"
    );
}

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        ))
    }
}

fn run_br_json(repo: &Path, args: &[&str]) -> String {
    let mut full_args = args.to_vec();
    full_args.push("--json");
    let output = Command::new("br")
        .args(&full_args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        String::from_utf8_lossy(&output.stdout).into_owned()
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        );
    }
}

fn parse_id_from_create(json: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(json).expect("br create output JSON");
    value
        .get("id")
        .and_then(|id| id.as_str())
        .unwrap_or_else(|| panic!("br create output missing id: {json}"))
        .to_string()
}

async fn beads_pm(repo: &Path) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn one_task() -> Vec<PlanTask> {
    vec![PlanTask {
        task_id: "t1".into(),
        agent: "codex".into(),
        task: "Task".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: vec![],
    }]
}

async fn build_owned_plan(
    pm: &spur_pm::PmService,
    plan_id: &str,
    title: &str,
    owner: &BrainSessionId,
) -> spur_mcp::EpicSubgraph {
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph =
        spur_mcp::build_epic_subgraph(pm, feature_gate.as_ref(), plan_id, title, None, &one_task())
            .await
            .expect("build epic subgraph");
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner(&owner.as_session_id().0)],
            ..Default::default()
        },
    )
    .await
    .expect("add owner label");
    subgraph
}

async fn close_plan_tasks(pm: &spur_pm::PmService, subgraph: &spur_mcp::EpicSubgraph) {
    for issue_id in subgraph.task_map.values() {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close plan task");
    }
}

fn create_executable_epic(repo: &Path, title: &str) -> String {
    let epic_id = parse_id_from_create(&run_br_json(
        repo,
        &[
            "create",
            "--type",
            "epic",
            "--title",
            title,
            "--priority",
            "2",
        ],
    ));
    let task_id = parse_id_from_create(&run_br_json(
        repo,
        &[
            "create",
            "--type",
            "task",
            "--title",
            &format!("{title} Task"),
            "--priority",
            "2",
        ],
    ));
    run_br(repo, &["dep", "add", &task_id, &epic_id]).expect("link task to epic");
    epic_id
}

fn response_text(response: &serde_json::Value) -> &str {
    response
        .get("result")
        .and_then(|result| result.get("content"))
        .and_then(|content| content.get(0))
        .and_then(|content| content.get("text"))
        .and_then(|text| text.as_str())
        .expect("response text")
}

fn error_message(response: &serde_json::Value) -> &str {
    response
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .expect("error message")
}

#[tokio::test]
async fn resume_plan_claims_unowned_plan() {
    if !br_available() {
        eprintln!("skipping resume_plan_claims_unowned_plan: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-resume-claim";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Resume Claim",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": plan_id }))
        .await;
    assert!(
        response.get("error").is_none(),
        "resume_plan should claim unowned plan: {response}"
    );

    let epic = pm
        .get_issue(&subgraph.epic_id)
        .await
        .expect("get claimed epic");
    let current_owner = labels::plan_owner(&session_id.as_session_id().0);
    assert!(
        epic.labels.iter().any(|label| label == &current_owner),
        "epic must carry current owner label {current_owner}; labels={:?}",
        epic.labels
    );
}

#[tokio::test]
async fn resume_plan_refuses_plan_owned_by_other_brain() {
    if !br_available() {
        eprintln!("skipping resume_plan_refuses_plan_owned_by_other_brain: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-resume-refuse";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Resume Refuse",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner("other-brain")],
            ..Default::default()
        },
    )
    .await
    .expect("add other owner label");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": plan_id }))
        .await;
    assert!(
        error_message(&response).contains("active handoff is not implemented in MVP"),
        "resume_plan must refuse active owners with MVP handoff message: {response}"
    );
}

#[tokio::test]
async fn resume_plan_rejects_duplicate_plan_epics() {
    if !br_available() {
        eprintln!("skipping resume_plan_rejects_duplicate_plan_epics: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-resume-duplicate";
    spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Resume Duplicate A",
        None,
        &one_task(),
    )
    .await
    .expect("build first epic subgraph");
    spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Resume Duplicate B",
        None,
        &one_task(),
    )
    .await
    .expect("build second epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": plan_id }))
        .await;
    assert!(
        error_message(&response).contains("ambiguous plan lookup"),
        "resume_plan must reject duplicate plan epics: {response}"
    );
}

#[tokio::test]
async fn resume_plan_refuses_mixed_current_and_other_owner_labels() {
    if !br_available() {
        eprintln!(
            "skipping resume_plan_refuses_mixed_current_and_other_owner_labels: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-resume-mixed-owners";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Resume Mixed Owners",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![
                labels::plan_owner(&session_id.as_session_id().0),
                labels::plan_owner("other-brain"),
            ],
            ..Default::default()
        },
    )
    .await
    .expect("add mixed owner labels");

    let (server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": plan_id }))
        .await;
    assert!(
        error_message(&response).contains("ambiguous owner labels"),
        "resume_plan must refuse mixed current and other owner labels: {response}"
    );
}

#[tokio::test]
async fn resume_plan_rejects_unowned_plan_when_current_brain_already_owns_active_plan() {
    if !br_available() {
        eprintln!(
            "skipping resume_plan_rejects_unowned_plan_when_current_brain_already_owns_active_plan: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    build_owned_plan(
        pm.as_ref(),
        "plan-owned-active",
        "Plan Owned Active",
        &session_id,
    )
    .await;
    let target_plan_id = "plan-unowned-target";
    spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        target_plan_id,
        "Plan Unowned Target",
        None,
        &one_task(),
    )
    .await
    .expect("build target epic subgraph");

    let (server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": target_plan_id }))
        .await;
    assert!(
        error_message(&response).contains("already owns active plan plan-owned-active"),
        "resume_plan must reject claiming a second active plan: {response}"
    );
}

#[tokio::test]
async fn execute_epic_rejects_new_epic_when_current_brain_already_owns_active_plan() {
    if !br_available() {
        eprintln!(
            "skipping execute_epic_rejects_new_epic_when_current_brain_already_owns_active_plan: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    build_owned_plan(
        pm.as_ref(),
        "plan-owned-active",
        "Plan Owned Active",
        &session_id,
    )
    .await;
    let target_epic_id = create_executable_epic(dir.path(), "Execute Different Epic");

    let (mut server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    let response = server
        .__test_call_execute_epic(&target_epic_id, Some("codex"))
        .await;
    assert!(
        error_message(&response).contains("already owns active plan plan-owned-active"),
        "execute_epic must reject starting a second active plan: {response}"
    );
}

#[tokio::test]
async fn terminal_owned_plan_does_not_block_resume_plan_claim() {
    if !br_available() {
        eprintln!(
            "skipping terminal_owned_plan_does_not_block_resume_plan_claim: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let terminal = build_owned_plan(
        pm.as_ref(),
        "plan-owned-terminal",
        "Plan Owned Terminal",
        &session_id,
    )
    .await;
    close_plan_tasks(pm.as_ref(), &terminal).await;
    let target_plan_id = "plan-unowned-target";
    spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        target_plan_id,
        "Plan Unowned Target",
        None,
        &one_task(),
    )
    .await
    .expect("build target epic subgraph");

    let (server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": target_plan_id }))
        .await;
    assert!(
        response.get("error").is_none(),
        "terminal owned plan must not block resume_plan claim: {response}"
    );
    assert!(
        response_text(&response).contains("\"status\": \"claimed\""),
        "resume_plan should claim target plan: {response}"
    );
}

#[tokio::test]
async fn terminal_owned_plan_does_not_block_execute_epic() {
    if !br_available() {
        eprintln!("skipping terminal_owned_plan_does_not_block_execute_epic: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let terminal = build_owned_plan(
        pm.as_ref(),
        "plan-owned-terminal",
        "Plan Owned Terminal",
        &session_id,
    )
    .await;
    close_plan_tasks(pm.as_ref(), &terminal).await;
    let target_epic_id = create_executable_epic(dir.path(), "Execute After Terminal");

    let (mut server, _channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_workers(vec![spur_mcp::WorkerInfo {
        name: "codex".into(),
        ..Default::default()
    }]);

    let response = server
        .__test_call_execute_epic(&target_epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "terminal owned plan must not block execute_epic: {response}"
    );
    let text = response_text(&response);
    assert!(
        text.contains(&format!("\"epic_id\": \"{target_epic_id}\"")),
        "execute_epic should start target epic: {response}"
    );
}
