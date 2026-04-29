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
//! Most state-mutation arms are idempotent — applying the same event twice
//! produces the same state as applying it once. The exceptions are:
//!
//! - `SpurEventBody::CostUpdate` (additive: `cost_usd += ...` at
//!   `adapter.rs:287`),
//! - `WorkerNotification(ToolCall)` (counter: `tool_call_count += 1` at :289),
//! - `WorkerFileTouched(Write)` (counter: `files_touched_count += 1` at :322).
//!
//! `crates/spur-core/tests/lineage_integration.rs:317` covers the spawn/phase
//! arms; counter arms are intentionally not idempotency-tested.
//!
//! The replay model in `crates/spur-core/src/event_replay.rs` is structurally
//! guarded against double-apply via PID-filtered file selection: the current
//! process's events arrive via the live broadcast subscription; prior
//! processes' events are applied exactly once to fresh empty projections.

use std::collections::{HashMap, VecDeque};

use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::PeerMessageId;
use spur_acp::{SpurEvent, SpurEventBody};

use spur_acp::LifecycleState;

use super::types::{
    Attempt, AttemptStatus, ExecutorId, ExecutorNode, PeerEdge, PeerEdgeState, ReviewRequest,
};

const MAX_ORPHAN_BUFFER_PER_EXEC: usize = 128;

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
    /// Buffer for `DelegationRequested` events waiting for their
    /// `DelegationDispatched` counterpart.  Key is `request_id`.
    /// Value is `(task_spec, issue_id)`.
    pending_task_by_request_id: HashMap<String, (String, Option<String>)>,
    /// Buffer for `DelegationDispatched` events that arrive before the
    /// corresponding `WorkerSpawned`/`ExecutorSpawned`. Key is `executor_id`.
    /// Value is `(task_spec, issue_id, delegation_id)` drained when the node appears.
    pending_dispatch_by_executor_id: HashMap<String, (String, Option<String>, DelegationId)>,
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
                    delegation_id: None,
                    peer_edges: Vec::new(),
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
                    // Counters-only projection. The rich interpretation
                    // of SessionUpdate now happens in the TUI via
                    // `WorkerStreams::route` → `react_trace::dispatch`.
                    // stream_buffer is no longer written from this arm.
                    if let spur_acp::SessionUpdate::ToolCall(tc) = &notification.update {
                        node.tool_call_count += 1;
                        node.latest_tool_call = Some(tc.title.clone());
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

            SpurEventBody::WorkerPeerMessageAccepted {
                message_id,
                source_delegation_id,
                target_delegation_id,
                kind,
                ..
            } => {
                self.attach_peer_edge(PeerEdge {
                    message_id: *message_id,
                    source_delegation_id: source_delegation_id.clone(),
                    target_delegation_id: target_delegation_id.clone(),
                    kind: *kind,
                    state: PeerEdgeState::Accepted,
                    injected_chars: 0,
                });
            }

            SpurEventBody::WorkerPeerMessageDelivered {
                message_id,
                injected_chars,
                ..
            } => {
                self.update_peer_edge_state(
                    message_id,
                    PeerEdgeState::Delivered,
                    Some(*injected_chars),
                );
            }

            SpurEventBody::WorkerPeerMessageConsumed { message_id, .. } => {
                self.update_peer_edge_state(message_id, PeerEdgeState::Consumed, None);
            }

            SpurEventBody::WorkerPeerMessageIgnored { message_id, .. } => {
                self.update_peer_edge_state(message_id, PeerEdgeState::Ignored, None);
            }

            SpurEventBody::WorkerPeerMessageRejected { .. }
            | SpurEventBody::WorkerPeerMessageMalformed { .. }
            | SpurEventBody::WorkerPeerMessageExpired { .. }
            | SpurEventBody::WorkerPeerMessageDropped { .. }
            | SpurEventBody::WorkerPeerMessageUndeliverable { .. }
            | SpurEventBody::WorkerPeerMessageQueued { .. }
            | SpurEventBody::WorkerPeerMessageAuditFailed { .. }
            | SpurEventBody::WorkerPeerMessageReconciledStranded { .. }
            | SpurEventBody::WorkerPeerMessageDrainStarted { .. }
            | SpurEventBody::WorkerPeerMessageDrainCappedOut { .. }
            | SpurEventBody::WorkerPeerMessageDrainTimedOut { .. }
            | SpurEventBody::WorkerPeerMailboxReconciled { .. } => {
                // Lifecycle events that don't currently mutate the edge graph.
            }

            SpurEventBody::BrainRetired { session, .. } => {
                let eid = ExecutorId::new(session.0.clone());
                if !self.nodes.contains_key(&eid) {
                    // Brain spawn not yet projected — buffer so it replays on
                    // arrival. Preserves replay-purity under out-of-order logs.
                    self.buffer_orphan(eid, event.clone());
                    return;
                }
                self.cascade_retire(&eid, event.occurred_at);
            }

            _ => {}
        }
    }

    /// Cascade terminal close-out from a retired brain to every non-terminal
    /// descendant. Iterative DFS over `child_ids: Vec` — deterministic order
    /// under replay (projection.rs doc rule #2). All timestamps come from the
    /// triggering event's `occurred_at`; never from `SystemTime::now()`.
    ///
    /// Idempotent: if the root is already terminal, returns without touching
    /// descendants (they were closed on the first apply).
    fn cascade_retire(&mut self, root: &ExecutorId, occurred_at: std::time::SystemTime) {
        // Short-circuit if root is already terminal (idempotency).
        if let Some(n) = self.nodes.get(root) {
            if matches!(
                n.phase,
                LifecycleState::Succeeded | LifecycleState::Failed | LifecycleState::Cancelled
            ) {
                return;
            }
        } else {
            return;
        }

        let mut stack: Vec<ExecutorId> = vec![root.clone()];
        while let Some(id) = stack.pop() {
            // Enqueue children FIRST so we don't hold a borrow across the
            // mutation below.
            if let Some(n) = self.nodes.get(&id) {
                for c in &n.child_ids {
                    stack.push(c.clone());
                }
            }
            if let Some(n) = self.nodes.get_mut(&id) {
                // Only touch non-terminal nodes. A descendant that already
                // succeeded/failed/cancelled keeps its terminal status.
                if matches!(
                    n.phase,
                    LifecycleState::Succeeded | LifecycleState::Failed | LifecycleState::Cancelled
                ) {
                    continue;
                }
                n.phase = LifecycleState::Cancelled;
                n.last_event_at = Some(occurred_at);
                if let Some(a) = n.current_attempt_mut() {
                    if a.ended_at.is_none() {
                        a.ended_at = Some(occurred_at);
                    }
                    a.status = AttemptStatus::Cancelled;
                }
                // A cascaded node can no longer hold a pending review.
                n.pending_review = None;
            }
            // Drain from the pending-review queue (deterministic Vec op).
            self.pending_review_order.retain(|x| x != &id);
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

    pub fn peer_edges_for_delegation(&self, delegation_id: &DelegationId) -> Vec<PeerEdge> {
        self.find_node_by_delegation(delegation_id)
            .map(|node| node.peer_edges.clone())
            .unwrap_or_default()
    }

    pub fn peer_edges_inbound_for_delegation(&self, target: &DelegationId) -> Vec<PeerEdge> {
        let mut out = Vec::new();
        for node in self.nodes.values() {
            for edge in &node.peer_edges {
                if &edge.target_delegation_id == target {
                    out.push(edge.clone());
                }
            }
        }
        out
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

    /// Drain and replay any child-orphan events buffered for `id`. Used by
    /// the legacy adapter after inserting a new root/child node so events
    /// that arrived before the spawn (e.g. out-of-order `BrainRetired`)
    /// land on the fresh node. Parent-orphan buffer is not drained here —
    /// it is keyed on a parent id that the legacy flow never names.
    pub(crate) fn drain_child_orphans_for(&mut self, id: &ExecutorId) {
        if let Some(queue) = self.orphan_buffer.remove(id) {
            for ev in queue {
                self.apply_inner(&ev);
            }
        }
    }

    pub(crate) fn node_mut_public(&mut self, id: &ExecutorId) -> Option<&mut ExecutorNode> {
        self.nodes.get_mut(id)
    }

    /// Mutable access to the pending-task buffer, keyed by `request_id`.
    /// Used by the legacy adapter to buffer `DelegationRequested` until the
    /// matching `DelegationDispatched` arrives with the concrete executor id.
    pub(crate) fn pending_task_by_request_id_mut(
        &mut self,
    ) -> &mut HashMap<String, (String, Option<String>)> {
        &mut self.pending_task_by_request_id
    }

    /// Mutable access to the orphan-dispatch buffer, keyed by `executor_id`.
    /// Used by the legacy adapter to stash `DelegationDispatched` payloads
    /// that arrive before the executor's node exists, so they can be drained
    /// on `WorkerSpawned`/`ExecutorSpawned` arrival.
    pub(crate) fn pending_dispatch_by_executor_id_mut(
        &mut self,
    ) -> &mut HashMap<String, (String, Option<String>, DelegationId)> {
        &mut self.pending_dispatch_by_executor_id
    }

    fn attach_peer_edge(&mut self, edge: PeerEdge) {
        if let Some(node) = self.find_node_mut_by_delegation(&edge.source_delegation_id) {
            if node
                .peer_edges
                .iter()
                .any(|existing| existing.message_id == edge.message_id)
            {
                return;
            }
            node.peer_edges.push(edge);
        }
    }

    fn update_peer_edge_state(
        &mut self,
        message_id: &PeerMessageId,
        new_state: PeerEdgeState,
        injected_chars: Option<u32>,
    ) {
        for node in self.nodes.values_mut() {
            for edge in &mut node.peer_edges {
                if &edge.message_id == message_id {
                    edge.state = new_state;
                    if let Some(injected_chars) = injected_chars {
                        edge.injected_chars = injected_chars;
                    }
                    return;
                }
            }
        }
    }

    fn find_node_by_delegation(&self, delegation_id: &DelegationId) -> Option<&ExecutorNode> {
        self.nodes.values().find(|node| {
            node.delegation_id.as_ref() == Some(delegation_id) || node.id.0 == delegation_id.0
        })
    }

    fn find_node_mut_by_delegation(
        &mut self,
        delegation_id: &DelegationId,
    ) -> Option<&mut ExecutorNode> {
        self.nodes.values_mut().find(|node| {
            node.delegation_id.as_ref() == Some(delegation_id) || node.id.0 == delegation_id.0
        })
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
