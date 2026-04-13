use std::collections::{HashMap, VecDeque};
use std::time::SystemTime;

use spur_acp::SpurEvent;

use super::types::{
    Attempt, AttemptStatus, ExecutorId, ExecutorNode, LifecycleState, ReviewRequest, Role,
};

const MAX_ORPHAN_BUFFER_PER_EXEC: usize = 128;

#[derive(Debug, Default, Clone)]
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>,
    /// Events received for an executor before its `ExecutorSpawned`.
    orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
}

impl ExecutorLineage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, event: &SpurEvent) {
        // Try legacy adapter first (BrainSpawned, WorkerSpawned, etc.)
        super::adapter::apply_legacy(self, event);

        match event {
            SpurEvent::ExecutorSpawned {
                id,
                parent_id,
                session_id,
                agent,
                role,
                task_spec,
            } => {
                let eid = ExecutorId::new(id);
                let parent = parent_id.as_ref().map(ExecutorId::new);
                let parsed_role = parse_role(role);
                let attempt = Attempt {
                    session_id: session_id.clone(),
                    started_at: SystemTime::now(),
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
                    role: parsed_role,
                    task_spec: task_spec.clone(),
                    phase: LifecycleState::Spawning,
                    attempts: vec![attempt],
                    pending_review: None,
                };
                match parent {
                    Some(p) if self.nodes.contains_key(&p) => {
                        self.nodes.get_mut(&p).unwrap().child_ids.push(eid.clone());
                        self.nodes.insert(eid.clone(), node);
                    }
                    _ => {
                        self.roots.push(eid.clone());
                        self.nodes.insert(eid.clone(), node);
                    }
                }
                // Replay any buffered orphan events for this id.
                if let Some(queue) = self.orphan_buffer.remove(&eid) {
                    for ev in queue {
                        self.apply(&ev);
                    }
                }
            }

            SpurEvent::ExecutorPhaseChanged { id, phase } => {
                let eid = ExecutorId::new(id);
                if let Some(new_phase) = parse_phase(phase) {
                    if let Some(node) = self.nodes.get_mut(&eid) {
                        node.phase = new_phase;
                        if let Some(status) = terminal_attempt_status(new_phase) {
                            if let Some(a) = node.current_attempt_mut() {
                                a.ended_at = Some(SystemTime::now());
                                a.status = status;
                            }
                        }
                    } else {
                        self.buffer_orphan(eid, event.clone());
                    }
                }
            }

            SpurEvent::ExecutorArtifact { id, artifact } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    let art = map_artifact(artifact);
                    if let Some(a) = node.current_attempt_mut() {
                        a.artifacts.push(art);
                    }
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEvent::ExecutorReviewRequested {
                id,
                kind,
                payload,
                requested_at,
            } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.phase = LifecycleState::AwaitingReview;
                    node.pending_review = Some(ReviewRequest {
                        kind: map_review_kind(kind),
                        payload: map_review_payload(payload),
                        requested_at: *requested_at,
                    });
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEvent::ExecutorReviewResolved { id, decision: _ } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.pending_review = None;
                    // Phase stays `AwaitingReview` until a subsequent
                    // PhaseChanged or RetryStarted moves it. Orchestrator owns
                    // that transition.
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEvent::ExecutorRetryStarted {
                id,
                attempt_n: _,
                reason: _,
                new_session_id,
            } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    let new_attempt = Attempt {
                        session_id: new_session_id.clone(),
                        started_at: SystemTime::now(),
                        ended_at: None,
                        status: AttemptStatus::Running,
                        cost_usd: 0.0,
                        artifacts: Vec::new(),
                        error: None,
                    };
                    node.attempts.push(new_attempt);
                    node.phase = LifecycleState::Running;
                } else {
                    self.buffer_orphan(eid, event.clone());
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

    pub fn nodes(&self) -> impl Iterator<Item = &ExecutorNode> {
        self.nodes.values()
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
        self.nodes
            .values()
            .filter(|n| n.pending_review.is_some())
            .map(|n| n.id.clone())
            .collect()
    }

    #[allow(dead_code)]
    pub(crate) fn insert_root(&mut self, node: ExecutorNode) {
        self.roots.push(node.id.clone());
        self.nodes.insert(node.id.clone(), node);
    }

    #[allow(dead_code)]
    pub(crate) fn insert_child(&mut self, parent: &ExecutorId, node: ExecutorNode) {
        if let Some(p) = self.nodes.get_mut(parent) {
            p.child_ids.push(node.id.clone());
        }
        self.nodes.insert(node.id.clone(), node);
    }

    #[allow(dead_code)]
    pub(crate) fn node_mut(&mut self, id: &ExecutorId) -> Option<&mut ExecutorNode> {
        self.nodes.get_mut(id)
    }
}

fn parse_role(s: &str) -> Role {
    match s {
        "Brain" => Role::Brain,
        "SubExecutor" => Role::SubExecutor,
        _ => Role::Executor,
    }
}

fn parse_phase(s: &str) -> Option<LifecycleState> {
    Some(match s {
        "Spawning" => LifecycleState::Spawning,
        "Running" => LifecycleState::Running,
        "AwaitingReview" => LifecycleState::AwaitingReview,
        "Resuming" => LifecycleState::Resuming,
        "Succeeded" => LifecycleState::Succeeded,
        "Failed" => LifecycleState::Failed,
        "Cancelled" => LifecycleState::Cancelled,
        _ => return None,
    })
}

fn terminal_attempt_status(p: LifecycleState) -> Option<AttemptStatus> {
    match p {
        LifecycleState::Succeeded => Some(AttemptStatus::Succeeded),
        LifecycleState::Failed => Some(AttemptStatus::Failed),
        LifecycleState::Cancelled => Some(AttemptStatus::Cancelled),
        _ => None,
    }
}

fn map_artifact(p: &spur_acp::ExecutorArtifactPayload) -> super::types::Artifact {
    use spur_acp::ExecutorArtifactPayload as S;
    use super::types::{Artifact, DiffSummary};
    match p {
        S::Diff(d) => Artifact::Diff(DiffSummary {
            files_changed: d.files_changed,
            insertions: d.insertions,
            deletions: d.deletions,
            files: d.files.clone(),
        }),
        S::PrUrl(u) => Artifact::PrUrl(u.clone()),
        S::FileList(f) => Artifact::FileList(f.clone()),
        S::Text(t) => Artifact::Text(t.clone()),
    }
}

fn map_review_kind(k: &spur_acp::ExecutorReviewKind) -> super::types::ReviewKind {
    use spur_acp::ExecutorReviewKind as S;
    use super::types::ReviewKind;
    match k {
        S::Completion => ReviewKind::Completion,
        S::Failure => ReviewKind::Failure,
        S::Conflict => ReviewKind::Conflict,
        S::Checkpoint => ReviewKind::Checkpoint,
    }
}

fn map_review_payload(p: &spur_acp::ExecutorReviewPayload) -> super::types::ReviewPayload {
    use super::types::{DiffSummary, ReviewPayload};
    ReviewPayload {
        summary: p.summary.clone(),
        diff_summary: p.diff_summary.as_ref().map(|d| DiffSummary {
            files_changed: d.files_changed,
            insertions: d.insertions,
            deletions: d.deletions,
            files: d.files.clone(),
        }),
        pr_url: p.pr_url.clone(),
        error: p.error.clone(),
    }
}
