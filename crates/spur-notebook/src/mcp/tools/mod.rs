use rmcp::{model::Tool, ErrorData as McpError};
use serde_json::{json, Value};
use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tauri::Emitter;

use super::{DaemonControlResponse, ServerDeps};

pub mod daemon_files;
pub mod daemon_lifecycle;
pub mod daemon_recents;
pub mod delete_cell;
pub mod get_notebook;
pub mod insert_cell;
pub mod interrupt;
pub mod kernel_info;
pub mod read_cell;
pub mod restart_kernel;
pub mod run_cell;
pub mod save;
pub mod set_cell_metadata;
pub mod snapshot;
pub mod start_kernel;
pub mod stop_kernel;
pub mod venv_create;
pub mod venv_delete;
pub mod venv_list;
pub mod venv_list_python_versions;
pub mod write_cell;

pub(crate) const BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const RECENTS_CHANGED_EVENT: &str = "notebook://recents_changed";

pub fn tools() -> Vec<Tool> {
    vec![
        snapshot::tool(),
        get_notebook::tool(),
        read_cell::tool(),
        kernel_info::tool(),
        insert_cell::tool(),
        write_cell::tool(),
        set_cell_metadata::tool(),
        save::tool(),
        delete_cell::tool(),
        interrupt::tool(),
        run_cell::tool(),
        start_kernel::tool(),
        restart_kernel::tool(),
        stop_kernel::tool(),
        venv_list::tool(),
        venv_create::tool(),
        venv_delete::tool(),
        venv_list_python_versions::tool(),
        daemon_lifecycle::new_tool(),
        daemon_lifecycle::open_tool(),
        daemon_lifecycle::close_tool(),
        daemon_lifecycle::reopen_tool(),
        daemon_recents::list_recents_tool(),
        daemon_recents::set_pinned_tool(),
        daemon_recents::remove_from_recents_tool(),
        daemon_files::move_to_trash_tool(),
        daemon_files::reveal_in_finder_tool(),
        daemon_files::discard_scratch_tool(),
    ]
}

fn empty_params() -> Value {
    json!({})
}

pub(super) fn daemon_unavailable() -> McpError {
    McpError::internal_error(
        "notebook daemon control plane is not available",
        Some(json!({ "code": "daemon_unavailable" })),
    )
}

pub(super) async fn current_notebook_slot_id(deps: &ServerDeps) -> Option<String> {
    let daemon = deps.daemon.as_ref()?;
    let path = daemon.current_path().await?;
    let path = path.to_string_lossy();
    // Omitted MCP kernel IDs must resolve to the same notebook:<path> slot the
    // UI uses; otherwise MCP-created variables land in a separate mcp:<uuid>
    // LocalKernel and disappear from later UI executions.
    Some(jute::state::notebook_slot_id(path.as_ref()))
}

pub(super) fn parse_no_args(method: &str, arguments: Value) -> Result<(), McpError> {
    let value = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };
    match value {
        Value::Object(map) if map.is_empty() => Ok(()),
        _ => Err(McpError::invalid_params(
            format!("{method} takes no arguments"),
            None,
        )),
    }
}

pub(super) fn validate_notebook_path(method: &str, raw: &str) -> Result<PathBuf, McpError> {
    if raw.is_empty() {
        return Err(invalid_path(method, "path must not be empty"));
    }

    let path = Path::new(raw);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_path(method, "path must not contain '..'"));
    }

    if requires_ipynb_extension(method)
        && path.extension().and_then(|extension| extension.to_str()) != Some("ipynb")
    {
        return Err(invalid_path(method, "path must have .ipynb extension"));
    }

    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path))
    }
}

fn requires_ipynb_extension(method: &str) -> bool {
    !matches!(
        method,
        "notebook.reveal_in_finder" | "notebook.set_pinned" | "notebook.remove_from_recents"
    )
}

fn invalid_path(method: &str, reason: &str) -> McpError {
    McpError::invalid_params(
        format!("{method} {reason}"),
        Some(json!({
            "code": "invalid_path",
            "reason": reason
        })),
    )
}

pub(super) fn check_response(
    response: DaemonControlResponse,
) -> Result<DaemonControlResponse, McpError> {
    if response.ok {
        Ok(response)
    } else {
        let (code, message) = match response.error {
            Some(error) => (error.code, error.message),
            None => (
                "daemon_command_failed".to_string(),
                "daemon command failed without an error body".to_string(),
            ),
        };
        Err(McpError::internal_error(
            message,
            Some(json!({ "code": code })),
        ))
    }
}

