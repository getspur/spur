//! submit_plan persist_as_epic — unit + integration tests over pure helpers.
//!
//! Because PmService is a concrete struct (not a trait) today, live-beads
//! integration is covered at the CLI level elsewhere. Here we test the
//! pure helper that decides WHAT IssueCreate values the handler would
//! dispatch given a plan + epic fields.

use spur_mcp::{build_entries_with_task_map, plan_epic_issue_creates, tools_list};
// `pub mod plan;` is declared in lib.rs, so spur_mcp::plan::PlanTask is accessible.
use spur_mcp::plan::{labels, PlanTask};

mod common;

fn test_materializer() -> std::sync::Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    std::sync::Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        std::sync::Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

fn sample_tasks(with_c: bool) -> Vec<PlanTask> {
    let mut v = vec![
        PlanTask {
            task_id: "a".into(),
            agent: "claude-code-acp".into(),
            task: "Do A.".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        },
        PlanTask {
            task_id: "b".into(),
            agent: "claude-code-acp".into(),
            task: "Do B.".into(),
            depends_on: vec!["a".into()],
            issue_id: Some("bd-42".into()),
            context_files: Vec::new(),
        },
    ];
    if with_c {
        v.push(PlanTask {
            task_id: "c".into(),
            agent: "codex".into(),
            task: "Do C.".into(),
            depends_on: vec!["a".into(), "b".into()],
            issue_id: None,
            context_files: Vec::new(),
        });
    }
    v
}

#[test]
fn epic_create_carries_plan_id_label_and_epic_type() {
    let tasks = sample_tasks(false);
    let (epic, _children) =
        plan_epic_issue_creates("plan-xyz", "Refactor foo", Some("Body"), &tasks).expect("ok");
    assert_eq!(epic.title, "Refactor foo");
    assert_eq!(epic.issue_type.as_deref(), Some("epic"));
    assert_eq!(epic.description.as_deref(), Some("Body"));
    assert!(
        epic.labels.iter().any(|l| l == "spur:plan-id:plan-xyz"),
        "epic must carry spur:plan-id label; got {:?}",
        epic.labels,
    );
    assert!(
        epic.labels.iter().any(|l| l == labels::PLAN_PENDING),
        "epic must start with spur:plan-pending label; got {:?}",
        epic.labels,
    );
}

#[test]
fn children_are_in_topological_order() {
    let tasks = sample_tasks(true);
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "Refactor foo", None, &tasks).expect("ok");
    let order: Vec<&str> = children.iter().map(|(k, _)| k.as_str()).collect();
    let pos_a = order.iter().position(|&k| k == "a").unwrap();
    let pos_b = order.iter().position(|&k| k == "b").unwrap();
    let pos_c = order.iter().position(|&k| k == "c").unwrap();
    assert!(pos_a < pos_b);
    assert!(pos_a < pos_c);
    assert!(pos_b < pos_c);
}

#[test]
fn children_carry_spur_plan_id_plan_task_id_and_agent_labels() {
    let tasks = sample_tasks(false);
    let (_epic, children) = plan_epic_issue_creates("plan-xyz", "Title", None, &tasks).expect("ok");
    let (_, child_b) = children
        .iter()
        .find(|(k, _)| k == "b")
        .expect("child b present");
    let labels: &Vec<String> = &child_b.labels;
    assert!(labels.iter().any(|l| l == "spur:plan-id:plan-xyz"));
    assert!(labels.iter().any(|l| l == "spur:plan-task-id:b"));
    assert!(labels.iter().any(|l| l == "spur:agent:claude-code-acp"));
    assert!(
        labels.iter().any(|l| l == "spur:source-issue:bd-42"),
        "child b sourced from bd-42 must carry spur:source-issue label"
    );
}

#[test]
fn children_depends_on_carries_task_id_keys_not_beads_ids() {
    let tasks = sample_tasks(false);
    let (_epic, children) = plan_epic_issue_creates("plan-xyz", "T", None, &tasks).expect("ok");
    let (_, child_b) = children.iter().find(|(k, _)| k == "b").unwrap();
    assert_eq!(child_b.depends_on, vec!["a".to_string()]);
}

#[test]
fn children_parent_field_is_unset_before_epic_creation() {
    let tasks = sample_tasks(false);
    let (_epic, children) = plan_epic_issue_creates("plan-xyz", "T", None, &tasks).expect("ok");
    for (_, c) in &children {
        assert!(c.parent.is_none(), "parent must be None at this stage");
    }
}

