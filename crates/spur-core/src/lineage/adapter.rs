//! Fold legacy `SpurEvent` variants into `ExecutorLineage`.
//!
//! For v1 of executor-lineage the orchestrator has not yet been updated to
//! emit the new `Executor*` events directly. This adapter synthesises the
//! minimal set of state transitions from events already in the wild so the
//! TUI can render lineage without orchestrator-side changes.

use std::time::SystemTime;

use spur_acp::{DelegationStatus, SpurEvent};

use super::projection::ExecutorLineage;
use super::types::{
    Attempt, AttemptStatus, ExecutorId, ExecutorNode, LifecycleState, Role,
};

pub fn apply_legacy(lineage: &mut ExecutorLineage, event: &SpurEvent) {
    match event {
        SpurEvent::BrainSpawned { agent, session } => {
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
                attempts: vec![fresh_attempt(session.clone())],
                pending_review: None,
            });
        }

        SpurEvent::WorkerSpawned { agent, session, worktree: _ } => {
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
                attempts: vec![fresh_attempt(session.clone())],
                pending_review: None,
            };
            match parent {
                Some(p) => lineage.attach_child(&p, node),
                None => lineage.insert_root_node(node),
            }
        }

        SpurEvent::DelegationRequested { from: _, to_agent, task } => {
            // Populate the task_spec of the most recent Executor owned by the
            // worker name, if empty.
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

        SpurEvent::DelegationCompleted { worker_session, status } => {
            let id = ExecutorId::new(worker_session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                let (phase, attempt_status, error) = match status {
                    DelegationStatus::Success => (LifecycleState::Succeeded, AttemptStatus::Succeeded, None),
                    DelegationStatus::Failed { error } => (LifecycleState::Failed, AttemptStatus::Failed, Some(error.clone())),
                    DelegationStatus::Timeout => (LifecycleState::Failed, AttemptStatus::Failed, Some("timeout".into())),
                    DelegationStatus::Conflict { files } => (LifecycleState::Failed, AttemptStatus::Failed, Some(format!("conflict in {} file(s)", files.len()))),
                };
                n.phase = phase;
                if let Some(a) = n.current_attempt_mut() {
                    a.ended_at = Some(SystemTime::now());
                    a.status = attempt_status;
                    a.error = error;
                }
            }
        }

        SpurEvent::SessionCompleted { session, success } => {
            let id = ExecutorId::new(session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                n.phase = if *success { LifecycleState::Succeeded } else { LifecycleState::Failed };
                if let Some(a) = n.current_attempt_mut() {
                    a.ended_at = Some(SystemTime::now());
                    a.status = if *success { AttemptStatus::Succeeded } else { AttemptStatus::Failed };
                }
            }
        }

        SpurEvent::CostUpdate { session, agent: _, estimated_cost_usd } => {
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

fn fresh_attempt(session: spur_acp::SessionId) -> Attempt {
    Attempt {
        session_id: session,
        started_at: SystemTime::now(),
        ended_at: None,
        status: AttemptStatus::Running,
        cost_usd: 0.0,
        artifacts: Vec::new(),
        error: None,
    }
}
