//! Independent regression review: active external blockers remain scheduling gates.
mod common;

use spur_core::plan::audit_sentinel::{encode_comment, AuditSentinelKind};
use spur_core::plan::{labels, projector::project_plan_from_beads, PlanTaskStatus};
use spur_pm::IssueCreate;

#[tokio::test]
async fn legacy_projection_keeps_active_external_blockers_pending() {
    let (_dir, pm) = common::temp_beads_pm().await;
    let plan_id = "PLAN-EXTERNAL-REVIEW";
    let external = common::create_task(pm.as_ref(), "Outside-plan prerequisite").await;
    let epic = pm
        .create_issue(IssueCreate {
            title: "External blocker review".into(),
            issue_type: Some("epic".into()),
            labels: vec![labels::plan_id(plan_id)],
            ..Default::default()
        })
        .await
        .expect("create epic");
    let child = pm
        .create_issue(IssueCreate {
            title: "Blocked plan task".into(),
            issue_type: Some("task".into()),
            parent: Some(epic),
            depends_on: vec![external.clone()],
            labels: vec![
                labels::plan_id(plan_id),
                labels::plan_task_id("T1"),
                labels::agent("codex"),
            ],
            ..Default::default()
        })
        .await
        .expect("create child");
    let active = pm.get_issue(&child).await.expect("read active blockers");
    assert_eq!(active.blocked_by, vec![external.clone()]);

    let gate = common::pro_feature_gate();
    let plan = project_plan_from_beads(pm.as_ref(), plan_id, gate.as_ref())
        .await
        .expect("project legacy plan");
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(
        plan.tasks[0].spec.depends_on,
        vec![external],
        "active external dependencies must not vanish from the projected gate"
    );
    assert!(matches!(plan.tasks[0].status, PlanTaskStatus::Pending));
}

#[tokio::test]
async fn projection_deduplicates_durable_task_and_active_issue_ids() {
    let (_dir, pm) = common::temp_beads_pm().await;
    let plan_id = "PLAN-CANONICAL-DEPENDENCY-REVIEW";
    let epic = pm
        .create_issue(IssueCreate {
            title: "Canonical dependency review".into(),
            issue_type: Some("epic".into()),
            labels: vec![labels::plan_id(plan_id)],
            ..Default::default()
        })
        .await
        .expect("create epic");
    let mut prerequisite: Option<String> = None;
    for (task_id, durable_dependencies) in [("T1", vec![]), ("T2", vec!["T1".to_string()])] {
        let child = pm
            .create_issue(IssueCreate {
                title: task_id.into(),
                issue_type: Some("task".into()),
                parent: Some(epic.clone()),
                depends_on: prerequisite.iter().cloned().collect(),
                labels: vec![
                    labels::plan_id(plan_id),
                    labels::plan_task_id(task_id),
                    labels::agent("codex"),
                ],
                ..Default::default()
            })
            .await
            .expect("create task");
        let spec = AuditSentinelKind::TaskSpec {
            task_id: task_id.into(),
            context_files: Vec::new(),
            planned_write_files: None,
            profile: None,
            skills: None,
            model: None,
            effort: None,
            config_overrides: None,
            task_text: None,
            agent: Some("codex".into()),
            depends_on: Some(durable_dependencies),
        };
        pm.advanced()
            .expect("Beads advanced API")
            .add_comment(&child, &encode_comment(&spec))
            .await
            .expect("record durable TaskSpec");
        prerequisite = Some(child);
    }

    let gate = common::pro_feature_gate();
    let plan = project_plan_from_beads(pm.as_ref(), plan_id, gate.as_ref())
        .await
        .expect("project extended plan");
    let downstream = plan
        .tasks
        .iter()
        .find(|task| task.spec.task_id == "T2")
        .expect("downstream task");
    assert_eq!(
        downstream.spec.depends_on,
        vec!["T1"],
        "durable task ID and active issue ID must become one canonical dependency"
    );
    assert!(matches!(downstream.status, PlanTaskStatus::Pending));
}
