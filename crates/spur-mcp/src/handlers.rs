use serde_json::Value;
use spur_pm::PmService;

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
