use super::{integrate_plan_branches, resolve_plan_base, run_git_capture, JsonRpcResponse};
use crate::plan::audit_sentinel::{encode_comment, AuditSentinelKind, CompletionState};
use crate::plan::{PlanMergeState, PlanTask};
use serde_json::{json, Value};
use spur_pm::test_workspace::TestBeadsWorkspace;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

async fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    run_git_capture(dir.path(), None, &["init", "-q"])
        .await
        .expect("git init");
    run_git_capture(dir.path(), None, &["config", "user.email", "test@spur"])
        .await
        .expect("git config user.email");
    run_git_capture(dir.path(), None, &["config", "user.name", "spur-test"])
        .await
        .expect("git config user.name");
    dir
}

async fn commit_file(repo: &std::path::Path, path: &str, body: &str, message: &str) {
    std::fs::write(repo.join(path), body).expect("write file");
    run_git_capture(repo, None, &["add", path])
        .await
        .expect("git add");
    run_git_capture(repo, None, &["commit", "-q", "-m", message])
        .await
        .expect("git commit");
}

struct PersistedMergeFixture {
    _dir: TempDir,
    _beads: TestBeadsWorkspace,
    pm: Arc<spur_pm::PmService>,
    server: super::McpCallbackServer,
    plan_id: String,
    epic_id: String,
}

async fn setup_persisted_merge_ready_plan(
    plan_id: &str,
    clear_cache: bool,
) -> PersistedMergeFixture {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let (beads, pm) = super::init_beads_pm(dir.path()).await;

    run_git_capture(
        dir.path(),
        None,
        &["branch", "spur/brain-snapshot-test", "HEAD"],
    )
    .await
    .expect("snapshot branch");
    let base_snapshot_oid = run_git_capture(
        dir.path(),
        None,
        &["rev-parse", "--verify", "spur/brain-snapshot-test"],
    )
    .await
    .expect("snapshot oid");

    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-a",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "spur/brain-snapshot-test"],
    )
    .await
    .expect("checkout snapshot branch");

    let tasks = vec![PlanTask {
        task_id: "task-a".into(),
        agent: "codex".into(),
        task: "Integrate worker branch".into(),
        depends_on: Vec::new(),
        issue_id: None,
        issue_title: None,
        context_files: Vec::new(),
    }];
    let feature_gate = super::pro_feature_gate();
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Epic",
        None,
        &tasks,
    )
    .await
    .expect("build epic subgraph");
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::plan_owner("brain")],
            ..Default::default()
        },
    )
    .await
    .expect("stamp plan_owner label on fixture epic");
    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("pro gate");
    let adv = pm.advanced().expect("advanced beads backend");
    crate::emit_plan_submit_audit(
        adv,
        plan_id,
        &subgraph,
        crate::PlanSubmitAuditContext {
            base_snapshot_branch: Some("spur/brain-snapshot-test"),
            base_snapshot_oid: Some(base_snapshot_oid.as_str()),
            execution_mode: Some("submit_plan"),
            brain_session_id: None,
            explicit_base: None,
        },
    )
    .await;

    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");
    adv.add_comment(
        &task_issue_id,
        &encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker-a".into()),
            result_summary: Some("worker branch ready".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        }),
    )
    .await
    .expect("completion audit");
    adv.add_comment(
        &task_issue_id,
        &encode_comment(&AuditSentinelKind::Approval {
            delegation_id: "del-1".into(),
        }),
    )
    .await
    .expect("approval audit");
    pm.update_issue(
        &task_issue_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task issue");

    let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
    let continuation_ctx = super::DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mut server, _channel) = super::McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::clone(&feature_gate),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let projected = crate::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        plan_id,
        feature_gate.as_ref(),
    )
    .await
    .expect("project persisted plan");
    assert_eq!(
        crate::plan::build_plan_status(plan_id, &projected)["ready_to_merge"],
        Value::Bool(true)
    );
    server.install_projected_plan(projected, false).await;
    if clear_cache {
        server.active_plans.lock().await.remove(plan_id);
    }

    PersistedMergeFixture {
        _dir: dir,
        _beads: beads,
        pm,
        server,
        plan_id: plan_id.to_string(),
        epic_id: subgraph.epic_id,
    }
}

