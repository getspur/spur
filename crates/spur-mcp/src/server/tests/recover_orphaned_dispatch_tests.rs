use super::{run_git_capture, DetachedContinuationCtx, McpCallbackServer};
use crate::plan::audit_sentinel::{AuditSentinelKind, CompletionState};
use crate::plan::PlanTask;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

async fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    run_git_capture(dir.path(), None, &["init", "-q", "-b", "main"])
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

fn no_op_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker_session| Box::pin(async {})),
    }
}

#[derive(Default)]
struct RecordingSink {
    events: std::sync::Mutex<Vec<spur_acp::SpurEventBody>>,
}

impl crate::events::McpEventSink for RecordingSink {
    fn emit(&self, event: spur_acp::SpurEventBody) {
        self.events.lock().unwrap().push(event);
    }
}

type RecordedContinuations =
    Arc<tokio::sync::Mutex<Vec<(spur_acp::domain::BrainContinuation, String)>>>;

fn recording_continuation_ctx() -> (DetachedContinuationCtx, RecordedContinuations) {
    let continuations = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let captured = Arc::clone(&continuations);
    (
        DetachedContinuationCtx {
            on_complete: Arc::new(move |cont, delegation_id| {
                let captured = Arc::clone(&captured);
                Box::pin(async move {
                    captured.lock().await.push((cont, delegation_id));
                })
            }),
        },
        continuations,
    )
}

fn response_text(response: &super::JsonRpcResponse) -> &str {
    response.result.as_ref().expect("success result")["content"][0]["text"]
        .as_str()
        .expect("text content")
}

struct RecoveryFixture {
    _beads: spur_pm::test_workspace::TestBeadsWorkspace,
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
    epic_id: String,
    task_issue_id: String,
}

async fn setup_recovery_task(
    repo: &std::path::Path,
    plan_id: &str,
    delegation_id: &str,
) -> RecoveryFixture {
    let (beads, pm) = super::init_beads_pm(repo).await;
    let feature_gate = super::pro_feature_gate();
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Recover orphan",
        None,
        &[PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Recover this orphan".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        }],
    )
    .await
    .expect("build epic subgraph");
    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");
    crate::plan::persist_dispatch_intent(
        pm.as_ref(),
        &task_issue_id,
        feature_gate.as_ref(),
        plan_id,
        delegation_id,
        "codex",
        1,
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("persist dispatch intent");

    RecoveryFixture {
        _beads: beads,
        pm,
        feature_gate,
        epic_id: subgraph.epic_id,
        task_issue_id,
    }
}

fn recovery_server(
    repo: &std::path::Path,
    pm: Arc<spur_pm::PmService>,
    feature_gate: Arc<spur_license::FeatureGate>,
) -> McpCallbackServer {
    let brain_session = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_session),
        Some(pm),
        None,
        no_op_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        feature_gate,
    );
    server.set_repo_root(repo.to_path_buf());
    server
}

