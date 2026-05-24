use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    service::RequestContext,
    ErrorData as McpError, RoleServer,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.run_cell";

#[derive(Debug, Deserialize)]
struct RunCellParams {
    id: String,
}

#[derive(Debug, Deserialize)]
struct RunCellResponse {
    id: String,
    status: String,
    #[serde(default)]
    events: Vec<Value>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Run a code cell through the notebook agent bridge.",
        rmcp_object(json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(
    bridge: &dyn BridgeRequester,
    arguments: Value,
    _context: RequestContext<RoleServer>,
) -> Result<CallToolResult, McpError> {
    call_inner(bridge, arguments).await
}

pub async fn call_with_progress(
    bridge: &dyn BridgeRequester,
    arguments: Value,
    _progress: &mut RecordingProgress,
) -> Result<CallToolResult, McpError> {
    call_inner(bridge, arguments).await
}

async fn call_inner(
    bridge: &dyn BridgeRequester,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: RunCellParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.run_cell requires { id }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.run_cell id must not be empty",
            None,
        ));
    }

    let value = bridge
        .request_no_timeout(METHOD, json!({ "id": params.id }))
        .await
        .map_err(|error| error.into_mcp_error())?;
    let response: RunCellResponse = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.run_cell bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!({
        "id": response.id,
        "events": response.events,
        "terminal": {
            "status": response.status
        }
    })))
}

#[derive(Default)]
pub struct RecordingProgress {
    events: Vec<Value>,
}

impl RecordingProgress {
    pub fn events(&self) -> Vec<Value> {
        self.events.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use crate::mcp::bridge::BridgeRequestFuture;
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeBridge {
        requests: Mutex<Vec<String>>,
        direct_kernel_calls: AtomicUsize,
    }

    impl FakeBridge {
        async fn requested_methods(&self) -> Vec<String> {
            self.requests.lock().await.clone()
        }

        fn direct_kernel_calls(&self) -> usize {
            self.direct_kernel_calls.load(Ordering::SeqCst)
        }
    }

    impl BridgeRequester for Arc<FakeBridge> {
        fn listener_registered(&self) -> bool {
            true
        }

        fn window_alive(&self) -> bool {
            true
        }

        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            method: &'static str,
            params: Value,
            _timeout: Duration,
        ) -> BridgeRequestFuture<'a> {
            Box::pin(async move {
                self.requests.lock().await.push(method.to_string());
                match method {
                    METHOD => Ok(json!({
                        "id": params["id"],
                        "status": "success",
                        "exec_count": 1,
                        "outputs": [],
                        "events": []
                    })),
                    _ => unreachable!("unexpected bridge method {method}"),
                }
            })
        }
    }

    #[tokio::test]
    async fn call_returns_terminal_status_from_bridge_response() {
        let bridge = Arc::new(FakeBridge::default());
        let mut progress = RecordingProgress::default();

        let result = call_with_progress(&bridge, json!({ "id": "code-1" }), &mut progress)
            .await
            .expect("run cell succeeds");
        let body = result.structured_content.expect("structured content");

        assert_eq!(body["events"], json!(progress.events()));
        assert_eq!(body["terminal"]["status"], "success");
    }

    #[tokio::test]
    async fn mcp_run_cell_routes_through_bridge() {
        let bridge = Arc::new(FakeBridge::default());
        let mut progress = RecordingProgress::default();

        let result = call_with_progress(&bridge, json!({ "id": "code-1" }), &mut progress)
            .await
            .expect("run cell succeeds");
        let body = result.structured_content.expect("structured content");

        assert_eq!(body["id"], "code-1");
        assert_eq!(body["events"], json!([]));
        assert_eq!(body["terminal"]["status"], "success");
        assert_eq!(bridge.requested_methods().await, vec![METHOD.to_string()]);
        assert_eq!(bridge.direct_kernel_calls(), 0);
    }
}
