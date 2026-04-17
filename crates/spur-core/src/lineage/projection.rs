//! Executor lineage projection.
//!
//! ## Load-bearing invariant: replay-purity
//!
//! `ExecutorLineage::apply` is a **pure function** of the `SpurEvent` stream.
//! Feeding the same events in the same order to two fresh projections always
//! produces byte-for-byte identical state — including `started_at`, `ended_at`,
//! `cost_usd`, and every other field.
//!
//! Two rules enforce this:
//! 1. Never call `SystemTime::now()` inside `apply` or `apply_legacy` or
//!    helpers they transitively call. Timestamps come from `event.occurred_at`.
//! 2. Never depend on `HashMap` iteration order for observable output. Use
//!    `Vec`/`VecDeque` when order matters (e.g., `pending_review_order`).
//!
//! ## Idempotency
//!
//! Every event arm is idempotent — applying the same event twice produces
//! the same state as applying it once. Exception: `SpurEventBody::CostUpdate`
//! is deliberately additive (two updates accumulate). Tests enforce both
//! invariants.

use std::collections::{HashMap, VecDeque};

use spur_acp::{SpurEvent, SpurEventBody};

use spur_acp::LifecycleState;

use super::types::{
    Attempt, AttemptStatus, ExecutorId, ExecutorNode, ReviewRequest, WorkerStreamEntry,
    WorkerStreamKind,
};

const MAX_ORPHAN_BUFFER_PER_EXEC: usize = 128;
const STREAM_BUFFER_CAP: usize = 200;

#[derive(Debug, Default, Clone)]
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>,
    /// Events received for an executor before its `ExecutorSpawned`.
    orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
    /// Parent-orphan buffer: `ExecutorSpawned` events whose `parent_id` is not
    /// yet in `nodes` are stashed here under the parent id, drained on parent
    /// arrival.
    parent_orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
    /// Insertion-ordered queue of ids with active pending reviews. Maintained
    /// alongside `nodes` so `pending_reviews()` returns deterministic order.
    pending_review_order: VecDeque<ExecutorId>,
}