#[tokio::test]
async fn brain_attach_rediscovery_replays_awaiting_review_task_once() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/rediscovery-awaiting-review";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
        .await
        .expect("checkout main");

    let fixture = setup_recovery_task(dir.path(), "rediscover-awaiting", "del-rediscover").await;
    let brain_session = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));
    fixture
        .pm
        .update_issue(
            &fixture.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::plan_owner(
                    &brain_session.as_session_id().0,
                )],
                ..Default::default()
            },
        )
        .await
        .expect("stamp owner label");
    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        fixture.feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = fixture.pm.advanced().expect("advanced beads backend");
    adv.add_comment(
        &fixture.task_issue_id,
        &crate::plan::audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-rediscover".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch.into()),
            result_summary: Some("ready for review".into()),
            artifact_uri: None,
            dispatched_base_oid: Some(base_oid.clone()),
        }),
    )
    .await
    .expect("completion audit");
    fixture
        .pm
        .update_issue(
            &fixture.task_issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::READY_FOR_REVIEW.to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("ready label");

    let sink = Arc::new(RecordingSink::default());
    let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
    let (continuation_ctx, continuations) = recording_continuation_ctx();
    let (mut server, _channel) = McpCallbackServer::new(
        None,
        Some(Arc::clone(&fixture.pm)),
        Some(sink_obj),
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::clone(&fixture.feature_gate),
    );
    server.set_repo_root(dir.path().to_path_buf());
    let server = Arc::new(server);

    server
        .set_brain_session_id(brain_session)
        .expect("bind brain session");

    for _ in 0..50 {
        if continuations.lock().await.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let recorded = continuations.lock().await;
    assert_eq!(recorded.len(), 1, "expected one replayed continuation");
    assert_eq!(recorded[0].1, "del-rediscover");
    assert_eq!(
        recorded[0].0.source,
        spur_acp::domain::ContinuationSource::PlanTaskAwaitingReview
    );
    drop(recorded);

    let events = sink.events.lock().unwrap();
    let awaiting_events: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                spur_acp::SpurEventBody::PlanTaskAwaitingReview {
                    plan_id,
                    task_id,
                    delegation_id,
                } if plan_id == "rediscover-awaiting"
                    && task_id == "task-a"
                    && delegation_id == "del-rediscover"
            )
        })
        .collect();
    assert_eq!(
        awaiting_events.len(),
        1,
        "expected one PlanTaskAwaitingReview event, got {events:?}"
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_promotes_dispatched_task_to_awaiting_review() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/v2/codex/brain/worker";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
        .await
        .expect("checkout main");

    let (_beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();
    let plan_id = "recover-orphan";
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Recover orphan",
        None,
        &[PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Recover this orphan".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        }],
    )
    .await
    .expect("build epic subgraph");
    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");
    crate::plan::persist_dispatch_intent(
        pm.as_ref(),
        &task_issue_id,
        feature_gate.as_ref(),
        plan_id,
        "del-A",
        "codex",
        1,
        std::time::Duration::from_secs(600),
    )
    .await
    .expect("persist dispatch intent");

    let brain_session = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_session),
        Some(Arc::clone(&pm)),
        None,
        no_op_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::clone(&feature_gate),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let response = server
        .handle_tool_call(
            Value::Null,
            json!({
                "name": "recover_orphaned_dispatch",
                "arguments": {
                    "issue_id": task_issue_id.clone(),
                    "worker_branch": worker_branch,
                    "dispatched_base_oid": base_oid.clone(),
                }
            }),
        )
        .await;

    assert!(
        response.error.is_none(),
        "unexpected error: {:?}",
        response.error
    );
    assert!(
        response_text(&response).contains("Task promoted to AwaitingReview"),
        "unexpected response: {}",
        response_text(&response)
    );

    let issue = pm.get_issue(&task_issue_id).await.expect("get issue");
    assert!(
        crate::plan::projector::has_ready_for_review_label_compat(&issue.labels),
        "recovered task must have ready-for-review label: {:?}",
        issue.labels
    );
    assert!(
        !issue
            .labels
            .contains(&crate::plan::labels::delegation_id("del-A")),
        "recovered task must clear dispatch label: {:?}",
        issue.labels
    );

    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = pm.advanced().expect("advanced beads backend");
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        &task_issue_id,
        adv.list_comments(&task_issue_id)
            .await
            .expect("list comments"),
    )
    .expect("projection should parse");
    let completion = audits.iter().find_map(|audit| match audit {
        AuditSentinelKind::Completion {
            delegation_id,
            completion_state,
            worker_branch: found_branch,
            dispatched_base_oid,
            ..
        } if delegation_id == "del-A" => Some((
            *completion_state,
            found_branch.as_deref(),
            dispatched_base_oid.as_deref(),
        )),
        _ => None,
    });
    assert_eq!(
        completion,
        Some((
            CompletionState::AwaitingReview,
            Some(worker_branch),
            Some(base_oid.as_str())
        ))
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_reemits_already_awaiting_review_without_new_audit() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/already-awaiting-review";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;

    let fixture = setup_recovery_task(dir.path(), "recover-reemit-awaiting", "del-reemit").await;
    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        fixture.feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = fixture.pm.advanced().expect("advanced beads backend");
    adv.add_comment(
        &fixture.task_issue_id,
        &crate::plan::audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-reemit".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch.into()),
            result_summary: Some("already ready".into()),
            artifact_uri: None,
            dispatched_base_oid: Some(base_oid.clone()),
        }),
    )
    .await
    .expect("completion audit");
    fixture
        .pm
        .update_issue(
            &fixture.task_issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::READY_FOR_REVIEW.to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("ready label");
    let before_comments = adv
        .list_comments(&fixture.task_issue_id)
        .await
        .expect("list comments before")
        .len();

    let sink = Arc::new(RecordingSink::default());
    let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
    let (continuation_ctx, continuations) = recording_continuation_ctx();
    let brain_session = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_session),
        Some(Arc::clone(&fixture.pm)),
        Some(sink_obj),
        continuation_ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        Arc::clone(&fixture.feature_gate),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let msg = server
        .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
        .await
        .expect("already AwaitingReview task should replay");

    assert!(
        msg.contains("continuation re-emitted"),
        "unexpected response: {msg}"
    );
    let after_comments = adv
        .list_comments(&fixture.task_issue_id)
        .await
        .expect("list comments after")
        .len();
    assert_eq!(
        after_comments, before_comments,
        "must not write duplicate audit"
    );

    let recorded = continuations.lock().await;
    assert_eq!(recorded.len(), 1, "expected one replayed continuation");
    assert_eq!(recorded[0].1, "del-reemit");
    drop(recorded);

    let events = sink.events.lock().unwrap();
    assert!(
        events.iter().any(|event| matches!(
            event,
            spur_acp::SpurEventBody::PlanTaskAwaitingReview {
                plan_id,
                task_id,
                delegation_id,
            } if plan_id == "recover-reemit-awaiting"
                && task_id == "task-a"
                && delegation_id == "del-reemit"
        )),
        "expected PlanTaskAwaitingReview event, got {events:?}"
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_accepts_legacy_delegation_label() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/v2/codex/brain/legacy-label";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
        .await
        .expect("checkout main");

    let fixture = setup_recovery_task(dir.path(), "recover-orphan-legacy", "del-legacy").await;
    fixture
        .pm
        .update_issue(
            &fixture.task_issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec!["delegation-id:del-legacy".to_string()],
                remove_labels: vec![crate::plan::labels::delegation_id("del-legacy")],
                ..Default::default()
            },
        )
        .await
        .expect("replace delegation label with legacy spelling");

    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );
    let msg = server
        .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
        .await
        .expect("legacy delegation label should recover");
    assert!(msg.contains("Task promoted to AwaitingReview"));

    let issue = fixture
        .pm
        .get_issue(&fixture.task_issue_id)
        .await
        .expect("load recovered issue");
    assert!(
        crate::plan::projector::has_ready_for_review_label_compat(&issue.labels),
        "ready-for-review label should be present: {:?}",
        issue.labels
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_prefers_dispatched_base_oid_label() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/recover-from-label";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
        .await
        .expect("checkout main");
    commit_file(dir.path(), "wrong-base.txt", "wrong\n", "wrong base").await;
    let wrong_base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("wrong base oid");

    let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
    fixture
        .pm
        .update_issue(
            &fixture.task_issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::dispatched_base_oid(&base_oid)],
                ..Default::default()
            },
        )
        .await
        .expect("persist dispatched base oid label");
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let message = server
        .recover_orphaned_dispatch_with_branch(
            &fixture.task_issue_id,
            worker_branch,
            &wrong_base_oid,
        )
        .await
        .expect("label-backed recovery should succeed");
    assert!(
        message.contains("Task promoted to AwaitingReview"),
        "unexpected response: {message}"
    );

    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        super::pro_feature_gate().as_ref(),
    )
    .expect("fixture enables advanced beads");
    let adv = fixture.pm.advanced().expect("advanced beads backend");
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        &fixture.task_issue_id,
        adv.list_comments(&fixture.task_issue_id)
            .await
            .expect("list comments"),
    )
    .expect("projection should parse");
    let recovered_base = audits.iter().find_map(|audit| match audit {
        AuditSentinelKind::Completion {
            delegation_id,
            dispatched_base_oid,
            ..
        } if delegation_id == "del-A" => dispatched_base_oid.as_deref(),
        _ => None,
    });
    assert_eq!(recovered_base, Some(base_oid.as_str()));
}

