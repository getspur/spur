use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::{
    BrainSessionId, DelegationResult, DelegationStatus, SessionId, SpurEvent, SpurEventBody,
};
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_mcp::McpEventSink;
use spur_pm::{IssueUpdate, PmService};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::task::TaskTracker;

mod common;

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

fn br_available() -> bool {
    common::beads::br_available()
}

fn run_br(repo: &Path, args: &[&str]) -> Result<(), String> {
    common::beads::run_br(repo, args).map(|_| ())
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
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

#[derive(Default)]
struct CaptureSink {
    events: std::sync::Mutex<Vec<SpurEvent>>,
}

impl McpEventSink for CaptureSink {
    fn emit(&self, body: SpurEventBody) {
        self.events.lock().unwrap().push(SpurEvent::now(body));
    }
}

struct PersistedFixture {
    _dir: TempDir,
    pm: Arc<PmService>,
    server: McpCallbackServer,
    sink: Arc<CaptureSink>,
}

impl PersistedFixture {
    async fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-q"]);
        run_git(dir.path(), &["config", "user.email", "test@spur"]);
        run_git(dir.path(), &["config", "user.name", "spur-test"]);
        std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
        run_git(dir.path(), &["add", "seed.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
        run_br(dir.path(), &["init"]).expect("br init");

        let pm = beads_pm(dir.path()).await;
        let sink = Arc::new(CaptureSink::default());
        let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            Some(sink_ref),
            continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            common::server_builder::pro_feature_gate(),
        );
        server.set_repo_root(dir.path().to_path_buf());

        Self {
            _dir: dir,
            pm,
            server,
            sink,
        }
    }

    async fn submit_persisted_plan(&self) -> (String, HashMap<String, String>) {
        let response = self
            .server
            .__test_call_submit_plan(json!({
                "persist_as_epic": true,
                "epic_title": "Snapshot Harness Epic",
                "tasks": [{
                    "task_id": "t1",
                    "agent": "codex",
                    "task": "Do something",
                    "depends_on": []
                }]
            }))
            .await;
        assert!(
            response.get("error").is_none(),
            "submit_plan should succeed: {response}"
        );
        extract_submit_details(&response)
    }
}

fn extract_submit_details(response: &Value) -> (String, HashMap<String, String>) {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let plan_id = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("plan_id: "))
        .map(str::to_string)
        .expect("submit_plan response must include plan_id");
    let task_map = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .map(|json| serde_json::from_str(json).expect("task_map json"))
        .expect("submit_plan response must include task_map");
    (plan_id, task_map)
}

