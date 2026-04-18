//! submit_plan persist_as_epic — unit + integration tests over pure helpers.
//!
//! Because PmService is a concrete struct (not a trait) today, live-beads
//! integration is covered at the CLI level elsewhere. Here we test the
//! pure helper that decides WHAT IssueCreate values the handler would
//! dispatch given a plan + epic fields.

use serde_json::json;
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
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "Title", None, &tasks).expect("ok");
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
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "T", None, &tasks).expect("ok");
    let (_, child_b) = children.iter().find(|(k, _)| k == "b").unwrap();
    assert_eq!(child_b.depends_on, vec!["a".to_string()]);
}

#[test]
fn children_parent_field_is_unset_before_epic_creation() {
    let tasks = sample_tasks(false);
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "T", None, &tasks).expect("ok");
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

#[allow(dead_code)]
fn _unused() -> serde_json::Value {
    json!({})
}
