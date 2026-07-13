use std::collections::{HashMap, HashSet};

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spur_acp::DelegationPlan;

use crate::plan::loops::spec::{LoopEscalation, LoopGovernors, LoopSpec};
use crate::{BaseSpec, OverlayCommit};

pub fn schema_value<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    let mut value = serde_json::to_value(&schema).unwrap();
    normalize_defs_refs(&mut value);
    value
}

pub fn schema_object<T: JsonSchema>() -> serde_json::Map<String, Value> {
    match schema_value::<T>() {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

fn normalize_defs_refs(value: &mut Value) {
    let definitions = match value {
        Value::Object(map) => {
            let definitions = map.remove("definitions").or_else(|| map.remove("$defs"));
            map.remove("$defs");
            definitions
        }
        _ => None,
    };
    let Some(Value::Object(definitions)) = definitions else {
        return;
    };

    // opencode's MCP client does not resolve nested `$ref`s in tool input schemas.
    inline_refs(value, &definitions, &mut HashSet::new());
    collapse_single_all_of(value);
}

fn inline_refs(
    value: &mut Value,
    definitions: &serde_json::Map<String, Value>,
    resolving: &mut HashSet<String>,
) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.remove("$ref") {
                let resolved = definition_ref_name(&reference)
                    .map(|name| resolve_ref(name, definitions, resolving))
                    .unwrap_or_else(permissive_object_schema);

                if let Value::Object(resolved_map) = resolved {
                    for (key, nested) in resolved_map {
                        map.entry(key).or_insert(nested);
                    }
                }
            }

            for nested in map.values_mut() {
                inline_refs(nested, definitions, resolving);
            }
        }
        Value::Array(values) => {
            for nested in values {
                inline_refs(nested, definitions, resolving);
            }
        }
        _ => {}
    }
}

fn resolve_ref(
    name: &str,
    definitions: &serde_json::Map<String, Value>,
    resolving: &mut HashSet<String>,
) -> Value {
    if !resolving.insert(name.to_string()) {
        return permissive_object_schema();
    }

    let mut resolved = definitions
        .get(name)
        .cloned()
        .unwrap_or_else(permissive_object_schema);
    inline_refs(&mut resolved, definitions, resolving);
    resolving.remove(name);
    resolved
}

fn definition_ref_name(reference: &str) -> Option<&str> {
    reference
        .strip_prefix("#/definitions/")
        .or_else(|| reference.strip_prefix("#/$defs/"))
}

fn collapse_single_all_of(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let sole_object = match map.remove("allOf") {
                Some(Value::Array(mut values)) if values.len() == 1 => {
                    match values.pop().unwrap() {
                        Value::Object(sole) => Some(sole),
                        other => {
                            map.insert("allOf".to_string(), Value::Array(vec![other]));
                            None
                        }
                    }
                }
                Some(other) => {
                    map.insert("allOf".to_string(), other);
                    None
                }
                None => None,
            };

            if let Some(sole_object) = sole_object {
                for (key, nested) in sole_object {
                    map.entry(key).or_insert(nested);
                }
            }

            for nested in map.values_mut() {
                collapse_single_all_of(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collapse_single_all_of(nested);
            }
        }
        _ => {}
    }
}

