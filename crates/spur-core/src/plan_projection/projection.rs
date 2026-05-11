use std::collections::HashMap;

use spur_acp::{PlanSnapshot, SessionId, SpurEvent, SpurEventBody};

use super::types::{TrackedPlan, TrackedTask};

#[derive(Debug, Default, Clone)]
pub struct PlanProjectionStore {
    by_plan: HashMap<String, TrackedPlan>,
    current_by_session: HashMap<SessionId, String>,
}

impl PlanProjectionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, event: &SpurEvent) {
        let SpurEventBody::PlanSnapshotUpdated {
            session_id,
            snapshot,
        } = &event.body
        else {
            return;
        };

        let tracked = tracked_plan_from_snapshot(session_id.clone(), snapshot, event.occurred_at);
        let replace_current = self
            .current_by_session
            .get(session_id)
            .and_then(|plan_id| self.by_plan.get(plan_id))
            .map(|current| should_promote_current(current, &tracked))
            .unwrap_or(true);

        if replace_current {
            self.current_by_session
                .insert(session_id.clone(), tracked.plan_id.clone());
        }

        self.by_plan.insert(tracked.plan_id.clone(), tracked);
    }

    pub fn plan(&self, plan_id: &str) -> Option<&TrackedPlan> {
        self.by_plan.get(plan_id)
    }

    pub fn current_for_session(&self, session_id: &SessionId) -> Option<&TrackedPlan> {
        self.current_by_session
            .get(session_id)
            .and_then(|plan_id| self.by_plan.get(plan_id))
            .or_else(|| {
                self.by_plan
                    .values()
                    .filter(|plan| &plan.session_id == session_id)
                    .max_by_key(|plan| (plan.is_active(), plan.updated_at))
            })
    }

    pub fn plans(&self) -> impl Iterator<Item = &TrackedPlan> {
        self.by_plan.values()
    }
}

fn should_promote_current(current: &TrackedPlan, incoming: &TrackedPlan) -> bool {
    if current.plan_id == incoming.plan_id {
        return true;
    }

    match (current.is_active(), incoming.is_active()) {
        (false, true) => true,
        (true, false) => false,
        _ => incoming.updated_at >= current.updated_at,
    }
}

fn tracked_plan_from_snapshot(
    session_id: SessionId,
    snapshot: &PlanSnapshot,
    updated_at: std::time::SystemTime,
) -> TrackedPlan {
    let stage_by_task_id = derive_stage_indices(snapshot);
    TrackedPlan {
        session_id,
        plan_id: snapshot.plan_id.clone(),
        epic_id: snapshot.epic_id.clone(),
        status: snapshot.status.clone(),
        progress: snapshot.progress.clone(),
        next_action: snapshot.next_action.clone(),
        ready_to_merge: snapshot.ready_to_merge,
        owner_brain_session_id: snapshot.owner_brain_session_id.clone(),
        counts: snapshot.counts.clone(),
        tasks: snapshot
            .tasks
            .iter()
            .map(|task| TrackedTask {
                task_id: task.task_id.clone(),
                task_name: task.task_name.clone(),
                agent: task.agent.clone(),
                issue_id: task.issue_id.clone(),
                issue_title: task.issue_title.clone(),
                status: task.status.clone(),
                attempt: task.attempt,
                max_attempts: task.max_attempts,
                depends_on: task.depends_on.clone(),
                blocked_by: task.blocked_by.clone(),
                unblocks: task.unblocks.clone(),
                summary: task.summary.clone(),
                feedback: task.feedback.clone(),
                error: task.error.clone(),
                worker_branch: task.worker_branch.clone(),
                delegation_id: task.delegation_id.clone(),
                diff_summary: task.diff_summary.clone(),
                mutation_id: task.mutation_id.clone(),
                superseded_by: task.superseded_by.clone(),
                next_action: task.next_action.clone(),
                stage_idx: *stage_by_task_id.get(&task.task_id).unwrap_or(&0),
            })
            .collect(),
        updated_at,
    }
}

fn derive_stage_indices(snapshot: &PlanSnapshot) -> HashMap<String, usize> {
    let deps_by_task: HashMap<String, Vec<String>> = snapshot
        .tasks
        .iter()
        .map(|task| (task.task_id.clone(), task.depends_on.clone()))
        .collect();
    let mut stage_by_task = HashMap::new();
    let mut stack = Vec::new();

    for task in &snapshot.tasks {
        let stage = derive_stage_idx(&task.task_id, &deps_by_task, &mut stage_by_task, &mut stack);
        stage_by_task.insert(task.task_id.clone(), stage);
    }

    stage_by_task
}

fn derive_stage_idx(
    task_id: &str,
    deps_by_task: &HashMap<String, Vec<String>>,
    stage_by_task: &mut HashMap<String, usize>,
    stack: &mut Vec<String>,
) -> usize {
    if let Some(stage) = stage_by_task.get(task_id) {
        return *stage;
    }

    if stack.iter().any(|seen| seen == task_id) {
        return 0;
    }

    stack.push(task_id.to_string());
    let stage = deps_by_task
        .get(task_id)
        .map(|parents| {
            parents
                .iter()
                .map(|parent| derive_stage_idx(parent, deps_by_task, stage_by_task, stack) + 1)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    stack.pop();

    stage_by_task.insert(task_id.to_string(), stage);
    stage
}
