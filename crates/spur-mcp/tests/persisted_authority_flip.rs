use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{
    self, AuditSentinelKind, CompletionAuditFields, CompletionState,
};
use spur_mcp::plan::labels;
use spur_mcp::plan::proposers::{ScopeDriftSplitProposer, TrivialScorer};
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::plan::signal_watcher::SignalWatcher;
use spur_mcp::plan::signals::{self, WorkerSignal};
use spur_mcp::plan::{PlanMergeState, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use spur_mcp::server::{
    compensate_mutation_orphans, resolve_dispatch_orphan, DetachedContinuationCtx,
    McpCallbackServer,
};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

mod common;

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
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

fn run_br_json(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            output.status
        ))
    }
}

fn extract_id(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .expect("json")
        .get("id")
        .and_then(|value| value.as_str())
        .expect("id")
        .to_string()
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

async fn add_labels(pm: &PmService, issue_id: &str, labels_to_add: &[String]) {
    for label in labels_to_add {
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![label.clone()],
                ..Default::default()
            },
        )
        .await
        .expect("add label");
    }
}

fn collect_audits(raw: &str) -> Vec<AuditSentinelKind> {
    serde_json::from_str::<serde_json::Value>(raw)
        .expect("comments json")
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|entry| entry.get("text").and_then(|value| value.as_str()))
        .filter_map(audit_sentinel::parse_comment)
        .filter_map(|result| result.ok())
        .collect()
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn persisted_task(agent: &str) -> Vec<PlanTask> {
    vec![PlanTask {
        task_id: "t1".into(),
        agent: agent.into(),
        task: "Task".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: vec![],
    }]
}

fn persisted_task_with_context(agent: &str, context_files: &[&str]) -> Vec<PlanTask> {
    vec![PlanTask {
        task_id: "t1".into(),
        agent: agent.into(),
        task: "Task".into(),
        depends_on: vec![],
        issue_id: None,
        context_files: context_files
            .iter()
            .map(|path| (*path).to_string())
            .collect(),
    }]
}

#[tokio::test]
async fn t_v0c_1_persisted_submit_path_does_not_direct_dispatch() {
    if !br_available() {
        eprintln!(
            "skipping t_v0c_1_persisted_submit_path_does_not_direct_dispatch: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (server, mut channel) = McpCallbackServer::new(
        &session_id,
        Some(pm),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "Persisted Epic",
            "tasks": persisted_task("codex"),
        }))
        .await;
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );

    let recv = tokio::time::timeout(Duration::from_millis(100), channel.request_rx.recv()).await;
    assert!(
        recv.is_err(),
        "persisted submit must not dispatch directly through the delegation channel"
    );
}

#[tokio::test]
async fn t_v0c_2_reconciler_dispatch_writes_label_and_dispatch_audit() {
    if !br_available() {
        eprintln!("skipping t_v0c_2_reconciler_dispatch_writes_label_and_dispatch_audit: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-2",
        "Plan Two",
        None,
        &persisted_task("codex"),
    )
    .await
    .expect("build epic subgraph");
    let task_id = subgraph.task_map.get("t1").expect("task id").clone();
    add_labels(
        pm.as_ref(),
        &subgraph.epic_id,
        &[labels::plan_owner("brain")],
    )
    .await;

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
        }),
        Some("plan-2".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");
    let _request = delegation_rx.recv().await.expect("dispatch request");
    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:delegation-id:")));

    let audits = collect_audits(&run_br_json(dir.path(), &["comments", "list", &task_id]).unwrap());
    assert!(audits
        .iter()
        .any(|audit| matches!(audit, AuditSentinelKind::Dispatch { .. })));
}

#[tokio::test]
async fn t_v0c_3_completion_success_writes_ready_for_review_and_completion() {
    if !br_available() {
        eprintln!("skipping t_v0c_3_completion_success_writes_ready_for_review_and_completion: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let task_id = extract_id(&run_br_json(dir.path(), &["create", "Task", "-t", "task"]).unwrap());

    pm.update_issue(
        &task_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::delegation_id("del-A")],
            ..Default::default()
        },
    )
    .await
    .expect("seed delegation label");

    spur_mcp::plan::persist_completion_result(
        pm.as_ref(),
        &task_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-3",
        "del-A",
        CompletionState::AwaitingReview,
        CompletionAuditFields {
            worker_branch: Some("feat/task".into()),
            result_summary: Some("worker finished".into()),
            ..Default::default()
        },
        false,
    )
    .await
    .expect("persist completion");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(issue.labels.contains(&labels::READY_FOR_REVIEW.to_string()));

    let audits = collect_audits(&run_br_json(dir.path(), &["comments", "list", &task_id]).unwrap());
    assert!(audits.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            completion_state: CompletionState::AwaitingReview,
            ..
        }
    )));
}

