use jute::commands::{inject_port_bootstrap, install_kernel_in_slot, start_local_kernel};
use jute::kernel_provision::{
    ensure_deno_kernelspec, ensure_evcxr_kernelspec, ensure_gonb_kernelspec,
    ensure_python3_kernelspec, python3_kernelspec_is_valid,
};
use jute::state::notebook_path_from_slot_id;
use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

use crate::dag::notebook_port_root;
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook.start_kernel";

#[derive(Debug, Deserialize)]
struct StartKernelParams {
    spec_name: String,
    #[serde(default)]
    slot_id: Option<String>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        concat!(
            "Provision and start a Jupyter kernel in a stable slot. When slot_id is omitted, ",
            "defaults to the slot bound to the currently open notebook (shared with the UI). ",
            "Falls back to a fresh mcp:<uuid> slot only if no notebook is loaded."
        ),
        rmcp_object(json!({
            "type": "object",
            "required": ["spec_name"],
            "properties": {
                "spec_name": { "type": "string", "minLength": 1 },
                "slot_id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: StartKernelParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook.start_kernel requires { spec_name, slot_id? }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.spec_name.is_empty() {
        return Err(McpError::invalid_params(
            "notebook.start_kernel spec_name must not be empty",
            None,
        ));
    }
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error("notebook.start_kernel requires notebook daemon state", None)
    })?;

    match provisioning_target_for_spec(&params.spec_name) {
        KernelspecProvisioningTarget::Deno => ensure_deno_kernelspec().await,
        KernelspecProvisioningTarget::Evcxr => ensure_evcxr_kernelspec().await,
        KernelspecProvisioningTarget::Gonb => ensure_gonb_kernelspec().await,
        KernelspecProvisioningTarget::Python3 => {
            if python3_kernelspec_is_valid().await {
                Ok(())
            } else {
                let app = deps.app.as_ref().ok_or_else(|| {
                    McpError::internal_error(
                        "notebook.start_kernel requires a Tauri app handle",
                        None,
                    )
                })?;
                ensure_python3_kernelspec(app).await
            }
        }
    }
    .map_err(|error| {
        McpError::internal_error(
            "notebook.start_kernel failed to provision kernelspec",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    let notebook_path =
        resolve_notebook_path(deps, params.slot_id.as_deref(), &params.spec_name).await;
    let port_root = notebook_path.as_deref().map(notebook_port_root);
    let working_dir = notebook_path.as_deref().and_then(|p| p.parent());
    let mut kernel = start_local_kernel(&params.spec_name, port_root.as_deref(), working_dir)
        .await
        .map_err(|error| {
            McpError::internal_error(
                "notebook.start_kernel failed to start kernel",
                Some(json!({ "error": error.to_string() })),
            )
        })?;
    if let Err(error) = inject_port_bootstrap(kernel.conn(), &params.spec_name).await {
        let _ = kernel.kill().await;
        return Err(McpError::internal_error(
            "notebook.start_kernel failed to inject port bootstrap",
            Some(json!({ "error": error.to_string() })),
        ));
    }

    let slot_id = resolve_slot_id(deps, params.slot_id).await;
    let (generation, _previous) = install_kernel_in_slot(state, &slot_id, params.spec_name, kernel);

    Ok(CallToolResult::structured(json!({
        "slot_id": slot_id,
        "generation": generation,
    })))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KernelspecProvisioningTarget {
    Deno,
    Evcxr,
    Gonb,
    Python3,
}

fn provisioning_target_for_spec(spec_name: &str) -> KernelspecProvisioningTarget {
    match spec_name {
        "deno" => KernelspecProvisioningTarget::Deno,
        "evcxr" => KernelspecProvisioningTarget::Evcxr,
        "gonb" => KernelspecProvisioningTarget::Gonb,
        _ => KernelspecProvisioningTarget::Python3,
    }
}

async fn resolve_slot_id(deps: &ServerDeps, explicit_slot_id: Option<String>) -> String {
    if let Some(slot_id) = explicit_slot_id {
        return slot_id;
    }

    super::current_notebook_slot_id(deps)
        .await
        .unwrap_or_else(|| format!("mcp:{}", Uuid::new_v4()))
}

/// Resolve the notebook path from either an explicit slot id or the daemon's
/// currently-open notebook.  Returns `None` when neither source provides a
/// path (e.g. no notebook is open and no slot-id-encoded path exists).
async fn resolve_notebook_path(
    deps: &ServerDeps,
    explicit_slot_id: Option<&str>,
    spec_name: &str,
) -> Option<PathBuf> {
    if let Some(slot_id) = explicit_slot_id {
        return notebook_path_from_slot_id(slot_id, spec_name).map(PathBuf::from);
    }

    let daemon = deps.daemon.as_ref()?;
    daemon.current_path().await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jute::state::{slot_id_for_spec, State};

    use super::*;
    use crate::mcp::bridge::{AgentBridge, TauriBridgeRequester};

    fn deps_without_notebook() -> ServerDeps {
        ServerDeps {
            bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
                AgentBridge::new(),
            ))),
            state: Some(Arc::new(State::new())),
            app: None,
            daemon: None,
            plugins: None,
        }
    }

    #[tokio::test]
    async fn omitted_slot_id_without_open_notebook_uses_mcp_slot() {
        let slot_id = resolve_slot_id(&deps_without_notebook(), None).await;

        assert!(
            slot_id.starts_with("mcp:"),
            "expected mcp fallback slot, got {slot_id}"
        );
    }

    #[tokio::test]
    async fn explicit_slot_id_is_preserved() {
        let slot_id =
            resolve_slot_id(&deps_without_notebook(), Some("custom:slot".to_string())).await;

        assert_eq!(slot_id, "custom:slot");
    }

    #[test]
    fn deno_spec_uses_deno_kernelspec_provisioning() {
        assert_eq!(
            provisioning_target_for_spec("deno"),
            KernelspecProvisioningTarget::Deno
        );
        assert_eq!(
            provisioning_target_for_spec("python3"),
            KernelspecProvisioningTarget::Python3
        );
        assert_eq!(
            provisioning_target_for_spec("custom"),
            KernelspecProvisioningTarget::Python3
        );
    }

    #[test]
    fn evcxr_and_gonb_map_to_their_provisioning_targets() {
        assert_eq!(
            provisioning_target_for_spec("evcxr"),
            KernelspecProvisioningTarget::Evcxr
        );
        assert_eq!(
            provisioning_target_for_spec("gonb"),
            KernelspecProvisioningTarget::Gonb
        );
    }

    // ── resolve_notebook_path: slot-id branch ─────────────────────────────────

    /// A slot id encoding a notebook path yields the notebook path, and its
    /// parent is used as the working directory.
    #[tokio::test]
    async fn notebook_path_from_slot_id_yields_correct_working_dir() {
        let notebook_path = "/home/user/projects/app/app.ipynb";
        let slot_id = slot_id_for_spec(notebook_path, "python3");
        let deps = deps_without_notebook();

        let resolved = resolve_notebook_path(&deps, Some(&slot_id), "python3").await;

        let resolved = resolved.expect("expected a notebook path from the slot id");
        assert_eq!(
            resolved,
            PathBuf::from(notebook_path),
            "resolved notebook path must match the path encoded in the slot id"
        );
        assert_eq!(
            resolved.parent(),
            Some(std::path::Path::new("/home/user/projects/app")),
            "working_dir derived from resolved path must be the notebook's directory"
        );
    }

    /// A bare mcp: slot id (not notebook-derived) yields None — no working dir.
    #[tokio::test]
    async fn mcp_slot_id_yields_no_notebook_path() {
        let deps = deps_without_notebook();
        let resolved = resolve_notebook_path(
            &deps,
            Some("mcp:550e8400-e29b-41d4-a716-446655440000"),
            "python3",
        )
        .await;
        assert!(
            resolved.is_none(),
            "mcp: slot id should not produce a notebook path"
        );
    }

    /// No explicit slot id and no open notebook (daemon is None) yields None.
    #[tokio::test]
    async fn no_slot_id_no_daemon_yields_no_notebook_path() {
        let deps = deps_without_notebook(); // daemon: None
        let resolved = resolve_notebook_path(&deps, None, "python3").await;
        assert!(
            resolved.is_none(),
            "no slot id + no daemon should yield no notebook path"
        );
    }
}