#[tokio::test]
async fn recover_orphaned_dispatch_uses_audit_delegation_when_label_mismatches() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/recover-mismatch";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
        .await
        .expect("checkout main");

    let fixture = setup_recovery_task(dir.path(), "recover-orphan-mismatch", "del-audit").await;
    fixture
        .pm
        .update_issue(
            &fixture.task_issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::delegation_id("del-label")],
                remove_labels: vec![crate::plan::labels::delegation_id("del-audit")],
                ..Default::default()
            },
        )
        .await
        .expect("swap delegation label");

    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );
    let msg = server
        .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
        .await
        .expect("audit delegation should win");
    assert!(msg.contains("Task promoted to AwaitingReview"));

    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        fixture.feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = fixture.pm.advanced().expect("advanced beads backend");
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(
        &fixture.task_issue_id,
        adv.list_comments(&fixture.task_issue_id)
            .await
            .expect("list comments"),
    )
    .expect("projection should parse");
    assert!(audits.iter().any(|audit| matches!(
        audit,
        AuditSentinelKind::Completion { delegation_id, .. } if delegation_id == "del-audit"
    )));
}

#[tokio::test]
async fn recover_orphaned_dispatch_rejects_label_only_delegation_without_audit() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/recover-label-only";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
        .await
        .expect("checkout main");

    let (beads, pm) = super::init_beads_pm(dir.path()).await;
    let feature_gate = super::pro_feature_gate();
    let plan_id = "recover-orphan-label-only";
    let subgraph = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Recover orphan",
        None,
        &[PlanTask {
            task_id: "task-a".into(),
            agent: "codex".into(),
            task: "Recover this orphan".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        }],
    )
    .await
    .expect("build epic subgraph");
    let task_issue_id = subgraph
        .task_map
        .get("task-a")
        .cloned()
        .expect("task issue id");
    pm.update_issue(
        &task_issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::delegation_id("del-label-only")],
            ..Default::default()
        },
    )
    .await
    .expect("add label-only delegation");

    let server = recovery_server(dir.path(), Arc::clone(&pm), Arc::clone(&feature_gate));
    let err = server
        .recover_orphaned_dispatch_with_branch(&task_issue_id, worker_branch, &base_oid)
        .await
        .expect_err("label-only delegation without audit must fail");
    assert!(
        err.contains("no audit attestation"),
        "unexpected error: {err}"
    );
    drop(beads);
}

