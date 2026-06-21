use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    if let Value::Object(map) = value {
        if let Some(definitions) = map.remove("definitions") {
            map.insert("$defs".to_string(), definitions);
        }
    }

    rewrite_definitions_refs(value);
}

fn rewrite_definitions_refs(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get_mut("$ref") {
                if let Some(suffix) = reference.strip_prefix("#/definitions/") {
                    *reference = format!("#/$defs/{suffix}");
                }
            }

            for nested in map.values_mut() {
                rewrite_definitions_refs(nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                rewrite_definitions_refs(nested);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewTaskBaseInput {
    pub plan_id: String,
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PreviewTaskBaseOutput {
    pub overlays: Vec<spur_mcp::tools::OverlayCommit>,
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
