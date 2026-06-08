//! Generic MCP plugin process management.
//!
//! A `.spurapp` manifest may declare an `mcp_server` block describing a child
//! process (Python, Node, …) that speaks the Model Context Protocol over stdio.
//! This module spawns those processes, performs the MCP `initialize` handshake,
//! discovers their tools via `tools/list`, and proxies `tools/call` requests.
//!
//! The module is deliberately app-agnostic: it knows nothing about any specific
//! plugin (html_video, etc.). It maps a small set of `server_type` values to an
//! interpreter, spawns the entry script, and exchanges line-delimited JSON-RPC
//! frames with the child.

use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};

use rmcp::model::Tool;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tracing::warn;

use crate::spur_app::SpurAppMcpServer;

/// Protocol version advertised to plugin servers during `initialize`. Servers
/// negotiate down to a version they support and echo their choice back.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Upper bound on how long any single JSON-RPC round trip (initialize,
/// tools/list, tools/call, ping) may take before we treat the plugin as wedged.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Failures that can occur while managing plugin child processes.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("no plugin named '{0}' is running")]
    UnknownPlugin(String),
    #[error("a plugin named '{0}' is already running")]
    AlreadyRunning(String),
    #[error("unsupported plugin server type: '{0}'")]
    UnsupportedServerType(String),
    #[error("failed to spawn plugin process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("plugin '{0}' did not expose a stdio pipe")]
    MissingPipe(String),
    #[error("plugin process exited or closed its stdio")]
    ProcessDied,
    #[error("timed out waiting for plugin response to '{0}'")]
    Timeout(String),
    #[error("plugin returned a JSON-RPC error for '{method}': {message} (code {code})")]
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
    #[error("io error talking to plugin: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to (de)serialize plugin message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to prepare plugin venv: {0}")]
    VenvSetup(String),
}

/// Resolved configuration for one plugin child process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConfig {
    /// Unique plugin name (used as the registry key and in log lines).
    pub name: String,
    /// Interpreter selector — "python", "node", etc.
    pub server_type: String,
    /// Entry script, relative to `working_dir`.
    pub entry: String,
    /// Optional path to a requirements/lock file (informational; this module
    /// does not install dependencies).
    pub requirements: Option<String>,
    /// Environment variables to set on the child process.
    pub env: BTreeMap<String, String>,
    /// Working directory the child is spawned in.
    pub working_dir: PathBuf,
}

impl PluginConfig {
    /// Build a config from a `.spurapp` manifest's `mcp_server` block.
    ///
    /// `name` identifies the plugin in the registry; `working_dir` is the
    /// extracted app root the entry script is resolved against. The manifest's
    /// declared env vars are copied verbatim — interpreter-specific tweaks (such
    /// as forcing unbuffered Python output) are layered on later in
    /// [`PluginConfig::command_env`].
    pub fn from_manifest(
        name: impl Into<String>,
        manifest: &SpurAppMcpServer,
        working_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            server_type: manifest.server_type.clone(),
            entry: manifest.entry.clone(),
            requirements: manifest.requirements.clone(),
            env: manifest.env.clone(),
            working_dir: working_dir.into(),
        }
    }

    /// Environment passed to the spawned process: the manifest env plus
    /// interpreter-specific additions required for line-delimited stdio. Python
    /// buffers stdout when not attached to a TTY, which would stall our
    /// line-oriented frame reads, so `PYTHONUNBUFFERED=1` is forced on.
    fn command_env(&self) -> BTreeMap<String, String> {
        let mut env = self.env.clone();
        if self.server_type == "python" {
            env.entry("PYTHONUNBUFFERED".to_string())
                .or_insert_with(|| "1".to_string());
        }
        env
    }
}

/// Directory name for per-app virtual environments, relative to `working_dir`.
const VENV_DIR: &str = ".spur-venv";

/// Marker file written after a successful venv + requirements install so
/// subsequent launches skip the setup step.
const VENV_READY_MARKER: &str = ".spur-venv/READY";

/// Map a `server_type` selector to the interpreter program to exec.
///
/// For Python plugins, returns the per-app venv python if it exists, otherwise
/// falls back to the system `python3`. The venv is created (and requirements
/// installed) by [`ensure_python_venv`] during launch.
fn program_for(server_type: &str, config: &PluginConfig) -> Result<PathBuf, PluginError> {
    match server_type {
        "python" => {
            let venv_python = config
                .working_dir
                .join(VENV_DIR)
                .join("bin")
                .join("python3");
            if venv_python.is_file() {
                Ok(venv_python)
            } else {
                Ok(PathBuf::from("python3"))
            }
        }
        "node" => Ok(PathBuf::from("node")),
        other => Err(PluginError::UnsupportedServerType(other.to_string())),
    }
}

