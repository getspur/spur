//! MCP adapter for importing `.spurapp` packages.

use std::path::PathBuf;

use directories::BaseDirs;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{mcp::ServerDeps, spur_app::archive::SpurAppArchiveError};

const METHOD: &str = "notebook_import_spur_app";

#[derive(Debug, Deserialize)]
struct ImportSpurAppParams {
    path: String,
    #[serde(default)]
    open: bool,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Import a .spurapp package and optionally open its embedded notebook.",
        rmcp_object(json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Source .spurapp package path."
                },
                "open": {
                    "type": "boolean",
                    "description": "Open the imported notebook through the notebook daemon."
                }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: ImportSpurAppParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook_import_spur_app requires { path }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let package_path = super::validate_notebook_path(METHOD, &params.path)?;
    let cache_root = default_cache_root()?;
    let imported =
        crate::spur_app::import_spur_app(package_path, cache_root).map_err(map_import_error)?;
    let notebook_path = imported.notebook_path.to_string_lossy().into_owned();

    if params.open {
        super::daemon_lifecycle::call_open(deps, json!({ "path": &notebook_path })).await?;
    }

    Ok(CallToolResult::structured(json!({
        "ok": true,
        "notebook_path": notebook_path,
        "manifest": imported.manifest,
        "preflight": super::spur_app_preflight_json(&imported.preflight)
    })))
}

fn default_cache_root() -> Result<PathBuf, McpError> {
    let base_dirs = BaseDirs::new().ok_or_else(|| {
        McpError::internal_error(
            "notebook_import_spur_app could not resolve home directory",
            Some(json!({ "code": "home_unavailable" })),
        )
    })?;
    Ok(base_dirs
        .home_dir()
        .join(".spur")
        .join("spurapps")
        .join("cache"))
}

fn map_import_error(error: SpurAppArchiveError) -> McpError {
    let message = error.to_string();
    match &error {
        SpurAppArchiveError::UnsafePath(_)
        | SpurAppArchiveError::DuplicatePath(_)
        | SpurAppArchiveError::MissingManifest
        | SpurAppArchiveError::InvalidManifestJson(_) => McpError::invalid_params(
            "notebook_import_spur_app received invalid SpurApp package",
            Some(json!({ "error": message })),
        ),
        SpurAppArchiveError::Zip(_) | SpurAppArchiveError::Io(_) => McpError::internal_error(
            "notebook_import_spur_app failed to import SpurApp",
            Some(json!({ "error": message })),
        ),
    }
}
