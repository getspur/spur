use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
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

fn run_br(repo: &Path, args: &[&str]) -> Result<(), String> {
    common::beads::run_br(repo, args).map(|_| ())
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
        issue_title: None,
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
        Some(&session_id),
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
        Some(&session_id),
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
        error_message(&response).contains("active handoff is not supported"),
        "resume_plan must refuse active owners with handoff-not-supported message: {response}"
    );
}

#[tokio::test]
async fn resume_plan_rejects_duplicate_plan_epics() {
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
        Some(&session_id),
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
        Some(&session_id),
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
async fn merge_plan_refuses_plan_owned_by_other_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-merge-refuse";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Merge Refuse",
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
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("merge_plan", json!({ "plan_id": plan_id }))
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("merge_plan") && msg.contains("active handoff is not implemented in MVP"),
        "merge_plan must refuse plans owned by another brain: {response}"
    );
}

#[tokio::test]
async fn merge_plan_refuses_unowned_plan() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-merge-unowned";
    spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Merge Unowned",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool("merge_plan", json!({ "plan_id": plan_id }))
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("merge_plan") && msg.contains("unowned"),
        "merge_plan must refuse unowned plans (no auto-claim): {response}"
    );
}

#[tokio::test]
async fn review_task_refuses_plan_owned_by_other_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-review-refuse";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Review Refuse",
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
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool(
            "review_task",
            json!({
                "plan_id": plan_id,
                "task_id": "t1",
                "decision": "approve",
            }),
        )
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("review_task") && msg.contains("active handoff is not implemented in MVP"),
        "review_task must refuse plans owned by another brain: {response}"
    );
}

#[tokio::test]
async fn review_task_refuses_unowned_plan() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-review-unowned";
    spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Plan Review Unowned",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool(
            "review_task",
            json!({
                "plan_id": plan_id,
                "task_id": "t1",
                "decision": "approve",
            }),
        )
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("review_task") && msg.contains("unowned"),
        "review_task must refuse unowned plans (no auto-claim): {response}"
    );
}

async fn collect_epic_sentinels(pm: &spur_pm::PmService, epic_id: &str) -> Vec<AuditSentinelKind> {
    let adv = pm
        .advanced()
        .expect("beads-backed PmService must return advanced()");
    let comments = adv
        .list_comments(epic_id)
        .await
        .expect("list_comments must succeed");
    comments
        .iter()
        .filter_map(|c| audit_sentinel::parse_comment(&c.body))
        .filter_map(|result| result.ok())
        .collect()
}

#[tokio::test]
async fn execute_epic_emits_plan_ownership_acquired_when_claiming_unowned_epic() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let epic_id = create_executable_epic(dir.path(), "Execute Claim Unowned");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
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
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should claim unowned epic: {response}"
    );

    let sentinels = collect_epic_sentinels(pm.as_ref(), &epic_id).await;
    let matches: Vec<&AuditSentinelKind> = sentinels
        .iter()
        .filter(|sentinel| {
            matches!(
                sentinel,
                AuditSentinelKind::PlanOwnershipAcquired { reason, .. }
                    if reason == "execute_epic"
            )
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one PlanOwnershipAcquired sentinel from execute_epic; sentinels: {sentinels:?}"
    );
    let AuditSentinelKind::PlanOwnershipAcquired {
        owner,
        token,
        reason,
        ..
    } = matches[0]
    else {
        unreachable!("filtered to PlanOwnershipAcquired");
    };
    assert_eq!(owner, &session_id.to_string());
    assert!(!token.is_empty(), "token must be a non-empty UUID");
    assert_eq!(reason, "execute_epic");
}

#[tokio::test]
async fn execute_epic_emits_plan_ownership_acquired_when_re_issued_by_current_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let epic_id = create_executable_epic(dir.path(), "Execute Re-issue");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner(&session_id.as_session_id().0)],
            ..Default::default()
        },
    )
    .await
    .expect("seed current-brain owner label");

    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
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
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should re-issue ownership when already owned by current brain: {response}"
    );

    let sentinels = collect_epic_sentinels(pm.as_ref(), &epic_id).await;
    let matches: Vec<&AuditSentinelKind> = sentinels
        .iter()
        .filter(|sentinel| {
            matches!(
                sentinel,
                AuditSentinelKind::PlanOwnershipAcquired { reason, .. }
                    if reason == "execute_epic"
            )
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one PlanOwnershipAcquired sentinel from execute_epic re-issue; sentinels: {sentinels:?}"
    );
    let AuditSentinelKind::PlanOwnershipAcquired {
        owner,
        token,
        reason,
        ..
    } = matches[0]
    else {
        unreachable!("filtered to PlanOwnershipAcquired");
    };
    assert_eq!(owner, &session_id.to_string());
    assert!(!token.is_empty(), "token must be a non-empty UUID");
    assert_eq!(reason, "execute_epic");

    let transfers = sentinels
        .iter()
        .filter(|s| matches!(s, AuditSentinelKind::PlanOwnershipTransferred { .. }))
        .count();
    assert_eq!(
        transfers, 0,
        "re-issue by current brain must not emit PlanOwnershipTransferred"
    );
}