#[tokio::test]
async fn t_v0c_4_reject_closes_task_and_blocks_watcher() {
    if !br_available() {
        eprintln!("skipping t_v0c_4_reject_closes_task_and_blocks_watcher: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let task_id = extract_id(&run_br_json(dir.path(), &["create", "Task", "-t", "task"]).unwrap());
    pm.update_issue(
        &task_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::READY_FOR_REVIEW.to_string()],
            ..Default::default()
        },
    )
    .await
    .expect("seed ready-for-review label");

    let state = Arc::new(Mutex::new(PlanState {
        plan_id: "plan-4".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "codex".into(),
                task: "Task".into(),
                depends_on: Vec::new(),
                issue_id: Some(task_id.clone()),
                context_files: Vec::new(),
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("done".into()),
            },
            result: None,
            worker_branch: Some("feat/task".into()),
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: Some("del-A".into()),
            dispatched_base_oid: None,
        }],
        brain_session_id: BrainSessionId::new(SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: Some("bd-epic".into()),
    }));

    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = pm.clone();
    let _ = spur_mcp::plan::handle_review_task(
        state,
        "plan-4",
        "t1",
        "reject",
        Some("needs more work"),
        Some(pm_arc),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("reject");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert_eq!(issue.status, "closed");
    assert!(issue.labels.contains(&labels::REVIEW_REJECTED.to_string()));
    assert!(!issue.labels.contains(&labels::READY_FOR_REVIEW.to_string()));
}

#[tokio::test]
async fn t_v0c_5_request_changes_stays_open_and_reconciler_redispatches() {
    if !br_available() {
        eprintln!("skipping t_v0c_5_request_changes_stays_open_and_reconciler_redispatches: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-5",
        "Plan Five",
        None,
        &persisted_task("codex"),
    )
    .await
    .expect("build epic subgraph");
    let task_id = subgraph.task_map.get("t1").expect("task id").clone();
    add_labels(
        pm.as_ref(),
        &subgraph.epic_id,
        &[labels::plan_owner("brain")],
    )
    .await;
    add_labels(
        pm.as_ref(),
        &task_id,
        &[labels::READY_FOR_REVIEW.to_string()],
    )
    .await;

    let state = Arc::new(Mutex::new(PlanState {
        plan_id: "plan-5".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "codex".into(),
                task: "Task".into(),
                depends_on: Vec::new(),
                issue_id: Some(task_id.clone()),
                context_files: Vec::new(),
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("done".into()),
            },
            result: None,
            worker_branch: Some("feat/task".into()),
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: Some("del-A".into()),
            dispatched_base_oid: None,
        }],
        brain_session_id: BrainSessionId::new(SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: Some("bd-epic".into()),
    }));

    let pm_arc: Arc<dyn spur_mcp::plan::PmLike> = pm.clone();
    let _ = spur_mcp::plan::handle_review_task(
        state,
        "plan-5",
        "t1",
        "request_changes",
        Some("retry"),
        Some(pm_arc),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
    )
    .await
    .expect("request_changes");

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: None,
            materializer: test_materializer(),
        }),
        Some("plan-5".into()),
        common::server_builder::pro_feature_gate(),
    );

    reconciler.tick_once().await.expect("tick_once");
    let request = delegation_rx.recv().await.expect("redispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
}

#[tokio::test]
async fn t_v0c_6_watcher_uses_projected_plan_state_not_stub_state() {
    if !br_available() {
        eprintln!(
            "skipping t_v0c_6_watcher_uses_projected_plan_state_not_stub_state: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-6",
        "Plan Six",
        None,
        &persisted_task_with_context("codex", &["docs/spec.md", "src/lib.rs"]),
    )
    .await
    .expect("build epic subgraph");
    let task_id = subgraph.task_map.get("t1").expect("task id").clone();

    add_labels(
        pm.as_ref(),
        &task_id,
        &[
            labels::READY_FOR_REVIEW.to_string(),
            labels::signal_kind("scope-drift"),
        ],
    )
    .await;
    pm.advanced()
        .expect("advanced beads surface")
        .add_comment(
            &task_id,
            &signals::encode_comment(&WorkerSignal::ScopeDrift {
                signal_id: Uuid::new_v4(),
                severity: 0.82,
                reason: "auth refactor pulls in 4 new subsystems".into(),
                estimated_subtasks: Some(3),
            }),
        )
        .await
        .expect("signal comment");

    let watcher = SignalWatcher::new(
        Arc::clone(&pm),
        ScopeDriftSplitProposer::default(),
        TrivialScorer,
        common::server_builder::pro_feature_gate(),
    );
    watcher.tick_once().await.expect("tick_once");

    let issue = pm.get_issue(&task_id).await.expect("get issue");
    assert!(issue
        .labels
        .iter()
        .any(|label| label.starts_with("spur:signal-processed:")));

    let projected = spur_mcp::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        "plan-6",
        common::server_builder::pro_feature_gate().as_ref(),
    )
    .await
    .expect("projected split plan");
    assert!(
        projected.tasks.len() > 1,
        "split children should reappear after restart projection"
    );
    let child = projected
        .tasks
        .iter()
        .find(|task| task.spec.issue_id.as_deref() != Some(task_id.as_str()))
        .expect("split child");
    assert_eq!(
        child.spec.context_files,
        vec!["docs/spec.md".to_string(), "src/lib.rs".to_string()]
    );
    assert!(
        matches!(child.status, PlanTaskStatus::Ready),
        "split child should be dispatchable after parent supersession"
    );
}

