use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use rusqlite::params;
use spur_acp::{BrainSessionId, SessionId, SpurEventBody};
use spur_mcp::plan::labels;
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer, McpEventSink};
use spur_pm::{IssueCreate, IssueUpdate, PmService};
use tempfile::TempDir;

mod common;

const SWEEP_COMMENT_PREFIX: &str = "SPUR startup sweep quarantined stale pending plan";

#[derive(Debug, Clone)]
struct SweepEvent {
    plan_id: Option<String>,
    epic_id: String,
    action: String,
    child_count: u32,
    reason: String,
}

#[derive(Default)]
struct CaptureSweepSink {
    events: Mutex<Vec<SweepEvent>>,
}

impl McpEventSink for CaptureSweepSink {
    fn emit(&self, body: SpurEventBody) {
        if let SpurEventBody::PlanPendingSweep {
            plan_id,
            epic_id,
            action,
            child_count,
            reason,
            ..
        } = body
        {
            self.events.lock().unwrap().push(SweepEvent {
                plan_id,
                epic_id,
                action,
                child_count,
                reason,
            });
        }
    }
}

struct PendingPlan {
    epic_id: String,
    child_ids: Vec<String>,
}

fn test_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) {
    let out = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
    }
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

async fn create_pending_plan(
    pm: &PmService,
    plan_id: Option<&str>,
    title: &str,
    child_count: usize,
) -> PendingPlan {
    let mut epic_labels = vec![labels::PLAN_PENDING.to_string()];
    if let Some(plan_id) = plan_id {
        epic_labels.push(labels::plan_id(plan_id));
    }
    let epic_id = pm
        .create_issue(IssueCreate {
            title: title.to_string(),
            issue_type: Some("epic".to_string()),
            labels: epic_labels,
            ..Default::default()
        })
        .await
        .expect("create pending epic");

    let mut child_ids = Vec::with_capacity(child_count);
    for idx in 0..child_count {
        let mut child_labels = Vec::new();
        if let Some(plan_id) = plan_id {
            child_labels.push(labels::plan_id(plan_id));
            child_labels.push(labels::plan_task_id(&format!("t{idx}")));
        }
        let child_id = pm
            .create_issue(IssueCreate {
                title: format!("{title} child {idx}"),
                issue_type: Some("task".to_string()),
                parent: Some(epic_id.clone()),
                labels: child_labels,
                ..Default::default()
            })
            .await
            .expect("create pending child");
        child_ids.push(child_id);
    }

    PendingPlan { epic_id, child_ids }
}

fn set_created_at(repo: &Path, issue_id: &str, seconds_ago: i64) {
    let timestamp = (Utc::now() - chrono::Duration::seconds(seconds_ago))
        .to_rfc3339_opts(SecondsFormat::Micros, true);
    let conn = rusqlite::Connection::open(repo.join(".beads/beads.db")).expect("open beads db");
    let changed = conn
        .execute(
            "UPDATE issues SET created_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![timestamp, issue_id],
        )
        .expect("backdate issue");
    assert_eq!(changed, 1, "issue {issue_id} must exist");
}

async fn start_server_for_sweep(
    repo: &Path,
    pm: Arc<PmService>,
    grace: Duration,
    sink: Option<Arc<dyn McpEventSink>>,
) {
    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&brain_sid),
        Some(pm),
        sink,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(repo.to_path_buf());
    server.set_plan_pending_grace(grace);

    let (_url, handle) = Arc::new(server)
        .start()
        .await
        .expect("server start should run pending sweep");
    drop(handle);
}

async fn comment_texts(pm: &PmService, issue_id: &str) -> Vec<String> {
    pm.advanced()
        .expect("beads advanced backend")
        .list_comments(issue_id)
        .await
        .expect("list comments")
        .into_iter()
        .map(|comment| comment.body)
        .collect()
}

