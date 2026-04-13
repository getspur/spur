//! Fold legacy `SpurEvent` variants into `ExecutorLineage`.
//!
//! For v1 of executor-lineage the orchestrator has not yet been updated to
//! emit the new `Executor*` events directly. This adapter synthesises the
//! minimal set of state transitions from events already in the wild so the
//! TUI can render lineage without orchestrator-side changes.

use std::time::SystemTime;

use spur_acp::{DelegationStatus, SpurEvent, SpurEventBody};

use super::projection::ExecutorLineage;
use super::types::{
    Attempt, AttemptStatus, ExecutorId, ExecutorNode, LifecycleState, Role,
};

pub fn apply_legacy(lineage: &mut ExecutorLineage, event: &SpurEvent) {
    match &event.body {
        SpurEventBody::BrainSpawned { agent, session } => {
            let id = ExecutorId::new(session.0.clone());
            if lineage.node(&id).is_some() {
                return;
            }
            lineage.insert_root_node(ExecutorNode {
                id: id.clone(),
                parent_id: None,
                child_ids: Vec::new(),
                agent: agent.clone(),
                role: Role::Brain,
                task_spec: String::new(),
                phase: LifecycleState::Running,
                attempts: vec![fresh_attempt(session.clone(), event.occurred_at)],
                pending_review: None,
            });
        }

        SpurEventBody::WorkerSpawned { agent, session, worktree: _ } => {
            let id = ExecutorId::new(session.0.clone());
            if lineage.node(&id).is_some() {
                return;
            }
            let parent = lineage
                .root_ids()
                .iter()
                .rev()
                .find(|rid| lineage.node(rid).map(|n| n.role == Role::Brain).unwrap_or(false))
                .cloned();
            let node = ExecutorNode {
                id: id.clone(),
                parent_id: parent.clone(),
                child_ids: Vec::new(),
                agent: agent.clone(),
                role: Role::Executor,
                task_spec: String::new(),
                phase: LifecycleState::Running,
                attempts: vec![fresh_attempt(session.clone(), event.occurred_at)],
                pending_review: None,
            };
            match parent {
                Some(p) => lineage.attach_child(&p, node),
                None => lineage.insert_root_node(node),
            }
        }

        SpurEventBody::DelegationRequested { from: _, to_agent, task } => {
            // Populate the task_spec of the most recent Executor owned by the
            // worker name, if empty.
            //
            // Known v1 limitation: `DelegationRequested` carries `from` (brain
            // session) and `to_agent` (agent name) but not the worker session
            // id. If two workers share an agent name with empty task_specs,
            // the most-recent one wins. Acceptable for v1 because (a) the
            // assignment is not destructive (empty-only), and (b) follow-up
            // spec will switch the orchestrator to emit `ExecutorSpawned`
            // directly with task_spec populated, removing this path.
            let id = lineage
                .nodes_mut_vec()
                .into_iter()
                .rev()
                .find(|n| n.role == Role::Executor && n.agent == *to_agent && n.task_spec.is_empty())
                .map(|n| n.id.clone());
            if let Some(id) = id {
                if let Some(n) = lineage.node_mut_public(&id) {
                    n.task_spec = task.clone();
                }
            }
        }

        SpurEventBody::DelegationCompleted { worker_session, status } => {
            let id = ExecutorId::new(worker_session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                let (phase, attempt_status, error) = match status {
                    DelegationStatus::Success => (LifecycleState::Succeeded, AttemptStatus::Succeeded, None),
                    DelegationStatus::Failed { error } => (LifecycleState::Failed, AttemptStatus::Failed, Some(error.clone())),
                    DelegationStatus::Timeout => (LifecycleState::Failed, AttemptStatus::Failed, Some("timeout".into())),
                    DelegationStatus::Conflict { files } => (LifecycleState::Failed, AttemptStatus::Failed, Some(format!("conflict in {} file(s)", files.len()))),
                    DelegationStatus::Rejected { reason } => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some(reason.clone()),
                    ),
                    DelegationStatus::Modified { reviewer_note } => (
                        LifecycleState::Succeeded,
                        AttemptStatus::Succeeded,
                        Some(format!("reviewer note: {}", reviewer_note)),
                    ),
                    DelegationStatus::TimedOut { waited_for, fallback } => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some(format!(
                            "review timeout after {}s (fallback: {:?})",
                            waited_for.as_secs(),
                            fallback
                        )),
                    ),
                    _ => {
                        tracing::warn!("unknown DelegationStatus variant — projection needs updating");
                        (LifecycleState::Failed, AttemptStatus::Failed, None)
                    }
                };
                n.phase = phase;
                if let Some(a) = n.current_attempt_mut() {
                    a.ended_at = Some(event.occurred_at);
                    a.status = attempt_status;
                    a.error = error;
                }
            }
        }

        SpurEventBody::SessionCompleted { session, success } => {
            let id = ExecutorId::new(session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                n.phase = if *success { LifecycleState::Succeeded } else { LifecycleState::Failed };
                if let Some(a) = n.current_attempt_mut() {
                    a.ended_at = Some(event.occurred_at);
                    a.status = if *success { AttemptStatus::Succeeded } else { AttemptStatus::Failed };
                }
            }
        }

        SpurEventBody::CostUpdate { session, agent: _, estimated_cost_usd } => {
            let id = ExecutorId::new(session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                if let Some(a) = n.current_attempt_mut() {
                    a.cost_usd += estimated_cost_usd;
                }
            }
        }

        _ => {}
    }
}

fn fresh_attempt(session: spur_acp::SessionId, started_at: SystemTime) -> Attempt {
    Attempt {
        session_id: session,
        started_at,
        ended_at: None,
        status: AttemptStatus::Running,
        cost_usd: 0.0,
        artifacts: Vec::new(),
        error: None,
    }
}
