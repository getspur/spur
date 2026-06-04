use std::path::Path;

use jute::backend::notebook::NotebookRoot;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::Emitter;

use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.save";

#[derive(Debug, Deserialize)]
struct SaveParams {
    path: String,
    contents: NotebookRoot,
    #[serde(default)]
    force: bool,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Persist a complete notebook document to disk.",
        rmcp_object(json!({
            "type": "object",
            "required": ["path", "contents"],
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "contents": {
                    "type": "object",
                    "required": ["metadata", "nbformat_minor", "nbformat", "cells"],
                    "properties": {
                        "metadata": { "type": "object" },
                        "nbformat_minor": { "type": "integer", "minimum": 0 },
                        "nbformat": { "type": "integer", "minimum": 1 },
                        "cells": { "type": "array" }
                    },
                    "additionalProperties": true
                },
                "force": {
                    "type": "boolean",
                    "description": "Override the empty-overwrite guard. Required when cells is empty and the existing notebook on disk has cells."
                }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: SaveParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.save requires { path, contents }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let path = super::validate_notebook_path(METHOD, &params.path)?;
    let saved_path = path.to_string_lossy().into_owned();
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.save requires notebook daemon state", None)
    })?;

    if params.contents.cells.is_empty() && !params.force {
        if let Some(existing_cells) = existing_cell_count(&path).await {
            if existing_cells > 0 {
                return Err(McpError::invalid_params(
                    "notebook.save refuses to overwrite a non-empty notebook with zero cells; pass force=true to override",
                    Some(json!({ "path": saved_path, "existing_cells": existing_cells })),
                ));
            }
        }
    }

    let notebook = state.get_notebook();
    if is_same_open_target(notebook.path().as_deref(), path.as_path()) {
        notebook.replace(path, params.contents);
    } else {
        state
            .save_coordinator
            .save(path, params.contents)
            .await
            .map_err(|error| {
                McpError::internal_error(
                    "notebook.save failed to write notebook",
                    Some(json!({ "error": error.to_string() })),
                )
            })?;
    }

    if let Some(app) = deps.app.as_ref() {
        app.emit("notebook://saved", &json!({ "path": saved_path }))
            .map_err(|error| {
                McpError::internal_error(
                    "notebook.save failed to emit saved event",
                    Some(json!({ "error": error.to_string() })),
                )
            })?;
    }

    Ok(CallToolResult::structured(json!({ "ok": true })))
}

fn is_same_open_target(store_path: Option<&Path>, candidate: &Path) -> bool {
    let Some(store_path) = store_path else {
        return false;
    };
    if store_path == candidate {
        return true;
    }
    match (
        std::fs::canonicalize(store_path),
        std::fs::canonicalize(candidate),
    ) {
        (Ok(store_path), Ok(candidate)) => store_path == candidate,
        _ => false,
    }
}

async fn existing_cell_count(path: &Path) -> Option<usize> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let root: NotebookRoot = serde_json::from_slice(&bytes).ok()?;
    Some(root.cells.len())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jute::state::State;

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps_with_state(state: Arc<State>) -> ServerDeps {
        ServerDeps {
            bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
                AgentBridge::new(),
            ))),
            state: Some(state),
            app: None,
            daemon: None,
        }
    }

    fn sample_notebook() -> NotebookRoot {
        serde_json::from_value(json!({
            "metadata": {},
            "nbformat_minor": 5,
            "nbformat": 4,
            "cells": [
                {
                    "cell_type": "markdown",
                    "id": "cell-1",
                    "metadata": {},
                    "source": "saved"
                }
            ]
        }))
        .expect("sample notebook parses")
    }

    fn empty_notebook() -> NotebookRoot {
        serde_json::from_value(json!({
            "metadata": {},
            "nbformat_minor": 5,
            "nbformat": 4,
            "cells": []
        }))
        .expect("empty notebook parses")
    }

    #[tokio::test]
    async fn refuses_to_clobber_non_empty_with_empty_cells() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-save-guard-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("guarded.ipynb");
        tokio::fs::write(&path, serde_json::to_vec(&sample_notebook()).unwrap())
            .await
            .expect("seed disk");
        let deps = deps_with_state(Arc::new(State::new()));

        let err = call(
            &deps,
            json!({
                "path": path.display().to_string(),
                "contents": empty_notebook()
            }),
        )
        .await
        .expect_err("guard rejects clobber");
        assert!(
            err.to_string().contains("refuses to overwrite"),
            "unexpected error: {err}"
        );
        let after = tokio::fs::read_to_string(&path).await.expect("file kept");
        let parsed: NotebookRoot = serde_json::from_str(&after).unwrap();
        assert_eq!(parsed.cells.len(), 1, "disk content preserved");
    }

    #[tokio::test]
    async fn force_flag_allows_empty_overwrite() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-save-force-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("forced.ipynb");
        tokio::fs::write(&path, serde_json::to_vec(&sample_notebook()).unwrap())
            .await
            .expect("seed disk");
        let deps = deps_with_state(Arc::new(State::new()));

        call(
            &deps,
            json!({
                "path": path.display().to_string(),
                "contents": empty_notebook(),
                "force": true
            }),
        )
        .await
        .expect("force overrides guard");
        let after = tokio::fs::read_to_string(&path).await.unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&after).unwrap();
        assert!(parsed.cells.is_empty());
    }

    #[tokio::test]
    async fn empty_save_allowed_when_no_existing_file() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-save-fresh-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("fresh.ipynb");
        let deps = deps_with_state(Arc::new(State::new()));

        call(
            &deps,
            json!({
                "path": path.display().to_string(),
                "contents": empty_notebook()
            }),
        )
        .await
        .expect("empty save succeeds for fresh path");
        assert!(tokio::fs::metadata(&path).await.is_ok());
    }

    #[tokio::test]
    async fn save_to_open_notebook_routes_through_store() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-save-open-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("open.ipynb");
        let state = Arc::new(State::new());
        state.get_notebook().load(&path, sample_notebook());
        let replacement: NotebookRoot = serde_json::from_value(json!({
            "metadata": {},
            "nbformat_minor": 5,
            "nbformat": 4,
            "cells": [
                {
                    "cell_type": "markdown",
                    "id": "cell-1",
                    "metadata": {},
                    "source": "replacement"
                }
            ]
        }))
        .expect("replacement notebook parses");
        let deps = deps_with_state(Arc::clone(&state));

        call(
            &deps,
            json!({
                "path": path.display().to_string(),
                "contents": replacement.clone()
            }),
        )
        .await
        .expect("save succeeds");

        let (snapshot, _version) = state.get_notebook().snapshot();
        assert_eq!(snapshot, replacement);
    }

    #[tokio::test]
    async fn save_to_symlinked_open_path_routes_through_store() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-save-open-symlink-")
            .tempdir()
            .expect("temp dir");
        let real_path = temp_dir.path().join("open.ipynb");
        tokio::fs::write(&real_path, serde_json::to_vec(&sample_notebook()).unwrap())
            .await
            .expect("seed disk");
        let alias_path = temp_dir.path().join("alias.ipynb");
        std::os::unix::fs::symlink(&real_path, &alias_path).expect("symlink notebook");
        let state = Arc::new(State::new());
        state.get_notebook().load(&real_path, sample_notebook());
        let replacement: NotebookRoot = serde_json::from_value(json!({
            "metadata": {},
            "nbformat_minor": 5,
            "nbformat": 4,
            "cells": [
                {
                    "cell_type": "markdown",
                    "id": "cell-1",
                    "metadata": {},
                    "source": "replacement through symlink"
                }
            ]
        }))
        .expect("replacement notebook parses");
        let deps = deps_with_state(Arc::clone(&state));

        call(
            &deps,
            json!({
                "path": alias_path.display().to_string(),
                "contents": replacement.clone()
            }),
        )
        .await
        .expect("save succeeds");

        let (snapshot, _version) = state.get_notebook().snapshot();
        assert_eq!(snapshot, replacement);
    }

    #[tokio::test]
    async fn saves_notebook_to_path() {
        let temp_dir = tempfile::Builder::new()
            .prefix("spur-notebook-mcp-save-")
            .tempdir()
            .expect("temp dir");
        let path = temp_dir.path().join("saved.ipynb");
        let notebook = sample_notebook();
        let deps = deps_with_state(Arc::new(State::new()));

        let result = call(
            &deps,
            json!({
                "path": path.display().to_string(),
                "contents": notebook.clone()
            }),
        )
        .await
        .expect("save succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["ok"], true);
        let saved = tokio::fs::read_to_string(&path)
            .await
            .expect("notebook written");
        let saved_notebook: NotebookRoot =
            serde_json::from_str(&saved).expect("saved notebook parses");
        assert_eq!(saved_notebook, notebook);
    }
}