async fn assert_has_sweep_comment(pm: &PmService, issue_id: &str) {
    let comments = comment_texts(pm, issue_id).await;
    assert!(
        comments
            .iter()
            .any(|comment| comment.starts_with(SWEEP_COMMENT_PREFIX)),
        "issue {issue_id} must have sweep comment; comments: {comments:?}"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn startup_sweep_resumes_after_prior_partial_child_quarantine() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    skip_if_no_loopback!("startup_sweep_resumes_after_prior_partial_child_quarantine");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let plan = create_pending_plan(pm.as_ref(), Some("partial-plan"), "Partial pending", 3).await;
    set_created_at(dir.path(), &plan.epic_id, 5);

    // Seed the state left by an earlier startup that quarantined child 0 and
    // then failed before closing the epic. This startup is the retry.
    pm.update_issue(
        &plan.child_ids[0],
        IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            comment: Some(format!(
                "{SWEEP_COMMENT_PREFIX} `partial-plan` before an injected startup failure."
            )),
            ..Default::default()
        },
    )
    .await
    .expect("seed previously quarantined child");

    start_server_for_sweep(dir.path(), Arc::clone(&pm), Duration::from_secs(1), None).await;

    let epic = pm.get_issue(&plan.epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());
    assert!(
        !epic
            .labels
            .iter()
            .any(|label| label == labels::PLAN_PENDING),
        "swept epic must have pending label removed: {:?}",
        epic.labels
    );
    for child_id in &plan.child_ids {
        let child = pm.get_issue(child_id).await.expect("get child");
        assert_eq!(child.status, pm.closed_status(), "child {child_id}");
        assert_has_sweep_comment(pm.as_ref(), child_id).await;
    }

    let child_0_sweep_comments = comment_texts(pm.as_ref(), &plan.child_ids[0])
        .await
        .into_iter()
        .filter(|body| body.starts_with(SWEEP_COMMENT_PREFIX))
        .count();
    assert_eq!(
        child_0_sweep_comments, 1,
        "child 0 must not be re-commented by the resumption sweep"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn startup_sweep_honors_plan_pending_grace_boundary() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    skip_if_no_loopback!("startup_sweep_honors_plan_pending_grace_boundary");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let stale = create_pending_plan(pm.as_ref(), Some("stale-plan"), "Stale pending", 1).await;
    let fresh = create_pending_plan(pm.as_ref(), Some("fresh-plan"), "Fresh pending", 1).await;
    set_created_at(dir.path(), &stale.epic_id, 5);

    start_server_for_sweep(dir.path(), Arc::clone(&pm), Duration::from_secs(1), None).await;

    let stale_epic = pm.get_issue(&stale.epic_id).await.expect("get stale epic");
    let stale_child = pm
        .get_issue(&stale.child_ids[0])
        .await
        .expect("get stale child");
    let fresh_epic = pm.get_issue(&fresh.epic_id).await.expect("get fresh epic");
    let fresh_child = pm
        .get_issue(&fresh.child_ids[0])
        .await
        .expect("get fresh child");

    assert_eq!(stale_epic.status, pm.closed_status());
    assert_eq!(stale_child.status, pm.closed_status());
    assert_eq!(fresh_epic.status, "open");
    assert_eq!(fresh_child.status, "open");
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn startup_sweep_quarantines_all_plan_children_with_comments() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    skip_if_no_loopback!("startup_sweep_quarantines_all_plan_children_with_comments");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let plan =
        create_pending_plan(pm.as_ref(), Some("recursive-plan"), "Recursive pending", 4).await;
    set_created_at(dir.path(), &plan.epic_id, 5);

    start_server_for_sweep(dir.path(), Arc::clone(&pm), Duration::from_secs(1), None).await;

    let epic = pm.get_issue(&plan.epic_id).await.expect("get epic");
    assert_eq!(epic.status, pm.closed_status());
    assert_has_sweep_comment(pm.as_ref(), &plan.epic_id).await;

    for child_id in &plan.child_ids {
        let child = pm.get_issue(child_id).await.expect("get child");
        assert_eq!(child.status, pm.closed_status(), "child {child_id}");
        assert_has_sweep_comment(pm.as_ref(), child_id).await;
    }
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn startup_sweep_skips_pending_epic_without_plan_id_and_emits_event() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    skip_if_no_loopback!("startup_sweep_skips_pending_epic_without_plan_id_and_emits_event");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = beads_pm(dir.path()).await;
    let plan = create_pending_plan(pm.as_ref(), None, "No plan id pending", 0).await;
    set_created_at(dir.path(), &plan.epic_id, 5);

    let sink = Arc::new(CaptureSweepSink::default());
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    start_server_for_sweep(
        dir.path(),
        Arc::clone(&pm),
        Duration::from_secs(1),
        Some(sink_ref),
    )
    .await;

    let epic = pm.get_issue(&plan.epic_id).await.expect("get epic");
    assert_eq!(epic.status, "open");
    assert!(
        epic.labels
            .iter()
            .any(|label| label == labels::PLAN_PENDING),
        "skipped epic must keep pending label: {:?}",
        epic.labels
    );

    let events = sink.events.lock().unwrap();
    assert!(events.iter().any(|event| {
        event.plan_id.is_none()
            && event.epic_id == plan.epic_id
            && event.action == "skipped"
            && event.child_count == 0
            && event.reason == "pending epic has no spur:plan-id label"
    }));
}