#[tokio::test]
async fn execute_epic_refuses_plan_owned_by_other_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let epic_id = create_executable_epic(dir.path(), "Execute Refuse Other");
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner("other-brain")],
            ..Default::default()
        },
    )
    .await
    .expect("seed other-brain owner label");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
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
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("execute_epic") && msg.contains("active handoff is not implemented in MVP"),
        "execute_epic must refuse plans owned by another brain: {response}"
    );

    let sentinels = collect_epic_sentinels(pm.as_ref(), &epic_id).await;
    let transfers = sentinels
        .iter()
        .filter(|s| matches!(s, AuditSentinelKind::PlanOwnershipTransferred { .. }))
        .count();
    assert_eq!(
        transfers, 0,
        "refused execute_epic must not emit PlanOwnershipTransferred"
    );
}

#[tokio::test]
async fn execute_epic_refuses_plan_with_ambiguous_owners() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let epic_id = create_executable_epic(dir.path(), "Execute Refuse Ambiguous");
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![
                labels::plan_owner("brain-current"),
                labels::plan_owner("other-brain"),
            ],
            ..Default::default()
        },
    )
    .await
    .expect("seed ambiguous owner labels");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
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
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("execute_epic") && msg.contains("ambiguous owner labels"),
        "execute_epic must refuse plans with ambiguous owner labels: {response}"
    );

    let sentinels = collect_epic_sentinels(pm.as_ref(), &epic_id).await;
    let transfers = sentinels
        .iter()
        .filter(|s| matches!(s, AuditSentinelKind::PlanOwnershipTransferred { .. }))
        .count();
    assert_eq!(
        transfers, 0,
        "refused execute_epic must not emit PlanOwnershipTransferred"
    );
}

#[tokio::test]
async fn execute_epic_allows_unowned_plan() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let epic_id = create_executable_epic(dir.path(), "Execute Gate Unowned");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
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
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic must allow unowned plans (claim path): {response}"
    );
}

#[tokio::test]
async fn execute_epic_allows_re_issue_by_current_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let epic_id = create_executable_epic(dir.path(), "Execute Gate Re-issue");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    pm.update_issue(
        &epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner(&session_id.as_session_id().0)],
            ..Default::default()
        },
    )
    .await
    .expect("seed current-brain owner label");

    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
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
        .__test_call_execute_epic(&epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic must allow re-issue by current brain: {response}"
    );
}

#[tokio::test]
async fn force_reclaim_plan_refuses_without_confirm() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-force-reclaim-no-confirm";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Force Reclaim No Confirm",
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
    .expect("seed other-brain owner label");
    let labels_before = pm
        .get_issue(&subgraph.epic_id)
        .await
        .expect("get pre-call epic")
        .labels;

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    // Missing confirm.
    let response = server
        .__test_call_tool("force_reclaim_plan", json!({ "plan_id": plan_id }))
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("force_reclaim_plan") && msg.contains("confirm"),
        "missing confirm must surface a clear safety error: {response}"
    );

    // Explicit confirm: false.
    let response = server
        .__test_call_tool(
            "force_reclaim_plan",
            json!({ "plan_id": plan_id, "confirm": false }),
        )
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("force_reclaim_plan") && msg.contains("confirm"),
        "confirm:false must surface a clear safety error: {response}"
    );

    // Labels must be untouched.
    let labels_after = pm
        .get_issue(&subgraph.epic_id)
        .await
        .expect("get post-call epic")
        .labels;
    assert_eq!(
        labels_before, labels_after,
        "force_reclaim_plan without confirm must NOT mutate epic labels"
    );

    // No PlanForceReclaimed audit comment must have been emitted.
    let sentinels = collect_epic_sentinels(pm.as_ref(), &subgraph.epic_id).await;
    let reclaims = sentinels
        .iter()
        .filter(|s| matches!(s, AuditSentinelKind::PlanForceReclaimed { .. }))
        .count();
    assert_eq!(
        reclaims, 0,
        "refused force_reclaim_plan must not emit PlanForceReclaimed sentinel"
    );
}

