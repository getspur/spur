//! Plan-graph mutation operations (v0b).
//!
//! `PlanMutationOp` is the unit of graph edit. A `MutationBatch` bundles ops
//! produced by a `MutationProposer` for atomic write-ahead + commit.
//! Extending the enum is additive — consumers match `#[non_exhaustive]`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::signals::WorkerSignal;

/// Shape of a new task to create as part of a mutation. Subset of the
/// existing `PlanTask` spec fields needed at mutation time.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDraft {
    pub title: String,
    pub description: String,
    pub assignee: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
}

/// How children of a split relate to the original downstream edges.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum DepRewirePolicy {
    /// Children form a sequential chain; original downstream rewires to
    /// the chain tail. Pipeline-stage / Unix-pipe tradition.
    Pipeline { tail_index: usize },
    /// Children are parallel; original downstream waits for all children.
    /// OpenMP / rayon join barrier tradition.
    Barrier,
    /// Caller supplies explicit edges: (child_index, downstream_task_id).
    Explicit { edges: Vec<(usize, String)> },
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PlanMutationOp {
    /// Replace `parent` with N children; rewire downstream per policy.
    SplitTask {
        parent: String, // beads issue id
        children: Vec<TaskDraft>,
        dep_rewire: DepRewirePolicy,
    },
}

#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationBatch {
    pub mutation_id: Uuid,
    pub ops: Vec<PlanMutationOp>,
    pub trigger_signal_id: Option<Uuid>,
    pub trigger_task_id: String,
}

/// Single source of truth for op → snake_case audit tag. Used by the
/// `MutationCommit` audit `op_tags` field and the projector. New ops added
/// in Phase 2c (RetryTask, ModifyTaskSpec, AbandonTask) extend this match.
pub fn op_tag_for(op: &PlanMutationOp) -> &'static str {
    match op {
        PlanMutationOp::SplitTask { .. } => "split_task",
    }
}

impl MutationBatch {
    /// Short op tag for the `MutationPlan` audit record `op` field. Returns
    /// the first op's tag (`"empty"` if the batch is empty).
    pub fn op_tag(&self) -> &'static str {
        self.ops.first().map(op_tag_for).unwrap_or("empty")
    }

    /// Per-op tags for the `MutationCommit` audit `op_tags` field. One entry
    /// per op in `ops`, in order.
    pub fn op_tags(&self) -> Vec<&'static str> {
        self.ops.iter().map(op_tag_for).collect()
    }
}

/// Unused import guard — kept so future ops can reference WorkerSignal fields.
#[allow(dead_code)]
fn _unused_worker_signal() -> Option<WorkerSignal> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_task_round_trips() {
        let batch = MutationBatch {
            mutation_id: Uuid::nil(),
            trigger_signal_id: Some(Uuid::nil()),
            trigger_task_id: "bd-102".into(),
            ops: vec![PlanMutationOp::SplitTask {
                parent: "bd-102".into(),
                children: vec![TaskDraft {
                    title: "Extract auth module".into(),
                    description: "...".into(),
                    assignee: Some("claude-code-acp".into()),
                    priority: None,
                }],
                dep_rewire: DepRewirePolicy::Barrier,
            }],
        };
        let json = serde_json::to_string(&batch).unwrap();
        let back: MutationBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back.trigger_task_id, "bd-102");
        assert_eq!(back.op_tag(), "split_task");
        assert_eq!(back.op_tags(), vec!["split_task"]);
    }
}