#[tokio::test]
async fn t_v0c_7_cache_miss_rehydrates_persisted_plan_from_beads() {
    if !br_available() {
        eprintln!(
            "skipping t_v0c_7_cache_miss_rehydrates_persisted_plan_from_beads: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let _subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-7",
        "Plan Seven",
        None,
        &persisted_task_with_context("codex", &["docs/spec.md", "src/lib.rs"]),
    )
    .await
    .expect("build epic subgraph");

    let projected = spur_mcp::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        "plan-7",
        common::server_builder::pro_feature_gate().as_ref(),
    )
    .await
    .expect("projected plan");
    assert_eq!(projected.plan_id, "plan-7");
    assert_eq!(projected.tasks.len(), 1);
    assert_eq!(
        projected.tasks[0].spec.context_files,
        vec!["docs/spec.md".to_string(), "src/lib.rs".to_string()]
    );
}

#[tokio::test]
async fn t_v0c_8_orphaned_dispatch_requeues_and_late_completion_is_superseded() {
    if !br_available() {
        eprintln!("skipping t_v0c_8_orphaned_dispatch_requeues_and_late_completion_is_superseded: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let task_id = extract_id(&run_br_json(dir.path(), &["create", "Task", "-t", "task"]).unwrap());
    add_labels(
        pm.as_ref(),
        &task_id,
        &[
            labels::plan_id("plan-8"),
            labels::plan_task_id("t1"),
            labels::agent("codex"),
            labels::delegation_id("del-stale"),
        ],
    )
    .await;

    let cleared = resolve_dispatch_orphan(
        Arc::clone(&pm),
        common::server_builder::pro_feature_gate(),
        &task_id,
    )
    .await
    .expect("resolve orphan");
    assert!(cleared, "dispatch orphan should be cleared");

    spur_mcp::plan::persist_completion_result(
        pm.as_ref(),
        &task_id,
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-8",
        "del-stale",
        CompletionState::Superseded,
        CompletionAuditFields {
            worker_branch: Some("feat/stale".into()),
            result_summary: Some("late completion".into()),
            ..Default::default()
        },
        false,
    )
    .await
    .expect("persist superseded completion");

    let audits = collect_audits(&run_br_json(dir.path(), &["comments", "list", &task_id]).unwrap());
    assert!(audits.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion {
            completion_state: CompletionState::Superseded,
            superseded: true,
            ..
        }
    )));
}

#[tokio::test]
async fn t_v0c_9_orphaned_mutation_plan_is_compensated_before_new_signals() {
    if !br_available() {
        eprintln!("skipping t_v0c_9_orphaned_mutation_plan_is_compensated_before_new_signals: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let task_id = extract_id(&run_br_json(dir.path(), &["create", "Task", "-t", "task"]).unwrap());
    pm.advanced()
        .expect("advanced")
        .add_comment(
            &task_id,
            &audit_sentinel::encode_comment(&AuditSentinelKind::MutationPlan {
                mutation_id: "11111111-1111-1111-1111-111111111111".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-1".into()),
                trigger_task_id: task_id.clone(),
            }),
        )
        .await
        .expect("mutation plan");

    compensate_mutation_orphans(
        Arc::clone(&pm),
        common::server_builder::pro_feature_gate(),
        &task_id,
    )
    .await
    .expect("compensate mutation orphan");

    let audits = collect_audits(&run_br_json(dir.path(), &["comments", "list", &task_id]).unwrap());
    assert!(audits.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::MutationInvariantViolation {
            violation,
            rollback_status,
            ..
        } if violation == "restart-orphan" && rollback_status == "compensated"
    )));
}