fn parse_force_reclaim_response_body(response: &serde_json::Value) -> serde_json::Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("force_reclaim_plan success response must include content[0].text");
    serde_json::from_str(text).expect("force_reclaim_plan body must be valid JSON")
}

#[tokio::test]
async fn force_reclaim_plan_takes_over_from_other_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-force-reclaim-takeover";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Force Reclaim Takeover",
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
    .expect("seed other-brain owner label");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool(
            "force_reclaim_plan",
            json!({
                "plan_id": plan_id,
                "confirm": true,
                "reason": "takeover test",
            }),
        )
        .await;
    assert!(
        response.get("error").is_none(),
        "force_reclaim_plan must succeed when confirm:true: {response}"
    );
    let body = parse_force_reclaim_response_body(&response);
    assert_eq!(
        body["prior_owner"].as_str(),
        Some(labels::compact_label_component("other-brain").as_str()),
        "prior_owner must reflect the previously stamped owner label value"
    );
    assert_eq!(
        body["new_owner"].as_str(),
        Some(session_id.to_string().as_str()),
        "new_owner must equal the current brain session id"
    );
    let audit_token = body["audit_token"]
        .as_str()
        .expect("audit_token must be a non-empty string");
    assert!(!audit_token.is_empty(), "audit_token must be non-empty");

    // Epic now carries exactly the new owner label and no other plan-owner labels.
    let epic = pm
        .get_issue(&subgraph.epic_id)
        .await
        .expect("get post-reclaim epic");
    let owners: Vec<&String> = epic
        .labels
        .iter()
        .filter(|label| labels::parse_plan_owner(label).is_some())
        .collect();
    let expected_owner = labels::plan_owner(&session_id.as_session_id().0);
    assert_eq!(
        owners.len(),
        1,
        "epic must carry exactly one owner label after reclaim; labels={:?}",
        epic.labels
    );
    assert_eq!(
        owners[0], &expected_owner,
        "epic must carry only the new owner label after reclaim"
    );

    // Audit sentinel records the takeover with the prior owner, new owner,
    // operator-supplied reason, and the same token surfaced to the caller.
    let sentinels = collect_epic_sentinels(pm.as_ref(), &subgraph.epic_id).await;
    let reclaims: Vec<&AuditSentinelKind> = sentinels
        .iter()
        .filter(|s| matches!(s, AuditSentinelKind::PlanForceReclaimed { .. }))
        .collect();
    assert_eq!(
        reclaims.len(),
        1,
        "expected exactly one PlanForceReclaimed sentinel; sentinels: {sentinels:?}"
    );
    let AuditSentinelKind::PlanForceReclaimed {
        plan_id: audit_plan_id,
        prior_owner,
        new_owner,
        token,
        reason,
    } = reclaims[0]
    else {
        unreachable!("filtered to PlanForceReclaimed");
    };
    assert_eq!(audit_plan_id, plan_id);
    assert_eq!(
        prior_owner.as_deref(),
        Some(labels::compact_label_component("other-brain").as_str())
    );
    assert_eq!(new_owner, &session_id.to_string());
    assert_eq!(token, audit_token);
    assert_eq!(reason.as_deref(), Some("takeover test"));
}

#[tokio::test]
async fn force_reclaim_plan_handles_unowned_plan() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-force-reclaim-unowned";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Force Reclaim Unowned",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool(
            "force_reclaim_plan",
            json!({
                "plan_id": plan_id,
                "confirm": true,
            }),
        )
        .await;
    assert!(
        response.get("error").is_none(),
        "force_reclaim_plan must succeed for unowned plans: {response}"
    );
    let body = parse_force_reclaim_response_body(&response);
    assert!(
        body["prior_owner"].is_null(),
        "prior_owner must be null when reclaiming an unowned plan; body={body}"
    );
    assert_eq!(
        body["new_owner"].as_str(),
        Some(session_id.to_string().as_str())
    );
    let audit_token = body["audit_token"].as_str().expect("audit_token string");
    assert!(!audit_token.is_empty());

    // Epic is now stamped with exactly the current brain.
    let epic = pm
        .get_issue(&subgraph.epic_id)
        .await
        .expect("get post-reclaim epic");
    let expected_owner = labels::plan_owner(&session_id.as_session_id().0);
    assert!(
        epic.labels.iter().any(|label| label == &expected_owner),
        "epic must carry the current brain owner label after force-reclaim; labels={:?}",
        epic.labels
    );

    // Audit sentinel records prior_owner: None, no reason, matching token.
    let sentinels = collect_epic_sentinels(pm.as_ref(), &subgraph.epic_id).await;
    let reclaims: Vec<&AuditSentinelKind> = sentinels
        .iter()
        .filter(|s| matches!(s, AuditSentinelKind::PlanForceReclaimed { .. }))
        .collect();
    assert_eq!(
        reclaims.len(),
        1,
        "expected one PlanForceReclaimed sentinel"
    );
    let AuditSentinelKind::PlanForceReclaimed {
        prior_owner,
        new_owner,
        token,
        reason,
        ..
    } = reclaims[0]
    else {
        unreachable!("filtered to PlanForceReclaimed");
    };
    assert!(
        prior_owner.is_none(),
        "prior_owner must be None for unowned plan"
    );
    assert_eq!(new_owner, &session_id.to_string());
    assert_eq!(token, audit_token);
    assert!(reason.is_none(), "reason must be None when not supplied");
}
fn run_br_json(repo: &Path, args: &[&str]) -> String {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"))
}