async fn setup_persisted_retried_plan(plan_id: &str, clear_cache: bool) -> PersistedMergeFixture {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let (beads, pm) = super::init_beads_pm(dir.path()).await;

    run_git_capture(
        dir.path(),
        None,
        &["branch", "spur/brain-snapshot-test", "HEAD"],
    )
    .await
    .expect("snapshot branch");
    let base_snapshot_oid = run_git_capture(
        dir.path(),
        None,
        &["rev-parse", "--verify", "spur/brain-snapshot-test"],
    )
    .await
    .expect("snapshot oid");

    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-a1",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-a1");
    commit_file(
        dir.path(),
        "worker-1.txt",
        "attempt-1\n",
        "worker attempt 1",
    )
    .await;

    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "spur/brain-snapshot-test"],
    )
    .await
    .expect("checkout snapshot branch");
    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-a2",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-a2");
    commit_file(
        dir.path(),
        "worker-2.txt",
        "attempt-2\n",
        "worker attempt 2",
    )
    .await;

    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "spur/brain-snapshot-test"],
    )
    .await
    .expect("checkout snapshot branch");

    let tasks = vec![PlanTask {
        task_id: "task-a".into(),
        agent: "codex".into(),
        task: "Integrate worker branch".into(),
        depends_on: Vec::new(),
        issue_id: None,
        issue_title: None,
        context_files: Vec::new(),
    }];
    let feature_gate = super::pro_feature_gate();
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Epic",
        None,
        &tasks,
    )
    .await
    .expect("build epic subgraph");
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::plan_owner("brain")],
            ..Default::default()
        },
    )
    .await
    .expect("stamp plan_owner label on fixture epic");
    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("pro gate");
    let adv = pm.advanced().expect("advanced beads backend");
    crate::emit_plan_submit_audit(
        adv,
        plan_id,
        &subgraph,
        crate::PlanSubmitAuditContext {
            base_snapshot_branch: Some("spur/brain-snapshot-test"),
            base_snapshot_oid: Some(base_snapshot_oid.as_str()),
            execution_mode: Some("submit_plan"),
            brain_session_id: None,
            explicit_base: None,
        },
    )
    .await;

    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");
    for audit in [
        AuditSentinelKind::Dispatch {
            delegation_id: "del-1".into(),
            worker: "codex".into(),
            attempt: 1,
        },
        AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker-a1".into()),
            result_summary: Some("attempt 1 summary".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        },
        AuditSentinelKind::Rejection {
            delegation_id: "del-1".into(),
            feedback: "needs changes".into(),
        },
        AuditSentinelKind::Dispatch {
            delegation_id: "del-2".into(),
            worker: "codex".into(),
            attempt: 2,
        },
        AuditSentinelKind::Completion {
            delegation_id: "del-2".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker-a2".into()),
            result_summary: Some("attempt 2 summary".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        },
        AuditSentinelKind::Approval {
            delegation_id: "del-2".into(),
        },
    ] {
        adv.add_comment(&task_issue_id, &encode_comment(&audit))
            .await
            .expect("attempt audit");
    }
    pm.update_issue(
        &task_issue_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("close task issue");

    let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
    let continuation_ctx = super::DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mut server, _channel) = super::McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::clone(&feature_gate),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let projected = crate::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        plan_id,
        feature_gate.as_ref(),
    )
    .await
    .expect("project persisted plan");
    assert_eq!(
        crate::plan::build_plan_status(plan_id, &projected)["ready_to_merge"],
        Value::Bool(true)
    );
    server.install_projected_plan(projected, false).await;
    if clear_cache {
        server.active_plans.lock().await.remove(plan_id);
    }

    PersistedMergeFixture {
        _dir: dir,
        _beads: beads,
        pm,
        server,
        plan_id: plan_id.to_string(),
        epic_id: subgraph.epic_id,
    }
}