fn permissive_object_schema() -> Value {
    let mut map = serde_json::Map::new();
    map.insert("type".to_string(), Value::String("object".to_string()));
    Value::Object(map)
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateToWorkerInput {
    /// Name of the worker agent to delegate to.
    pub agent: String,
    /// Named agent profile from `.spur/agents/<name>.md` (or a pass-through
    /// agent/mode name the worker binary already knows). Materialized into the
    /// worker worktree and selected on the fresh session; fail-soft on selection.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Explore pool skill names to materialize into the worker worktree before the session starts.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Override the worker's model (config-option value id, e.g. "gpt-5-codex"). Fail-soft if the agent rejects it.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Override the worker's reasoning effort (thought-level value id, e.g. "low"/"medium"/"high"). Fail-soft if the agent rejects it.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Generic session config overrides by advertised config-option id. Fail-soft per entry if the agent rejects it.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_overrides: Option<HashMap<String, String>>,
    /// Task description for the worker.
    pub task: String,
    /// Optional supplementary file paths.
    pub context_files: Option<Vec<String>>,
    /// Structured reasoning for this delegation.
    pub delegation_plan: Option<DelegationPlan>,
    /// Optional beads issue ID to auto-track.
    pub issue_id: Option<String>,
    /// Optional explicit worker base. Omit for legacy RepoMain behavior.
    pub base: Option<BaseSpec>,
    /// Default-on worker MCP subset exposure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_mcp: Option<bool>,
    /// Opt in to worker progress events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_progress: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelTaskInput {
    /// Worker agent name.
    pub agent: String,
    /// Named agent profile from `.spur/agents/<name>.md` (or a pass-through
    /// agent/mode name the worker binary already knows). Materialized into the
    /// worker worktree and selected on the fresh session; fail-soft on selection.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Explore pool skill names to materialize into the worker worktree before the session starts.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Override the worker's model (config-option value id, e.g. "gpt-5-codex"). Fail-soft if the agent rejects it.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Override the worker's reasoning effort (thought-level value id, e.g. "low"/"medium"/"high"). Fail-soft if the agent rejects it.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Generic session config overrides by advertised config-option id. Fail-soft per entry if the agent rejects it.
    #[schemars(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_overrides: Option<HashMap<String, String>>,
    /// Task description.
    pub task: String,
    /// Optional supplementary file paths for this task.
    pub context_files: Option<Vec<String>>,
    /// Optional beads issue ID to auto-track for this task.
    pub issue_id: Option<String>,
    /// Per-task structured reasoning.
    pub delegation_plan: Option<DelegationPlan>,
    /// Optional explicit worker base. Omit for legacy RepoMain behavior.
    pub base: Option<BaseSpec>,
    /// Default-on worker MCP subset exposure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_mcp: Option<bool>,
    /// Opt in to worker progress events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_worker_progress: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelInput {
    /// List of tasks to delegate in parallel.
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
    pub overlays: Vec<OverlayCommit>,
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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SubmitLoopParams {
    pub spec: LoopSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpurLoopDoctorParams {
    pub original_command: String,
    pub draft: LoopDoctorDraft,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoopDoctorDraft {
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cadence_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<String>,
    pub tasks: Vec<LoopDoctorDraftTask>,
    #[serde(default)]
    pub governors: LoopGovernors,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<LoopEscalation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoopDoctorDraftTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_overrides: Option<HashMap<String, String>>,
    pub task: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub triage: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpurLoopDoctorOutput {
    pub status: String,
    pub friendly_preview: String,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_submit_loop_params: Option<SubmitLoopParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoopIdParams {
    pub loop_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetLoopAutonomyParams {
    pub loop_id: String,
    /// Must be one of `l1`, `l2`, or `l3`.
    pub level: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetLoopStatusParams {
    pub loop_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_runs: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn delegate_input_omitted_enable_flags_deserialize_as_none() {
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
    fn submit_loop_params_schema_is_fully_inlined() {
        // Regression: opencode's MCP client does not resolve nested `$ref`s in
        // tool input schemas, so any `$ref` on a required object field (like
        // `spec`) caused the client to stringify the argument. serde then
        // rejected it with "invalid type: string, expected struct LoopSpec".
        // The published schema must inline every `$defs` ref so clients
        // recognize object fields without `$ref` resolution.
        let schema = schema_value::<SubmitLoopParams>();
        let pretty = serde_json::to_string_pretty(&schema).unwrap();

        assert!(
            !pretty.contains("$ref"),
            "tool schema still contains a $ref after normalization:\n{pretty}"
        );
        assert!(
            schema.get("$defs").is_none() && schema.get("definitions").is_none(),
            "tool schema must drop top-level defs once refs are inlined:\n{pretty}"
        );

        let spec = &schema["properties"]["spec"];
        assert_eq!(
            spec["type"], "object",
            "`spec` is not an inline object schema"
        );
        let spec_props = spec["properties"]
            .as_object()
            .expect("`spec.properties` must be an inlined object");
        for field in [
            "goal",
            "pattern",
            "cadence_secs",
            "template",
            "governors",
            "escalation",
        ] {
            assert!(
                spec_props.contains_key(field),
                "`spec.{field}` was not inlined into the schema"
            );
        }

        // Nested struct (LoopGovernors) must also be inlined, proving recursion.
        let governors = &spec["properties"]["governors"];
        assert_eq!(
            governors["type"], "object",
            "`spec.governors` is not an inline object schema"
        );
        assert!(
            governors["properties"]["max_cost_micros_per_generation"].is_object(),
            "`spec.governors.max_cost_micros_per_generation` was not inlined"
        );
    }
}
