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

const METHOD: &str = "open_design_get";

#[derive(Debug, Deserialize)]
struct OpenDesignGetParams {
    kind: String,
    id: String,
    #[serde(default)]
    include_skeleton: Option<bool>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Fetch one bundled open-design design system or deck theme.",
        rmcp_object(json!({
            "type": "object",
            "required": ["kind", "id"],
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["design-systems", "deck-themes"]
                },
                "id": { "type": "string", "minLength": 1 },
                "include_skeleton": { "type": "boolean" }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: OpenDesignGetParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ kind, id, include_skeleton? }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let kind = parse_kind(&params.kind)?;
    let resource_dir = deps
        .app
        .as_ref()
        .and_then(|app| app.path().resource_dir().ok());
    let root = open_design::resolve_root(kind, resource_dir.as_deref())
        .ok_or(LibraryError::RootNotFound(kind))
        .map_err(|error| map_library_error(METHOD, error))?;

    match kind {
        Kind::DesignSystems => {
            let design_md = open_design::get_design_system(&root, &params.id)
                .map_err(|error| map_library_error(METHOD, error))?;
            Ok(CallToolResult::structured(json!({
                "id": params.id,
                "kind": kind.as_str(),
                "design_md": design_md
            })))
        }
        Kind::DeckThemes => {
            let theme = open_design::get_deck_theme(
                &root,
                &params.id,
                params.include_skeleton.unwrap_or(false),
            )
            .map_err(|error| map_library_error(METHOD, error))?;
            Ok(CallToolResult::structured(json!({
                "id": theme.id,
                "kind": kind.as_str(),
                "skill_md": theme.skill_md,
                "example_html": theme.example_html,
                "deck_skeleton_html": theme.deck_skeleton_html,
                "files": theme.files
            })))
        }
    }
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
