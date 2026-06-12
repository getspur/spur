//! `notebook_app_init` — scaffold a new Spur App from a template (U4 init
//! front door; see `docs/superpowers/specs/2026-06-10-spur-app-sdk-design.ipynb` §5).
//!
//! Input:  `{ "app_root": "<dir>", "name"?: "<app-name>", "template"?: "minimal" }`
//! Output: `{ "ok": true, "app_root", "name", "template", "files": [...], "next_steps": [...] }`
//!
//! The scaffolded app is doctor-green out of the box: run `notebook_app_doctor`
//! on the returned `app_root` before packing.

use std::path::{Component, Path, PathBuf};

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    mcp::ServerDeps,
    spur_app::scaffold::{scaffold_app, templates, ScaffoldError, ScaffoldOptions},
};

const METHOD: &str = "notebook_app_init";
const DEFAULT_TEMPLATE: &str = "minimal";

#[derive(Debug, Deserialize)]
struct AppInitParams {
    app_root: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    template: Option<String>,
}

pub fn tool() -> Tool {
    let template_names: Vec<&str> = templates().iter().map(|t| t.name).collect();
    let template_docs: Vec<String> = templates()
        .iter()
        .map(|t| format!("{}: {}", t.name, t.description))
        .collect();
    Tool::new(
        METHOD,
        "Scaffold a new Spur App directory (manifest, entry notebook, server, skill, tests) \
         from a template. The result is doctor-green; follow up with notebook_app_doctor.",
        rmcp_object(json!({
            "type": "object",
            "required": ["app_root"],
            "properties": {
                "app_root": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Directory to scaffold into (created if absent; must not already contain app files)."
                },
                "name": {
                    "type": "string",
                    "description": "App name (lowercase letters, digits, '-', '_'). Defaults to the app_root directory name."
                },
                "template": {
                    "type": "string",
                    "enum": template_names,
                    "description": template_docs.join(" | ")
                }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(_deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: AppInitParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ app_root }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let app_root = validate_app_root(&params.app_root)?;
    let name = match params.name {
        Some(name) => name,
        None => default_app_name(&app_root)?,
    };
    let template = params
        .template
        .unwrap_or_else(|| DEFAULT_TEMPLATE.to_string());

    let scaffolded = scaffold_app(ScaffoldOptions {
        app_root: app_root.clone(),
        name: name.clone(),
        template: template.clone(),
    })
    .map_err(map_scaffold_error)?;

    let app_root_display = scaffolded.app_root.to_string_lossy().to_string();
    Ok(CallToolResult::structured(json!({
        "ok": true,
        "app_root": app_root_display,
        "name": name,
        "template": template,
        "files": scaffolded.files,
        "next_steps": [
            format!(
                "Run notebook_app_doctor {{ \"path\": {app_root_display:?} }} — it must report ok: true before packing."
            ),
            format!("Open {app_root_display}/app.ipynb in app mode to spawn the plugin and grant capabilities."),
            "Pack with notebook_export_spur_app once the doctor is green — never hand-roll a .spurapp.".to_string(),
        ]
    })))
}

/// Like [`super::validate_notebook_path`], but for a directory target: rejects
/// empty and `..` paths, resolves relative paths against the daemon cwd, and
/// imposes no file-extension requirement.
fn validate_app_root(raw: &str) -> Result<PathBuf, McpError> {
    if raw.is_empty() {
        return Err(McpError::invalid_params(
            format!("{METHOD} app_root must not be empty"),
            Some(json!({ "code": "invalid_path" })),
        ));
    }
    let path = Path::new(raw);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(McpError::invalid_params(
            format!("{METHOD} app_root must not contain '..'"),
            Some(json!({ "code": "invalid_path" })),
        ));
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path))
    }
}

fn default_app_name(app_root: &Path) -> Result<String, McpError> {
    app_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| {
            McpError::invalid_params(
                format!(
                    "{METHOD} could not derive an app name from app_root; pass name explicitly"
                ),
                Some(json!({ "code": "invalid_name" })),
            )
        })
}

