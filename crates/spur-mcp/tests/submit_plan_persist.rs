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

/// INV-5: verify that `handle_review_task` releases the plan-state lock BEFORE
/// it calls `pm.update_issue`, so concurrent readers are not blocked by network
/// latency.
///
/// Mechanism: a `SleepyPm` suspends inside `update_issue`. With
/// `current_thread + start_paused`, the sleep suspends the approve task but
/// does NOT advance virtual time until all tasks are waiting. After two
/// `yield_now`s the approve task has had a chance to either (a) drop the lock
/// before sleeping (fixed) or (b) still be holding the lock during the sleep
/// (unfixed). We use `try_lock` to observe which case we are in — it must
/// succeed with the fix in place.
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

    // SleepyPm sleeps 1 s (virtual) inside update_issue.
    let sleepy_pm = spur_mcp::test_support::make_sleepy_pm(Duration::from_secs(1));

    // Start approve in the background.
    let plan_ref = Arc::clone(&plan_arc);
    let pm_ref = sleepy_pm.clone();
    let approve = tokio::spawn(async move {
        spur_mcp::plan::handle_review_task(
            plan_ref,
            "p1",
            "t1",
            "approve",
            Some("ok"),
            Some(pm_ref.as_ref()),
            None,
            None,
            None,
        )
        .await
    });

    // Yield twice so the approve task can run:
    //   1st yield: approve acquires lock, runs apply_decision_and_extract, drops lock, enters
    //              update_issue → tokio::time::sleep(1s) → task suspends.
    //   2nd yield: (belt-and-suspenders, in case scheduler needs another round)
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // With the fix the lock must be available now — approve dropped it before sleeping.
    // Without the fix it is still held, and try_lock returns Err.
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
