use rmcp::{model::Tool, ErrorData as McpError};
use serde_json::{json, Value};
use std::time::Duration;

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
pub mod snapshot;
pub mod start_kernel;
pub mod stop_kernel;
pub mod venv_create;
pub mod venv_delete;
pub mod venv_list;
pub mod venv_list_python_versions;
pub mod write_cell;

pub(crate) const BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn tools() -> Vec<Tool> {
    vec![
        snapshot::tool(),
        get_notebook::tool(),
        read_cell::tool(),
        kernel_info::tool(),
        insert_cell::tool(),
        write_cell::tool(),
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

fn parse_no_args(method: &str, arguments: Value) -> Result<(), McpError> {
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
}
