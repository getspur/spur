use jute::backend::commands::RunCellEvent;
use rmcp::{
    model::{
        object as rmcp_object, CallToolResult, ProgressNotification, ProgressNotificationParam,
        ProgressToken, Tool,
    },
    service::{Peer, RequestContext},
    ErrorData as McpError, RoleServer,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.run_cell";
const READ_CELL_METHOD: &str = "notebook.read_cell";
const KERNEL_INFO_METHOD: &str = "notebook.kernel_info";
const DISCONNECTED_STATUS: &str = "disconnected";

#[derive(Debug, Deserialize)]
struct RunCellParams {
    id: String,
}

#[derive(Debug, Deserialize)]
struct BridgeCell {
    id: String,
    kind: String,
    source: String,
    #[serde(default)]
    kernel_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct KernelInfo {
    kernel_id: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Run a code cell and stream RunCellEvent JSON through MCP progress notifications.",
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
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, McpError> {
    let progress = McpProgress::new(context);
    call_inner(bridge, arguments, ProgressTarget::Mcp(progress)).await
}

pub async fn call_with_progress(
    bridge: &dyn BridgeRequester,
    arguments: Value,
    progress: &mut RecordingProgress,
) -> Result<CallToolResult, McpError> {
    call_inner(bridge, arguments, ProgressTarget::Recording(progress)).await
}

async fn call_inner(
    bridge: &dyn BridgeRequester,
    arguments: Value,
    mut progress: ProgressTarget<'_>,
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

    let cell = read_cell(bridge, &params.id).await?;
    if cell.kind != "code" {
        return Err(McpError::invalid_params(
            "notebook.run_cell can only run code cells",
            Some(json!({ "id": cell.id, "kind": cell.kind })),
        ));
    }
    let kernel_id = active_kernel_id(bridge, cell.kernel_id).await?;
    let rx = bridge
        .run_cell_events(&kernel_id, &cell.source)
        .await
        .map_err(|error| error.into_mcp_error())?;

    let mut event_count = 0_u64;
    let mut events = Vec::new();
    let mut terminal_status = DISCONNECTED_STATUS.to_string();
    let mut finished = false;
    while let Ok(event) = rx.recv().await {
        event_count += 1;
        let event_value = serde_json::to_value(&event).map_err(|error| {
            McpError::internal_error(
                "could not encode RunCellEvent result",
                Some(json!({ "error": error.to_string() })),
            )
        })?;
        match &event {
            RunCellEvent::Finished { status, .. } => {
                terminal_status = status.clone();
                finished = true;
            }
            RunCellEvent::Disconnect(_) if !finished => {
                terminal_status = DISCONNECTED_STATUS.to_string();
            }
            _ => {}
        }
        progress.send(event_count, &event).await?;
        events.push(event_value);
    }

    Ok(CallToolResult::structured(json!({
        "id": params.id,
        "events": events,
        "terminal": {
            "status": terminal_status
        }
    })))
}

async fn active_kernel_id(
    bridge: &dyn BridgeRequester,
    cell_kernel_id: Option<String>,
) -> Result<String, McpError> {
    if let Some(kernel_id) = cell_kernel_id {
        return Ok(kernel_id);
    }

    let kernel = kernel_info(bridge).await?;
    Ok(kernel.kernel_id)
}

async fn read_cell(bridge: &dyn BridgeRequester, id: &str) -> Result<BridgeCell, McpError> {
    let value = bridge
        .request(READ_CELL_METHOD, json!({ "id": id }), BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;
    serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.read_cell bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })
}

async fn kernel_info(bridge: &dyn BridgeRequester) -> Result<KernelInfo, McpError> {
    let value = bridge
        .request(KERNEL_INFO_METHOD, json!({}), BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;
    serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.kernel_info bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })
}

enum ProgressTarget<'a> {
    Mcp(McpProgress),
    Recording(&'a mut RecordingProgress),
}

impl ProgressTarget<'_> {
    async fn send(&mut self, progress: u64, event: &RunCellEvent) -> Result<(), McpError> {
        match self {
            Self::Mcp(target) => target.send(progress, event).await,
            Self::Recording(target) => {
                target.push(event)?;
                Ok(())
            }
        }
    }
}

struct McpProgress {
    peer: Peer<RoleServer>,
    token: Option<ProgressToken>,
}

impl McpProgress {
    fn new(context: RequestContext<RoleServer>) -> Self {
        let token = context.meta.get_progress_token();
        Self {
            peer: context.peer,
            token,
        }
    }

    async fn send(&self, progress: u64, event: &RunCellEvent) -> Result<(), McpError> {
        let Some(token) = &self.token else {
            return Ok(());
        };
        let message = serde_json::to_value(event).map_err(|error| {
            McpError::internal_error(
                "could not encode RunCellEvent progress",
                Some(json!({ "error": error.to_string() })),
            )
        })?;
        let params = ProgressNotificationParam::new(token.clone(), progress as f64)
            .with_message(message.to_string());
        self.peer
            .send_notification(ProgressNotification::new(params).into())
            .await
            .map_err(|error| {
                McpError::internal_error(
                    "could not send notebook.run_cell progress",
                    Some(json!({ "error": error.to_string() })),
                )
            })
    }
}

#[derive(Default)]
pub struct RecordingProgress {
    events: Vec<Value>,
}

impl RecordingProgress {
    fn push(&mut self, event: &RunCellEvent) -> Result<(), McpError> {
        self.events
            .push(serde_json::to_value(event).map_err(|error| {
                McpError::internal_error(
                    "could not encode RunCellEvent progress",
                    Some(json!({ "error": error.to_string() })),
                )
            })?);
        Ok(())
    }

    pub fn events(&self) -> Vec<Value> {
        self.events.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::mcp::bridge::{BridgeRequestFuture, RunCellEventFuture};

    use super::*;

    struct FakeBridge;

    impl BridgeRequester for FakeBridge {
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
                match method {
                    READ_CELL_METHOD => Ok(json!({
                        "id": params["id"],
                        "kind": "code",
                        "source": "2 + 2"
                    })),
                    KERNEL_INFO_METHOD => Ok(json!({ "kernel_id": "kernel-1" })),
                    _ => unreachable!("unexpected bridge method {method}"),
                }
            })
        }

        fn run_cell_events<'a>(
            &'a self,
            _kernel_id: &'a str,
            _code: &'a str,
        ) -> RunCellEventFuture<'a> {
            Box::pin(async move {
                let (tx, rx) = async_channel::unbounded();
                tx.send(RunCellEvent::Started).await.unwrap();
                tx.send(RunCellEvent::Finished {
                    exec_count: Some(1),
                    status: "ok".to_string(),
                })
                .await
                .unwrap();
                drop(tx);
                Ok(rx)
            })
        }
    }

    #[tokio::test]
    async fn call_returns_terminal_status_from_finished_event() {
        let bridge = FakeBridge;
        let mut progress = RecordingProgress::default();

        let result = call_with_progress(&bridge, json!({ "id": "code-1" }), &mut progress)
            .await
            .expect("run cell succeeds");
        let body = result.structured_content.expect("structured content");

        assert_eq!(body["events"], json!(progress.events()));
        assert_eq!(body["terminal"]["status"], "ok");
    }
}
