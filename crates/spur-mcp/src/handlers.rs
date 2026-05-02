use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use spur_pm::{IssueUpdate, PmService};
use tokio::sync::Mutex;

use crate::outcome_materializer::OutcomeMaterializer;
use crate::plan::outcomes::OutcomeStore;
use crate::plan::PlanState;

#[derive(Debug, Clone)]
pub struct WorkerCallContext {
    pub delegation_id: String,
    pub brain_session_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum McpHandlerError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("upstream PM failure: {0}")]
    UpstreamPm(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl McpHandlerError {
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => -32602,
            Self::NotFound(_) => -32004,
            Self::Unauthorized(_) => -32001,
            Self::UpstreamPm(_) => -32603,
            Self::Internal(_) => -32603,
        }
    }

    pub fn to_jsonrpc_response(&self, id: Value) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": self.json_rpc_code(),
                "message": self.to_string(),
            }
        })
    }
}

pub async fn get_issue(
    pm: &PmService,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let issue_id = args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing required field 'id'".into()))?;

    let issue = pm
        .get_issue(issue_id)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    serde_json::to_value(issue)
        .map_err(|e| McpHandlerError::Internal(format!("failed to serialize issue: {e}")))
}

pub async fn update_issue(
    pm: &PmService,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let id = args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing 'id'".into()))?;

    let comment = args
        .get("comment")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let add_labels: Vec<String> = args
        .get("add_labels")
        .and_then(serde_json::Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let remove_labels: Vec<String> = args
        .get("remove_labels")
        .and_then(serde_json::Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let status = args
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let priority = args
        .get("priority")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);

    let assignee = args
        .get("assignee")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let update = IssueUpdate {
        status,
        comment,
        add_labels,
        remove_labels,
        priority,
        assignee,
    };

    pm.update_issue(id, update)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    Ok(serde_json::json!({ "ok": true }))
}

/// Abstracts `McpCallbackServer::load_or_project_plan` so freestanding
/// handlers can resolve a plan without depending on the full server.
///
/// Kept `dyn`-compatible (no generic methods, no `Self: Sized`) so handlers
/// can take `&dyn PlanResolver`. Reused by Task 11 (`get_task_diff`).
#[async_trait]
pub trait PlanResolver: Send + Sync {
    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<Mutex<PlanState>>, String>;
}

pub async fn get_plan_status(
    plan_resolver: &dyn PlanResolver,
    reconciler_outcomes: &Mutex<OutcomeStore>,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let plan_id = args
        .get("plan_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            McpHandlerError::InvalidParams("Missing required field 'plan_id'".into())
        })?
        .to_string();

    let plan_state = plan_resolver
        .load_or_project_plan(&plan_id)
        .await
        .map_err(|_| McpHandlerError::InvalidParams(format!("Unknown plan_id: '{plan_id}'")))?;

    let state = plan_state.lock().await;
    let mut status = crate::plan::build_plan_status(&plan_id, &state);
    let outcomes = reconciler_outcomes.lock().await;
    if let serde_json::Value::Object(ref mut fields) = status {
        fields.insert(
            "recent_outcomes".into(),
            serde_json::json!(outcomes.recent_outcomes(&plan_id)),
        );
        fields.insert(
            "stuck_tasks".into(),
            serde_json::json!(outcomes.stuck_tasks_for_plan(&plan_id)),
        );
    }
    Ok(status)
}

