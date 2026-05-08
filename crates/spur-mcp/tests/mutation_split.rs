//! T-F6: SplitTask happy path — children created, downstream rewired, audit trail emitted.

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::labels::{mutation_id_label, signal_processed_label, superseded_by_labels};
use spur_mcp::plan::mutation::{DepRewirePolicy, MutationBatch, PlanMutationOp, TaskDraft};
use spur_mcp::plan::mutation_executor::apply_mutation;
use tempfile::TempDir;
use uuid::Uuid;

mod common;

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
}

fn br_id(raw: &str) -> String {
    raw.trim().trim_matches('"').to_string()
}

fn sentinels_from_comments(comments: &[spur_pm::Comment]) -> Vec<AuditSentinelKind> {
    comments
        .iter()
        .filter_map(|comment| audit_sentinel::parse_comment(&comment.body))
        .filter_map(|result| result.ok())
        .collect()
}

fn task_draft(title: &str, description: &str) -> TaskDraft {
    serde_json::from_value(json!({
        "title": title,
        "description": description,
        "assignee": null,
        "priority": null
    }))
    .expect("TaskDraft JSON must deserialize")
}

fn mutation_batch(
    mutation_id: Uuid,
    trigger_signal_id: Option<Uuid>,
    trigger_task_id: String,
    ops: Vec<PlanMutationOp>,
) -> MutationBatch {
    serde_json::from_value(json!({
        "mutation_id": mutation_id,
        "trigger_signal_id": trigger_signal_id,
        "trigger_task_id": trigger_task_id,
        "ops": ops
    }))
    .expect("MutationBatch JSON must deserialize")
}

#[tokio::test]
async fn split_task_happy_path_rewires_downstream_and_commits() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]).expect("br init failed");

    let parent =
        br_id(&run_br(dir.path(), &["create", "Parent", "--silent", "-t", "task"]).unwrap());
    let downstream = br_id(
        &run_br(
            dir.path(),
            &["create", "Downstream", "--silent", "-t", "task"],
        )
        .unwrap(),
    );
    run_br(dir.path(), &["dep", "add", &downstream, &parent]).expect("seed downstream dep");

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads-backed PmService"),
    );

    let batch = mutation_batch(
        Uuid::new_v4(),
        Some(Uuid::new_v4()),
        parent.clone(),
        vec![PlanMutationOp::SplitTask {
            parent: parent.clone(),
            children: vec![
                task_draft("Child A", "First split child"),
                task_draft("Child B", "Second split child"),
            ],
            dep_rewire: DepRewirePolicy::Barrier,
        }],
    );

    let child_ids = apply_mutation(
        pm.clone(),
        common::server_builder::pro_feature_gate(),
        &batch,
    )
    .await
    .expect("apply_mutation should succeed");
    assert_eq!(child_ids.len(), 2, "expected 2 created children");

    let parent_issue = pm.get_issue(&parent).await.expect("load parent");
    assert_eq!(
        parent_issue.status,
        pm.closed_status(),
        "superseded parent should be mapped to backend closed status"
    );
    let expected_parent_labels = superseded_by_labels(&child_ids);
    for label in &expected_parent_labels {
        assert!(
            parent_issue.labels.iter().any(|existing| existing == label),
            "missing parent superseded-by label {label}; labels={:?}",
            parent_issue.labels
        );
    }
    let processed_label =
        signal_processed_label(&batch.trigger_signal_id.expect("trigger_signal_id"));
    assert!(
        parent_issue
            .labels
            .iter()
            .any(|label| label == &processed_label),
        "missing signal processed label {processed_label}; labels={:?}",
        parent_issue.labels
    );

    let mutation_label = mutation_id_label(&batch.mutation_id);
    for child_id in &child_ids {
        let child = pm.get_issue(child_id).await.expect("load child");
        assert!(
            child.labels.iter().any(|label| label == &mutation_label),
            "child {child_id} missing mutation label {mutation_label}; labels={:?}",
            child.labels
        );
    }

    let downstream_issue = pm.get_issue(&downstream).await.expect("load downstream");
    assert!(
        !downstream_issue.blocked_by.iter().any(|dep| dep == &parent),
        "downstream must no longer depend on parent; blocked_by={:?}",
        downstream_issue.blocked_by
    );
    for child_id in &child_ids {
        assert!(
            downstream_issue
                .blocked_by
                .iter()
                .any(|dep| dep == child_id),
            "downstream must depend on child {child_id}; blocked_by={:?}",
            downstream_issue.blocked_by
        );
    }

    let comments = pm
        .advanced()
        .expect("advanced surface")
        .list_comments(&parent)
        .await
        .expect("list comments");
    let sentinels = sentinels_from_comments(&comments);

    let plan_pos = sentinels.iter().position(|sentinel| {
        matches!(
            sentinel,
            AuditSentinelKind::MutationPlan {
                mutation_id,
                trigger_task_id,
                ..
            } if mutation_id == &batch.mutation_id.to_string() && trigger_task_id == &parent
        )
    });
    let commit_pos = sentinels.iter().position(|sentinel| {
        matches!(
            sentinel,
            AuditSentinelKind::MutationCommit {
                mutation_id,
                children_created,
                op_tags,
                affected_task_ids,
            } if mutation_id == &batch.mutation_id.to_string()
                && children_created == &child_ids
                && op_tags.as_slice() == ["split_task"]
                && affected_task_ids.first().map(String::as_str) == Some(parent.as_str())
        )
    });

    assert!(
        plan_pos.is_some(),
        "MutationPlan sentinel missing: {sentinels:?}"
    );
    assert!(
        commit_pos.is_some(),
        "MutationCommit sentinel missing: {sentinels:?}"
    );
    assert!(
        plan_pos.unwrap() < commit_pos.unwrap(),
        "MutationPlan must precede MutationCommit: {sentinels:?}"
    );

    let cycles = pm
        .advanced()
        .expect("advanced surface")
        .dep_cycles()
        .await
        .expect("dep_cycles");
    assert!(
        cycles.is_empty(),
        "happy path must remain acyclic: {cycles:?}"
    );
}