pub(super) async fn emit_recents_changed(deps: &ServerDeps) -> Result<(), McpError> {
    let Some(app) = deps.app.as_ref() else {
        return Ok(());
    };
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook daemon control plane is required to emit recents_changed",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let event = daemon.recents_changed_event().await.map_err(|error| {
        McpError::internal_error(
            "failed to load recent notebooks for recents_changed",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let _ = app.emit(RECENTS_CHANGED_EVENT, &event);
    Ok(())
}

fn require_app<'a>(
    deps: &'a crate::mcp::ServerDeps,
    method: &str,
) -> Result<&'a tauri::AppHandle, McpError> {
    deps.app.as_ref().ok_or_else(|| {
        McpError::internal_error(format!("{method} requires a Tauri app handle"), None)
    })
}

fn jute_error(method: &str, error: &jute::Error) -> McpError {
    McpError::internal_error(
        format!("{method} failed"),
        Some(json!({ "error": error.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_include_direct_notebook_file_tools() {
        let names = tools()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "notebook.save"));
        assert!(names.iter().any(|name| name == "notebook.get_notebook"));
        assert!(names
            .iter()
            .any(|name| name == "notebook.set_cell_metadata"));
    }

    #[test]
    fn tools_include_daemon_lifecycle_recents_and_file_tools() {
        let names = tools()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();

        for expected in [
            "notebook.new",
            "notebook.open",
            "notebook.close",
            "notebook.reopen",
            "notebook.venv_list",
            "notebook.venv_create",
            "notebook.venv_delete",
            "notebook.venv_list_python_versions",
            "notebook.set_cell_metadata",
            "notebook.list_recents",
            "notebook.set_pinned",
            "notebook.remove_from_recents",
            "notebook.move_to_trash",
            "notebook.reveal_in_finder",
            "notebook.discard_scratch",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing tool: {expected}"
            );
        }
    }

    fn assert_invalid_path_code(error: McpError) {
        let serialized = serde_json::to_value(&error).expect("error serializes");
        assert_eq!(serialized["data"]["code"], "invalid_path");
    }

    #[test]
    fn validate_notebook_path_rejects_empty_path() {
        let error =
            validate_notebook_path("notebook.save", "").expect_err("empty path should be rejected");
        assert_invalid_path_code(error);
    }

    #[test]
    fn validate_notebook_path_rejects_parent_dir_prefix() {
        let error = validate_notebook_path("notebook.save", "../foo.ipynb")
            .expect_err("parent-dir prefix should be rejected");
        assert_invalid_path_code(error);
    }

    #[test]
    fn validate_notebook_path_rejects_parent_dir_component() {
        let error = validate_notebook_path("notebook.save", "foo/../bar.ipynb")
            .expect_err("parent-dir component should be rejected");
        assert_invalid_path_code(error);
    }

    #[test]
    fn validate_notebook_path_requires_ipynb_extension_for_notebook_tools() {
        let error = validate_notebook_path("notebook.save", "notes.txt")
            .expect_err("non-notebook path should be rejected");
        assert_invalid_path_code(error);
    }

    #[test]
    fn validate_notebook_path_allows_non_ipynb_extension_for_reveal_and_recents() {
        let reveal = validate_notebook_path("notebook.reveal_in_finder", "notes.txt")
            .expect("reveal accepts non-notebook paths");
        let pinned = validate_notebook_path("notebook.set_pinned", "notes.txt")
            .expect("recents accepts non-notebook paths");

        assert_eq!(
            reveal,
            std::env::current_dir()
                .expect("current dir")
                .join("notes.txt")
        );
        assert_eq!(
            pinned,
            std::env::current_dir()
                .expect("current dir")
                .join("notes.txt")
        );
    }

    #[test]
    fn validate_notebook_path_accepts_valid_absolute_path() {
        let path = std::env::temp_dir().join("valid.ipynb");
        let resolved =
            validate_notebook_path("notebook.save", path.to_str().expect("utf-8 temp path"))
                .expect("absolute notebook path accepted");

        assert_eq!(resolved, path);
    }

    #[test]
    fn validate_notebook_path_accepts_valid_relative_path() {
        let resolved = validate_notebook_path("notebook.save", "relative/notebook.ipynb")
            .expect("relative notebook path accepted");

        assert_eq!(
            resolved,
            std::env::current_dir()
                .expect("current dir")
                .join("relative/notebook.ipynb")
        );
    }
}
