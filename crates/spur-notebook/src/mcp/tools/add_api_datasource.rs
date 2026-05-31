use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{check_response, daemon_unavailable};
use crate::mcp::{DaemonControlRequest, ServerDeps};

const METHOD: &str = "notebook_add_api_datasource";

#[derive(Debug, Deserialize)]
struct AddApiDatasourceParams {
    name: String,
    source: String,
}

fn daemon_request(command: jute::commands::DaemonControlCommand) -> DaemonControlRequest {
    DaemonControlRequest {
        id: None,
        request: jute::commands::DaemonControlRequest::new(command),
    }
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Add an API-backed table-function datasource to the active notebook catalog.",
        rmcp_object(json!({
            "type": "object",
            "required": ["name", "source"],
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "source": {
                    "type": "string",
                    "enum": ["polymarket"]
                }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: AddApiDatasourceParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ name, source }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(daemon_request(
            jute::commands::DaemonControlCommand::AddApiDatasource {
                name: params.name,
                source: params.source,
            },
        ))
        .await;
    let response = check_response(response)?;
    let result = response.result.ok_or_else(|| {
        McpError::internal_error(
            format!("{METHOD} daemon response missing datasource"),
            Some(json!({ "code": "daemon_missing_datasource" })),
        )
    })?;
    let entry = match serde_json::from_value::<jute::commands::DaemonControlResult>(result) {
        Ok(jute::commands::DaemonControlResult::Datasource(entry)) => entry,
        Ok(result) => {
            return Err(McpError::internal_error(
                format!("{METHOD} daemon response returned unexpected result: {result:?}"),
                Some(json!({ "code": "daemon_unexpected_result" })),
            ));
        }
        Err(error) => {
            return Err(McpError::internal_error(
                format!("{METHOD} daemon response did not decode"),
                Some(json!({
                    "code": "daemon_result_decode_failed",
                    "error": error.to_string()
                })),
            ));
        }
    };

    Ok(CallToolResult::structured(json!({ "entry": entry })))
}
