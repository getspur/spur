use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use spur_acp::*;
use spur_pm::{IssueFilter, IssueUpdate, PmService, PrParams};

use crate::tools::{self, DelegationChannel, DelegationRequest};

/// Maximum time to block on a delegation result before falling back to
/// async polling.  Must be well under the brain's MCP-client timeout
/// (typically 120 s) to leave margin for HTTP round-trip overhead.
const DELEGATION_BLOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

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

/// Descriptor for a worker-capable agent, returned by the
/// `list_available_workers` MCP tool.
///
/// Populated by `build_worker_info` from a merged `AgentConfig`.
/// See design spec section C.1.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerInfo {
    pub name:         String,
    pub tier:         Option<String>,
    pub description:  Option<String>,
    #[serde(default)]
    pub good_for:     Vec<String>,
    #[serde(default)]
    pub avoid_for:    Vec<String>,
    pub output_shape: Option<String>,
    pub cost_tier:    Option<String>,
}

/// Build the public `WorkerInfo` from a merged `AgentConfig`.
/// Call AFTER `apply_builtin_defaults` to see inherited values.
pub fn build_worker_info(cfg: &spur_acp::config::AgentConfig) -> WorkerInfo {
    use spur_acp::config::Tier;
    WorkerInfo {
        name:         cfg.name.clone(),
        tier:         cfg.delegation.tier.map(|t| match t {
            Tier::Specialist => "specialist".into(),
            Tier::Generalist => "generalist".into(),
        }),
        description:  cfg.delegation.description.clone(),
        good_for:     cfg.delegation.good_for.clone(),
        avoid_for:    cfg.delegation.avoid_for.clone(),
        output_shape: cfg.delegation.output_shape.clone(),
        cost_tier:    Some(format!("{:?}", cfg.cost_tier).to_lowercase()),
    }
}

// ─── McpCallbackServer ───────────────────────────────────────────────

/// MCP callback server that brain agents connect to via HTTP.
///
/// Exposes delegation and PM tools via JSON-RPC over HTTP POST,
/// compatible with the MCP Streamable HTTP transport.
pub struct McpCallbackServer {
    /// Channel to send delegation requests to the orchestrator.
    delegation_tx: mpsc::Sender<DelegationRequest>,
    /// Available worker agents (set once at creation).
    workers: Vec<WorkerInfo>,
    /// Brain session this server belongs to.
    brain_session_id: SessionId,
    /// Delegation IDs whose background collector is still awaiting a result.
    active_delegations: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Results that a background collector has received but the brain has
    /// not yet polled via `check_delegation_status` / `wait_delegation`.
    completed_delegations: Arc<tokio::sync::Mutex<HashMap<String, DelegationResult>>>,
    /// Optional PM service for direct issue/PR operations.
    pm_service: Option<Arc<PmService>>,
}

impl McpCallbackServer {
    /// Create a new MCP callback server for the given session.
    ///
    /// Returns the server instance and a `DelegationChannel` that the
    /// orchestrator uses to receive requests and send responses.
    pub fn new(session_id: &SessionId, pm_service: Option<Arc<PmService>>) -> (Self, DelegationChannel) {
        let (req_tx, req_rx) = mpsc::channel::<DelegationRequest>(32);

        let server = Self {
            delegation_tx: req_tx,
            workers: Vec::new(),
            brain_session_id: session_id.clone(),
            active_delegations: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            completed_delegations: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            pm_service,
        };

        let channel = DelegationChannel { request_rx: req_rx };
        (server, channel)
    }

