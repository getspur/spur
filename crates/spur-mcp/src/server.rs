use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use spur_acp::*;

use crate::tools::{self, DelegationChannel, DelegationRequest, DelegationResponse};

// ─── JSON-RPC types ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    fn method_not_found(id: Value, method: &str) -> Self {
        Self::error(id, -32601, format!("Method not found: {method}"))
    }

    fn invalid_params(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32602, msg)
    }

    fn internal_error(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32603, msg)
    }
}

// ─── Worker info (static data set at startup) ─────────────────────────

/// Describes an available worker agent, provided to the server at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub name: String,
    pub description: String,
    pub cost_tier: CostTier,
}

// ─── McpCallbackServer ───────────────────────────────────────────────

/// MCP callback server that brain agents connect to during ACP initialization.
/// Exposes delegation and PM tools via JSON-RPC over Unix domain socket.
pub struct McpCallbackServer {
    socket_path: PathBuf,
    /// Channel to send delegation requests to the orchestrator.
    delegation_tx: mpsc::Sender<DelegationRequest>,
    /// Channel to receive delegation results from the orchestrator.
    delegation_rx: Mutex<mpsc::Receiver<DelegationResponse>>,
    /// Available worker agents (set once at creation).
    workers: Vec<WorkerInfo>,
}

impl McpCallbackServer {
    /// Create a new MCP callback server for the given session.
    ///
    /// Returns the server instance and a `DelegationChannel` that the
    /// orchestrator uses to receive requests and send responses.
    pub fn new(session_id: &SessionId) -> (Self, DelegationChannel) {
        let socket_path = PathBuf::from(format!("/tmp/spur-mcp-{session_id}.sock"));

        // Server -> Orchestrator: delegation requests
        let (req_tx, req_rx) = mpsc::channel::<DelegationRequest>(32);
        // Orchestrator -> Server: delegation responses
        let (resp_tx, resp_rx) = mpsc::channel::<DelegationResponse>(32);

        let server = Self {
            socket_path,
            delegation_tx: req_tx,
            delegation_rx: Mutex::new(resp_rx),
            workers: Vec::new(),
        };

        let channel = DelegationChannel {
            request_rx: req_rx,
            response_tx: resp_tx,
        };

        (server, channel)
    }

    /// Set the list of available worker agents.
    pub fn set_workers(&mut self, workers: Vec<WorkerInfo>) {
        self.workers = workers;
    }

    /// Return the MCP endpoint info to pass to agents during ACP init.
    pub fn endpoint(&self) -> McpEndpoint {
        McpEndpoint {
            socket_path: self.socket_path.clone(),
            server_name: "spur-mcp".into(),
        }
    }

