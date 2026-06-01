use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Manager as _;

use crate::{
    mcp::ServerDeps,
    open_design::{self, Kind, LibraryError},
};

const METHOD: &str = "open_design_search";
const DEFAULT_LIMIT: usize = 8;

#[derive(Debug, Deserialize)]
struct OpenDesignSearchParams {
    query: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Search bundled open-design design systems and deck themes.",
        rmcp_object(json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": { "type": "string" },
                "kind": {
                    "type": "string",
                    "enum": ["design-systems", "deck-themes"]
                },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: OpenDesignSearchParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ query, kind?, limit? }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let kind = params.kind.as_deref().map(parse_kind).transpose()?;
    let resource_dir = deps
        .app
        .as_ref()
        .and_then(|app| app.path().resource_dir().ok());
    let items = open_design::search(
        &params.query,
        kind,
        params.limit.unwrap_or(DEFAULT_LIMIT),
        resource_dir.as_deref(),
    )
    .map_err(|error| map_library_error(METHOD, error))?;

    Ok(CallToolResult::structured(json!({ "items": items })))
}

fn parse_kind(kind: &str) -> Result<Kind, McpError> {
    Kind::parse(kind).ok_or_else(|| {
        McpError::invalid_params(
            format!("{METHOD} kind must be design-systems or deck-themes"),
            Some(json!({ "kind": kind })),
        )
    })
}

fn map_library_error(method: &str, error: LibraryError) -> McpError {
    match error {
        LibraryError::RootNotFound(kind) => McpError::invalid_params(
            format!("{method} could not find open design library root for {kind}"),
            Some(json!({
                "code": "open_design_root_not_found",
                "kind": kind.as_str()
            })),
        ),
        LibraryError::NotFound { kind, id } => McpError::invalid_params(
            format!("{method} could not find {kind} item: {id}"),
            Some(json!({
                "code": "open_design_not_found",
                "kind": kind,
                "id": id
            })),
        ),
        LibraryError::Io(error) => McpError::internal_error(
            format!("{method} failed to read open design library"),
            Some(json!({ "error": error.to_string() })),
        ),
        LibraryError::Json(error) => McpError::internal_error(
            format!("{method} failed to decode open design library"),
            Some(json!({ "error": error.to_string() })),
        ),
    }
}
