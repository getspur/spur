use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Manager as _;

use crate::{html_video, mcp::ServerDeps};

const METHOD: &str = "html_video_get_template";

#[derive(Debug, Deserialize)]
struct HtmlVideoGetTemplateParams {
    id: String,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Fetch one bundled html video template with template HTML and metadata.",
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

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: HtmlVideoGetTemplateParams =
        serde_json::from_value(arguments).map_err(|error| {
            McpError::invalid_params(
                format!("{METHOD} requires {{ id }}"),
                Some(json!({ "error": error.to_string() })),
            )
        })?;

    let resource_dir = deps
        .app
        .as_ref()
        .and_then(|app| app.path().resource_dir().ok());
    let template = html_video::get_template(&params.id, resource_dir.as_deref())
        .map_err(|error| map_library_error(METHOD, error))?;
    Ok(CallToolResult::structured(json!({
        "id": template.metadata.id,
        "metadata": template.metadata,
        "html": template.html,
        "skill_md": template.skill_md
    })))
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