impl ExecutorLineage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, event: &SpurEvent) {
        // Legacy adapter runs ONLY on top-level apply — NOT on orphan replay,
        // to prevent future re-entry into the adapter from replayed events.
        super::adapter::apply_legacy(self, event);
        self.apply_inner(event);
    }

    fn apply_inner(&mut self, event: &SpurEvent) {
        match &event.body {
            SpurEventBody::ExecutorSpawned {
                id,
                parent_id,
                session_id,
                agent,
                role,
                task_spec,
            } => {
                let eid = ExecutorId::new(id);
                let parent = parent_id.as_ref().map(ExecutorId::new);

                // If a parent is named but not yet present, buffer and wait.
                if let Some(ref p) = parent {
                    if !self.nodes.contains_key(p) {
                        self.buffer_parent_orphan(p.clone(), event.clone());
                        return;
                    }
                }

                let attempt = Attempt {
                    session_id: session_id.clone(),
                    started_at: event.occurred_at,
                    ended_at: None,
                    status: AttemptStatus::Running,
                    cost_usd: 0.0,
                    artifacts: Vec::new(),
                    error: None,
                };
                let node = ExecutorNode {
                    id: eid.clone(),
                    parent_id: parent.clone(),
                    child_ids: Vec::new(),
                    agent: agent.clone(),
                    role: *role,
                    task_spec: task_spec.clone(),
                    phase: LifecycleState::Spawning,
                    attempts: vec![attempt],
                    pending_review: None,
                    last_event_at: None,
                    tool_call_count: 0,
                    latest_tool_call: None,
                    files_touched_count: 0,
                    latest_diff_summary: None,
                    latest_diff_text: None,
                    last_error: None,
                    stream_buffer: VecDeque::new(),
                    issue_id: None,
                };
                match parent {
                    Some(p) => {
                        self.nodes.get_mut(&p).unwrap().child_ids.push(eid.clone());
                        self.nodes.insert(eid.clone(), node);
                    }
                    None => {
                        self.roots.push(eid.clone());
                        self.nodes.insert(eid.clone(), node);
                    }
                }

                // Replay any CHILD-orphan events buffered under this new node.
                if let Some(queue) = self.orphan_buffer.remove(&eid) {
                    for ev in queue {
                        self.apply_inner(&ev);
                    }
                }
                // Replay any PARENT-orphan events (children whose spawn arrived
                // before this parent).
                if let Some(queue) = self.parent_orphan_buffer.remove(&eid) {
                    for ev in queue {
                        self.apply_inner(&ev);
                    }
                }
            }

            SpurEventBody::ExecutorPhaseChanged { id, phase } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.phase = *phase;
                    node.last_event_at = Some(event.occurred_at);
                    if let Some(status) = terminal_attempt_status(*phase) {
                        if let Some(a) = node.current_attempt_mut() {
                            a.ended_at = Some(event.occurred_at);
                            a.status = status;
                            // Copy error to top-level for quick render access.
                            if a.error.is_some() {
                                node.last_error = a.error.clone();
                            }
                        }
                    }
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEventBody::ExecutorArtifact { id, artifact } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.last_event_at = Some(event.occurred_at);
                    if let Some(a) = node.current_attempt_mut() {
                        a.artifacts.push(artifact.clone());
                    }
                    if let spur_acp::Artifact::Diff { summary, text } = artifact {
                        node.files_touched_count = summary.files_changed;
                        node.latest_diff_summary = Some(summary.clone());
                        node.latest_diff_text = text.clone();
                    }
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEventBody::ExecutorReviewRequested {
                id,
                attempt_n,
                kind,
                payload,
            } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.phase = LifecycleState::AwaitingReview;
                    node.last_event_at = Some(event.occurred_at);
                    node.pending_review = Some(ReviewRequest {
                        kind: kind.clone(),
                        payload: payload.clone(),
                        requested_at: event.occurred_at,
                        attempt_n: *attempt_n,
                    });
                    if !self.pending_review_order.contains(&eid) {
                        self.pending_review_order.push_back(eid.clone());
                    }
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEventBody::ExecutorReviewResolved { id, decision: _ } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.pending_review = None;
                    node.last_event_at = Some(event.occurred_at);
                    // Phase stays `AwaitingReview` until a subsequent
                    // PhaseChanged or RetryStarted moves it. Orchestrator owns
                    // that transition.
                    self.pending_review_order.retain(|x| x != &eid);
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEventBody::ExecutorReviewCancelled { id, reason } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.pending_review = None;
                    node.last_event_at = Some(event.occurred_at);
                    self.pending_review_order.retain(|x| x != &eid);
                    tracing::info!(
                        executor_id = %id,
                        reason = %reason,
                        "review cancelled — pending_review cleared"
                    );
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEventBody::ExecutorRetryStarted {
                id,
                attempt_n,
                reason: _,
                new_session_id,
            } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    let expected = node.attempts.len() as u32 + 1;
                    if *attempt_n != expected {
                        tracing::warn!(
                            executor_id = %id,
                            got = *attempt_n,
                            expected,
                            "attempt_n mismatch — orchestrator may have dropped retry events"
                        );
                    }
                    let new_attempt = Attempt {
                        session_id: new_session_id.clone(),
                        started_at: event.occurred_at,
                        ended_at: None,
                        status: AttemptStatus::Running,
                        cost_usd: 0.0,
                        artifacts: Vec::new(),
                        error: None,
                    };
                    node.attempts.push(new_attempt);
                    node.phase = LifecycleState::Running;
                    node.last_event_at = Some(event.occurred_at);
                    node.stream_buffer.clear();
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEventBody::WorkerNotification {
                executor_id,
                notification,
                ..
            } => {
                let eid = ExecutorId::new(executor_id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.last_event_at = Some(event.occurred_at);
                    let entry = match &notification.update {
                        spur_acp::SessionUpdate::AgentThoughtChunk(chunk) => {
                            extract_text_content(chunk).map(|t| WorkerStreamEntry {
                                kind: WorkerStreamKind::Thought,
                                text: t,
                                occurred_at: event.occurred_at,
                            })
                        }
                        spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                            extract_text_content(chunk).map(|t| WorkerStreamEntry {
                                kind: WorkerStreamKind::Message,
                                text: t,
                                occurred_at: event.occurred_at,
                            })
                        }
                        spur_acp::SessionUpdate::ToolCall(tc) => {
                            node.tool_call_count += 1;
                            node.latest_tool_call = Some(tc.title.clone());
                            Some(WorkerStreamEntry {
                                kind: WorkerStreamKind::ToolCall,
                                text: tc.title.clone(),
                                occurred_at: event.occurred_at,
                            })
                        }
                        _ => None,
                    };
                    if let Some(e) = entry {
                        if node.stream_buffer.len() >= STREAM_BUFFER_CAP {
                            node.stream_buffer.pop_front();
                        }
                        node.stream_buffer.push_back(e);
                    }
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEventBody::WorkerProgress {
                executor_id,
                name,
                pct,
                ..
            } => {
                let eid = ExecutorId::new(executor_id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.latest_tool_call = Some(match pct {
                        Some(p) => format!("{} ({}%)", name, p),
                        None => name.clone(),
                    });
                    node.last_event_at = Some(event.occurred_at);
                }
            }

            SpurEventBody::WorkerFileTouched {
                executor_id,
                path,
                kind,
                ..
            } => {
                let eid = ExecutorId::new(executor_id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    if *kind == spur_acp::FileTouchKind::Write {
                        node.files_touched_count += 1;
                    }
                    node.latest_tool_call = Some(format!("{}", path.display()));
                    node.last_event_at = Some(event.occurred_at);
                }
            }

            _ => {}
        }
    }

    fn buffer_orphan(&mut self, id: ExecutorId, ev: SpurEvent) {
        let q = self.orphan_buffer.entry(id).or_default();
        if q.len() < MAX_ORPHAN_BUFFER_PER_EXEC {
            q.push_back(ev);
        } else {
            tracing::warn!("orphan buffer overflow; dropping event");
        }
    }

    fn buffer_parent_orphan(&mut self, parent_id: ExecutorId, event: SpurEvent) {
        let q = self.parent_orphan_buffer.entry(parent_id).or_default();
        if q.len() < MAX_ORPHAN_BUFFER_PER_EXEC {
            q.push_back(event);
        } else {
            tracing::warn!("parent-orphan buffer overflow; dropping event");
        }
    }

    pub fn nodes(&self) -> impl Iterator<Item = &ExecutorNode> {
        self.nodes.values()
    }

    /// Return nodes linked to the given issue ID.
    pub fn nodes_for_issue(&self, issue_id: &str) -> Vec<&ExecutorNode> {
        self.nodes()
            .filter(|n| n.issue_id.as_deref() == Some(issue_id))
            .collect()
    }

    pub fn node(&self, id: &ExecutorId) -> Option<&ExecutorNode> {
        self.nodes.get(id)
    }

    pub fn root_ids(&self) -> &[ExecutorId] {
        &self.roots
    }

    pub fn children_of(&self, id: &ExecutorId) -> Vec<&ExecutorNode> {
        match self.nodes.get(id) {
            Some(node) => node
                .child_ids
                .iter()
                .filter_map(|cid| self.nodes.get(cid))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn pending_reviews(&self) -> Vec<ExecutorId> {
        self.pending_review_order.iter().cloned().collect()
    }

    pub(crate) fn insert_root_node(&mut self, node: ExecutorNode) {
        self.roots.push(node.id.clone());
        self.nodes.insert(node.id.clone(), node);
    }

    pub(crate) fn attach_child(&mut self, parent: &ExecutorId, node: ExecutorNode) {
        if let Some(p) = self.nodes.get_mut(parent) {
            p.child_ids.push(node.id.clone());
        }
        self.nodes.insert(node.id.clone(), node);
    }

    pub(crate) fn node_mut_public(&mut self, id: &ExecutorId) -> Option<&mut ExecutorNode> {
        self.nodes.get_mut(id)
    }

    pub(crate) fn nodes_mut_vec(&mut self) -> Vec<&mut ExecutorNode> {
        self.nodes.values_mut().collect()
    }
}

fn terminal_attempt_status(p: LifecycleState) -> Option<AttemptStatus> {
    match p {
        LifecycleState::Succeeded => Some(AttemptStatus::Succeeded),
        LifecycleState::Failed => Some(AttemptStatus::Failed),
        LifecycleState::Cancelled => Some(AttemptStatus::Cancelled),
        _ => None,
    }
}

/// Extract text from a `ContentChunk`, returning `None` for empty or
/// non-text content.
fn extract_text_content(chunk: &spur_acp::ContentChunk) -> Option<String> {
    match &chunk.content {
        spur_acp::ContentBlock::Text(tc) if !tc.text.is_empty() => Some(tc.text.clone()),
        _ => None,
    }
}