fn parse_id_from_create(json: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(json).expect("br create output JSON");
    value
        .get("id")
        .and_then(|id| id.as_str())
        .unwrap_or_else(|| panic!("br create output missing id: {json}"))
        .to_string()
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
    let adv = pm.advanced().expect("advanced beads backend");
    for issue_id in subgraph.task_map.values() {
        adv.add_comment(
            issue_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
                delegation_id: "del-terminal".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-terminal".into()),
                result_summary: None,
                artifact_uri: None,
                dispatched_base_oid: Some("0000000000000000000000000000000000000001".into()),
            }),
        )
        .await
        .expect("seed completion");
        adv.add_comment(
            issue_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::Approval {
                delegation_id: "del-terminal".into(),
            }),
        )
        .await
        .expect("seed approval");
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
#[tokio::test]
async fn resume_plan_rejects_unowned_plan_when_current_brain_already_owns_active_plan() {
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
        Some(&session_id),
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
        Some(&session_id),
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
        Some(&session_id),
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
        Some(&session_id),
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

#[tokio::test]
async fn concurrent_resume_plan_claims_only_one_active_plan_for_current_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    for (plan_id, title) in [
        ("plan-concurrent-resume-a", "Concurrent Resume A"),
        ("plan-concurrent-resume-b", "Concurrent Resume B"),
    ] {
        spur_mcp::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            plan_id,
            title,
            None,
            &one_task(),
        )
        .await
        .expect("build epic subgraph");
    }

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let (first, second) = tokio::join!(
        server.__test_call_tool(
            "resume_plan",
            json!({ "plan_id": "plan-concurrent-resume-a" })
        ),
        server.__test_call_tool(
            "resume_plan",
            json!({ "plan_id": "plan-concurrent-resume-b" })
        ),
    );

    let successes = [first.get("error").is_none(), second.get("error").is_none()]
        .into_iter()
        .filter(|success| *success)
        .count();
    assert_eq!(
        successes, 1,
        "exactly one concurrent resume_plan should claim ownership: first={first} second={second}"
    );
    let errors = [first, second]
        .into_iter()
        .filter_map(|response| response.get("error").cloned())
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "one resume_plan response should error");
    assert!(
        errors[0]
            .get("message")
            .and_then(|message| message.as_str())
            .is_some_and(|message| message.contains("already owns active plan")),
        "losing resume_plan response must report active-plan cardinality: {:?}",
        errors[0]
    );
}

#[tokio::test]
async fn concurrent_execute_epic_starts_only_one_active_plan_for_current_brain() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let first_epic = create_executable_epic(dir.path(), "Concurrent Execute A");
    let second_epic = create_executable_epic(dir.path(), "Concurrent Execute B");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
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

    let (first, second) = tokio::join!(
        server.__test_call_execute_epic(&first_epic, Some("codex")),
        server.__test_call_execute_epic(&second_epic, Some("codex")),
    );

    let successes = [first.get("error").is_none(), second.get("error").is_none()]
        .into_iter()
        .filter(|success| *success)
        .count();
    assert_eq!(
        successes, 1,
        "exactly one concurrent execute_epic should start ownership: first={first} second={second}"
    );
    let errors = [first, second]
        .into_iter()
        .filter_map(|response| response.get("error").cloned())
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "one execute_epic response should error");
    assert!(
        errors[0]
            .get("message")
            .and_then(|message| message.as_str())
            .is_some_and(|message| message.contains("already owns active plan")),
        "losing execute_epic response must report active-plan cardinality: {:?}",
        errors[0]
    );
}