#[tokio::test]
async fn reconstruct_historical_attempts_classifies_retry_requested_as_worker_failure_recovery() {
    let dir = init_repo().await;
    let (_beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();
    let task_issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "Retry reconstruction task".into(),
            description: Some("task body".into()),
            ..Default::default()
        })
        .await
        .expect("create task issue");
    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("pro gate");
    let adv = pm.advanced().expect("advanced beads backend");

    for audit in [
        AuditSentinelKind::Dispatch {
            delegation_id: "del-1".into(),
            worker: "codex".into(),
            attempt: 1,
        },
        AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::Failed,
            superseded: false,
            worker_branch: Some("spur/worker-failed".into()),
            result_summary: Some("worker crashed".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        },
        AuditSentinelKind::RetryRequested {
            delegation_id: "del-1".into(),
            attempt: 1,
            error: "worker crashed".into(),
            worker_branch: Some("spur/worker-failed".into()),
            amended_prompt_summary: None,
        },
        AuditSentinelKind::Dispatch {
            delegation_id: "del-2".into(),
            worker: "codex".into(),
            attempt: 2,
        },
    ] {
        adv.add_comment(&task_issue_id, &encode_comment(&audit))
            .await
            .expect("attempt audit");
    }

    let history = super::reconstruct_historical_attempts(
        pm.as_ref(),
        feature_gate.as_ref(),
        &task_issue_id,
        2,
    )
    .await
    .expect("reconstruct history");

    assert_eq!(history.len(), 1);
    let attempt = &history[0];
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.worker_branch.as_deref(), Some("spur/worker-failed"));
    assert_eq!(
        attempt.kind(),
        crate::plan::AttemptRecordKind::WorkerFailureRecovery
    );
}

async fn setup_cached_overlay_diff_plan(
    plan_id: &str,
    use_dispatched_base_oid: bool,
) -> PersistedMergeFixture {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let (beads, pm) = super::init_beads_pm(dir.path()).await;

    run_git_capture(
        dir.path(),
        None,
        &["branch", "spur/brain-snapshot-test", "HEAD"],
    )
    .await
    .expect("snapshot branch");
    let base_snapshot_oid = run_git_capture(
        dir.path(),
        None,
        &["rev-parse", "--verify", "spur/brain-snapshot-test"],
    )
    .await
    .expect("snapshot oid");

    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-a",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-a");
    commit_file(dir.path(), "foo.rs", "fn foo() {}\n", "task a").await;

    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "spur/brain-snapshot-test"],
    )
    .await
    .expect("checkout snapshot");
    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-b",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-b");
    run_git_capture(dir.path(), None, &["cherry-pick", "spur/worker-a"])
        .await
        .expect("apply task-a overlay");
    let t2_dispatched_base_oid =
        run_git_capture(dir.path(), None, &["rev-parse", "--verify", "HEAD"])
            .await
            .expect("overlay base oid");
    commit_file(dir.path(), "bar.rs", "fn bar() {}\n", "task b").await;
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "spur/brain-snapshot-test"],
    )
    .await
    .expect("checkout snapshot");

    let tasks = vec![
        PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Create foo".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        },
        PlanTask {
            task_id: "task-b".into(),
            agent: "codex".into(),
            task: "Create bar".into(),
            depends_on: vec!["task-a".into()],
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        },
    ];
    let feature_gate = super::pro_feature_gate();
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Epic",
        None,
        &tasks,
    )
    .await
    .expect("build epic subgraph");
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::plan_owner("brain")],
            ..Default::default()
        },
    )
    .await
    .expect("stamp plan_owner label on fixture epic");

    let task_a_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task-a issue id");
    let task_b_issue_id = subgraph
        .task_map
        .get("task-b")
        .cloned()
        .expect("task-b issue id");
    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("pro gate");
    let adv = pm.advanced().expect("advanced beads backend");
    crate::emit_plan_submit_audit(
        adv,
        plan_id,
        &subgraph,
        crate::PlanSubmitAuditContext {
            base_snapshot_branch: Some("spur/brain-snapshot-test"),
            base_snapshot_oid: Some(base_snapshot_oid.as_str()),
            execution_mode: Some("submit_plan"),
            brain_session_id: None,
            explicit_base: None,
        },
    )
    .await;
    for (issue_id, audit) in [
        (
            task_a_issue_id.as_str(),
            AuditSentinelKind::Completion {
                delegation_id: "del-a".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-a".into()),
                result_summary: Some("foo ready".into()),
                artifact_uri: None,
                dispatched_base_oid: Some(base_snapshot_oid.clone()),
            },
        ),
        (
            task_b_issue_id.as_str(),
            AuditSentinelKind::Completion {
                delegation_id: "del-b".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker-b".into()),
                result_summary: Some("bar ready".into()),
                artifact_uri: None,
                dispatched_base_oid: use_dispatched_base_oid.then_some(t2_dispatched_base_oid),
            },
        ),
    ] {
        adv.add_comment(issue_id, &encode_comment(&audit))
            .await
            .expect("completion audit");
        let delegation_id = match audit {
            AuditSentinelKind::Completion { delegation_id, .. } => delegation_id,
            _ => unreachable!("test fixture only emits completions"),
        };
        adv.add_comment(
            issue_id,
            &encode_comment(&AuditSentinelKind::Approval { delegation_id }),
        )
        .await
        .expect("approval audit");
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close task issue");
    }

    let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
    let continuation_ctx = super::DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let (mut server, _channel) = super::McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        None,
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::clone(&feature_gate),
    );
    server.set_repo_root(dir.path().to_path_buf());
    let projected = crate::plan::projector::project_plan_from_beads(
        pm.as_ref(),
        plan_id,
        feature_gate.as_ref(),
    )
    .await
    .expect("project persisted plan");
    server.install_projected_plan(projected, false).await;

    PersistedMergeFixture {
        _dir: dir,
        _beads: beads,
        pm,
        server,
        plan_id: plan_id.to_string(),
        epic_id: subgraph.epic_id,
    }
}

