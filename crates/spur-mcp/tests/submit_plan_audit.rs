//! Integration test: `emit_plan_submit_audit` writes a `[[spur-audit v1]]`
//! PlanSubmit sentinel comment on the epic issue, readable back through
//! `audit_sentinel::parse_comment`.
//!
//! Requires `br` on PATH and a writable temp directory. Skipped (not failed)
//! when `br` is unavailable, per the pattern in `audit_sentinel_round_trip.rs`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::PlanTask;
use tempfile::TempDir;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
}

fn minimal_tasks() -> Vec<PlanTask> {
    vec![
        PlanTask {
            task_id: "t1".into(),
            agent: "claude-code-acp".into(),
            task: "Do T1.".into(),
            depends_on: vec![],
            issue_id: None,
            context_files: vec![],
        },
        PlanTask {
            task_id: "t2".into(),
            agent: "claude-code-acp".into(),
            task: "Do T2.".into(),
            depends_on: vec!["t1".into()],
            issue_id: None,
            context_files: vec![],
        },
    ]
}

fn collect_sentinels(texts: &[String]) -> Vec<AuditSentinelKind> {
    texts
        .iter()
        .filter_map(|text| audit_sentinel::parse_comment(text))
        .filter_map(|result| result.ok())
        .collect()
}

#[tokio::test]
async fn submit_plan_persists_plan_owner_on_epic() {
    if !br_available() {
        eprintln!("skipping submit_plan_persists_plan_owner_on_epic: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );
    let brain_session = BrainSessionId::new(SessionId("brain-owner-submit".into()));
    let (server, _channel) = common::server_builder::MockServerBuilder::pro()
        .with_session_id(brain_session.clone())
        .with_pm_service(Arc::clone(&pm))
        .build();

    let response = server
        .__test_call_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "Owner Persist Epic",
            "tasks": [{
                "task_id": "t1",
                "agent": "claude-code-acp",
                "task": "Do T1.",
                "depends_on": [],
                "context_files": []
            }]
        }))
        .await;

    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan text response");
    let epic_id = text
        .lines()
        .find_map(|line| line.strip_prefix("epic_id: "))
        .and_then(|rest| rest.split_whitespace().next())
        .expect("submit_plan response must include epic_id");
    let epic = pm.get_issue(epic_id).await.expect("load created epic");

    let owner_label = spur_mcp::plan::labels::plan_owner(brain_session.as_session_id().0.as_str());
    assert!(
        epic.labels.contains(&owner_label),
        "epic {epic_id} must carry owner label {owner_label}; got labels: {:?}",
        epic.labels
    );
}

#[tokio::test]
async fn emit_plan_submit_audit_writes_sentinel_on_epic() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(
        None,  // no github_repo
        true,  // beads_enabled
        false, // github_enabled
        dir.path(),
        None, // closed_status default
    )
    .await
    .expect("PmService::try_new failed")
    .expect("expected Some(PmService)");

    let tasks = minimal_tasks();
    let subgraph = spur_mcp::build_epic_subgraph(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        "P1",
        "Audit Test Epic",
        None,
        &tasks,
    )
    .await
    .expect("build_epic_subgraph must succeed");

    // Emit audit sentinel via the helper — this is the same code path as
    // handle_submit_plan.
    let adv = pm
        .advanced()
        .expect("beads-backed PmService must return advanced()");
    spur_mcp::emit_plan_submit_audit(
        adv,
        "P1",
        &subgraph,
        None,
        None,
        None,
        Some(&spur_acp::SessionId("brain-1".into())),
    )
    .await;

    // Read comments back via br and assert the PlanSubmit sentinel is present.
    let list_out = run_br(dir.path(), &["comments", "list", &subgraph.epic_id])
        .expect("br comments list failed");
    let items: serde_json::Value =
        serde_json::from_str(&list_out).expect("br comments list output must be valid JSON");
    let texts: Vec<String> = items
        .as_array()
        .expect("comments list must be a JSON array")
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect();

    let expected_task_ids: std::collections::HashSet<String> =
        subgraph.task_map.values().cloned().collect();

    let sentinels = collect_sentinels(&texts);
    let found = sentinels.iter().any(|sentinel| match sentinel {
        AuditSentinelKind::PlanSubmit {
            plan_id,
            epic_issue_id,
            task_ids,
            ..
        } => {
            plan_id == "P1"
                && epic_issue_id == &subgraph.epic_id
                && task_ids
                    .iter()
                    .cloned()
                    .collect::<std::collections::HashSet<_>>()
                    == expected_task_ids
        }
        _ => false,
    });

    assert!(
        found,
        "PlanSubmit audit sentinel not found on epic {}; comments: {texts:?}",
        subgraph.epic_id
    );
}

