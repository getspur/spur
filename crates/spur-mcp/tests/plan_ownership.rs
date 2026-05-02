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