    /// Start listening on the Unix domain socket.
    ///
    /// Spawns a background task that accepts connections and dispatches
    /// JSON-RPC requests to tool handlers. Returns a `JoinHandle` for
    /// the listener task.
    pub fn start(self: Arc<Self>) -> Result<JoinHandle<()>> {
        // Clean up any stale socket file.
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)
                .context("Failed to remove stale socket file")?;
        }

        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("Failed to bind Unix socket at {:?}", self.socket_path))?;

        info!(path = %self.socket_path.display(), "MCP callback server listening");

        let server = Arc::clone(&self);
        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        debug!("MCP client connected");
                        let server = Arc::clone(&server);
                        tokio::spawn(async move {
                            if let Err(e) = server.handle_connection(stream).await {
                                error!(error = %e, "Error handling MCP connection");
                            }
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to accept MCP connection");
                    }
                }
            }
        });

        Ok(handle)
    }

    /// Stop listening and clean up the socket file.
    pub fn shutdown(&self) -> Result<()> {
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)
                .context("Failed to remove socket file during shutdown")?;
            info!(path = %self.socket_path.display(), "MCP socket cleaned up");
        }
        Ok(())
    }

    /// Returns the socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    // ─── Connection handler ───────────────────────────────────────────

    async fn handle_connection(&self, stream: tokio::net::UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        while let Some(line) = lines.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            debug!(request = %line, "Received JSON-RPC request");

            let request: JsonRpcRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let resp = JsonRpcResponse::error(
                        Value::Null,
                        -32700,
                        format!("Parse error: {e}"),
                    );
                    let mut buf = serde_json::to_vec(&resp)?;
                    buf.push(b'\n');
                    writer.write_all(&buf).await?;
                    continue;
                }
            };

            let id = request.id.clone().unwrap_or(Value::Null);
            let response = self.dispatch(request).await;

            let mut buf = serde_json::to_vec(&response)?;
            buf.push(b'\n');
            writer.write_all(&buf).await?;
            writer.flush().await?;
        }

        debug!("MCP client disconnected");
        Ok(())
    }

    // ─── Request dispatcher ───────────────────────────────────────────

    async fn dispatch(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone().unwrap_or(Value::Null);

        match request.method.as_str() {
            "initialize" => {
                JsonRpcResponse::success(
                    id,
                    json!({
                        "protocolVersion": "2024-11-05",
                        "serverInfo": {
                            "name": "spur-mcp",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "capabilities": {
                            "tools": {}
                        }
                    }),
                )
            }
            "notifications/initialized" => {
                // Client acknowledgement -- no response needed for notifications,
                // but since we always send a response, return an empty success.
                JsonRpcResponse::success(id, json!({}))
            }
            "tools/list" => {
                let defs = tools::tools_list();
                JsonRpcResponse::success(id, json!({ "tools": defs }))
            }
            "tools/call" => self.handle_tool_call(id, request.params).await,
            _ => JsonRpcResponse::method_not_found(id, &request.method),
        }
    }

    // ─── Tool call dispatcher ─────────────────────────────────────────

    async fn handle_tool_call(&self, id: Value, params: Value) -> JsonRpcResponse {
        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing 'name' in params"),
        };

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));

        debug!(tool = %tool_name, "Handling tool call");

        match tool_name.as_str() {
            "delegate_to_worker" => self.handle_delegate_to_worker(id, arguments).await,
            "delegate_parallel" => self.handle_delegate_parallel(id, arguments).await,
            "list_available_workers" => self.handle_list_available_workers(id).await,
            "get_issue" => self.handle_get_issue(id, arguments).await,
            "update_issue" => self.handle_update_issue(id, arguments).await,
            "create_pr" => self.handle_create_pr(id, arguments).await,
            "report_progress" => self.handle_report_progress(id, arguments).await,
            "get_session_cost" => self.handle_get_session_cost(id).await,
            _ => JsonRpcResponse::error(
                id,
                -32601,
                format!("Unknown tool: {tool_name}"),
            ),
        }
    }

    // ─── Tool handlers ────────────────────────────────────────────────

    async fn handle_delegate_to_worker(&self, id: Value, args: Value) -> JsonRpcResponse {
        let agent = match args.get("agent").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'agent'"),
        };
        let task = match args.get("task").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'task'"),
        };
        let context_files: Vec<String> = args
            .get("context_files")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let request_id = uuid::Uuid::new_v4().to_string();

        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: agent.clone(),
            task: task.clone(),
            context_files,
        };

        info!(agent = %agent, request_id = %request_id, "Sending delegation request");

        if let Err(e) = self.delegation_tx.send(delegation).await {
            error!(error = %e, "Failed to send delegation request");
            return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
        }

        // Wait for the matching response from the orchestrator.
        match self.wait_for_response(&request_id).await {
            Ok(response) => {
                let result_json = match serde_json::to_value(&response.result) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("Failed to serialize result: {e}"),
                        )
                    }
                };
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&result_json)
                                .unwrap_or_else(|_| result_json.to_string())
                        }]
                    }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("Delegation failed: {e}")),
        }
    }

    async fn handle_delegate_parallel(&self, id: Value, args: Value) -> JsonRpcResponse {
        let tasks = match args.get("tasks").and_then(|v| v.as_array()) {
            Some(t) => t.clone(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'tasks'"),
        };

        if tasks.is_empty() {
            return JsonRpcResponse::invalid_params(id, "'tasks' array must not be empty");
        }

        let mut request_ids = Vec::with_capacity(tasks.len());

        // Send all delegation requests.
        for task_obj in &tasks {
            let agent = match task_obj.get("agent").and_then(|v| v.as_str()) {
                Some(a) => a.to_string(),
                None => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        "Each task must have an 'agent' field",
                    )
                }
            };
            let task = match task_obj.get("task").and_then(|v| v.as_str()) {
                Some(t) => t.to_string(),
                None => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        "Each task must have a 'task' field",
                    )
                }
            };

            let request_id = uuid::Uuid::new_v4().to_string();

            let delegation = DelegationRequest {
                id: request_id.clone(),
                agent: agent.clone(),
                task,
                context_files: Vec::new(),
            };

            info!(agent = %agent, request_id = %request_id, "Sending parallel delegation request");

            if let Err(e) = self.delegation_tx.send(delegation).await {
                error!(error = %e, "Failed to send parallel delegation request");
                return JsonRpcResponse::internal_error(
                    id,
                    "Failed to send delegation request",
                );
            }

            request_ids.push(request_id);
        }

        // Wait for all responses.
        let mut results = Vec::with_capacity(request_ids.len());
        for request_id in &request_ids {
            match self.wait_for_response(request_id).await {
                Ok(response) => {
                    results.push(serde_json::to_value(&response.result).unwrap_or(json!(null)));
                }
                Err(e) => {
                    results.push(json!({
                        "status": "Failed",
                        "error": format!("Delegation {request_id} failed: {e}")
                    }));
                }
            }
        }

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&results)
                        .unwrap_or_else(|_| json!(results).to_string())
                }]
            }),
        )
    }

    async fn handle_list_available_workers(&self, id: Value) -> JsonRpcResponse {
        let workers_json = serde_json::to_value(&self.workers).unwrap_or(json!([]));
        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&workers_json)
                        .unwrap_or_else(|_| workers_json.to_string())
                }]
            }),
        )
    }

    async fn handle_get_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let source = match args.get("source").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'source'"),
        };
        let issue_id = match args.get("id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'id'"),
        };

        // Forward to orchestrator as a delegation request with a special agent name.
        let request_id = uuid::Uuid::new_v4().to_string();
        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: "__pm_get_issue".into(),
            task: json!({ "source": source, "id": issue_id }).to_string(),
            context_files: Vec::new(),
        };

        if let Err(e) = self.delegation_tx.send(delegation).await {
            return JsonRpcResponse::internal_error(id, format!("Failed to forward request: {e}"));
        }

        match self.wait_for_response(&request_id).await {
            Ok(response) => {
                let text = response
                    .result
                    .summary
                    .unwrap_or_else(|| "No issue data returned".into());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": text }]
                    }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("get_issue failed: {e}")),
        }
    }

    async fn handle_update_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let source = match args.get("source").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'source'"),
        };
        let issue_id = match args.get("id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'id'"),
        };
        let status = args.get("status").and_then(|v| v.as_str()).map(String::from);
        let comment = args
            .get("comment")
            .and_then(|v| v.as_str())
            .map(String::from);

        let request_id = uuid::Uuid::new_v4().to_string();
        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: "__pm_update_issue".into(),
            task: json!({
                "source": source,
                "id": issue_id,
                "status": status,
                "comment": comment,
            })
            .to_string(),
            context_files: Vec::new(),
        };

        if let Err(e) = self.delegation_tx.send(delegation).await {
            return JsonRpcResponse::internal_error(id, format!("Failed to forward request: {e}"));
        }

        match self.wait_for_response(&request_id).await {
            Ok(response) => {
                let text = response
                    .result
                    .summary
                    .unwrap_or_else(|| "Issue updated".into());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": text }]
                    }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("update_issue failed: {e}")),
        }
    }

    async fn handle_create_pr(&self, id: Value, args: Value) -> JsonRpcResponse {
        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'title'"),
        };
        let body = match args.get("body").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'body'"),
        };
        let branch = match args.get("branch").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'branch'"),
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: "__pm_create_pr".into(),
            task: json!({
                "title": title,
                "body": body,
                "branch": branch,
            })
            .to_string(),
            context_files: Vec::new(),
        };

        if let Err(e) = self.delegation_tx.send(delegation).await {
            return JsonRpcResponse::internal_error(id, format!("Failed to forward request: {e}"));
        }

        match self.wait_for_response(&request_id).await {
            Ok(response) => {
                let text = response
                    .result
                    .summary
                    .unwrap_or_else(|| "PR created".into());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": text }]
                    }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("create_pr failed: {e}")),
        }
    }

    async fn handle_report_progress(&self, id: Value, args: Value) -> JsonRpcResponse {
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(id, "Missing required field 'message'")
            }
        };

        // Fire-and-forget: send as a delegation request but don't wait for response.
        let request_id = uuid::Uuid::new_v4().to_string();
        let delegation = DelegationRequest {
            id: request_id,
            agent: "__progress".into(),
            task: message.clone(),
            context_files: Vec::new(),
        };

        if let Err(e) = self.delegation_tx.send(delegation).await {
            warn!(error = %e, "Failed to send progress report");
        }

        info!(message = %message, "Progress reported");

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{ "type": "text", "text": "Progress reported." }]
            }),
        )
    }

    async fn handle_get_session_cost(&self, id: Value) -> JsonRpcResponse {
        let request_id = uuid::Uuid::new_v4().to_string();
        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: "__session_cost".into(),
            task: String::new(),
            context_files: Vec::new(),
        };

        if let Err(e) = self.delegation_tx.send(delegation).await {
            return JsonRpcResponse::internal_error(id, format!("Failed to forward request: {e}"));
        }

        match self.wait_for_response(&request_id).await {
            Ok(response) => {
                let text = response
                    .result
                    .summary
                    .unwrap_or_else(|| {
                        json!({ "estimated_cost_usd": response.result.estimated_cost_usd })
                            .to_string()
                    });
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": text }]
                    }),
                )
            }
            Err(e) => {
                JsonRpcResponse::internal_error(id, format!("get_session_cost failed: {e}"))
            }
        }
    }

    // ─── Helpers ──────────────────────────────────────────────────────

    /// Wait for a delegation response with the given request ID.
    ///
    /// Consumes responses from the channel until the matching one is found.
    /// Non-matching responses are dropped with a warning (this should not
    /// happen in normal operation since we process requests sequentially
    /// per connection, but provides resilience).
    async fn wait_for_response(&self, request_id: &str) -> Result<DelegationResponse> {
        let mut rx = self.delegation_rx.lock().await;
        loop {
            match rx.recv().await {
                Some(response) if response.id == request_id => {
                    return Ok(response);
                }
                Some(other) => {
                    warn!(
                        expected = %request_id,
                        got = %other.id,
                        "Received out-of-order delegation response, dropping"
                    );
                }
                None => {
                    anyhow::bail!("Delegation response channel closed");
                }
            }
        }
    }
}
