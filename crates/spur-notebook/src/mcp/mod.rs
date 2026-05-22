use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use directories::BaseDirs;
use rmcp::{
    model::{
        object as rmcp_object, CallToolRequestParams, CallToolResult, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use serde_json::{json, Value};
use tokio::{net::UnixListener, sync::oneshot, task::JoinHandle};

use self::bridge::{BridgeRequester, TauriBridgeRequester};
use self::{bridge::AgentBridge, transport::LengthPrefixedJsonTransport};

pub mod bridge;
pub mod tools;
pub mod transport;

#[derive(Clone)]
pub struct NotebookMcpServer {
    bridge: Arc<dyn BridgeRequester>,
}

impl NotebookMcpServer {
    pub fn new(bridge: Arc<dyn BridgeRequester>) -> Self {
        Self { bridge }
    }

    fn tools(&self) -> Vec<Tool> {
        let mut all_tools = vec![Tool::new(
            "notebook.ping",
            "Smoke-test the SPUR notebook MCP socket.",
            rmcp_object(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
        )];
        all_tools.extend(self::tools::tools());
        all_tools
    }

    fn tool(&self, name: &str) -> Option<Tool> {
        self.tools().into_iter().find(|tool| tool.name == name)
    }
}

impl ServerHandler for NotebookMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions =
            Some("Use notebook tools to inspect and operate the active SPUR notebook.".into());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut implementation = Implementation::default();
        implementation.name = "notebook".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tool(name)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "notebook.ping" => Ok(CallToolResult::structured(json!({
                "ok": true,
                "tool": "notebook.ping",
                "listenerRegistered": self.bridge.listener_registered(),
                "windowAlive": self.bridge.window_alive()
            }))),
            "notebook.snapshot" => tools::snapshot::call(self.bridge.as_ref()).await,
            "notebook.read_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::read_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.kernel_info" => tools::kernel_info::call(self.bridge.as_ref()).await,
            "notebook.insert_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::insert_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.write_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::write_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.delete_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::delete_cell::call(self.bridge.as_ref(), arguments).await
            }
            "notebook.interrupt" => tools::interrupt::call(self.bridge.as_ref()).await,
            "notebook.run_cell" => {
                let arguments = request
                    .arguments
                    .map(Value::Object)
                    .unwrap_or_else(|| json!({}));
                tools::run_cell::call(self.bridge.as_ref(), arguments, context).await
            }
            name => Err(McpError::invalid_params(
                format!("unknown notebook tool: {name}"),
                Some(json!({ "tool": name })),
            )),
        }
    }
}

pub struct NotebookMcpServerHandle {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl NotebookMcpServerHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = (&mut self.task).await;
        let _ = std::fs::remove_file(&self.socket_path);
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for NotebookMcpServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

pub fn socket_path_for_slot(slot_id: &str) -> Result<PathBuf> {
    if slot_id.is_empty() || slot_id.contains('/') || slot_id.contains('\0') {
        anyhow::bail!("invalid notebook slot id: {slot_id}");
    }
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs
        .home_dir()
        .join(".spur")
        .join("notebooks")
        .join(format!("{slot_id}.sock")))
}

pub async fn start_server(socket_path: impl AsRef<Path>) -> Result<NotebookMcpServerHandle> {
    start_server_with_bridge(socket_path, Arc::new(AgentBridge::new())).await
}

pub async fn start_server_with_bridge(
    socket_path: impl AsRef<Path>,
    bridge: Arc<AgentBridge>,
) -> Result<NotebookMcpServerHandle> {
    start_server_with_bridge_requester(
        socket_path,
        Arc::new(TauriBridgeRequester::without_app(bridge)),
    )
    .await
}

pub async fn start_server_with_app_bridge(
    socket_path: impl AsRef<Path>,
    bridge: Arc<AgentBridge>,
    app: tauri::AppHandle,
) -> Result<NotebookMcpServerHandle> {
    start_server_with_bridge_requester(
        socket_path,
        Arc::new(TauriBridgeRequester::with_app(bridge, app)),
    )
    .await
}

async fn start_server_with_bridge_requester(
    socket_path: impl AsRef<Path>,
    bridge: Arc<dyn BridgeRequester>,
) -> Result<NotebookMcpServerHandle> {
    let socket_path = socket_path.as_ref().to_path_buf();
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match tokio::fs::remove_file(&socket_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to remove stale socket {}", socket_path.display())
            });
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task_socket_path = socket_path.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let (stream, _addr) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            tracing::debug!(%error, "notebook MCP unix accept failed");
                            continue;
                        }
                    };
                    let bridge = Arc::clone(&bridge);
                    tokio::spawn(async move {
                        let transport = LengthPrefixedJsonTransport::<RoleServer>::new(stream);
                        match NotebookMcpServer::new(bridge).serve(transport).await {
                            Ok(running) => {
                                let _ = running.waiting().await;
                            }
                            Err(error) => {
                                tracing::debug!(%error, "notebook MCP session failed to initialize");
                            }
                        }
                    });
                }
            }
        }
        bridge.drain_on_shutdown().await;
        let _ = tokio::fs::remove_file(task_socket_path).await;
    });

    Ok(NotebookMcpServerHandle {
        socket_path,
        shutdown_tx: Some(shutdown_tx),
        task,
    })
}