fn decode_merge_status(response: super::JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "merge_plan should succeed: {:?}",
        response.error
    );

    let result = response.result.expect("merge_plan result");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("merge_plan text response");
    serde_json::from_str(text).expect("merge_plan status JSON")
}

fn decode_task_diff_response(text: &str) -> Value {
    serde_json::from_str(text).expect("get_task_diff response JSON")
}

fn task_diff_text(response: JsonRpcResponse) -> String {
    let result = response
        .result
        .expect("get_task_diff JsonRpcResponse must be Ok");
    result["content"][0]["text"]
        .as_str()
        .expect("get_task_diff response must carry content[0].text")
        .to_string()
}

#[derive(Clone, Default)]
struct CapturedWarnings {
    events: Arc<Mutex<Vec<String>>>,
}

impl CapturedWarnings {
    fn contains(&self, needle: &str) -> bool {
        self.events
            .lock()
            .expect("warning capture lock")
            .iter()
            .any(|event| event.contains(needle))
    }
}

impl tracing::Subscriber for CapturedWarnings {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::WARN
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }

        struct Visitor {
            fields: String,
        }

        impl tracing::field::Visit for Visitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if !self.fields.is_empty() {
                    self.fields.push(' ');
                }
                self.fields.push_str(field.name());
                self.fields.push('=');
                self.fields.push_str(&format!("{value:?}"));
            }
        }

        let mut visitor = Visitor {
            fields: String::new(),
        };
        event.record(&mut visitor);
        self.events
            .lock()
            .expect("warning capture lock")
            .push(visitor.fields);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test]
async fn resolve_plan_base_captures_oid() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;

    let repo_root = dir.path().to_path_buf();
    let snapshot = resolve_plan_base(Some(&repo_root), None)
        .await
        .expect("resolve_plan_base");

    let expected_oid = run_git_capture(
        dir.path(),
        None,
        &[
            "rev-parse",
            "--verify",
            snapshot.branch.as_deref().expect("snapshot branch"),
        ],
    )
    .await
    .expect("rev-parse snapshot branch");

    assert_eq!(snapshot.oid.as_deref(), Some(expected_oid.as_str()));
}

#[tokio::test]
async fn integrate_plan_branches_succeeds_without_touching_active_checkout() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    run_git_capture(
        dir.path(),
        None,
        &["branch", "spur/brain-snapshot-test", "HEAD"],
    )
    .await
    .expect("snapshot branch");

    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-a",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-a");
    commit_file(dir.path(), "a.txt", "alpha\n", "worker a").await;

    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "spur/brain-snapshot-test"],
    )
    .await
    .expect("checkout snapshot");
    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-b",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-b");
    commit_file(dir.path(), "b.txt", "beta\n", "worker b").await;

    let outcome = integrate_plan_branches(
        dir.path(),
        "spur/brain-snapshot-test",
        "spur/plan-merge-test",
        &[
            ("task-a".into(), "spur/worker-a".into()),
            ("task-b".into(), "spur/worker-b".into()),
        ],
    )
    .await
    .expect("integration should succeed");

    match outcome {
        PlanMergeState::Succeeded {
            merge_branch,
            merged_task_ids,
        } => {
            assert_eq!(merge_branch, "spur/plan-merge-test");
            assert_eq!(merged_task_ids, vec!["task-a", "task-b"]);
        }
        other => panic!("expected successful merge state, got {other:?}"),
    }

    let a_contents = run_git_capture(dir.path(), None, &["show", "spur/plan-merge-test:a.txt"])
        .await
        .expect("show merged a.txt");
    let b_contents = run_git_capture(dir.path(), None, &["show", "spur/plan-merge-test:b.txt"])
        .await
        .expect("show merged b.txt");
    assert_eq!(a_contents, "alpha");
    assert_eq!(b_contents, "beta");
}

