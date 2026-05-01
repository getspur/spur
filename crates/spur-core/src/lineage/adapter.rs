//! Fold legacy `SpurEvent` variants into `ExecutorLineage`.
//!
//! For v1 of executor-lineage the orchestrator has not yet been updated to
//! emit the new `Executor*` events directly. This adapter synthesises the
//! minimal set of state transitions from events already in the wild so the
//! TUI can render lineage without orchestrator-side changes.

use std::time::SystemTime;

use spur_acp::domain::delegation::DelegationId;
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
                delegation_id: None,
                peer_edges: Vec::new(),
            });
            // Replay any events (e.g. an early `BrainRetired`) that arrived
            // before this spawn was projected. Symmetric with the
            // `ExecutorSpawned` arm in `apply_inner`.
            lineage.drain_child_orphans_for(&id);
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
                        .map(|n| {
                            // Prefer the most recent *non-terminal* Brain
                            // root. A retired brain must not adopt new
                            // children after `BrainRetired` has cascaded it.
                            n.role == Role::Brain
                                && !matches!(
                                    n.phase,
                                    LifecycleState::Succeeded
                                        | LifecycleState::Failed
                                        | LifecycleState::Cancelled
                                )
                        })
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
                delegation_id: None,
                peer_edges: Vec::new(),
            };
            match parent {
                Some(p) => lineage.attach_child(&p, node),
                None => lineage.insert_root_node(node),
            }
            // Drain any `DelegationDispatched` payload that arrived before
            // this `WorkerSpawned` (orphan-dispatch buffer). The authoritative
            // request_id→executor_id mapping was stashed at dispatch time.
            if let Some((task, issue_id, delegation_id)) = lineage
                .pending_dispatch_by_executor_id_mut()
                .remove(&session.0)
            {
                if let Some(n) = lineage.node_mut_public(&ExecutorId::new(session.0.clone())) {
                    n.task_spec = task;
                    n.issue_id = issue_id;
                    n.delegation_id = Some(delegation_id);
                }
            }
        }

        SpurEventBody::DelegationRequested {
            from: _,
            to_agent: _,
            task,
            request_id,
            delegation_plan: _,
            issue_id,
        } => {
            // Buffer (task, issue_id) under request_id.  `request_id` is the
            // SOLE correlation key — the matching `DelegationDispatched` event
            // carries the authoritative executor_id and drains this buffer.
            // No agent-name heuristics are applied here; they are unsound when
            // multiple workers of the same agent run concurrently.
            //
            // Duplicate-payload detection: if an entry already exists for this
            // request_id with a differing payload, warn and keep the first.
            // Identical replays stay silent (preserved by `or_insert_with`).
            if let Some((existing_task, existing_issue_id)) = lineage
                .pending_task_by_request_id_mut()
                .get(request_id)
                .cloned()
            {
                if existing_task != *task || existing_issue_id != *issue_id {
                    tracing::warn!(
                        request_id = %request_id,
                        new_task = %task,
                        existing_task = %existing_task,
                        "duplicate DelegationRequested with differing payload; keeping first"
                    );
                }
            }
            lineage
                .pending_task_by_request_id_mut()
                .entry(request_id.clone())
                .or_insert_with(|| (task.clone(), issue_id.clone()));
        }

        SpurEventBody::DelegationDispatched {
            from: _,
            request_id,
            executor_id,
        } => {
            // Drain the pending-task buffer for this request and
            // unconditionally stamp the named executor.  `DelegationDispatched`
            // is authoritative: the request_id→executor_id mapping it carries
            // overrides any earlier agent-name guess.  Idempotent: subsequent
            // dispatches for the same request_id are no-ops (entry already
            // removed).
            //
            // Orphan-dispatch safety: if the executor node does not yet exist
            // (dispatch observed before `WorkerSpawned`), stash the payload in
            // `pending_dispatch_by_executor_id` and let the spawn arm drain it.
            if let Some((task, issue_id)) =
                lineage.pending_task_by_request_id_mut().remove(request_id)
            {
                let eid = ExecutorId::new(executor_id.clone());
                let delegation_id = DelegationId(request_id.clone());
                if let Some(n) = lineage.node_mut_public(&eid) {
                    n.task_spec = task;
                    n.issue_id = issue_id;
                    n.delegation_id = Some(delegation_id);
                } else {
                    lineage
                        .pending_dispatch_by_executor_id_mut()
                        .insert(executor_id.clone(), (task, issue_id, delegation_id));
                }
            }
        }

        SpurEventBody::DispatchOverlayApplied { .. } => {}

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
                    DelegationStatus::Cancelled { reason } => (
                        LifecycleState::Cancelled,
                        AttemptStatus::Cancelled,
                        Some(reason.clone()),
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
