//! Fold legacy `SpurEvent` variants into `ExecutorLineage`.
//!
//! For v1 of executor-lineage the orchestrator has not yet been updated to
//! emit the new `Executor*` events directly. This adapter synthesises the
//! minimal set of state transitions from events already in the wild so the
//! TUI can render lineage without orchestrator-side changes.

use std::time::SystemTime;

use spur_acp::{DelegationStatus, SpurEvent, SpurEventBody};

use super::projection::ExecutorLineage;
use super::types::{Attempt, AttemptStatus, ExecutorId, ExecutorNode, LifecycleState, Role};

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
                last_event_at: None,
                tool_call_count: 0,
                latest_tool_call: None,
                files_touched_count: 0,
                latest_diff_summary: None,
                latest_diff_text: None,
                last_error: None,
                stream_buffer: std::collections::VecDeque::new(),
                issue_id: None,
            });
        }

        SpurEventBody::WorkerSpawned {
            agent,
            session,
            worktree: _,
        } => {
            let id = ExecutorId::new(session.0.clone());
            if lineage.node(&id).is_some() {
                return;
            }
            let parent = lineage
                .root_ids()
                .iter()
                .rev()
                .find(|rid| {
                    lineage
                        .node(rid)
                        .map(|n| n.role == Role::Brain)
                        .unwrap_or(false)
                })
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
                last_event_at: None,
                tool_call_count: 0,
                latest_tool_call: None,
                files_touched_count: 0,
                latest_diff_summary: None,
                latest_diff_text: None,
                last_error: None,
                stream_buffer: std::collections::VecDeque::new(),
                issue_id: None,
            };
            match parent {
                Some(p) => lineage.attach_child(&p, node),
                None => lineage.insert_root_node(node),
            }
        }

        SpurEventBody::DelegationRequested {
            from: _,
            to_agent,
            task,
            request_id,
            delegation_plan: _,
            issue_id,
        } => {
            // Buffer (task, issue_id) under request_id.  The matching
            // `DelegationDispatched` event carries the concrete executor_id
            // and drains this buffer, ensuring correct per-request attribution
            // even when multiple workers of the same agent run concurrently.
            lineage
                .pending_task_by_request_id_mut()
                .entry(request_id.clone())
                .or_insert_with(|| (task.clone(), issue_id.clone()));

            // Eager-stamp fallback for environments that never emit
            // `DelegationDispatched` (e.g. older orchestrators / test streams).
            // Only applies when exactly one executor matches `to_agent` with
            // an empty task_spec — ambiguous cases are left to
            // `DelegationDispatched`.
            let candidates: Vec<ExecutorId> = lineage
                .nodes_mut_vec()
                .into_iter()
                .filter(|n| {
                    n.role == Role::Executor && n.agent == *to_agent && n.task_spec.is_empty()
                })
                .map(|n| n.id.clone())
                .collect();
            if candidates.len() == 1 {
                let eid = candidates.into_iter().next().unwrap();
                // Also drain the buffer so DelegationDispatched is a no-op.
                if let Some((t, iid)) = lineage.pending_task_by_request_id_mut().remove(request_id)
                {
                    if let Some(n) = lineage.node_mut_public(&eid) {
                        n.task_spec = t;
                        n.issue_id = iid;
                    }
                }
            }
        }

        SpurEventBody::DelegationDispatched {
            from: _,
            request_id,
            executor_id,
        } => {
            // Drain the pending-task buffer for this request and stamp the
            // named executor.  Idempotent: subsequent dispatches for the same
            // request_id are no-ops (entry already removed).
            if let Some((task, issue_id)) =
                lineage.pending_task_by_request_id_mut().remove(request_id)
            {
                let eid = ExecutorId::new(executor_id.clone());
                if let Some(n) = lineage.node_mut_public(&eid) {
                    if n.task_spec.is_empty() {
                        n.task_spec = task;
                        n.issue_id = issue_id;
                    }
                }
            }
        }

        SpurEventBody::DelegationCompleted {
            worker_session,
            status,
        } => {
            let id = ExecutorId::new(worker_session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                let (phase, attempt_status, error) = match status {
                    DelegationStatus::Success => {
                        (LifecycleState::Succeeded, AttemptStatus::Succeeded, None)
                    }
                    DelegationStatus::Failed { error } => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some(error.clone()),
                    ),
                    DelegationStatus::Timeout => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some("timeout".into()),
                    ),
                    DelegationStatus::Conflict { files } => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some(format!("conflict in {} file(s)", files.len())),
                    ),
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
                    DelegationStatus::TimedOut {
                        waited_for,
                        fallback,
                    } => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some(format!(
                            "review timeout after {}s (fallback: {:?})",
                            waited_for.as_secs(),
                            fallback
                        )),
                    ),
                    _ => {
                        tracing::warn!(
                            "unknown DelegationStatus variant — projection needs updating"
                        );
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
                n.phase = if *success {
                    LifecycleState::Succeeded
                } else {
                    LifecycleState::Failed
                };
                if let Some(a) = n.current_attempt_mut() {
                    a.ended_at = Some(event.occurred_at);
                    a.status = if *success {
                        AttemptStatus::Succeeded
                    } else {
                        AttemptStatus::Failed
                    };
                }
            }
        }

        SpurEventBody::CostUpdate {
            session,
            agent: _,
            estimated_cost_usd,
        } => {
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