#[test]
fn cycle_produces_error() {
    let tasks = vec![
        PlanTask {
            task_id: "a".into(),
            agent: "x".into(),
            task: "A".into(),
            depends_on: vec!["b".into()],
            issue_id: None,
            context_files: Vec::new(),
        },
        PlanTask {
            task_id: "b".into(),
            agent: "x".into(),
            task: "B".into(),
            depends_on: vec!["a".into()],
            issue_id: None,
            context_files: Vec::new(),
        },
    ];
    let err = plan_epic_issue_creates("p", "t", None, &tasks).unwrap_err();
    assert!(
        err.contains("incomplete") || err.contains("cycle"),
        "cycle error text should mention incomplete or cycle; got: {err}"
    );
}

#[test]
fn submit_plan_schema_still_advertises_tasks_as_required() {
    let schema = tools_list()
        .into_iter()
        .find(|t| t.name == "submit_plan")
        .unwrap()
        .input_schema;
    let required: Vec<&str> = schema
        .get("required")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"tasks"));
}

#[tokio::test]
async fn submit_plan_response_advertises_continuation_fire() {
    let server = common::server_builder::mock_pro_server();
    let response = server
        .__test_call_submit_plan(serde_json::json!({
            "tasks": [{
                "task_id": "t1",
                "agent": "codex",
                "task": "Do one thing",
                "depends_on": []
            }]
        }))
        .await;
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    assert_eq!(
        response["result"]["continuation_will_fire"],
        serde_json::json!(true),
        "submit_plan response should advertise detached continuations: {response}"
    );
}

