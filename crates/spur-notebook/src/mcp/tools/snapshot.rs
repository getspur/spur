use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{empty_params, BRIDGE_TIMEOUT};
use crate::mcp::bridge::BridgeRequester;

const METHOD: &str = "notebook.snapshot";
const SOURCE_PREVIEW_CHARS: usize = 160;

#[derive(Debug, Deserialize)]
struct BridgeSnapshotCell {
    id: String,
    kind: String,
    version: u64,
    exec_count: Option<u32>,
    status: String,
    source: String,
}

#[derive(Debug, Serialize)]
struct SnapshotCell {
    id: String,
    kind: String,
    version: u64,
    exec_count: Option<u32>,
    status: String,
    source_preview: String,
    source_hash: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Return a coarse snapshot of all loaded notebook cells.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub async fn call(bridge: &dyn BridgeRequester) -> Result<CallToolResult, McpError> {
    let value = bridge
        .request(METHOD, empty_params(), BRIDGE_TIMEOUT)
        .await
        .map_err(|error| error.into_mcp_error())?;
    let cells = decode_bridge_cells(value)?;
    Ok(CallToolResult::structured(json!(cells)))
}

fn decode_bridge_cells(value: Value) -> Result<Vec<SnapshotCell>, McpError> {
    let cells: Vec<BridgeSnapshotCell> = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.snapshot bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(cells
        .into_iter()
        .map(|cell| {
            let source_preview = cell.source.chars().take(SOURCE_PREVIEW_CHARS).collect();
            let source_hash = blake3_16_hex(&cell.source);
            SnapshotCell {
                id: cell.id,
                kind: cell.kind,
                version: cell.version,
                exec_count: cell.exec_count,
                status: cell.status,
                source_preview,
                source_hash,
            }
        })
        .collect())
}

fn blake3_16_hex(source: &str) -> String {
    let hash = blake3::hash(source.as_bytes());
    hash.as_bytes()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