#[tokio::test]
async fn t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch() {
    if !br_available() {
        eprintln!(
            "skipping t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("t_v0c_10_startup_reclaims_mid_plan_and_continues_dispatch");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-10",
        "Plan Ten",
        None,
        &persisted_task("codex"),
    )
    .await
    .expect("build epic subgraph");
    let task_id = subgraph.task_map.get("t1").expect("task id").clone();
    add_labels(
        pm.as_ref(),
        &subgraph.epic_id,
        &[labels::plan_owner("brain")],
    )
    .await;
    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, mut channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_reconciler_enabled(true, Some(Arc::new(Notify::new())));
    server.set_repo_root(dir.path().to_path_buf());

    let server = Arc::new(server);
    let (_url, handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");
    server.fast_forward_reconciler();
    let request = tokio::time::timeout(Duration::from_secs(5), channel.request_rx.recv())
        .await
        .expect("reconciler dispatch timeout")
        .expect("dispatch request");
    handle.abort();

    assert!(server.__test_active_plan_count().await > 0);
    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
}

#[tokio::test]
async fn t_v0c_11_startup_reclaim_clears_stale_dispatch_before_redispatch() {
    if !br_available() {
        eprintln!(
            "skipping t_v0c_11_startup_reclaim_clears_stale_dispatch_before_redispatch: `br` not on PATH"
        );
        return;
    }
    skip_if_no_loopback!("t_v0c_11_startup_reclaim_clears_stale_dispatch_before_redispatch");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "plan-11",
        "Plan Eleven",
        None,
        &persisted_task("codex"),
    )
    .await
    .expect("build epic subgraph");
    let task_id = subgraph.task_map.get("t1").expect("task id").clone();
    add_labels(
        pm.as_ref(),
        &subgraph.epic_id,
        &[labels::plan_owner("brain")],
    )
    .await;
    let stale_delegation_id = "del-stale";
    add_labels(
        pm.as_ref(),
        &task_id,
        &[labels::delegation_id(stale_delegation_id)],
    )
    .await;

    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, mut channel) = McpCallbackServer::new(
        &session_id,
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_reconciler_enabled(true, None);
    server.set_repo_root(dir.path().to_path_buf());

    let server = Arc::new(server);
    let (_url, handle) = Arc::clone(&server)
        .start()
        .await
        .expect("start server (loopback bind already probed at fn entry)");

    let issue_after_start = pm
        .get_issue(&task_id)
        .await
        .expect("get issue after startup reclaim");
    assert!(
        !issue_after_start
            .labels
            .contains(&labels::delegation_id(stale_delegation_id)),
        "startup reclaim should clear stale dispatch intent before redispatch"
    );

    let request = tokio::time::timeout(Duration::from_secs(5), channel.request_rx.recv())
        .await
        .expect("redispatch timeout")
        .expect("dispatch request");

    let issue_after_redispatch = pm
        .get_issue(&task_id)
        .await
        .expect("get issue after redispatch");
    let fresh_delegation_id = request.id.as_str().to_string();
    assert_eq!(request.issue_id.as_deref(), Some(task_id.as_str()));
    assert_ne!(
        fresh_delegation_id, stale_delegation_id,
        "redispatch should mint a fresh delegation id"
    );
    assert!(
        issue_after_redispatch
            .labels
            .contains(&labels::delegation_id(&fresh_delegation_id)),
        "reconciler should persist fresh dispatch intent for the new delegation"
    );
    assert!(
        !issue_after_redispatch
            .labels
            .contains(&labels::delegation_id(stale_delegation_id)),
        "stale dispatch intent must stay cleared after redispatch"
    );

    let audits = spur_mcp::plan::projector::collect_sorted_audits(
        pm.advanced()
            .expect("advanced beads surface")
            .list_comments(&task_id)
            .await
            .expect("task comments"),
    );
    let orphan_cleared_idx = audits
        .iter()
        .position(|audit| {
            matches!(
                audit,
                AuditSentinelKind::DispatchOrphanCleared { delegation_id, .. }
                    if delegation_id == stale_delegation_id
            )
        })
        .expect("startup reclaim should record orphan clearing");
    let redispatch_idx = audits
        .iter()
        .position(|audit| {
            matches!(
                audit,
                AuditSentinelKind::Dispatch { delegation_id, .. }
                    if delegation_id == &fresh_delegation_id
            )
        })
        .expect("redispatch should record a fresh dispatch audit");
    assert!(
        orphan_cleared_idx < redispatch_idx,
        "stale dispatch should be cleared before a fresh redispatch is recorded"
    );

    handle.abort();
}