fn map_scaffold_error(error: ScaffoldError) -> McpError {
    let message = error.to_string();
    match &error {
        ScaffoldError::InvalidName(_) => McpError::invalid_params(
            format!("{METHOD} received an invalid app name"),
            Some(json!({ "code": "invalid_name", "error": message })),
        ),
        ScaffoldError::UnknownTemplate(_, _) => McpError::invalid_params(
            format!("{METHOD} received an unknown template"),
            Some(json!({
                "code": "unknown_template",
                "error": message,
                "available": templates().iter().map(|t| t.name).collect::<Vec<_>>()
            })),
        ),
        ScaffoldError::AppRootNotEmpty(path) => McpError::invalid_params(
            format!("{METHOD} refused to overwrite existing app files"),
            Some(json!({
                "code": "app_root_not_empty",
                "error": message,
                "conflict": path.to_string_lossy()
            })),
        ),
        ScaffoldError::Io(_) | ScaffoldError::ManifestJson(_) => McpError::internal_error(
            format!("{METHOD} failed to scaffold the app"),
            Some(json!({ "error": message })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps() -> ServerDeps {
        ServerDeps::from_bridge(Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))))
    }

    #[tokio::test]
    async fn init_scaffolds_minimal_app_with_next_steps() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-app");
        let result = call(
            &deps(),
            json!({ "app_root": root.to_string_lossy(), "name": "my-app" }),
        )
        .await
        .expect("scaffold succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["ok"], true);
        assert_eq!(body["template"], "minimal");
        assert_eq!(body["name"], "my-app");
        let files: Vec<&str> = body["files"]
            .as_array()
            .expect("files array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(files.contains(&"spur-app.json"));
        assert!(files.contains(&"server/main.py"));
        assert!(files.contains(&"sdk/call_tool.ts"));
        assert!(!body["next_steps"].as_array().expect("steps").is_empty());
        assert!(root.join("spur-app.json").is_file());
    }

    #[tokio::test]
    async fn init_defaults_name_to_app_root_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("gallery-demo");
        let result = call(&deps(), json!({ "app_root": root.to_string_lossy() }))
            .await
            .expect("scaffold succeeds");
        let body = result.structured_content.expect("structured content");
        assert_eq!(body["name"], "gallery-demo");
    }

    #[tokio::test]
    async fn init_scaffolded_frontend_only_app_is_doctor_green() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("surface");
        call(
            &deps(),
            json!({ "app_root": root.to_string_lossy(), "template": "frontend-only" }),
        )
        .await
        .expect("scaffold succeeds");

        let doctor = crate::mcp::tools::notebook_app_doctor::call(
            &deps(),
            json!({ "path": root.to_string_lossy() }),
        )
        .await
        .expect("doctor runs");
        let body = doctor.structured_content.expect("structured content");
        assert_eq!(
            body["ok"], true,
            "scaffolded app must be doctor-green, findings: {}",
            body["findings"]
        );
    }

    #[tokio::test]
    async fn init_rejects_unknown_template_with_available_list() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("x");
        let error = call(
            &deps(),
            json!({ "app_root": root.to_string_lossy(), "template": "nope" }),
        )
        .await
        .expect_err("unknown template rejected");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert_eq!(serialized["data"]["code"], "unknown_template");
        assert!(serialized["data"]["available"]
            .as_array()
            .expect("available templates")
            .iter()
            .any(|v| v == "minimal"));
    }

    #[tokio::test]
    async fn init_rejects_parent_dir_app_root() {
        let error = call(&deps(), json!({ "app_root": "../escape" }))
            .await
            .expect_err("parent-dir app_root rejected");
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert_eq!(serialized["data"]["code"], "invalid_path");
    }
}