/// INV-7: verify that `run_plan` emits `PlanCompleted` when all tasks are
/// already in a terminal Approved state on entry (so the executor loop exits
/// immediately without dispatching), but leaves `PlanReadyToMerge` to the
/// durable reconciler projection path.
#[tokio::test]
async fn run_plan_emits_plan_completed_on_terminal_state() {
    use spur_acp::{SpurEvent, SpurEventBody};
    use spur_mcp::plan::{run_plan, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
    use spur_mcp::McpEventSink;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    let state = PlanState {
        plan_id: "p1".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "a".into(),
                task: "T".into(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::Approved { summary: None },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
    };

    /// A test sink that captures emitted event bodies synchronously.
    struct CaptureSink {
        events: std::sync::Mutex<Vec<SpurEvent>>,
    }
    impl McpEventSink for CaptureSink {
        fn emit(&self, body: SpurEventBody) {
            self.events.lock().unwrap().push(SpurEvent::now(body));
        }
    }

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;

    let (dtx, _drx) = mpsc::channel(8);

    run_plan(
        Arc::new(Mutex::new(state)),
        dtx,
        Some(sink_ref),
        None,
        None,
        Arc::new(DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }),
        test_materializer(),
        common::server_builder::pro_feature_gate(),
    )
    .await;

    let events = sink.events.lock().unwrap();
    let saw_completed = events.iter().any(|e| {
        matches!(
            &e.body,
            SpurEventBody::PlanCompleted { plan_id, approved, .. }
                if plan_id == "p1" && *approved == 1
        )
    });
    let saw_ready = events.iter().any(|e| {
        matches!(
            &e.body,
            SpurEventBody::PlanReadyToMerge { plan_id } if plan_id == "p1"
        )
    });
    assert!(
        saw_completed,
        "PlanCompleted must be emitted; got: {:?}",
        events.iter().map(|e| &e.body).collect::<Vec<_>>()
    );
    assert!(
        !saw_ready,
        "PlanReadyToMerge must be emitted only from durable reconciliation; got: {:?}",
        events.iter().map(|e| &e.body).collect::<Vec<_>>()
    );
}

/// INV-5: verify that `handle_review_task` releases the plan-state lock BEFORE
/// it calls `pm.update_issue`, so concurrent readers are not blocked by network
/// latency.
///
/// Mechanism: `SleepyPm` fires a oneshot signal the instant `update_issue` is
/// entered (before the virtual sleep).  The test awaits that signal, which
/// proves the approve task has genuinely reached the beads-I/O await point —
/// ruling out a false-pass from an early-error exit that never held the lock in
/// the first place.  Only then does it call `try_lock`.
///
/// With the fix the lock is dropped before `update_issue` is called, so
/// `try_lock` succeeds.  Without the fix the lock is still held at that point,
/// so `try_lock` would return `Err`.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn review_approve_releases_plan_lock_before_beads_io() {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    let state = spur_mcp::plan::PlanState {
        plan_id: "p1".into(),
        tasks: vec![spur_mcp::plan::PlanTaskEntry {
            spec: spur_mcp::plan::PlanTask {
                task_id: "t1".into(),
                agent: "a".into(),
                task: "T".into(),
                depends_on: vec![],
                issue_id: Some("bd-1".into()),
                context_files: vec![],
            },
            status: spur_mcp::plan::PlanTaskStatus::AwaitingReview { summary: None },
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
    };
    let plan_arc: Arc<Mutex<spur_mcp::plan::PlanState>> = Arc::new(Mutex::new(state));

    // SleepyPm sleeps 1 s (virtual) inside update_issue and fires `entered_rx`
    // the moment update_issue is entered — before the sleep.
    let (sleepy_pm, entered_rx) =
        spur_mcp::test_support::make_sleepy_pm_with_signal(Duration::from_secs(1));

    // Start approve in the background.
    let plan_ref = Arc::clone(&plan_arc);
    let approve = tokio::spawn(async move {
        spur_mcp::plan::handle_review_task(
            plan_ref,
            "p1",
            "t1",
            "approve",
            Some("ok"),
            Some(sleepy_pm),
            None,
            None,
            None,
            common::server_builder::pro_feature_gate(),
        )
        .await
    });

    // Wait until approve has provably entered update_issue's await point.
    // This guarantees we are NOT racing against an early-error path and that
    // the plan lock has been held and (with the fix) released.
    entered_rx
        .await
        .expect("approve must reach update_issue before the test can proceed");

    // The lock must be available: approve dropped it before calling update_issue.
    // Without the fix it would still be held here, and try_lock would fail.
    let guard = plan_arc.try_lock();
    assert!(
        guard.is_ok(),
        "plan lock must be released before pm.update_issue — INV-5 violated"
    );
    drop(guard);

    // Let the approve finish (auto-advances virtual time past the 1 s sleep).
    approve
        .await
        .expect("approve task panicked")
        .expect("approve returned Err");
}

/// DN-6: verify that `run_plan`, on terminal loop exit, promotes ANY
/// non-terminal task (not just Pending-with-failed-dep) to Failed — including a
/// Pending task whose declared dependency does not exist in the plan at all.
#[tokio::test]
async fn run_plan_marks_pending_tasks_failed_on_terminal_exit() {
    use spur_acp::{SpurEvent, SpurEventBody};
    use spur_mcp::plan::{run_plan, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
    use spur_mcp::McpEventSink;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    struct CaptureSink {
        events: std::sync::Mutex<Vec<SpurEvent>>,
    }
    impl McpEventSink for CaptureSink {
        fn emit(&self, body: SpurEventBody) {
            self.events.lock().unwrap().push(SpurEvent::now(body));
        }
    }

    let state = PlanState {
        plan_id: "p1".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "a".into(),
                task: "T".into(),
                depends_on: vec!["missing-dep".into()],
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::Pending,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: spur_mcp::plan::PlanMergeState::NotStarted,
        epic_id: None,
    };

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let (dtx, _drx) = mpsc::channel(8);
    let plan_arc = Arc::new(Mutex::new(state));

    run_plan(
        Arc::clone(&plan_arc),
        dtx,
        Some(sink_ref),
        None,
        None,
        Arc::new(DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }),
        test_materializer(),
        common::server_builder::pro_feature_gate(),
    )
    .await;

    let st = plan_arc.lock().await;
    assert!(
        matches!(st.tasks[0].status, PlanTaskStatus::Failed { .. }),
        "stuck Pending task must become Failed on terminal exit, got {:?}",
        st.tasks[0].status
    );
    drop(st);

    let events = sink.events.lock().unwrap();
    let pc = events
        .iter()
        .find_map(|e| match &e.body {
            SpurEventBody::PlanCompleted { failed, .. } => Some(*failed),
            _ => None,
        })
        .expect("PlanCompleted must be emitted");
    assert_eq!(
        pc, 1,
        "stuck Pending task must be counted as failed in PlanCompleted"
    );
}

// ─── Task 1: build_entries_with_task_map backfill tests ──────────────────────

/// Helper: three tasks a (no issue_id), b (no issue_id), c (no issue_id).
fn tasks_abc() -> Vec<PlanTask> {
    vec![
        PlanTask {
            task_id: "a".into(),
            agent: "claude-code-acp".into(),
            task: "Do A.".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        },
        PlanTask {
            task_id: "b".into(),
            agent: "claude-code-acp".into(),
            task: "Do B.".into(),
            depends_on: vec!["a".into()],
            issue_id: None,
            context_files: Vec::new(),
        },
        PlanTask {
            task_id: "c".into(),
            agent: "codex".into(),
            task: "Do C.".into(),
            depends_on: vec!["b".into()],
            issue_id: None,
            context_files: Vec::new(),
        },
    ]
}

/// Case 1: ephemeral plan (task_map = None) — every entry must have issue_id == None.
#[test]
fn build_entries_ephemeral_keeps_all_issue_ids_none() {
    let tasks = tasks_abc();
    let entries = build_entries_with_task_map(tasks, None);
    for entry in &entries {
        assert!(
            entry.spec.issue_id.is_none(),
            "ephemeral plan: expected issue_id=None for task {}, got {:?}",
            entry.spec.task_id,
            entry.spec.issue_id,
        );
    }
}

/// Case 2: persisted plan with partial task_map — matched tasks get beads IDs,
/// unmatched task ("c") keeps None.
#[test]
fn build_entries_backfills_task_map_and_leaves_unmatched_none() {
    use std::collections::HashMap;
    let tasks = tasks_abc();
    let mut task_map = HashMap::new();
    task_map.insert("a".to_string(), "bd-1".to_string());
    task_map.insert("b".to_string(), "bd-2".to_string());

    let entries = build_entries_with_task_map(tasks, Some(&task_map));

    let entry_a = entries.iter().find(|e| e.spec.task_id == "a").unwrap();
    let entry_b = entries.iter().find(|e| e.spec.task_id == "b").unwrap();
    let entry_c = entries.iter().find(|e| e.spec.task_id == "c").unwrap();

    assert_eq!(
        entry_a.spec.issue_id.as_deref(),
        Some("bd-1"),
        "task 'a' must be backfilled with bd-1"
    );
    assert_eq!(
        entry_b.spec.issue_id.as_deref(),
        Some("bd-2"),
        "task 'b' must be backfilled with bd-2"
    );
    assert!(
        entry_c.spec.issue_id.is_none(),
        "task 'c' has no task_map entry and must remain None"
    );
}

/// Case 3: pre-existing issue_id is NOT overwritten by task_map value.
/// Rationale: the incoming issue_id on PlanTask is a spur:source-issue reference
/// pointing to a pre-existing issue. The task_map value is the newly-created beads
/// child. Only populate when the field is None — the source-issue reference takes
/// precedence.
#[test]
fn build_entries_does_not_overwrite_existing_issue_id() {
    use std::collections::HashMap;
    let tasks = vec![PlanTask {
        task_id: "a".into(),
        agent: "claude-code-acp".into(),
        task: "Do A.".into(),
        depends_on: Vec::new(),
        // pre-existing source-issue reference
        issue_id: Some("bd-42".into()),
        context_files: Vec::new(),
    }];
    let mut task_map = HashMap::new();
    // task_map carries the newly-created beads child ID
    task_map.insert("a".to_string(), "bd-99".to_string());

    let entries = build_entries_with_task_map(tasks, Some(&task_map));

    assert_eq!(
        entries[0].spec.issue_id.as_deref(),
        Some("bd-42"),
        "pre-existing source-issue ref must NOT be overwritten by task_map; got {:?}",
        entries[0].spec.issue_id,
    );
}

// ─── Persisted submit direct-dispatch retirement regression ────────────────

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_pm::PmService;
use tempfile::TempDir;

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

struct PersistedSubmitFixture {
    #[allow(dead_code)]
    _dir: TempDir,
    server: McpCallbackServer,
    channel: spur_mcp::tools::DelegationChannel,
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

async fn persisted_submit_fixture() -> PersistedSubmitFixture {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["config", "user.email", "test@spur"]);
    run_git(dir.path(), &["config", "user.name", "spur-test"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
    run_git(dir.path(), &["add", "seed.txt"]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    run_br(dir.path(), &["init"]).expect("br init");
    let pm = beads_pm(dir.path()).await;
    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, channel) = McpCallbackServer::new(
        &session_id,
        Some(pm),
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    PersistedSubmitFixture {
        _dir: dir,
        server,
        channel,
    }
}

impl PersistedSubmitFixture {
    async fn submit_persisted_plan(&self) {
        let response = self
            .server
            .__test_call_submit_plan(json!({
                "persist_as_epic": true,
                "epic_title": "Persisted Submit Regression Epic",
                "tasks": [{
                    "task_id": "t1",
                    "agent": "codex",
                    "task": "Do something",
                    "depends_on": [],
                }]
            }))
            .await;
        assert!(
            response.get("error").is_none(),
            "submit_plan should succeed: {response}"
        );
    }
}

#[tokio::test]
async fn persisted_submit_plan_does_not_enqueue_delegation_request() {
    if !br_available() {
        eprintln!(
            "skipping persisted_submit_plan_does_not_enqueue_delegation_request: `br` not on PATH"
        );
        return;
    }

    let mut fixture = persisted_submit_fixture().await;
    fixture.submit_persisted_plan().await;

    let recv = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.channel.request_rx.recv(),
    )
    .await;

    assert!(
        recv.is_err(),
        "persisted submit_plan must not dispatch directly"
    );
}
