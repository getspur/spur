use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Manager as _;

use crate::{html_video, mcp::ServerDeps};

const METHOD: &str = "html_video_search_templates";
const DEFAULT_TOP: usize = 5;

#[derive(Debug, Deserialize)]
struct HtmlVideoSearchTemplatesParams {
    intent: String,
    #[serde(default)]
    top: Option<usize>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Search bundled html video templates.",
        rmcp_object(json!({
            "type": "object",
            "required": ["intent"],
            "properties": {
                "intent": { "type": "string", "minLength": 1 },
                "top": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: HtmlVideoSearchTemplatesParams =
        serde_json::from_value(arguments).map_err(|error| {
            McpError::invalid_params(
                format!("{METHOD} requires { intent, top? }"),
                Some(json!({ "error": error.to_string() })),
            )
        })?;
    let top = params.top.unwrap_or(DEFAULT_TOP);
    if top == 0 {
        return Err(McpError::invalid_params(
            format!("{METHOD} top must be >= 1"),
            Some(json!({ "top": top })),
        ));
    }

    let resource_dir = deps
        .app
        .as_ref()
        .and_then(|app| app.path().resource_dir().ok());
    let items = html_video::search(&params.intent, top, resource_dir.as_deref())
        .map_err(|error| map_library_error(METHOD, error))?;

    Ok(CallToolResult::structured(json!({ "items": items })))
}

fn map_library_error(method: &str, error: html_video::LibraryError) -> McpError {
    match error {
        html_video::LibraryError::RootNotFound => McpError::invalid_params(
            format!("{method} could not find html video library root"),
            Some(json!({ "code": "html_video_library_not_found" })),
        ),
        html_video::LibraryError::NotFound { id } => McpError::invalid_params(
            format!("{method} could not find template"),
            Some(json!({
                "code": "html_video_template_not_found",
                "id": id,
            })),
        ),
        html_video::LibraryError::InvalidIndex { path, reason } => McpError::internal_error(
            format!("{method} encountered invalid index: {reason}"),
            Some(json!({ "path": path })),
        ),
        html_video::LibraryError::Io(error) => McpError::internal_error(
            format!("{method} failed to read html video library"),
            Some(json!({ "error": error.to_string() })),
        ),
        html_video::LibraryError::Json(error) => McpError::internal_error(
            format!("{method} failed to parse html video index"),
            Some(json!({ "error": error.to_string() })),
        ),
    }
}