#[tokio::test]
async fn plan_submit_audit_includes_brain_session_id() {
    if !br_available() {
        eprintln!("skipping plan_submit_audit_includes_brain_session_id: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");

    let subgraph = spur_mcp::build_epic_subgraph(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        "P1b",
        "Audit Test Epic",
        None,
        &minimal_tasks(),
    )
    .await
    .expect("build_epic_subgraph must succeed");

    let adv = pm
        .advanced()
        .expect("beads-backed PmService must return advanced()");
    spur_mcp::emit_plan_submit_audit(
        adv,
        "P1b",
        &subgraph,
        None,
        None,
        Some("submit_plan"),
        Some(&spur_acp::SessionId("brain-99".into())),
    )
    .await;

    let list_out = run_br(dir.path(), &["comments", "list", &subgraph.epic_id])
        .expect("br comments list failed");
    let items: serde_json::Value =
        serde_json::from_str(&list_out).expect("br comments list output must be valid JSON");
    let texts: Vec<String> = items
        .as_array()
        .expect("comments list must be a JSON array")
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect();
    let sentinels = collect_sentinels(&texts);

    assert!(sentinels.iter().any(|sentinel| matches!(
        sentinel,
        AuditSentinelKind::PlanSubmit {
            plan_id,
            brain_session_id: Some(brain_session_id),
            ..
        } if plan_id == "P1b" && brain_session_id == "brain-99"
    )));
}

#[tokio::test]
async fn plan_submit_audit_includes_merge_base_and_execution_mode() {
    if !br_available() {
        eprintln!(
            "skipping plan_submit_audit_includes_merge_base_and_execution_mode: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");

    let subgraph = spur_mcp::build_epic_subgraph(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        "P2",
        "Audit Test Epic",
        None,
        &minimal_tasks(),
    )
    .await
    .expect("build_epic_subgraph must succeed");

    let adv = pm
        .advanced()
        .expect("beads-backed PmService must return advanced()");
    spur_mcp::emit_plan_submit_audit(
        adv,
        "P2",
        &subgraph,
        Some("refs/heads/main"),
        Some("0123456789abcdef0123456789abcdef01234567"),
        Some("execute_epic"),
        Some(&spur_acp::SessionId("brain-2".into())),
    )
    .await;

    let list_out = run_br(dir.path(), &["comments", "list", &subgraph.epic_id])
        .expect("br comments list failed");
    let items: serde_json::Value =
        serde_json::from_str(&list_out).expect("br comments list output must be valid JSON");
    let texts: Vec<String> = items
        .as_array()
        .expect("comments list must be a JSON array")
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect();
    let sentinels = collect_sentinels(&texts);

    assert!(sentinels.iter().any(|sentinel| matches!(
        sentinel,
        AuditSentinelKind::PlanSubmit {
            plan_id,
            base_snapshot_branch: Some(base_snapshot_branch),
            base_snapshot_oid: Some(base_snapshot_oid),
            execution_mode: Some(execution_mode),
            ..
        } if plan_id == "P2"
            && base_snapshot_branch == "refs/heads/main"
            && base_snapshot_oid == "0123456789abcdef0123456789abcdef01234567"
            && execution_mode == "execute_epic"
    )));
}

#[tokio::test]
async fn plan_submit_sentinel_round_trips_base_snapshot_oid() {
    if !br_available() {
        eprintln!("skipping plan_submit_sentinel_round_trips_base_snapshot_oid: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService)");

    let subgraph = spur_mcp::build_epic_subgraph(
        &pm,
        common::server_builder::pro_feature_gate().as_ref(),
        "P3",
        "Audit Test Epic",
        None,
        &minimal_tasks(),
    )
    .await
    .expect("build_epic_subgraph must succeed");

    let adv = pm
        .advanced()
        .expect("beads-backed PmService must return advanced()");
    let sentinel = AuditSentinelKind::PlanSubmit {
        plan_id: "P3".into(),
        epic_issue_id: subgraph.epic_id.clone(),
        task_ids: subgraph.task_map.values().cloned().collect(),
        base_snapshot_branch: Some("spur/brain-snapshot-test".into()),
        base_snapshot_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
        execution_mode: Some("submit_plan".into()),
        brain_session_id: Some("brain-3".into()),
    };
    adv.add_comment(
        &subgraph.epic_id,
        &audit_sentinel::encode_comment(&sentinel),
    )
    .await
    .expect("write sentinel");

    let list_out = run_br(dir.path(), &["comments", "list", &subgraph.epic_id])
        .expect("br comments list failed");
    let items: serde_json::Value =
        serde_json::from_str(&list_out).expect("br comments list output must be valid JSON");
    let texts: Vec<String> = items
        .as_array()
        .expect("comments list must be a JSON array")
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect();
    let sentinels = collect_sentinels(&texts);

    assert!(sentinels.iter().any(|candidate| matches!(
        candidate,
        AuditSentinelKind::PlanSubmit {
            plan_id,
            base_snapshot_branch: Some(base_snapshot_branch),
            base_snapshot_oid: Some(base_snapshot_oid),
            ..
        } if plan_id == "P3"
            && base_snapshot_branch == "spur/brain-snapshot-test"
            && base_snapshot_oid == "0123456789abcdef0123456789abcdef01234567"
    )));
}