fn snapshot_events(events: &[SpurEvent]) -> Vec<(SessionId, &spur_acp::PlanSnapshot)> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            SpurEventBody::PlanSnapshotUpdated {
                session_id,
                snapshot,
            } => Some((session_id.clone(), snapshot.as_ref())),
            _ => None,
        })
        .collect()
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn persisted_submit_plan_emits_plan_snapshot_updated() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let fixture = PersistedFixture::new().await;
    let (plan_id, _task_map) = fixture.submit_persisted_plan().await;

    let events = fixture.sink.events.lock().unwrap();
    let snapshots = snapshot_events(&events);
    assert!(
        !snapshots.is_empty(),
        "expected at least one PlanSnapshotUpdated event"
    );
    let (latest_session_id, latest) = snapshots.last().unwrap();
    assert_eq!(*latest_session_id, SessionId("brain".into()));
    assert_eq!(latest.plan_id, plan_id);
    assert_eq!(latest.status, "running");
    assert_eq!(latest.counts.pending, 1);
    assert_eq!(latest.tasks.len(), 1);
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn persisted_plan_snapshot_carries_owner_brain_session_id() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let fixture = PersistedFixture::new().await;
    let (_plan_id, _task_map) = fixture.submit_persisted_plan().await;

    let events = fixture.sink.events.lock().unwrap();
    let snapshots = snapshot_events(&events);
    let (_, latest) = snapshots.last().expect("at least one snapshot event");
    assert_eq!(
        latest.owner_brain_session_id.as_deref(),
        Some("brain"),
        "snapshot must surface the submitting brain's session id as owner"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn review_task_emits_refreshed_plan_snapshot() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let fixture = PersistedFixture::new().await;
    let (plan_id, task_map) = fixture.submit_persisted_plan().await;
    let issue_id = task_map.get("t1").expect("task id mapped");

    fixture
        .pm
        .update_issue(
            issue_id,
            IssueUpdate {
                add_labels: vec!["ready-for-review".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("mark task ready for review");

    let response = fixture
        .server
        .__test_call_tool(
            "review_task",
            json!({
                "plan_id": plan_id,
                "task_id": "t1",
                "decision": "approve"
            }),
        )
        .await;
    assert!(
        response.get("error").is_none(),
        "review_task should succeed: {response}"
    );

    let events = fixture.sink.events.lock().unwrap();
    let snapshots = snapshot_events(&events);
    let (latest_session_id, latest) = snapshots.last().expect("latest plan snapshot");
    assert_eq!(*latest_session_id, SessionId("brain".into()));
    assert_eq!(latest.status, "approved");
    assert!(latest.ready_to_merge);
    assert_eq!(latest.counts.approved, 1);
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn recover_persisted_plans_emits_plan_snapshot_updated() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let fixture = PersistedFixture::new().await;
    let (plan_id, _task_map) = fixture.submit_persisted_plan().await;

    let sink = Arc::new(CaptureSink::default());
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let session_id = BrainSessionId::new(SessionId("brain-2".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&fixture.pm)),
        Some(sink_ref),
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(fixture._dir.path().to_path_buf());
    server
        .__test_recover_persisted_plans()
        .await
        .expect("recover persisted plans");

    let events = sink.events.lock().unwrap();
    let snapshots = snapshot_events(&events);
    let (latest_session_id, latest) = snapshots.last().expect("latest plan snapshot");
    assert_eq!(*latest_session_id, SessionId("brain".into()));
    assert_eq!(latest.plan_id, plan_id);
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn recover_persisted_plans_uses_legacy_session_fallback_when_missing() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]).expect("br init");

    let pm = beads_pm(dir.path()).await;
    let tasks = vec![spur_mcp::plan::PlanTask {
        task_id: "t1".into(),
        agent: "codex".into(),
        task: "Do something".into(),
        depends_on: Vec::new(),
        issue_id: None,
        context_files: Vec::new(),
    }];
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        common::server_builder::pro_feature_gate().as_ref(),
        "legacy-plan",
        "Legacy Snapshot Harness Epic",
        None,
        &tasks,
    )
    .await
    .expect("build epic subgraph");
    spur_mcp::emit_plan_submit_audit(
        pm.advanced().expect("advanced beads backend"),
        "legacy-plan",
        &subgraph,
        None,
        None,
        Some("submit_plan"),
        None,
    )
    .await;

    let sink = Arc::new(CaptureSink::default());
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let session_id = BrainSessionId::new(SessionId("brain-2".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        Some(sink_ref),
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server
        .__test_recover_persisted_plans()
        .await
        .expect("recover persisted legacy plan");

    let events = sink.events.lock().unwrap();
    let snapshots = snapshot_events(&events);
    let (latest_session_id, latest) = snapshots.last().expect("latest legacy plan snapshot");
    assert_eq!(
        *latest_session_id,
        SessionId("persisted-plan:legacy-plan".into())
    );
    assert_eq!(latest.plan_id, "legacy-plan");
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn reconciler_dispatch_and_completion_emit_refreshed_snapshots() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let fixture = PersistedFixture::new().await;
    let (plan_id, task_map) = fixture.submit_persisted_plan().await;
    let issue_id = task_map.get("t1").expect("task id mapped").clone();
    fixture.sink.events.lock().unwrap().clear();

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(1);
    let tracker = TaskTracker::new();
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&fixture.sink) as Arc<dyn McpEventSink>;
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&fixture.pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tracker.clone(),
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            event_sink: Some(sink_ref),
            materializer: test_materializer(),
            continuation_ctx: common::server_builder::continuation_ctx_arc(),
        }),
        Some(plan_id.clone()),
        common::server_builder::pro_feature_gate(),
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(
        did_work,
        "reconciler should dispatch the ready persisted task"
    );

    let request = delegation_rx.recv().await.expect("dispatch request");
    assert_eq!(request.issue_id.as_deref(), Some(issue_id.as_str()));

    {
        let events = fixture.sink.events.lock().unwrap();
        let snapshots = snapshot_events(&events);
        let (latest_session_id, latest) = snapshots.last().expect("dispatch snapshot");
        assert_eq!(*latest_session_id, SessionId("brain".into()));
        assert_eq!(latest.plan_id, plan_id);
        assert_eq!(latest.counts.dispatched, 1);
        assert_eq!(latest.counts.pending, 0);
        assert_eq!(latest.tasks.len(), 1);
        assert_eq!(latest.tasks[0].status, "dispatched");
        assert_eq!(
            latest.tasks[0].delegation_id.as_deref(),
            Some(request.id.as_str())
        );
    }

    request
        .respond_to
        .send(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("worker finished".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-t1".into()),
            artifact: None,
        })
        .expect("send delegation result");

    tracker.close();
    tracker.wait().await;

    let events = fixture.sink.events.lock().unwrap();
    let snapshots = snapshot_events(&events);
    let (latest_session_id, latest) = snapshots.last().expect("completion snapshot");
    assert_eq!(*latest_session_id, SessionId("brain".into()));
    assert_eq!(latest.plan_id, plan_id);
    assert_eq!(latest.counts.awaiting_review, 1);
    assert_eq!(latest.counts.dispatched, 0);
    assert_eq!(latest.tasks[0].status, "awaiting_review");
    assert_eq!(latest.tasks[0].summary.as_deref(), Some("worker finished"));
    assert_eq!(
        latest.tasks[0].worker_branch.as_deref(),
        Some("spur/worker-t1")
    );
}
