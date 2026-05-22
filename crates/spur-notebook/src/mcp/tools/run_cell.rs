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

#[derive(Debug, Deserialize)]
struct RunCellParams {
    id: String,
}

#[derive(Debug, Deserialize)]
struct BridgeCell {
    id: String,
    kind: String,
    source: String,
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
    let kernel = kernel_info(bridge).await?;
    let rx = bridge
        .run_cell_events(&kernel.kernel_id, &cell.source)
        .await
        .map_err(|error| error.into_mcp_error())?;

    let mut event_count = 0_u64;
    while let Ok(event) = rx.recv().await {
        event_count += 1;
        progress.send(event_count, &event).await?;
    }

    Ok(CallToolResult::structured(json!({
        "id": params.id,
        "events": event_count
    })))
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