#[tokio::test]
#[ignore = "pinned residual; requires deterministic-recovery follow-up"]
async fn recover_orphaned_dispatch_with_split_dispatched_base_oid_labels() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let worker_branch = "spur/worker/split-dispatched-base-labels";
    run_git_capture(dir.path(), None, &["branch", worker_branch])
        .await
        .expect("create worker branch");

    let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
    fixture
        .pm
        .update_issue(
            &fixture.task_issue_id,
            spur_pm::IssueUpdate {
                add_labels: vec![
                    crate::plan::labels::dispatched_base_oid("aaa1"),
                    crate::plan::labels::dispatched_base_oid("bbb2"),
                ],
                ..Default::default()
            },
        )
        .await
        .expect("persist split dispatched base oid labels");
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let err = server
        .recover_orphaned_dispatch_with_branch(
            &fixture.task_issue_id,
            worker_branch,
            "fallback-base",
        )
        .await
        .expect_err("current split-label behavior selects a non-git OID and fails validation");
    assert!(
        err.contains("base=aaa1"),
        "current split-label recovery should select the first label; got: {err}"
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_rejects_more_than_one_commit() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/two-commits";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker-a.txt", "a\n", "worker change a").await;
    commit_file(dir.path(), "worker-b.txt", "b\n", "worker change b").await;

    let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let err = server
        .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
        .await
        .expect_err("two worker commits must be rejected");
    assert!(
        err.contains("2 commits") || err.contains("expected exactly 1"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_rejects_zero_commits() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/zero-commits";
    run_git_capture(dir.path(), None, &["branch", worker_branch, &base_oid])
        .await
        .expect("create worker branch");

    let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let err = server
        .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
        .await
        .expect_err("zero worker commits must be rejected");
    assert!(err.contains("0 commits"), "unexpected error: {err}");
}

#[tokio::test]
async fn recover_orphaned_dispatch_reemits_legacy_already_awaiting_review_delegation() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/already-completed";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;

    let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
    super::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        fixture.feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = fixture.pm.advanced().expect("advanced beads backend");
    adv.add_comment(
        &fixture.task_issue_id,
        &crate::plan::audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: "del-A".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch.into()),
            result_summary: Some("already done".into()),
            artifact_uri: None,
            dispatched_base_oid: Some(base_oid.clone()),
        }),
    )
    .await
    .expect("completion audit");
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let msg = server
        .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
        .await
        .expect("already AwaitingReview delegation should replay");
    assert!(
        msg.contains("continuation re-emitted"),
        "unexpected response: {msg}"
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_rejects_missing_branch() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let err = server
        .recover_orphaned_dispatch_with_branch(
            &fixture.task_issue_id,
            "spur/worker/does-not-exist",
            &base_oid,
        )
        .await
        .expect_err("missing worker branch must be rejected");
    assert!(err.contains("not found"), "unexpected error: {err}");
}