/// Create a per-app Python venv and install requirements (if declared).
///
/// Idempotent — skips setup when the `READY` marker file exists. The marker
/// records a content hash of the requirements file so it re-installs when
/// requirements change.
async fn ensure_python_venv(config: &PluginConfig) -> Result<(), PluginError> {
    let venv_dir = config.working_dir.join(VENV_DIR);
    let marker_path = config.working_dir.join(VENV_READY_MARKER);

    let requirements_path = config
        .requirements
        .as_ref()
        .map(|rel| config.working_dir.join(rel));

    let current_hash = match &requirements_path {
        Some(path) => {
            let bytes = tokio::fs::read(path).await.map_err(|e| {
                PluginError::VenvSetup(format!("failed to read {}: {e}", path.display()))
            })?;
            Some(blake3::hash(&bytes).to_hex().to_string())
        }
        None => None,
    };

    let needs_setup = if marker_path.is_file() {
        let stored = tokio::fs::read_to_string(&marker_path)
            .await
            .unwrap_or_default();
        let stored_hash = stored.trim();
        match (&current_hash, stored_hash.is_empty()) {
            (Some(hash), false) => hash != stored_hash,
            (None, true) => false,
            _ => true,
        }
    } else {
        true
    };

    if !needs_setup {
        return Ok(());
    }

    if venv_dir.is_dir() {
        tokio::fs::remove_dir_all(&venv_dir).await.map_err(|e| {
            PluginError::VenvSetup(format!(
                "failed to remove old venv {}: {e}",
                venv_dir.display()
            ))
        })?;
    }

    let venv_python = venv_dir.join("bin").join("python3");

    let create_status = tokio::process::Command::new("python3")
        .args(["-m", "venv", venv_dir.to_str().unwrap_or(VENV_DIR)])
        .current_dir(&config.working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .await
        .map_err(|e| PluginError::VenvSetup(format!("failed to run python3 -m venv: {e}")))?;

    if !create_status.success() {
        return Err(PluginError::VenvSetup(format!(
            "python3 -m venv exited with {}",
            create_status.code().unwrap_or(-1)
        )));
    }

    if let Some(rel) = &config.requirements {
        let req_arg = format!("-r{rel}");
        let install_status = tokio::process::Command::new(&venv_python)
            .args(["-m", "pip", "install", "--quiet", &req_arg])
            .current_dir(&config.working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .map_err(|e| {
                PluginError::VenvSetup(format!(
                    "failed to run pip install for {}: {e}",
                    config.name
                ))
            })?;

        if !install_status.success() {
            return Err(PluginError::VenvSetup(format!(
                "pip install -r{} exited with {} for plugin {}",
                rel,
                install_status.code().unwrap_or(-1),
                config.name,
            )));
        }
    }

    let marker_content = current_hash.as_deref().unwrap_or("");
    tokio::fs::write(&marker_path, marker_content)
        .await
        .map_err(|e| {
            PluginError::VenvSetup(format!(
                "failed to write venv marker {}: {e}",
                marker_path.display()
            ))
        })?;

    Ok(())
}

fn build_command(config: &PluginConfig, program: PathBuf) -> Result<Command, PluginError> {
    let mut command = Command::new(program);
    command
        .arg(&config.entry)
        .current_dir(&config.working_dir)
        .envs(config.command_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

/// Mutable stdio state for one plugin, guarded by a mutex so `&self` callers can
/// issue serialized request/response round trips.
struct PluginIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl PluginIo {
    /// Write a JSON-RPC request and block until the response with the matching
    /// id arrives, skipping interleaved notifications and unrelated responses.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, PluginError> {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&request).await?;

        let read = async {
            loop {
                let mut line = String::new();
                let read = self.stdout.read_line(&mut line).await?;
                if read == 0 {
                    return Err(PluginError::ProcessDied);
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let message: Value = match serde_json::from_str(trimmed) {
                    Ok(value) => value,
                    Err(error) => {
                        warn!(method, %error, "ignoring malformed plugin stdout line");
                        continue;
                    }
                };
                // Skip notifications (no id) and responses to other requests.
                if message.get("id").and_then(Value::as_i64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    return Err(PluginError::Rpc {
                        method: method.to_string(),
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string(),
                    });
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        };

        match tokio::time::timeout(REQUEST_TIMEOUT, read).await {
            Ok(result) => result,
            Err(_) => Err(PluginError::Timeout(method.to_string())),
        }
    }

    /// Write a JSON-RPC notification (no id, no response expected).
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), PluginError> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&notification).await
    }

    async fn write_message(&mut self, message: &Value) -> Result<(), PluginError> {
        let mut bytes = serde_json::to_vec(message)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Perform the MCP `initialize` handshake followed by the
    /// `notifications/initialized` acknowledgement.
    async fn initialize(&mut self) -> Result<(), PluginError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "spur-notebook",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn list_tools(&mut self) -> Result<Vec<Tool>, PluginError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result.get("tools").cloned().unwrap_or_else(|| json!([]));
        Ok(serde_json::from_value(tools)?)
    }

    async fn call_tool(&mut self, tool_name: &str, arguments: Value) -> Result<Value, PluginError> {
        self.request(
            "tools/call",
            json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        )
        .await
    }

    async fn ping(&mut self) -> Result<(), PluginError> {
        self.request("ping", json!({})).await.map(|_| ())
    }
}

/// One running plugin child process plus its cached tool catalog.
pub struct PluginHandle {
    config: PluginConfig,
    child: Child,
    io: Mutex<PluginIo>,
    tools: Vec<Tool>,
}

impl PluginHandle {
    /// Spawn the child process, perform the MCP handshake, and discover tools.
    async fn launch(config: PluginConfig) -> Result<Self, PluginError> {
        if config.server_type == "python" {
            ensure_python_venv(&config).await?;
        }
        let program = program_for(&config.server_type, &config)?;
        let mut command = build_command(&config, program)?;
        let mut child = command.spawn().map_err(PluginError::Spawn)?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| PluginError::MissingPipe(config.name.clone()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| PluginError::MissingPipe(config.name.clone()))?;

        // Drain stderr into the tracing log so plugin diagnostics are visible.
        if let Some(stderr) = child.stderr.take() {
            let plugin = config.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    warn!(plugin = %plugin, "{line}");
                }
            });
        }

        let mut io = PluginIo {
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        };
        io.initialize().await?;
        let tools = io.list_tools().await?;

        Ok(Self {
            config,
            child,
            io: Mutex::new(io),
            tools,
        })
    }

    /// Tool names this plugin exposed at launch.
    fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// Kill the child process and reap it. The per-app venv is left in place so
    /// subsequent launches skip the setup step.
    async fn shutdown(mut self) {
        if let Err(error) = self.child.start_kill() {
            warn!(plugin = %self.config.name, %error, "failed to signal plugin shutdown");
        }
        let _ = self.child.wait().await;
    }
}