    /// Spawn a background task that awaits a delegation oneshot and stores
    /// the result in `completed_delegations` for later polling.
    fn spawn_result_collector(
        delegation_id: String,
        rx: tokio::sync::oneshot::Receiver<DelegationResult>,
        active: Arc<tokio::sync::Mutex<HashSet<String>>>,
        completed: Arc<tokio::sync::Mutex<HashMap<String, DelegationResult>>>,
    ) {
        tokio::spawn(async move {
            let result = match rx.await {
                Ok(r) => r,
                Err(_) => DelegationResult {
                    status: DelegationStatus::Failed {
                        error: "Orchestrator disconnected".into(),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                },
            };
            active.lock().await.remove(&delegation_id);
            completed.lock().await.insert(delegation_id, result);
        });
    }

    /// Set the list of available worker agents.
    pub fn set_workers(&mut self, workers: Vec<WorkerInfo>) {
        self.workers = workers;
    }

    /// Start listening on a random localhost port.
    ///
    /// Returns the URL (e.g. `http://127.0.0.1:12345`) and a `JoinHandle`.
    pub async fn start(self: Arc<Self>) -> Result<(String, JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind TCP listener")?;

        let addr = listener.local_addr()?;
        let url = format!("http://{addr}");

        info!(url = %url, "MCP callback server listening (HTTP)");

        let server = Arc::clone(&self);
        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let server = Arc::clone(&server);
                        tokio::spawn(async move {
                            if let Err(e) = server.handle_http(stream).await {
                                debug!(error = %e, "HTTP connection error");
                            }
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to accept TCP connection");
                    }
                }
            }
        });

        Ok((url, handle))
    }

    // ─── Minimal HTTP handler ─────────────────────────────────────────

    async fn handle_http(&self, mut stream: tokio::net::TcpStream) -> Result<()> {
        let (reader, mut writer) = stream.split();
        let mut buf_reader = BufReader::new(reader);

        // Read request line.
        let mut request_line = String::new();
        buf_reader.read_line(&mut request_line).await?;

        // Read headers to find Content-Length. HTTP header names are
        // case-insensitive per RFC 9110; compare after lowercasing.
        let mut content_length: usize = 0;
        loop {
            let mut header = String::new();
            buf_reader.read_line(&mut header).await?;
            if header.trim().is_empty() {
                break;
            }
            if let Some((name, val)) = header.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = val.trim().parse().unwrap_or(0);
                }
            }
        }

        // Cap body size. Real MCP JSON-RPC payloads are a few KB; 1 MiB is
        // generous and prevents attacker-controlled allocation from an
        // inflated Content-Length header.
        const MAX_BODY: usize = 1024 * 1024;
        if content_length > MAX_BODY {
            let resp = b"Payload Too Large";
            let http = format!(
                "HTTP/1.1 413 Payload Too Large\r\nContent-Length: {}\r\n\r\n",
                resp.len()
            );
            writer.write_all(http.as_bytes()).await?;
            writer.write_all(resp).await?;
            return Ok(());
        }

        // Read body.
        let mut body = vec![0u8; content_length];
        buf_reader.read_exact(&mut body).await?;

        // Dispatch JSON-RPC.
        let response_body = if request_line.starts_with("POST") {
            match serde_json::from_slice::<JsonRpcRequest>(&body) {
                Ok(req) => {
                    let resp = self.dispatch(req).await;
                    serde_json::to_vec(&resp)?
                }
                Err(e) => {
                    let resp =
                        JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {e}"));
                    serde_json::to_vec(&resp)?
                }
            }
        } else {
            // GET or other — return 405.
            let resp = b"Method Not Allowed";
            let http = format!(
                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: {}\r\n\r\n",
                resp.len()
            );
            writer.write_all(http.as_bytes()).await?;
            writer.write_all(resp).await?;
            return Ok(());
        };

        // Write HTTP response.
        let http_header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            response_body.len()
        );
        writer.write_all(http_header.as_bytes()).await?;
        writer.write_all(&response_body).await?;

        Ok(())
    }

    // ─── Request dispatcher ───────────────────────────────────────────

    async fn dispatch(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let id = request.id.clone().unwrap_or(Value::Null);

        match request.method.as_str() {
            "initialize" => JsonRpcResponse::success(
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
            ),
            "notifications/initialized" => JsonRpcResponse::success(id, json!({})),
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
            "delegate_async" => self.handle_delegate_async(id, arguments).await,
            "wait_delegation" => self.handle_wait_delegation(id, arguments).await,
            "check_delegation_status" => {
                self.handle_check_delegation_status(id, arguments).await
            }
            "list_available_workers" => self.handle_list_available_workers(id).await,
            "get_issue" => self.handle_get_issue(id, arguments).await,
            "list_issues" => self.handle_list_issues(id, arguments).await,
            "update_issue" => self.handle_update_issue(id, arguments).await,
            "create_pr" => self.handle_create_pr(id, arguments).await,
            "report_progress" => self.handle_report_progress(id, arguments).await,
            "get_session_cost" => self.handle_get_session_cost(id).await,
            _ => JsonRpcResponse::error(id, -32601, format!("Unknown tool: {tool_name}")),
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
        let delegation_plan: Option<spur_acp::DelegationPlan> = args
            .get("delegation_plan")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let issue_id = args.get("issue_id").and_then(|v| v.as_str()).map(String::from);

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: agent.clone(),
            task: task.clone(),
            context_files,
            respond_to: tx,
            brain_session_id: self.brain_session_id.clone(),
            delegation_plan,
            issue_id,
        };

        info!(agent = %agent, request_id = %request_id, "Sending delegation request");

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            error!("Failed to send delegation request");
            return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
        }

        // Spawn a background collector so the oneshot is never dropped by
        // a timeout.  Then poll the completed-results map with a bounded
        // wait that stays well under the brain's 120 s HTTP timeout.
        self.active_delegations
            .lock()
            .await
            .insert(request_id.clone());
        Self::spawn_result_collector(
            request_id.clone(),
            rx,
            Arc::clone(&self.active_delegations),
            Arc::clone(&self.completed_delegations),
        );

        let deadline = tokio::time::Instant::now() + DELEGATION_BLOCK_TIMEOUT;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;

            if let Some(result) = self.completed_delegations.lock().await.remove(&request_id) {
                let result_json = match serde_json::to_value(&result) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("Failed to serialize result: {e}"),
                        )
                    }
                };
                return JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&result_json)
                                .unwrap_or_else(|_| result_json.to_string())
                        }]
                    }),
                );
            }

            if tokio::time::Instant::now() >= deadline {
                info!(
                    agent = %agent,
                    request_id = %request_id,
                    "Delegation exceeded block timeout, returning delegation_id for polling"
                );
                return JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!(
                                "Delegation to '{agent}' is still running (exceeded {timeout}s block limit). \
                                 Call check_delegation_status with delegation_id '{request_id}' to poll for the result.",
                                timeout = DELEGATION_BLOCK_TIMEOUT.as_secs(),
                            )
                        }]
                    }),
                );
            }
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

        let shared_plan: Option<spur_acp::DelegationPlan> = args
            .get("delegation_plan")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let shared_issue_id = args.get("issue_id").and_then(|v| v.as_str()).map(String::from);

        let mut receivers = Vec::with_capacity(tasks.len());

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
            let (tx, rx) = tokio::sync::oneshot::channel();

            let delegation = DelegationRequest {
                id: request_id.clone(),
                agent: agent.clone(),
                task,
                context_files: Vec::new(),
                respond_to: tx,
                brain_session_id: self.brain_session_id.clone(),
                delegation_plan: shared_plan.clone(),
                issue_id: shared_issue_id.clone(),
            };

            info!(agent = %agent, request_id = %request_id, "Sending parallel delegation request");

            if let Err(_e) = self.delegation_tx.send(delegation).await {
                error!("Failed to send parallel delegation request");
                return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
            }

            self.active_delegations
                .lock()
                .await
                .insert(request_id.clone());
            Self::spawn_result_collector(
                request_id.clone(),
                rx,
                Arc::clone(&self.active_delegations),
                Arc::clone(&self.completed_delegations),
            );
            receivers.push((request_id, agent));
        }

        // Poll completed-results map with a batch timeout.
        let deadline = tokio::time::Instant::now() + DELEGATION_BLOCK_TIMEOUT;
        let mut results = Vec::with_capacity(receivers.len());
        let mut pending: Vec<(String, String)> = receivers;

        while !pending.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let mut still_pending = Vec::new();
            for (request_id, agent) in pending {
                if let Some(result) =
                    self.completed_delegations.lock().await.remove(&request_id)
                {
                    results.push(serde_json::to_value(&result).unwrap_or(json!(null)));
                } else {
                    still_pending.push((request_id, agent));
                }
            }
            pending = still_pending;
        }

        // Any still-pending delegations get returned as "running".
        for (request_id, agent) in pending {
            results.push(json!({
                "status": "running",
                "delegation_id": request_id,
                "agent": agent,
                "message": "Use check_delegation_status to poll for the result."
            }));
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

    async fn handle_check_delegation_status(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let delegation_id = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        // Completed — return and remove.
        if let Some(result) = self.completed_delegations.lock().await.remove(&delegation_id) {
            let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result_json)
                            .unwrap_or_else(|_| result_json.to_string())
                    }]
                }),
            );
        }

        // Still running.
        if self.active_delegations.lock().await.contains(&delegation_id) {
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": json!({"status": "running", "delegation_id": delegation_id}).to_string()
                    }]
                }),
            );
        }

        JsonRpcResponse::error(
            id,
            -32602,
            format!("Unknown delegation: {delegation_id}"),
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
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let issue_id = match args.get("id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'id'"),
        };

        match pm.get_issue(&issue_id).await {
            Ok(issue) => {
                let text = serde_json::to_string_pretty(&issue)
                    .unwrap_or_else(|_| format!("{issue:?}"));
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("get_issue failed: {e}")),
        }
    }

    async fn handle_list_issues(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };

        let labels: Vec<String> = args
            .get("labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let filter = IssueFilter {
            status: args.get("status").and_then(|v| v.as_str()).map(String::from),
            assignee: args.get("assignee").and_then(|v| v.as_str()).map(String::from),
            priority_min: args.get("priority_min").and_then(|v| v.as_i64()).map(|n| n as i32),
            priority_max: args.get("priority_max").and_then(|v| v.as_i64()).map(|n| n as i32),
            issue_type: args.get("issue_type").and_then(|v| v.as_str()).map(String::from),
            text_search: args.get("text_search").and_then(|v| v.as_str()).map(String::from),
            limit: Some(args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(100) as usize),
            labels,
            since: None,
        };

        match pm.list_issues(filter).await {
            Ok(issues) => {
                let text = serde_json::to_string_pretty(&issues)
                    .unwrap_or_else(|_| format!("{issues:?}"));
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("list_issues failed: {e}")),
        }
    }

    async fn handle_update_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let issue_id = match args.get("id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'id'"),
        };

        let add_labels: Vec<String> = args
            .get("add_labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let remove_labels: Vec<String> = args
            .get("remove_labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let update = IssueUpdate {
            status: args.get("status").and_then(|v| v.as_str()).map(String::from),
            comment: args.get("comment").and_then(|v| v.as_str()).map(String::from),
            priority: args.get("priority").and_then(|v| v.as_i64()).map(|n| n as i32),
            assignee: args.get("assignee").and_then(|v| v.as_str()).map(String::from),
            add_labels,
            remove_labels,
        };

        match pm.update_issue(&issue_id, update).await {
            Ok(()) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": "Issue updated." }] }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("update_issue failed: {e}")),
        }
    }

    async fn handle_create_pr(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No PR service configured"),
        };
        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'title'"),
        };
        let body = match args.get("body").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'body'"),
        };
        let head_branch = match args.get("branch").and_then(|v| v.as_str()) {
            Some(b) => b.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'branch'"),
        };

        let params = PrParams {
            title,
            body,
            head_branch,
            base_branch: args.get("base_branch").and_then(|v| v.as_str()).map(String::from),
            repo: args.get("repo").and_then(|v| v.as_str()).map(String::from),
        };

        match pm.create_pr(params).await {
            Ok(url) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": format!("PR created: {url}") }] }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("create_pr failed: {e}")),
        }
    }

    async fn handle_report_progress(&self, id: Value, args: Value) -> JsonRpcResponse {
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'message'"),
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let delegation = DelegationRequest {
            id: request_id,
            agent: "__progress".into(),
            task: message.clone(),
            context_files: Vec::new(),
            respond_to: tx,
            brain_session_id: self.brain_session_id.clone(),
            delegation_plan: None,
            issue_id: None,
        };

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            warn!("Failed to send progress report");
        }

        info!(message = %message, "Progress reported");
        JsonRpcResponse::success(
            id,
            json!({ "content": [{ "type": "text", "text": "Progress reported." }] }),
        )
    }

    async fn handle_get_session_cost(&self, id: Value) -> JsonRpcResponse {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let delegation = DelegationRequest {
            id: request_id,
            agent: "__session_cost".into(),
            task: String::new(),
            context_files: Vec::new(),
            respond_to: tx,
            brain_session_id: self.brain_session_id.clone(),
            delegation_plan: None,
            issue_id: None,
        };

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            return JsonRpcResponse::internal_error(id, "Failed to forward request");
        }

        match rx.await {
            Ok(result) => {
                let text = result.summary.clone().unwrap_or_else(|| {
                    json!({ "estimated_cost_usd": result.estimated_cost_usd }).to_string()
                });
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(_) => JsonRpcResponse::internal_error(
                id,
                "get_session_cost failed: orchestrator disconnected",
            ),
        }
    }

    async fn handle_delegate_async(&self, id: Value, args: Value) -> JsonRpcResponse {
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
        let delegation_plan: Option<spur_acp::DelegationPlan> = args
            .get("delegation_plan")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let issue_id = args.get("issue_id").and_then(|v| v.as_str()).map(String::from);

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: agent.clone(),
            task,
            context_files,
            respond_to: tx,
            brain_session_id: self.brain_session_id.clone(),
            delegation_plan,
            issue_id,
        };

        info!(agent = %agent, request_id = %request_id, "Sending async delegation request");

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
        }

        self.active_delegations
            .lock()
            .await
            .insert(request_id.clone());

        Self::spawn_result_collector(
            request_id.clone(),
            rx,
            Arc::clone(&self.active_delegations),
            Arc::clone(&self.completed_delegations),
        );

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": json!({"delegation_id": request_id}).to_string()
                }]
            }),
        )
    }

    async fn handle_wait_delegation(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(id, "Missing required field 'delegation_id'")
            }
        };

        // Already completed — return immediately.
        if let Some(result) = self.completed_delegations.lock().await.remove(&delegation_id) {
            let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result_json)
                            .unwrap_or_else(|_| result_json.to_string())
                    }]
                }),
            );
        }

        // Unknown delegation.
        if !self.active_delegations.lock().await.contains(&delegation_id) {
            return JsonRpcResponse::error(
                id,
                -32602,
                format!("Unknown or already-collected delegation: {delegation_id}"),
            );
        }

        // Poll with a bounded wait so we never exceed the brain's HTTP timeout.
        let deadline = tokio::time::Instant::now() + DELEGATION_BLOCK_TIMEOUT;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            if let Some(result) = self.completed_delegations.lock().await.remove(&delegation_id) {
                let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
                return JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&result_json)
                                .unwrap_or_else(|_| result_json.to_string())
                        }]
                    }),
                );
            }

            if tokio::time::Instant::now() >= deadline {
                return JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!(
                                "Delegation '{delegation_id}' is still running. \
                                 Call check_delegation_status with this delegation_id to poll again."
                            )
                        }]
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod build_worker_info_tests {
    use super::build_worker_info;
    use spur_acp::config::AgentConfig;

    fn minimal_agent(name: &str) -> AgentConfig {
        let toml = format!(
            r#"name = "{}"
command = "x"
transport = "acp""#,
            name
        );
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn build_worker_info_populates_all_fields() {
        let mut cfg = minimal_agent("claude-code-acp");
        spur_acp::agents::defaults::apply_builtin_defaults(&mut cfg);
        let info = build_worker_info(&cfg);
        assert_eq!(info.name, "claude-code-acp");
        assert!(info.description.is_some());
        assert!(info.tier.is_some());
        assert!(!info.good_for.is_empty());
        assert!(info.output_shape.is_some());
    }

    #[test]
    fn build_worker_info_handles_empty_descriptor() {
        let cfg = minimal_agent("unknown-agent");
        // without apply_builtin_defaults, all fields stay empty
        let info = build_worker_info(&cfg);
        assert_eq!(info.name, "unknown-agent");
        assert!(info.description.is_none());
        assert!(info.good_for.is_empty());
    }
}
