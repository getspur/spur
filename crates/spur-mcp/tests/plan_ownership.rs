use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
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

#[tokio::test]
async fn merge_plan_refuses_plan_owned_by_other_brain() {
    if !br_available() {
        eprintln!("skipping merge_plan_refuses_plan_owned_by_other_brain: `br` not on PATH");
        return;
    }

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
        &session_id,
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
        msg.contains("merge_plan")
            && msg.contains("active handoff is not implemented in MVP"),
        "merge_plan must refuse plans owned by another brain: {response}"
    );
}

#[tokio::test]
async fn merge_plan_refuses_unowned_plan() {
    if !br_available() {
        eprintln!("skipping merge_plan_refuses_unowned_plan: `br` not on PATH");
        return;
    }

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
        &session_id,
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
    if !br_available() {
        eprintln!("skipping review_task_refuses_plan_owned_by_other_brain: `br` not on PATH");
        return;
    }

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
        &session_id,
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
        msg.contains("review_task")
            && msg.contains("active handoff is not implemented in MVP"),
        "review_task must refuse plans owned by another brain: {response}"
    );
}

#[tokio::test]
async fn review_task_refuses_unowned_plan() {
    if !br_available() {
        eprintln!("skipping review_task_refuses_unowned_plan: `br` not on PATH");
        return;
    }

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
        &session_id,
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

async fn collect_epic_sentinels(
    pm: &spur_pm::PmService,
    epic_id: &str,
) -> Vec<AuditSentinelKind> {
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn execute_epic_emits_plan_ownership_acquired_when_claiming_unowned_epic() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-execute-claim-unowned";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Execute Claim Unowned",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
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
        .__test_call_execute_epic(&subgraph.epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should claim unowned epic: {response}"
    );

    let sentinels = collect_epic_sentinels(pm.as_ref(), &subgraph.epic_id).await;
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn execute_epic_emits_plan_ownership_acquired_when_re_issued_by_current_brain() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-execute-reissue";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Execute Re-issue",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner(&session_id.as_session_id().0)],
            ..Default::default()
        },
    )
    .await
    .expect("seed current-brain owner label");

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
        .__test_call_execute_epic(&subgraph.epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic should re-issue ownership when already owned by current brain: {response}"
    );

    let sentinels = collect_epic_sentinels(pm.as_ref(), &subgraph.epic_id).await;
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

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn execute_epic_refuses_plan_owned_by_other_brain() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-execute-refuse-other";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Execute Refuse Other",
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
        .__test_call_execute_epic(&subgraph.epic_id, Some("codex"))
        .await;
    let msg = error_message(&response);
    assert!(
        msg.contains("execute_epic")
            && msg.contains("active handoff is not implemented in MVP"),
        "execute_epic must refuse plans owned by another brain: {response}"
    );

    let sentinels = collect_epic_sentinels(pm.as_ref(), &subgraph.epic_id).await;
    let transfers = sentinels
        .iter()
        .filter(|s| matches!(s, AuditSentinelKind::PlanOwnershipTransferred { .. }))
        .count();
    assert_eq!(
        transfers, 0,
        "refused execute_epic must not emit PlanOwnershipTransferred"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn execute_epic_allows_unowned_plan() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-execute-gate-unowned";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Execute Gate Unowned",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
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
        .__test_call_execute_epic(&subgraph.epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic must allow unowned plans (claim path): {response}"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn execute_epic_allows_re_issue_by_current_brain() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let feature_gate = common::server_builder::pro_feature_gate();
    let plan_id = "plan-execute-gate-reissue";
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Execute Gate Re-issue",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let session_id = BrainSessionId::new(SessionId("brain-current".into()));
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner(&session_id.as_session_id().0)],
            ..Default::default()
        },
    )
    .await
    .expect("seed current-brain owner label");

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
        .__test_call_execute_epic(&subgraph.epic_id, Some("codex"))
        .await;
    assert!(
        response.get("error").is_none(),
        "execute_epic must allow re-issue by current brain: {response}"
    );
}
