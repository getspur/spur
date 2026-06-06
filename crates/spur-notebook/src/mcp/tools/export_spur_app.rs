//! MCP adapter for exporting notebooks as `.spurapp` packages.

use std::{fs, path::PathBuf};

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    mcp::ServerDeps,
    spur_app::{
        archive::{self, SpurAppArchiveError},
        SpurAppExportOptions,
    },
};

const METHOD: &str = "notebook_export_spur_app";

#[derive(Debug, Deserialize)]
struct ExportSpurAppParams {
    notebook_path: String,
    output_path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    widget_assets: Vec<String>,
    #[serde(default)]
    include_port_snapshots: bool,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Export an .ipynb notebook into a .spurapp package.",
        rmcp_object(json!({
            "type": "object",
            "required": ["notebook_path", "output_path"],
            "properties": {
                "notebook_path": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Source .ipynb notebook path."
                },
                "output_path": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Destination .spurapp package path."
                },
                "name": { "type": "string" },
                "widget_assets": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 }
                },
                "include_port_snapshots": { "type": "boolean" }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(_deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: ExportSpurAppParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook_export_spur_app requires { notebook_path, output_path }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let notebook_path =
        super::validate_notebook_path("notebook_export_spur_app.source", &params.notebook_path)?;
    let output_path =
        super::validate_notebook_path("notebook_export_spur_app.output", &params.output_path)?;
    let widget_assets = params
        .widget_assets
        .iter()
        .map(|path| super::validate_notebook_path("notebook_export_spur_app.widget_asset", path))
        .collect::<Result<Vec<PathBuf>, McpError>>()?;
    let dependency_roots = notebook_path
        .parent()
        .map(|path| path.to_path_buf())
        .into_iter()
        .collect();

    let exported = crate::spur_app::export_spur_app(SpurAppExportOptions {
        notebook_path,
        output_path,
        name: params.name,
        widget_assets,
        include_port_snapshots: params.include_port_snapshots,
        dependency_roots,
    })
    .map_err(map_export_error)?;

    let manifest_file = fs::File::open(&exported.output_path)
        .map_err(SpurAppArchiveError::Io)
        .map_err(map_export_error)?;
    let manifest = archive::read_manifest(manifest_file).map_err(map_export_error)?;

    Ok(CallToolResult::structured(json!({
        "ok": true,
        "path": exported.output_path.to_string_lossy(),
        "manifest": manifest,
        "asset_count": exported.asset_count,
        "preflight": super::spur_app_preflight_json(&exported.preflight)
    })))
}

fn map_export_error(error: SpurAppArchiveError) -> McpError {
    let message = error.to_string();
    match &error {
        SpurAppArchiveError::UnsafePath(_)
        | SpurAppArchiveError::DuplicatePath(_)
        | SpurAppArchiveError::MissingManifest
        | SpurAppArchiveError::InvalidManifestJson(_) => McpError::invalid_params(
            "notebook_export_spur_app received invalid SpurApp input",
            Some(json!({ "error": message })),
        ),
        SpurAppArchiveError::Zip(_) | SpurAppArchiveError::Io(_) => McpError::internal_error(
            "notebook_export_spur_app failed to export SpurApp",
            Some(json!({ "error": message })),
        ),
    }
}