#[tokio::test]
async fn integrate_plan_branches_reports_conflict_and_keeps_partial_branch() {
    let dir = init_repo().await;
    commit_file(dir.path(), "shared.txt", "base\n", "seed").await;
    run_git_capture(
        dir.path(),
        None,
        &["branch", "spur/brain-snapshot-test", "HEAD"],
    )
    .await
    .expect("snapshot branch");

    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-a",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-a");
    commit_file(dir.path(), "shared.txt", "worker-a\n", "worker a").await;

    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "spur/brain-snapshot-test"],
    )
    .await
    .expect("checkout snapshot");
    run_git_capture(
        dir.path(),
        None,
        &[
            "checkout",
            "-q",
            "-b",
            "spur/worker-b",
            "spur/brain-snapshot-test",
        ],
    )
    .await
    .expect("checkout worker-b");
    commit_file(dir.path(), "shared.txt", "worker-b\n", "worker b").await;

    let outcome = integrate_plan_branches(
        dir.path(),
        "spur/brain-snapshot-test",
        "spur/plan-merge-conflict",
        &[
            ("task-a".into(), "spur/worker-a".into()),
            ("task-b".into(), "spur/worker-b".into()),
        ],
    )
    .await
    .expect("integration should return conflict state");

    match outcome {
        PlanMergeState::Conflict {
            merge_branch,
            conflict_task_id,
            conflict_worker_branch,
            merged_task_ids,
            files,
        } => {
            assert_eq!(merge_branch, "spur/plan-merge-conflict");
            assert_eq!(conflict_task_id, "task-b");
            assert_eq!(conflict_worker_branch, "spur/worker-b");
            assert_eq!(merged_task_ids, vec!["task-a"]);
            assert!(
                files.iter().any(|f| f == "shared.txt"),
                "conflict files should mention shared.txt: {files:?}"
            );
        }
        other => panic!("expected conflict merge state, got {other:?}"),
    }

    let merged_contents = run_git_capture(
        dir.path(),
        None,
        &["show", "spur/plan-merge-conflict:shared.txt"],
    )
    .await
    .expect("show partial merge branch");
    assert_eq!(merged_contents, "worker-a");
}

#[tokio::test]
async fn merge_plan_rehydrates_when_cache_missing() {
    let fixture = setup_persisted_merge_ready_plan("plan-merge-recover", true).await;

    let response = fixture
        .server
        .handle_merge_plan(Value::Null, json!({ "plan_id": fixture.plan_id }))
        .await;
    let status = decode_merge_status(response);
    assert_eq!(status["merge"]["status"], "succeeded");
    assert_eq!(status["ready_to_merge"], true);
    assert_eq!(status["merge"]["merged_task_ids"], json!(["task-a"]));
}

#[tokio::test]
async fn merge_plan_clears_integration_pending_on_success() {
    let fixture = setup_persisted_merge_ready_plan("plan-merge-clear-label", true).await;
    fixture
        .pm
        .update_issue(
            &fixture.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::INTEGRATION_PENDING.to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("add integration-pending label");

    let response = fixture
        .server
        .handle_merge_plan(Value::Null, json!({ "plan_id": fixture.plan_id }))
        .await;
    let status = decode_merge_status(response);
    assert_eq!(status["merge"]["status"], "succeeded");

    let epic = fixture
        .pm
        .get_issue(&fixture.epic_id)
        .await
        .expect("get epic");
    assert!(
        !epic
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::INTEGRATION_PENDING),
        "merge_plan should clear integration-pending: {:?}",
        epic.labels
    );
}

#[tokio::test]
async fn get_task_diff_rehydrates_latest_attempt_when_cache_missing() {
    let fixture = setup_persisted_merge_ready_plan("plan-diff-recover", true).await;

    let raw = fixture
        .server
        .handle_get_task_diff(
            json!(1),
            json!({
                "plan_id": fixture.plan_id,
                "task_id": "task-a",
            }),
        )
        .await;
    let text = task_diff_text(raw);
    let response = decode_task_diff_response(&text);

    assert_eq!(response["worker_branch"], "spur/worker-a");
    assert_eq!(response["summary"], "worker branch ready");
    assert!(
        response["diff"]
            .as_str()
            .map(|diff| diff.contains("worker.txt"))
            .unwrap_or(false),
        "latest-attempt cache miss should rebuild full diff text: {response}"
    );
}

