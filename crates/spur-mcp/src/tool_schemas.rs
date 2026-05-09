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
    /// Opt in to exposing a curated worker MCP subset. Omit for the default no-MCP worker contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_mcp: Option<bool>,
    /// Opt in to worker progress events. Omit to preserve existing silent worker behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_progress: Option<bool>,
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
    /// Opt in to exposing a curated worker MCP subset. Omit for the default no-MCP worker contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_mcp: Option<bool>,
    /// Opt in to worker progress events. Omit to preserve existing silent worker behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_progress: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelInput {
    /// List of tasks to delegate in parallel. Each task carries its own context_files, issue_id, and delegation_plan.
    pub tasks: Vec<DelegateParallelTaskInput>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewTaskBaseInput {
    pub plan_id: String,
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PreviewTaskBaseOutput {
    pub overlays: Vec<crate::tools::OverlayCommit>,
    /// HEAD after overlays applied, if clean. None if conflict.
    pub predicted_base_oid: Option<String>,
    pub conflict: Option<PreviewConflict>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PreviewConflict {
    pub dep_task_id: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanTruncateAndRestartInput {
    pub plan_id: String,
    /// Task that is currently blocked. All non-terminal tasks, including this
    /// one, are superseded in the original plan and re-dispatched in a new
    /// plan rooted at the staging branch.
    pub blocked_task_id: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanTruncateAndRestartOutput {
    /// New branch containing approved tips cherry-picked in DAG order.
    pub staging_branch: String,
    /// Original-plan task IDs that were marked Superseded.
    pub superseded_task_ids: Vec<String>,
    /// New plan ID rooted at `staging_branch`.
    pub new_plan_id: String,
    /// Populated when a cherry-pick collides while building the staging branch.
    pub conflict: Option<StagingConflict>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StagingConflict {
    pub dep_task_id: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecoverOrphanedDispatchInput {
    /// Beads issue ID of the stuck dispatched task.
    pub issue_id: String,
    /// Worker branch that contains the completed work.
    pub worker_branch: String,
    /// Git OID that the dispatch started from.
    pub dispatched_base_oid: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegate_input_default_enable_flags_false() {
        let json = r#"{"agent": "kimi", "task": "do work"}"#;
        let input: DelegateToWorkerInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.enable_worker_mcp, None);
        assert_eq!(input.enable_worker_progress, None);
    }

    #[test]
    fn delegate_input_explicit_enable_worker_mcp() {
        let json = r#"{"agent": "kimi", "task": "do work", "enable_worker_mcp": true}"#;
        let input: DelegateToWorkerInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.enable_worker_mcp, Some(true));
    }

    #[test]
    fn plan_truncate_and_restart_input_rejects_unknown_fields() {
        let json = r#"{
            "plan_id": "plan-1",
            "blocked_task_id": "T2",
            "unexpected": true
        }"#;
        let error = serde_json::from_str::<PlanTruncateAndRestartInput>(json).unwrap_err();
        assert!(
            error.to_string().contains("unknown field"),
            "expected unknown field error, got {error}"
        );
    }
}