/// Phase 2 Task 10: freestanding `fetch_outcome_artifact` handler.
///
/// SECURITY INVARIANT: the lookup `OutcomeKey` is built from
/// `ctx.brain_session_id`, NEVER from a caller-supplied parameter — this is
/// what enforces cross-session isolation (formerly server.rs:2997-3001).
///
/// A miss in the underlying store is reported as `Unauthorized` rather than
/// `NotFound` so that a caller in session A cannot probe whether a given
/// `(delegation_id, attempt)` exists in session B.
pub async fn fetch_outcome_artifact(
    materializer: &OutcomeMaterializer,
    outcome_store: &dyn spur_blob_store::OutcomeStore,
    ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    use spur_acp::DelegationId;
    use spur_blob_store::Section;

    let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.into(),
        _ => {
            return Err(McpHandlerError::InvalidParams(
                "Missing or empty 'delegation_id'".into(),
            ));
        }
    };

    let section_str = args
        .get("section")
        .and_then(|v| v.as_str())
        .unwrap_or("full");
    let section = match section_str {
        "full" => Section::Full,
        "status_only" => Section::StatusOnly,
        "summary" => Section::Summary,
        "diff_only" => Section::DiffOnly,
        other => {
            return Err(McpHandlerError::InvalidParams(format!(
                "Unknown section '{other}'. Must be one of: status_only, summary, diff_only, full."
            )));
        }
    };

    // Distinguish missing (use latest) from invalid (reject). Without this
    // split, a non-numeric or negative `attempt` value would silently fall
    // through to the missing-arg fallback and return data for a different
    // attempt than the brain asked for.
    let attempt = match args.get("attempt") {
        None | Some(serde_json::Value::Null) => {
            materializer.latest_attempt(&delegation_id).await.unwrap_or(1)
        }
        Some(v) => match v.as_u64() {
            Some(n) if (1..=u32::MAX as u64).contains(&n) => n as u32,
            _ => {
                return Err(McpHandlerError::InvalidParams(
                    "Invalid 'attempt': must be u32 >= 1".into(),
                ));
            }
        },
    };

    let key = spur_acp::domain::outcome::OutcomeKey {
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId(
            ctx.brain_session_id.clone(),
        )),
        delegation_id: delegation_id.clone(),
        attempt,
    };

    let start = std::time::Instant::now();
    let content = match outcome_store.get(&key, Some(Section::Full)).await {
        Ok(content) => content,
        Err(spur_blob_store::StoreError::NotFound(_)) => {
            tracing::warn!(
                target: "spur.metrics.outcome_fetch_not_found",
                ?key,
                section = section_str,
                "outcome not found; reported as Unauthorized to prevent cross-session probing"
            );
            return Err(McpHandlerError::Unauthorized(format!(
                "Outcome artifact not accessible for delegation_id={delegation_id} attempt={attempt}"
            )));
        }
        Err(spur_blob_store::StoreError::Unauthorized { requested, actual }) => {
            tracing::warn!(
                target: "spur.metrics.outcome_fetch_unauthorized",
                ?requested,
                ?actual,
                "cross-session fetch rejected"
            );
            return Err(McpHandlerError::Unauthorized(
                "cross-session outcome read forbidden".into(),
            ));
        }
        Err(error) => {
            return Err(McpHandlerError::Internal(format!(
                "OutcomeStore::get failed: {error}"
            )));
        }
    };

    let projected_text = crate::server::project_section(&content.bytes, section, &key)
        .map_err(|error| McpHandlerError::Internal(format!("Section projection failed: {error}")))?;

    tracing::info!(
        target: "spur.metrics.outcome_fetched",
        ?key,
        section = section_str,
        byte_size = projected_text.len() as u64,
        latency_ms = start.elapsed().as_millis() as u64,
    );

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": projected_text }]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_call_context_construction() {
        let ctx = WorkerCallContext {
            delegation_id: "d-1".into(),
            brain_session_id: "b-1".into(),
        };
        assert_eq!(ctx.delegation_id, "d-1");
    }

    #[test]
    fn handler_error_to_jsonrpc_response() {
        let err = McpHandlerError::InvalidParams("missing field 'id'".into());
        let resp = err.to_jsonrpc_response(serde_json::json!(7));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("-32602"));
        assert!(s.contains("missing field"));
    }

    #[test]
    fn upstream_pm_failure_maps_to_internal_error() {
        let err = McpHandlerError::UpstreamPm("503 service unavailable".into());
        let resp = err.to_jsonrpc_response(serde_json::json!(1));
        assert!(serde_json::to_string(&resp).unwrap().contains("-32603"));
    }
}