#[tokio::test]
async fn get_task_diff_uses_dispatched_base_oid_when_present() {
    let fixture = setup_cached_overlay_diff_plan("plan-diff-overlay", true).await;

    let raw = fixture
        .server
        .handle_get_task_diff(
            json!(1),
            json!({
                "plan_id": fixture.plan_id,
                "task_id": "task-b",
            }),
        )
        .await;
    let text = task_diff_text(raw);
    let response = decode_task_diff_response(&text);
    let diff = response["diff"].as_str().expect("diff text");

    assert!(
        diff.contains("bar.rs"),
        "task-b diff should include its own change: {diff}"
    );
    assert!(
        !diff.contains("foo.rs"),
        "task-b diff must not include inherited task-a overlay: {diff}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn get_task_diff_warns_and_falls_back_for_legacy_task() {
    let fixture = setup_cached_overlay_diff_plan("plan-diff-legacy", false).await;
    let warnings = CapturedWarnings::default();
    let _guard = tracing::subscriber::set_default(warnings.clone());

    let raw = fixture
        .server
        .handle_get_task_diff(
            json!(1),
            json!({
                "plan_id": fixture.plan_id,
                "task_id": "task-b",
            }),
        )
        .await;
    let text = task_diff_text(raw);
    let response = decode_task_diff_response(&text);
    let diff = response["diff"].as_str().expect("diff text");

    assert!(
        diff.contains("foo.rs"),
        "legacy fallback should retain the base snapshot range: {diff}"
    );
    assert!(
        diff.contains("bar.rs"),
        "legacy fallback should include the worker change: {diff}"
    );
    assert!(
        warnings.contains("dispatched_base_oid"),
        "legacy fallback should emit a warning mentioning dispatched_base_oid"
    );
}

#[tokio::test]
async fn get_task_diff_historical_attempts_remain_summary_only() {
    let fixture = setup_persisted_retried_plan("plan-diff-history", true).await;

    let raw = fixture
        .server
        .handle_get_task_diff(
            json!(1),
            json!({
                "plan_id": fixture.plan_id,
                "task_id": "task-a",
                "attempt": 1,
            }),
        )
        .await;
    let text = task_diff_text(raw);
    let response = decode_task_diff_response(&text);

    assert_eq!(response["status"], "historical");
    assert_eq!(response["worker_branch"], "spur/worker-a1");
    assert_eq!(response["summary"], "attempt 1 summary");
    assert_eq!(response["feedback"], "needs changes");
    assert!(
        response.get("diff").is_none(),
        "historical responses must remain summary-only: {response}"
    );
    assert!(
        response["note"]
            .as_str()
            .map(|note| note.contains("Historical attempt"))
            .unwrap_or(false),
        "historical responses must explain the summary-only contract: {response}"
    );
}

#[tokio::test]
async fn comment_lookup_returns_false_when_advanced_feature_unlicensed() {
    use spur_acp::{BrainSessionId, SessionId};

    let dir = TempDir::new().expect("tempdir");
    let (_beads, pm) = super::init_beads_pm(dir.path()).await;

    let session_id = BrainSessionId::new(SessionId("comment-lookup-non-pro".into()));
    let continuation_ctx = super::DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    };
    let outcome_store: Arc<dyn spur_blob_store::OutcomeStore> =
        Arc::new(spur_blob_store::MemoryOutcomeStore::new());
    let (server, _channel) = super::McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        continuation_ctx,
        outcome_store,
        super::unlicensed_feature_gate(),
    );

    let issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "non-pro probe".into(),
            issue_type: Some("task".into()),
            ..Default::default()
        })
        .await
        .expect("create issue");

    pm.update_issue(
        &issue_id,
        spur_pm::IssueUpdate {
            comment: Some(format!(
                "{} `non-pro` quarantine seed.",
                super::PLAN_PENDING_SWEEP_COMMENT_PREFIX
            )),
            ..Default::default()
        },
    )
    .await
    .expect("seed prefix comment");

    let result = server
        .issue_has_plan_pending_sweep_comment(pm.as_ref(), &issue_id)
        .await
        .expect("non-pro lookup must not propagate an error");
    assert!(
            !result,
            "non-pro feature gate must yield Ok(false) so the sweep skips conservatively instead of aborting"
        );
}
