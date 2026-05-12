use spur_acp::{PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SpurEventBody};

use crate::events::McpEventSink;

use super::{
    build_plan_status, display_name, AttemptRecordKind, PlanState, PlanTaskEntry, PlanTaskStatus,
    MAX_ATTEMPTS,
};

pub fn build_plan_snapshot(state: &PlanState) -> PlanSnapshot {
    let status = build_plan_status(&state.plan_id, state);
    PlanSnapshot {
        plan_id: state.plan_id.clone(),
        epic_id: state.epic_id.clone(),
        status: status["status"].as_str().unwrap_or("partial").to_string(),
        progress: status["progress"].as_str().unwrap_or_default().to_string(),
        next_action: status["next_action"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        ready_to_merge: status["ready_to_merge"].as_bool().unwrap_or(false),
        counts: build_snapshot_counts(state),
        tasks: state
            .tasks
            .iter()
            .map(|task| build_task_snapshot(state, task))
            .collect(),
        owner_brain_session_id: owner_brain_session_id_from_state(state),
        // TODO: populate from latest PlanOwnershipAcquired/Transferred audit sentinel.
        // Derivation requires scanning the epic audit history, which the snapshot
        // builder does not currently access — left as None for this slice.
        owner_token: None,
        owner_acquired_at: None,
    }
}

/// Derive `PlanSnapshot.owner_brain_session_id` from the brain session id on
/// `PlanState`. Returns `None` for the projector's `persisted-plan:*` fallback
/// (used when no `PlanSubmit` audit recorded a brain session id), which
/// indicates the original submitter is unknown rather than asserting an owner
/// that never existed.
fn owner_brain_session_id_from_state(state: &PlanState) -> Option<String> {
    let raw = &state.brain_session_id.as_session_id().0;
    if raw.starts_with("persisted-plan:") {
        None
    } else {
        Some(raw.clone())
    }
}

pub fn emit_plan_snapshot(sink: Option<&dyn McpEventSink>, state: &PlanState) {
    if state.epic_id.is_none() {
        return;
    }
    let Some(sink) = sink else {
        return;
    };
    sink.emit(SpurEventBody::PlanSnapshotUpdated {
        session_id: state.brain_session_id.as_session_id().clone(),
        snapshot: Box::new(build_plan_snapshot(state)),
    });
}

fn build_snapshot_counts(state: &PlanState) -> PlanSnapshotCounts {
    let mut counts = PlanSnapshotCounts::default();
    for task in &state.tasks {
        match task.status {
            PlanTaskStatus::Pending => counts.pending += 1,
            PlanTaskStatus::Ready => counts.ready += 1,
            PlanTaskStatus::Dispatched { .. } => counts.dispatched += 1,
            PlanTaskStatus::AwaitingReview { .. } => counts.awaiting_review += 1,
            PlanTaskStatus::Approved { .. } => counts.approved += 1,
            PlanTaskStatus::Rejected { .. } => counts.rejected += 1,
            PlanTaskStatus::Failed { .. } => counts.failed += 1,
            PlanTaskStatus::Cancelled { .. } | PlanTaskStatus::Superseded { .. } => {
                counts.cancelled += 1
            }
            PlanTaskStatus::BlockedOnSetupConflict { .. } => counts.pending += 1,
            // bd-2m2u Phase 2d — currently-escalated tasks pause the engine
            // until brain `submit_plan_mutation` resolves them.
            PlanTaskStatus::EscalatedToBrain { .. } => counts.escalated += 1,
        }
        // bd-2m2u Phase 2d — observability roll-up: every WorkerFailureRecovery
        // history record on every task contributes to `auto_retried`.
        for record in &task.history {
            if matches!(record.kind(), AttemptRecordKind::WorkerFailureRecovery) {
                counts.auto_retried += 1;
            }
        }
    }
    counts
}

fn build_task_snapshot(state: &PlanState, task: &PlanTaskEntry) -> PlanSnapshotTask {
    let diff_summary = task
        .result
        .as_ref()
        .and_then(|result| result.diff_summary.clone());
    let (
        status,
        summary,
        feedback,
        error,
        worker_branch,
        delegation_id,
        mutation_id,
        superseded_by,
    ) = match &task.status {
        PlanTaskStatus::Pending => (
            "pending".to_string(),
            task.result
                .as_ref()
                .and_then(|result| result.summary.clone()),
            None,
            None,
            task.worker_branch.clone(),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::Ready => (
            "ready".to_string(),
            task.result
                .as_ref()
                .and_then(|result| result.summary.clone()),
            None,
            None,
            task.worker_branch.clone(),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::Dispatched { delegation_id } => (
            "dispatched".to_string(),
            task.result
                .as_ref()
                .and_then(|result| result.summary.clone()),
            None,
            None,
            task.worker_branch.clone(),
            Some(delegation_id.clone()),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::AwaitingReview { summary } => (
            "awaiting_review".to_string(),
            summary.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.summary.clone())
            }),
            None,
            None,
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::Approved { summary } => (
            "approved".to_string(),
            summary.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.summary.clone())
            }),
            None,
            None,
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::Rejected { feedback } => (
            "rejected".to_string(),
            task.result
                .as_ref()
                .and_then(|result| result.summary.clone()),
            feedback.clone(),
            None,
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::Failed { error } => (
            "failed".to_string(),
            task.result
                .as_ref()
                .and_then(|result| result.summary.clone()),
            None,
            Some(error.clone()),
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::Cancelled { reason } => (
            "cancelled".to_string(),
            None,
            None,
            Some(reason.clone()),
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::Superseded { mutation_id, by } => (
            "superseded".to_string(),
            None,
            None,
            None,
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            Some(mutation_id.clone()),
            by.clone(),
        ),
        PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files } => (
            "blocked_on_setup_conflict".to_string(),
            Some(format!(
                "Setup overlay conflict applying {dep_task_id}: {} file(s)",
                files.len()
            )),
            None,
            Some(files.join(", ")),
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
        PlanTaskStatus::EscalatedToBrain { last_error } => (
            "escalated_to_brain".to_string(),
            task.result
                .as_ref()
                .and_then(|result| result.summary.clone()),
            None,
            Some(last_error.clone()),
            task.worker_branch.clone().or_else(|| {
                task.result
                    .as_ref()
                    .and_then(|result| result.worker_branch.clone())
            }),
            task.last_delegation_id.clone(),
            None,
            Vec::new(),
        ),
    };

    PlanSnapshotTask {
        task_id: task.spec.task_id.clone(),
        task_name: display_name(&task.spec.task),
        agent: task.spec.agent.clone(),
        issue_id: task.spec.issue_id.clone(),
        issue_title: task.spec.issue_title.clone(),
        status,
        attempt: task.attempt,
        max_attempts: MAX_ATTEMPTS,
        depends_on: task.spec.depends_on.clone(),
        blocked_by: blocked_by(state, task),
        unblocks: unblocks(state, task),
        summary,
        feedback,
        error,
        worker_branch,
        delegation_id,
        diff_summary,
        mutation_id,
        superseded_by,
        next_action: task_next_action(task),
    }
}

fn blocked_by(state: &PlanState, task: &PlanTaskEntry) -> Vec<String> {
    if !matches!(task.status, PlanTaskStatus::Pending) {
        return Vec::new();
    }

    task.spec
        .depends_on
        .iter()
        .filter(|dep| {
            !state.tasks.iter().any(|other| {
                other.spec.task_id == ***dep
                    && matches!(
                        other.status,
                        PlanTaskStatus::Approved { .. }
                            | PlanTaskStatus::Cancelled { .. }
                            | PlanTaskStatus::Superseded { .. }
                    )
            })
        })
        .cloned()
        .collect()
}

fn unblocks(state: &PlanState, task: &PlanTaskEntry) -> Vec<String> {
    state
        .tasks
        .iter()
        .filter(|other| {
            other
                .spec
                .depends_on
                .iter()
                .any(|dep| dep == &task.spec.task_id)
        })
        .map(|other| other.spec.task_id.clone())
        .collect()
}

fn task_next_action(task: &PlanTaskEntry) -> String {
    match task.status {
        PlanTaskStatus::AwaitingReview { .. } => "review".to_string(),
        PlanTaskStatus::Rejected { .. } | PlanTaskStatus::Failed { .. } => "inspect".to_string(),
        PlanTaskStatus::BlockedOnSetupConflict { .. } => "inspect".to_string(),
        // bd-2m2u Phase 2d — brain must call `submit_plan_mutation` to
        // resolve. Surfaced as "submit_mutation" so TUI / brain UX can
        // route directly to the recovery flow.
        PlanTaskStatus::EscalatedToBrain { .. } => "submit_mutation".to_string(),
        PlanTaskStatus::Pending | PlanTaskStatus::Ready | PlanTaskStatus::Dispatched { .. } => {
            "wait".to_string()
        }
        PlanTaskStatus::Approved { .. }
        | PlanTaskStatus::Cancelled { .. }
        | PlanTaskStatus::Superseded { .. } => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_plan_snapshot, PlanState, PlanTaskEntry, PlanTaskStatus};
    use crate::plan::{PlanMergeState, PlanTask};
    use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus, DiffSummary, SessionId};

    fn sample_entry(task_id: &str, depends_on: &[&str], status: PlanTaskStatus) -> PlanTaskEntry {
        PlanTaskEntry {
            spec: PlanTask {
                task_id: task_id.to_string(),
                agent: "codex".into(),
                task: format!("Task {task_id}"),
                depends_on: depends_on.iter().map(|dep| dep.to_string()).collect(),
                issue_id: Some(format!("bd-{task_id}")),
                issue_title: None,
                context_files: Vec::new(),
            },
            status,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: None,
            dispatched_base_oid: None,
        }
    }

    fn sample_state(tasks: Vec<PlanTaskEntry>) -> PlanState {
        PlanState {
            plan_id: "plan-1".into(),
            tasks,
            brain_session_id: BrainSessionId::new(SessionId("brain-1".into())),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        }
    }

    #[test]
    fn build_plan_snapshot_preserves_superseded_metadata_and_diff_summary() {
        let mut superseded = sample_entry(
            "task-old",
            &[],
            PlanTaskStatus::Superseded {
                mutation_id: "mut-1".into(),
                by: vec!["task-new".into()],
            },
        );
        superseded.worker_branch = Some("spur/worker-old".into());
        superseded.result = Some(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: Some(DiffSummary {
                files_changed: 2,
                insertions: 10,
                deletions: 4,
                files: Vec::new(),
            }),
            summary: Some("superseded work".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-old".into()),
            artifact: None,
        });

        let snapshot = build_plan_snapshot(&sample_state(vec![superseded]));
        let task = &snapshot.tasks[0];
        assert_eq!(task.status, "superseded");
        assert_eq!(task.mutation_id.as_deref(), Some("mut-1"));
        assert_eq!(task.superseded_by, vec!["task-new".to_string()]);
        assert_eq!(
            task.diff_summary.as_ref().map(|summary| (
                summary.files_changed,
                summary.insertions,
                summary.deletions
            )),
            Some((2, 10, 4))
        );
    }

    #[test]
    fn blocked_by_treats_superseded_dependencies_as_satisfied() {
        let parent = sample_entry(
            "task-parent",
            &[],
            PlanTaskStatus::Superseded {
                mutation_id: "mut-1".into(),
                by: vec!["task-child".into()],
            },
        );
        let pending = sample_entry("task-leaf", &["task-parent"], PlanTaskStatus::Pending);

        let snapshot = build_plan_snapshot(&sample_state(vec![parent, pending]));
        let leaf = snapshot
            .tasks
            .iter()
            .find(|task| task.task_id == "task-leaf")
            .expect("leaf task");
        assert!(
            leaf.blocked_by.is_empty(),
            "superseded parents should not block"
        );
    }

    #[test]
    fn build_plan_snapshot_populates_owner_brain_session_id_from_state() {
        let pending = sample_entry("task-1", &[], PlanTaskStatus::Pending);
        let snapshot = build_plan_snapshot(&sample_state(vec![pending]));
        assert_eq!(
            snapshot.owner_brain_session_id.as_deref(),
            Some("brain-1"),
            "owner_brain_session_id should mirror PlanState.brain_session_id",
        );
        assert!(
            snapshot.owner_token.is_none(),
            "owner_token is a None placeholder for this slice"
        );
        assert!(
            snapshot.owner_acquired_at.is_none(),
            "owner_acquired_at is a None placeholder for this slice"
        );
    }

    #[test]
    fn build_plan_snapshot_omits_owner_for_persisted_plan_fallback() {
        // Mirrors the projector fallback when no PlanSubmit audit recorded a brain
        // session id: state.brain_session_id is `persisted-plan:<plan_id>`. That
        // value is a placeholder, not a real owner — the snapshot must surface
        // None so consumers don't render a fictitious owner label.
        let pending = sample_entry("task-1", &[], PlanTaskStatus::Pending);
        let mut state = sample_state(vec![pending]);
        state.brain_session_id = BrainSessionId::new(SessionId("persisted-plan:plan-1".into()));
        let snapshot = build_plan_snapshot(&state);
        assert!(
            snapshot.owner_brain_session_id.is_none(),
            "persisted-plan:* fallback must not surface as an owner"
        );
    }
}