#[tokio::test]
async fn recover_orphaned_dispatch_rejects_missing_plan_id() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/missing-plan-id";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;

    let fixture = setup_recovery_task(dir.path(), "recover-orphan-missing-plan-id", "del-A").await;
    fixture
        .pm
        .update_issue(
            &fixture.task_issue_id,
            spur_pm::IssueUpdate {
                remove_labels: vec![crate::plan::labels::plan_id(
                    "recover-orphan-missing-plan-id",
                )],
                ..Default::default()
            },
        )
        .await
        .expect("remove plan id label");
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let err = server
        .recover_orphaned_dispatch_with_branch(&fixture.task_issue_id, worker_branch, &base_oid)
        .await
        .expect_err("missing plan-id label must be rejected");
    assert!(
        err.contains("missing spur:plan-id"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn recover_orphaned_dispatch_rejects_non_ancestor_base() {
    let dir = init_repo().await;
    commit_file(dir.path(), "base.txt", "base\n", "seed").await;
    let original_base_oid = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("base oid");
    let worker_branch = "spur/worker/diverged-base";
    run_git_capture(
        dir.path(),
        None,
        &["checkout", "-q", "-b", worker_branch, &original_base_oid],
    )
    .await
    .expect("checkout worker branch");
    commit_file(dir.path(), "worker.txt", "worker\n", "worker change").await;
    run_git_capture(dir.path(), None, &["checkout", "-q", "main"])
        .await
        .expect("checkout main");
    commit_file(dir.path(), "main.txt", "main\n", "main moved").await;
    let non_ancestor_base = run_git_capture(dir.path(), None, &["rev-parse", "HEAD"])
        .await
        .expect("non-ancestor base oid");

    let fixture = setup_recovery_task(dir.path(), "recover-orphan", "del-A").await;
    let server = recovery_server(
        dir.path(),
        Arc::clone(&fixture.pm),
        Arc::clone(&fixture.feature_gate),
    );

    let err = server
        .recover_orphaned_dispatch_with_branch(
            &fixture.task_issue_id,
            worker_branch,
            &non_ancestor_base,
        )
        .await
        .expect_err("non-ancestor base must be rejected");
    assert!(
        err.contains("not an ancestor") || err.contains("G-Strict validation failed"),
        "unexpected error: {err}"
    );
}
