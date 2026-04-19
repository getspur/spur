//! submit_plan persist_as_epic — unit + integration tests over pure helpers.
//!
//! Because PmService is a concrete struct (not a trait) today, live-beads
//! integration is covered at the CLI level elsewhere. Here we test the
//! pure helper that decides WHAT IssueCreate values the handler would
//! dispatch given a plan + epic fields.

use spur_mcp::{plan_epic_issue_creates, tools_list};
// `pub mod plan;` is declared in lib.rs, so spur_mcp::plan::PlanTask is accessible.
use spur_mcp::plan::PlanTask;

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
        epic.labels.iter().any(|l| l == "spur.plan_id=plan-xyz"),
        "epic must carry spur.plan_id label; got {:?}",
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
    assert!(labels.iter().any(|l| l == "spur.plan_id=plan-xyz"));
    assert!(labels.iter().any(|l| l == "spur.plan_task_id=b"));
    assert!(labels.iter().any(|l| l == "spur.agent=claude-code-acp"));
    assert!(
        labels.iter().any(|l| l == "spur.source_issue=bd-42"),
        "child b sourced from bd-42 must carry spur.source_issue label"
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

/// INV-7: verify that `run_plan` emits `PlanCompleted` and `PlanReadyToMerge`
/// when all tasks are already in a terminal Approved state on entry (so the
/// executor loop exits immediately without dispatching).
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
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
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

    run_plan(Arc::new(Mutex::new(state)), dtx, Some(sink_ref)).await;

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
        saw_ready,
        "PlanReadyToMerge must be emitted (all Approved); got: {:?}",
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
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
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
            Some(sleepy_pm.as_ref()),
            None,
            None,
            None,
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
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        epic_id: None,
    };

    let sink = Arc::new(CaptureSink {
        events: std::sync::Mutex::new(Vec::new()),
    });
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let (dtx, _drx) = mpsc::channel(8);
    let plan_arc = Arc::new(Mutex::new(state));

    run_plan(Arc::clone(&plan_arc), dtx, Some(sink_ref)).await;

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
