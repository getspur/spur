use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spur_acp::DelegationPlan;

pub fn schema_value<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    serde_json::to_value(&schema).unwrap()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateToWorkerInput {
    /// Name of the worker agent to delegate to
    pub agent: String,
    /// Task description for the worker. Structure as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT.
    pub task: String,
    /// Optional supplementary file paths. Prefer inlining relevant excerpts in the task field's CONTEXT section.
    pub context_files: Option<Vec<String>>,
    /// Structured reasoning for this delegation. At minimum pass {chosen, rationale}. For 2+ subtasks or >3 files, include candidates and decomposition.
    pub delegation_plan: Option<DelegationPlan>,
    /// Optional beads issue ID to auto-track
    pub issue_id: Option<String>,
    /// Optional explicit worker base. Omit for legacy behavior (RepoMain).
    /// Use WithOverlay to apply dependency cherry-picks.
    pub base: Option<crate::tools::BaseSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelTaskInput {
    /// Worker agent name
    pub agent: String,
    /// Task description
    pub task: String,
    /// Optional supplementary file paths for this task. Prepended as a '## Relevant Files' section in the worker prompt.
    pub context_files: Option<Vec<String>>,
    /// Optional beads issue ID to auto-track for this task. Must be unique across tasks in a single batch.
    pub issue_id: Option<String>,
    /// Per-task structured reasoning. Used for reviewer mismatch detection. Takes precedence over the batch-level delegation_plan.
    pub delegation_plan: Option<DelegationPlan>,
    /// Optional explicit worker base. Omit for legacy behavior (RepoMain).
    pub base: Option<crate::tools::BaseSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelInput {
    /// List of tasks to delegate in parallel. Each task carries its own context_files, issue_id, and delegation_plan.
    pub tasks: Vec<DelegateParallelTaskInput>,
}