/// A collection of running plugins keyed by name.
///
/// The registry is `Send + Sync`, so callers can hold it behind an
/// `Arc<tokio::sync::Mutex<_>>` and share it across the async runtime.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, PluginHandle>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of plugins currently running.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether any plugin is running.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Spawn a plugin from its config, returning the discovered tool names.
    pub async fn spawn(&mut self, config: PluginConfig) -> Result<Vec<String>, PluginError> {
        if self.plugins.contains_key(&config.name) {
            return Err(PluginError::AlreadyRunning(config.name));
        }
        let name = config.name.clone();
        let handle = PluginHandle::launch(config).await?;
        let names = handle.tool_names();
        self.plugins.insert(name, handle);
        Ok(names)
    }

    fn handle(&self, name: &str) -> Result<&PluginHandle, PluginError> {
        self.plugins
            .get(name)
            .ok_or_else(|| PluginError::UnknownPlugin(name.to_string()))
    }

    /// Proxy a `tools/call` to the named plugin.
    pub async fn call_tool(
        &self,
        name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value, PluginError> {
        let handle = self.handle(name)?;
        let mut io = handle.io.lock().await;
        io.call_tool(tool_name, arguments).await
    }

    /// Query the named plugin's current tool catalog via `tools/list`.
    pub async fn list_tools(&self, name: &str) -> Result<Vec<Tool>, PluginError> {
        let handle = self.handle(name)?;
        let mut io = handle.io.lock().await;
        io.list_tools().await
    }

    /// Health-check the named plugin with an MCP `ping`.
    pub async fn ping(&self, name: &str) -> Result<(), PluginError> {
        let handle = self.handle(name)?;
        let mut io = handle.io.lock().await;
        io.ping().await
    }

    /// Every running plugin's tool catalog (captured at launch), flattened in
    /// stable plugin-name order. Used to merge plugin tools into the notebook
    /// server's `tools/list` response.
    pub fn all_tools(&self) -> Vec<Tool> {
        self.plugins
            .values()
            .flat_map(|handle| handle.tools.iter().cloned())
            .collect()
    }

    /// Whether any running plugin exposes a tool with the given name (matched
    /// against the catalog captured at launch).
    pub fn has_tool(&self, tool_name: &str) -> bool {
        self.plugins
            .values()
            .any(|handle| handle.tools.iter().any(|tool| tool.name == tool_name))
    }

    /// Resolve which running plugin owns a tool, if any.
    pub fn plugin_for_tool(&self, tool_name: &str) -> Option<&str> {
        self.plugins
            .iter()
            .find(|(_, handle)| handle.tools.iter().any(|tool| tool.name == tool_name))
            .map(|(name, _)| name.as_str())
    }

    /// Stop the named plugin, killing its child process.
    pub async fn shutdown(&mut self, name: &str) -> Result<(), PluginError> {
        let handle = self
            .plugins
            .remove(name)
            .ok_or_else(|| PluginError::UnknownPlugin(name.to_string()))?;
        handle.shutdown().await;
        Ok(())
    }

    /// Stop every running plugin.
    pub async fn shutdown_all(&mut self) {
        let handles = std::mem::take(&mut self.plugins);
        for (_, handle) in handles {
            handle.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time guarantee that the registry can live behind an Arc<Mutex<_>>.
    fn _assert_send_sync<T: Send + Sync>() {}
    const _: fn() = || _assert_send_sync::<PluginRegistry>();

    #[test]
    fn plugin_config_from_manifest_builds_env() {
        let mut env = BTreeMap::new();
        env.insert("API_TOKEN".to_string(), "secret".to_string());
        env.insert("REGION".to_string(), "us".to_string());
        let manifest = SpurAppMcpServer {
            server_type: "python".to_string(),
            entry: "server.py".to_string(),
            requirements: Some("requirements.txt".to_string()),
            env,
        };

        let config = PluginConfig::from_manifest("demo", &manifest, "/tmp/app");

        assert_eq!(config.name, "demo");
        assert_eq!(config.server_type, "python");
        assert_eq!(config.entry, "server.py");
        assert_eq!(config.requirements.as_deref(), Some("requirements.txt"));
        assert_eq!(config.working_dir, PathBuf::from("/tmp/app"));
        // Manifest env is copied verbatim.
        assert_eq!(
            config.env.get("API_TOKEN").map(String::as_str),
            Some("secret")
        );
        assert_eq!(config.env.get("REGION").map(String::as_str), Some("us"));

        // The spawn-time env layers on the unbuffered-stdout flag for Python
        // without mutating the stored manifest env.
        let command_env = config.command_env();
        assert_eq!(
            command_env.get("PYTHONUNBUFFERED").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            command_env.get("API_TOKEN").map(String::as_str),
            Some("secret")
        );
        assert!(!config.env.contains_key("PYTHONUNBUFFERED"));
    }

    #[test]
    fn plugin_config_command_env_skips_python_flag_for_node() {
        let manifest = SpurAppMcpServer {
            server_type: "node".to_string(),
            entry: "server.js".to_string(),
            requirements: None,
            env: BTreeMap::new(),
        };
        let config = PluginConfig::from_manifest("node-demo", &manifest, "/tmp/app");

        assert!(!config.command_env().contains_key("PYTHONUNBUFFERED"));
    }

    #[test]
    fn plugin_registry_starts_empty() {
        let registry = PluginRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(!registry.has_tool("anything"));
        assert_eq!(registry.plugin_for_tool("anything"), None);
    }

    #[test]
    fn program_for_rejects_unknown_server_type() {
        let config = PluginConfig {
            name: "test".to_string(),
            server_type: "python".to_string(),
            entry: "x".to_string(),
            requirements: None,
            env: BTreeMap::new(),
            working_dir: PathBuf::from("/tmp"),
        };
        assert!(program_for("python", &config).is_ok());
        assert!(program_for("node", &config).is_ok());
        match program_for("ruby", &config) {
            Err(PluginError::UnsupportedServerType(kind)) => assert_eq!(kind, "ruby"),
            other => panic!("expected UnsupportedServerType, got {other:?}"),
        }
    }
}
