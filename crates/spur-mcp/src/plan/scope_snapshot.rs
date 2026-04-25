use spur_acp::domain::delegation::DelegationId;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct PlanScopeSnapshot {
    pub plan_version: u64,
    /// Set of `(source_task_id, target_task_id)` pairs allowed by the plan
    /// DAG. v1 only populates direct dependency edges from
    /// `PlanTask::depends_on`; brain-approved explicit peer edges that
    /// extend beyond DAG dependencies are a deferred v1 limitation
    /// (see spec "V1 Defaults" — sibling tasks without a direct dependency
    /// are not enough by themselves).
    pub peer_edges: HashSet<(String, String)>,
    /// Maps `delegation_id` to the task it executes.
    pub delegation_to_task: HashMap<DelegationId, String>,
    /// Maps `delegation_id` to its issue id.
    pub delegation_to_issue: HashMap<DelegationId, String>,
    /// Set of plan task ids that are superseded.
    pub superseded_tasks: HashSet<String>,
    /// Set of plan task ids whose lifecycle is terminal (succeeded, failed, cancelled).
    pub terminal_tasks: HashSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EdgeCheck {
    Allowed,
    NotInDag,
    SourceMissing,
    TargetMissing,
    SourceSuperseded,
    TargetSuperseded,
    SourceTerminal,
}

impl PlanScopeSnapshot {
    pub fn check_peer_edge(&self, source: &DelegationId, target: &DelegationId) -> EdgeCheck {
        let src_task = match self.delegation_to_task.get(source) {
            Some(t) => t,
            None => return EdgeCheck::SourceMissing,
        };
        let tgt_task = match self.delegation_to_task.get(target) {
            Some(t) => t,
            None => return EdgeCheck::TargetMissing,
        };
        if self.superseded_tasks.contains(src_task) {
            return EdgeCheck::SourceSuperseded;
        }
        if self.superseded_tasks.contains(tgt_task) {
            return EdgeCheck::TargetSuperseded;
        }
        if self.terminal_tasks.contains(src_task) {
            return EdgeCheck::SourceTerminal;
        }
        if !self
            .peer_edges
            .contains(&(src_task.clone(), tgt_task.clone()))
        {
            return EdgeCheck::NotInDag;
        }
        EdgeCheck::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PlanScopeSnapshot {
        let mut delegation_to_task = HashMap::new();
        delegation_to_task.insert(DelegationId("src-1".into()), "task-a".into());
        delegation_to_task.insert(DelegationId("tgt-1".into()), "task-b".into());
        let mut peer_edges = HashSet::new();
        peer_edges.insert(("task-a".into(), "task-b".into()));
        PlanScopeSnapshot {
            plan_version: 1,
            peer_edges,
            delegation_to_task,
            delegation_to_issue: HashMap::new(),
            superseded_tasks: HashSet::new(),
            terminal_tasks: HashSet::new(),
        }
    }

    #[test]
    fn allowed_edge_returns_allowed() {
        let snap = fixture();
        assert_eq!(
            snap.check_peer_edge(&DelegationId("src-1".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::Allowed
        );
    }

    #[test]
    fn missing_source_returns_source_missing() {
        let snap = fixture();
        assert_eq!(
            snap.check_peer_edge(&DelegationId("nope".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::SourceMissing
        );
    }

    #[test]
    fn superseded_target_blocks_edge() {
        let mut snap = fixture();
        snap.superseded_tasks.insert("task-b".into());
        assert_eq!(
            snap.check_peer_edge(&DelegationId("src-1".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::TargetSuperseded
        );
    }

    #[test]
    fn edge_not_in_dag_blocks_communication() {
        let mut snap = fixture();
        snap.peer_edges.clear();
        assert_eq!(
            snap.check_peer_edge(&DelegationId("src-1".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::NotInDag
        );
    }
}
