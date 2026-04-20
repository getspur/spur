use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use rmcp::{
    model::{
        object as rmcp_object, CallToolRequestParams, CallToolResult, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData as McpError, RoleServer, ServerHandler,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info};

use spur_acp::*;
use spur_pm::{IssueFilter, IssueUpdate, PmService, PrParams};

use crate::tools::{self, DelegationChannel, DelegationRequest};

/// Maximum time to block on a delegation result before falling back to
/// async polling.  Must be well under the brain's MCP-client timeout
/// (typically 120 s) to leave margin for HTTP round-trip overhead.
const DELEGATION_BLOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// How long completed delegation results are retained before lazy eviction.
/// Generous: the brain should poll within seconds, but we keep results
/// around for 10 minutes to tolerate slow or distracted brains.
const COMPLETED_TTL: std::time::Duration = std::time::Duration::from_secs(600);

// ─── JSON-RPC types ───────────────────────────────────────────────────

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

impl JsonRpcError {
    fn into_mcp_error(self) -> McpError {
        McpError::new(
            rmcp::model::ErrorCode(self.code as i32),
            self.message,
            self.data,
        )
    }
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
    pub name: String,
    pub tier: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub good_for: Vec<String>,
    #[serde(default)]
    pub avoid_for: Vec<String>,
    pub output_shape: Option<String>,
    pub cost_tier: Option<String>,
}

/// Build the public `WorkerInfo` from a merged `AgentConfig`.
/// Call AFTER `apply_builtin_defaults` to see inherited values.
pub fn build_worker_info(cfg: &spur_acp::config::AgentConfig) -> WorkerInfo {
    use spur_acp::config::Tier;
    WorkerInfo {
        name: cfg.name.clone(),
        tier: cfg.delegation.tier.map(|t| match t {
            Tier::Specialist => "specialist".into(),
            Tier::Generalist => "generalist".into(),
        }),
        description: cfg.delegation.description.clone(),
        good_for: cfg.delegation.good_for.clone(),
        avoid_for: cfg.delegation.avoid_for.clone(),
        output_shape: cfg.delegation.output_shape.clone(),
        cost_tier: Some(format!("{:?}", cfg.cost_tier).to_lowercase()),
    }
}

// ─── Detached continuation types ─────────────────────────────────────

/// Boxed async callback invoked by `spawn_result_collector` when a detached
/// delegation finishes.
///
/// Arguments:
/// - `BrainContinuation` — the completed delegation result.
/// - `String` — worker-session identifier (delegation UUID used as proxy for
///   the `DelegationCompleted` UI event; unique per delegation).
///
/// Implementer routes the continuation back to the orchestrator ingress
/// (emit UI event first, then try_send / overflow — INV-C3).
pub type DetachedCompletionCallback = Arc<
    dyn Fn(
            spur_acp::domain::BrainContinuation,
            String, // worker_session proxy
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Bundle of handles required to funnel detached delegation completions back
/// into the orchestrator's ingress channel.
///
/// Uses a boxed async callback so that `spur-mcp` does not need to depend on
/// `spur-core` (which would create a circular dependency).  `spur-core` wires
/// the real `report_detached_completion` implementation in
/// `Orchestrator::build_continuation_ctx`.
pub struct DetachedContinuationCtx {
    /// See [`DetachedCompletionCallback`] for the callback contract.
    pub on_complete: DetachedCompletionCallback,
}

/// Why a delegation went detached (used to set `ContinuationSource`).
pub enum DetachedSourceKind {
    AsyncRequested,
    BlockTimeout,
}

/// All the handles `spawn_result_collector` needs to call
/// `report_detached_completion` when a detached delegation finishes.
pub struct DetachedCompletionHandle {
    pub ctx: Arc<DetachedContinuationCtx>,
    pub source_kind: DetachedSourceKind,
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
    /// Brain session this server belongs to. INV-2: typed as BrainSessionId.
    brain_session_id: spur_acp::BrainSessionId,
    /// Delegation IDs whose background collector is still awaiting a result.
    active_delegations: Arc<tokio::sync::Mutex<HashSet<String>>>,
    /// Results that a background collector has received but the brain has
    /// not yet polled via `check_delegation_status` / `wait_delegation`.
    /// Stored with insertion timestamp for TTL-based lazy eviction.
    completed_delegations:
        Arc<tokio::sync::Mutex<HashMap<String, (DelegationResult, tokio::time::Instant)>>>,
    /// Tracks spawned result-collector tasks for graceful shutdown.
    task_tracker: TaskTracker,
    /// Optional PM service for direct issue/PR operations.
    pm_service: Option<Arc<PmService>>,
    /// Optional event sink for emitting MCP lifecycle events.
    event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    /// Active execution plans submitted via `submit_plan`.
    active_plans:
        Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<crate::plan::PlanState>>>>>,
    /// Phase 2.5 idempotency guard: maps `epic_id → plan_id` for the
    /// currently-active plan (if any). A sentinel `"__pending__"` value is
    /// used briefly during the PmService fetch to prevent concurrent
    /// `execute_epic` calls from racing into double-dispatch. Terminal plans
    /// are cleared lazily on the next `execute_epic` call for the same epic.
    plan_registry: Arc<tokio::sync::Mutex<crate::plan::PlanRegistry>>,
    /// INV-6: handle to the orchestrator's per-delegation cancellation token
    /// registry. `None` in test harnesses that don't wire a real orchestrator.
    cancellation_control: Option<CancellationControl>,
    /// Bundle of handles for routing detached delegation completions back
    /// into the orchestrator ingress via `report_detached_completion`.
    continuation_ctx: Arc<DetachedContinuationCtx>,
    /// Phase 1c: how long `handle_delegate_to_worker` / `handle_delegate_parallel`
    /// wait inline for a worker's oneshot to fire before handing the receiver
    /// to the detached collector. Default `0` — pure async-first.
    /// Wired from `SpurConfig.delegation.inline_wait_ms`.
    inline_wait: std::time::Duration,
}

/// Validate args for `delegate_parallel` beyond what the schema shape
/// enforces. Currently: per-task `issue_id` values must be pairwise
/// unique across the batch when non-null. Public (crate-level) for
/// integration test access.
pub fn validate_parallel_args(args: &Value) -> Result<(), String> {
    let tasks = args
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'tasks' array".to_string())?;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (idx, task) in tasks.iter().enumerate() {
        if let Some(id) = task.get("issue_id").and_then(|v| v.as_str()) {
            if !seen.insert(id) {
                return Err(format!(
                    "delegate_parallel: issue_id values must be unique across tasks (duplicate '{id}' at index {idx})",
                ));
            }
        }
    }
    Ok(())
}

pub fn parse_delegation_plan(
    container: &Value,
) -> Result<Option<spur_acp::DelegationPlan>, String> {
    match container.get("delegation_plan") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| format!("invalid delegation_plan: {error}")),
    }
}

/// Parse the `tasks` array from a `delegate_parallel` args payload into
/// a list of partially-populated `DelegationRequest` skeletons. Public
/// (crate-level) so integration tests can exercise the parse logic
/// without a live MCP session.
///
/// The returned requests have dummy oneshot senders — do not dispatch
/// them; they are for field-value assertions only.
pub fn parse_parallel_tasks(
    args: &Value,
    brain_session_id: &spur_acp::BrainSessionId,
) -> Result<Vec<DelegationRequest>, String> {
    let tasks = args
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Missing 'tasks' array".to_string())?;
    let mut out = Vec::with_capacity(tasks.len());
    for task_obj in tasks {
        let agent = task_obj
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "task.agent missing".to_string())?
            .to_string();
        let task = task_obj
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "task.task missing".to_string())?
            .to_string();
        let context_files: Vec<String> = task_obj
            .get("context_files")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let issue_id = task_obj
            .get("issue_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let delegation_plan = parse_delegation_plan(task_obj)?;
        let (tx, _rx) = tokio::sync::oneshot::channel();
        out.push(DelegationRequest {
            id: uuid::Uuid::new_v4().to_string(),
            agent,
            task,
            context_files,
            respond_to: tx,
            brain_session_id: brain_session_id.clone(),
            delegation_plan,
            issue_id,
        });
    }
    Ok(out)
}

/// Result of building a beads epic subgraph for a persisted plan.
#[derive(Debug, Clone)]
pub struct EpicSubgraph {
    pub epic_id: String,
    /// Maps each `PlanTask.task_id` → beads child issue ID.
    pub task_map: std::collections::HashMap<String, String>,
}

/// Compose a beads epic + child issues + dependency edges from a
/// validated plan. Labels each child with `spur.plan_id=<plan_id>` so
/// review_task can correlate approvals back to beads.
///
/// Creates issues in topological order (deps-first) so each child's
/// `depends_on` references beads IDs that already exist. Callers must
/// ensure the plan is validated (no cycles) before invoking.
///
/// On failure mid-creation: partial state lands in beads (epic +
/// whatever children succeeded). Caller should surface the error and
/// leave cleanup to the brain / human. Transactional rollback is out
/// of scope for v1 — beads CLI doesn't expose txn primitives.
pub async fn build_epic_subgraph(
    pm: &spur_pm::PmService,
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<EpicSubgraph, String> {
    let (epic_create, child_specs) =
        plan_epic_issue_creates(plan_id, epic_title, epic_body, tasks)?;

    let epic_id = pm
        .create_issue(epic_create)
        .await
        .map_err(|e| format!("failed to create beads epic: {e}"))?;

    let mut task_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (task_id, mut child_create) in child_specs {
        // Rewrite `depends_on` from task_id keys → created beads IDs.
        child_create.depends_on = child_create
            .depends_on
            .iter()
            .map(|dep_key| {
                task_map.get(dep_key).cloned().ok_or_else(|| {
                    format!("task '{task_id}' depends on '{dep_key}' which was not yet created",)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        child_create.parent = Some(epic_id.clone());

        let child_id = pm
            .create_issue(child_create)
            .await
            .map_err(|e| format!("failed to create child for task '{task_id}': {e}"))?;
        task_map.insert(task_id, child_id);
    }

    Ok(EpicSubgraph { epic_id, task_map })
}

/// Pure helper: compute the IssueCreate values that build_epic_subgraph
/// would dispatch to PmService. Returns the epic's IssueCreate plus a
/// Vec of (task_id, IssueCreate) for each child in topological order.
/// Child IssueCreate.depends_on carries task_id keys, NOT beads IDs —
/// the caller rewrites them as children are created.
pub fn plan_epic_issue_creates(
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<
    (
        spur_pm::types::IssueCreate,
        Vec<(String, spur_pm::types::IssueCreate)>,
    ),
    String,
> {
    let epic_create = spur_pm::types::IssueCreate {
        title: epic_title.to_string(),
        description: epic_body.map(String::from),
        issue_type: Some("epic".to_string()),
        labels: vec![format!("spur.plan_id={}", plan_id)],
        ..Default::default()
    };

    let order = topological_order(tasks)?;
    let mut child_specs = Vec::with_capacity(tasks.len());
    for idx in order {
        let task = &tasks[idx];
        let mut labels = vec![
            format!("spur.plan_id={}", plan_id),
            format!("spur.plan_task_id={}", task.task_id),
            format!("spur.agent={}", task.agent),
        ];
        if let Some(existing) = &task.issue_id {
            labels.push(format!("spur.source_issue={}", existing));
        }
        let child_create = spur_pm::types::IssueCreate {
            title: format!("{}: {}", task.task_id, truncate_for_title(&task.task)),
            description: Some(task.task.clone()),
            issue_type: Some("task".to_string()),
            labels,
            // depends_on carries task_id keys; rewritten by build_epic_subgraph.
            depends_on: task.depends_on.clone(),
            // parent set by build_epic_subgraph once epic_id is known.
            parent: None,
            ..Default::default()
        };
        child_specs.push((task.task_id.clone(), child_create));
    }
    Ok((epic_create, child_specs))
}

/// Truncate a task description to a reasonable issue-title length.
/// Beads has no hard limit but overly long titles are unwieldy in UIs.
fn truncate_for_title(s: &str) -> String {
    const MAX_TITLE_LEN: usize = 80;
    let first_line = s.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= MAX_TITLE_LEN {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(MAX_TITLE_LEN - 3).collect();
        format!("{truncated}...")
    }
}

/// Return task indices in a valid topological order. Callers must have
/// already validated that the plan is acyclic via `plan::validate_plan`.
fn topological_order(tasks: &[crate::plan::PlanTask]) -> Result<Vec<usize>, String> {
    use std::collections::HashMap;
    let key_to_idx: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.as_str(), i))
        .collect();

    let mut in_degree: Vec<usize> = tasks.iter().map(|t| t.depends_on.len()).collect();
    let mut ready: std::collections::VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| if d == 0 { Some(i) } else { None })
        .collect();

    let mut out = Vec::with_capacity(tasks.len());
    while let Some(i) = ready.pop_front() {
        out.push(i);
        for (j, t) in tasks.iter().enumerate() {
            if t.depends_on
                .iter()
                .any(|dep| key_to_idx.get(dep.as_str()).copied() == Some(i))
            {
                in_degree[j] -= 1;
                if in_degree[j] == 0 {
                    ready.push_back(j);
                }
            }
        }
    }

    if out.len() != tasks.len() {
        return Err(format!(
            "topological order incomplete: {} of {} tasks reachable (cycle?)",
            out.len(),
            tasks.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod topo_tests {
    use super::topological_order;
    use crate::plan::PlanTask;

    fn t(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            task_id: id.to_string(),
            agent: "x".to_string(),
            task: "body".to_string(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            context_files: Vec::new(),
        }
    }

    #[test]
    fn linear_chain_is_ordered() {
        let tasks = vec![t("a", &[]), t("b", &["a"]), t("c", &["b"])];
        let order = topological_order(&tasks).unwrap();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn diamond_respects_all_parents() {
        // a → b, a → c, b+c → d
        let tasks = vec![
            t("a", &[]),
            t("b", &["a"]),
            t("c", &["a"]),
            t("d", &["b", "c"]),
        ];
        let order = topological_order(&tasks).unwrap();
        let pos_a = order.iter().position(|&i| i == 0).unwrap();
        let pos_b = order.iter().position(|&i| i == 1).unwrap();
        let pos_c = order.iter().position(|&i| i == 2).unwrap();
        let pos_d = order.iter().position(|&i| i == 3).unwrap();
        assert!(pos_a < pos_b && pos_a < pos_c);
        assert!(pos_b < pos_d && pos_c < pos_d);
    }

    #[test]
    fn cycle_is_detected() {
        let tasks = vec![t("a", &["b"]), t("b", &["a"])];
        let err = topological_order(&tasks).unwrap_err();
        assert!(err.contains("incomplete") || err.contains("cycle"));
    }
}

impl McpCallbackServer {
    /// Create a new MCP callback server for the given session.
    ///
    /// Returns the server instance and a `DelegationChannel` that the
    /// orchestrator uses to receive requests and send responses.
    pub fn new(
        session_id: &spur_acp::BrainSessionId,
        pm_service: Option<Arc<PmService>>,
        event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
        continuation_ctx: DetachedContinuationCtx,
    ) -> (Self, DelegationChannel) {
        let (req_tx, req_rx) = mpsc::channel::<DelegationRequest>(32);

        let server = Self {
            delegation_tx: req_tx,
            workers: Vec::new(),
            brain_session_id: session_id.clone(),
            active_delegations: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            completed_delegations: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            task_tracker: TaskTracker::new(),
            pm_service,
            event_sink,
            active_plans: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            plan_registry: Arc::new(tokio::sync::Mutex::new(crate::plan::PlanRegistry::default())),
            cancellation_control: None,
            continuation_ctx: Arc::new(continuation_ctx),
            inline_wait: std::time::Duration::from_millis(0),
        };

        let channel = DelegationChannel { request_rx: req_rx };
        (server, channel)
    }

    /// INV-6: Wire the orchestrator's `CancellationControl` handle into this
    /// server so `handle_cancel_delegation` can cancel active delegations
    /// directly rather than routing through the delegation channel.
    pub fn set_cancellation_control(&mut self, cc: CancellationControl) {
        self.cancellation_control = Some(cc);
    }

    /// Phase 1c: set how long `handle_delegate_to_worker` /
    /// `handle_delegate_parallel` wait inline for the worker oneshot before
    /// falling through to the detached collector. Default is `0`
    /// (async-first); orchestrator wires this from
    /// `SpurConfig.delegation.inline_wait_ms` at server construction.
    pub fn set_inline_wait(&mut self, d: std::time::Duration) {
        self.inline_wait = d;
    }

    /// Spawn a background task that awaits a delegation oneshot and stores
    /// the result in `completed_delegations` for later polling.
    ///
    /// When `detached` is `Some`, the task additionally calls
    /// `report_detached_completion` to route the result back into the
    /// orchestrator ingress (INV-C3 ordering: UI event BEFORE ingress).
    ///
    /// Exposed for integration tests in sibling crates. Not part of the
    /// stable public API — `#[doc(hidden)]` keeps it out of rustdoc.
    #[doc(hidden)]
    pub fn spawn_result_collector(
        tracker: &TaskTracker,
        delegation_id: String,
        rx: tokio::sync::oneshot::Receiver<DelegationResult>,
        active: Arc<tokio::sync::Mutex<HashSet<String>>>,
        completed: Arc<
            tokio::sync::Mutex<HashMap<String, (DelegationResult, tokio::time::Instant)>>,
        >,
        detached: Option<DetachedCompletionHandle>,
    ) {
        tracker.spawn(async move {
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
                    worker_branch: None,
                    artifact: None,
                },
            };
            active.lock().await.remove(&delegation_id);

            // INV-ASYNC-2 (source_kind-gated): the continuation bridge is the
            // SOLE delivery channel for `BlockTimeout` (the new async-first
            // path — `delegate_to_worker` auto-reprompt). Writing to the
            // map on that path would let `check_delegation_status` redeliver
            // what the brain already received as a continuation turn — the
            // double-delivery failure mode closed by INV-ASYNC-1.
            //
            // For `AsyncRequested` (legacy `delegate_async` + `wait_delegation`
            // flow) the map write is PRESERVED: `handle_wait_delegation`
            // (server.rs:2282+) reads from this map to return results to
            // callers that still use the deprecated polling API. Phase 3
            // removes `wait_delegation`; at that point this arm can drop
            // too. Any pre-existing double-delivery on the AsyncRequested
            // path (bridge + wait_delegation for the same id) is unchanged
            // from pre-Phase-1a behavior and is not introduced here.
            //
            // `detached = None` is also retained as a fallback for unit
            // tests exercising the collector directly.
            let keep_map_entry = match &detached {
                None => true,
                Some(h) => matches!(h.source_kind, DetachedSourceKind::AsyncRequested),
            };
            if keep_map_entry {
                completed.lock().await.insert(
                    delegation_id.clone(),
                    (result.clone(), tokio::time::Instant::now()),
                );
            }

            if let Some(h) = detached {
                use spur_acp::domain::{
                    BrainContinuation, ContinuationPayload, ContinuationSource,
                };

                let source = if matches!(result.status, DelegationStatus::Cancelled { .. }) {
                    ContinuationSource::Cancelled
                } else {
                    match h.source_kind {
                        DetachedSourceKind::AsyncRequested => ContinuationSource::AsyncRequested,
                        DetachedSourceKind::BlockTimeout => ContinuationSource::BlockTimeout,
                    }
                };

                let cont = BrainContinuation {
                    delegation_id: delegation_id.clone(),
                    source,
                    payload: ContinuationPayload {
                        status: result.status.clone(),
                        summary: result.summary.clone(),
                        diff_summary: result.diff_summary.clone(),
                        worker_branch: result.worker_branch.clone(),
                        artifact: result.artifact.clone(),
                    },
                    created_at: std::time::Instant::now(),
                };
                // Route the completion back to the orchestrator ingress via
                // the injected callback (wired in spur-core to avoid a
                // circular dependency). The delegation_id is used as a
                // worker_session proxy for the DelegationCompleted UI event.
                (h.ctx.on_complete)(cont, delegation_id.clone()).await;
            }
        });
    }

    /// Set the list of available worker agents.
    pub fn set_workers(&mut self, workers: Vec<WorkerInfo>) {
        self.workers = workers;
    }

    /// Gracefully shut down the server: close the task tracker and wait
    /// for all in-flight result collectors to finish.
    pub async fn shutdown(&self) {
        self.task_tracker.close();
        self.task_tracker.wait().await;
    }

    /// Test-only: invoke the `delegate_to_worker` JSON-RPC handler directly.
    ///
    /// Exposed solely so integration tests in sibling crates (e.g.
    /// `spur-core/tests/continuation_integration.rs`) can exercise the
    /// block-timeout / detached-completion paths without standing up the full
    /// HTTP stack. Returns the raw JSON-RPC response as a `serde_json::Value`.
    #[doc(hidden)]
    pub async fn __test_call_delegate_to_worker(&self, agent: &str, task: &str) -> Value {
        let resp = self
            .handle_delegate_to_worker(Value::from(1), json!({ "agent": agent, "task": task }))
            .await;
        serde_json::to_value(&resp).expect("serialize JsonRpcResponse")
    }

    /// Test-only: peek whether a result is sitting in `completed_delegations`
    /// awaiting a `check_delegation_status` poll. Used to detect the
    /// double-delivery failure mode (map write AND continuation callback both
    /// firing for the same delegation).
    #[doc(hidden)]
    pub async fn __test_completed_has(&self, delegation_id: &str) -> bool {
        self.completed_delegations
            .lock()
            .await
            .contains_key(delegation_id)
    }

    /// Remove completed delegation results older than `COMPLETED_TTL`.
    /// Called lazily from polling handlers to bound memory growth.
    async fn evict_stale_completions(&self) {
        self.completed_delegations
            .lock()
            .await
            .retain(|_, (_, ts)| ts.elapsed() < COMPLETED_TTL);
    }

    /// Start listening on a random localhost port.
    ///
    /// Returns the MCP endpoint URL (e.g. `http://127.0.0.1:12345/mcp`) and
    /// a `JoinHandle`.
    pub async fn start(self: Arc<Self>) -> Result<(String, JoinHandle<()>)> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("Failed to bind TCP listener")?;

        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/mcp");
        let mut config = StreamableHttpServerConfig::default();
        config.stateful_mode = true;
        let session_manager = Arc::new(LocalSessionManager::default());
        let service = {
            let server = Arc::clone(&self);
            StreamableHttpService::new(move || Ok(Arc::clone(&server)), session_manager, config)
        };
        let router = Router::new().nest_service("/mcp", service);

        info!(url = %url, "MCP callback server listening (streamable HTTP)");

        let handle = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                debug!(%error, "RMCP callback server exited");
            }
        });

        Ok((url, handle))
    }

    fn rmcp_tools(&self) -> Vec<Tool> {
        tools::tools_list()
            .into_iter()
            .map(|def| Tool::new(def.name, def.description, rmcp_object(def.input_schema)))
            .collect()
    }

    fn rmcp_tool(&self, name: &str) -> Option<Tool> {
        self.rmcp_tools().into_iter().find(|tool| tool.name == name)
    }

    fn call_tool_result_from_legacy_response(
        response: JsonRpcResponse,
        tool_name: &str,
    ) -> Result<CallToolResult, McpError> {
        match (response.result, response.error) {
            (Some(result), None) => serde_json::from_value(result).map_err(|error| {
                McpError::internal_error(
                    format!("failed to serialize tool result for {tool_name}: {error}"),
                    None,
                )
            }),
            (None, Some(error)) => Err(error.into_mcp_error()),
            (Some(_), Some(_)) | (None, None) => Err(McpError::internal_error(
                format!("tool handler returned an invalid response envelope for {tool_name}"),
                None,
            )),
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
            "check_delegation_status" => self.handle_check_delegation_status(id, arguments).await,
            "cancel_delegation" => self.handle_cancel_delegation(id, arguments).await,
            "list_available_workers" => self.handle_list_available_workers(id).await,
            "get_issue" => self.handle_get_issue(id, arguments).await,
            "list_issues" => self.handle_list_issues(id, arguments).await,
            "update_issue" => self.handle_update_issue(id, arguments).await,
            "create_issue" => self.handle_create_issue(id, arguments).await,
            "add_dependency" => self.handle_add_dependency(id, arguments).await,
            "create_pr" => self.handle_create_pr(id, arguments).await,
            "graph_triage" => self.handle_graph_triage(id, arguments).await,
            "graph_plan" => self.handle_graph_plan(id, arguments).await,
            "graph_insights" => self.handle_graph_insights(id, arguments).await,
            "graph_alerts" => self.handle_graph_alerts(id, arguments).await,
            "graph_subgraph" => self.handle_graph_subgraph(id, arguments).await,
            "submit_plan" => self.handle_submit_plan(id, arguments).await,
            "execute_epic" => self.handle_execute_epic(id, arguments).await,
            "get_plan_status" => self.handle_get_plan_status(id, arguments).await,
            "get_task_diff" => match self.handle_get_task_diff(&arguments).await {
                Ok(text) => JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                ),
                Err(e) => JsonRpcResponse::internal_error(id, e),
            },
            "review_task" => match self.handle_review_task(&arguments).await {
                Ok(text) => JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                ),
                Err(e) => JsonRpcResponse::internal_error(id, e),
            },
            _ => JsonRpcResponse::error(id, -32601, format!("Unknown tool: {tool_name}")),
        }
    }

    // ─── Tool handlers ────────────────────────────────────────────────

    async fn handle_delegate_to_worker(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bad_params = |message| JsonRpcResponse::invalid_params(id.clone(), message);
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
        let delegation_plan = match parse_delegation_plan(&args).map_err(bad_params) {
            Ok(plan) => plan,
            Err(response) => return response,
        };
        let issue_id = args
            .get("issue_id")
            .and_then(|v| v.as_str())
            .map(String::from);

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

        self.active_delegations
            .lock()
            .await
            .insert(request_id.clone());

        // Phase 1c (INV-ASYNC-1/2/3/7): biased select! over
        //   (fast arm: &mut rx), (slow arm: sleep(inline_wait))
        // Fast arm → return inline completion, drain active_delegations, no
        // collector spawn, no map write.
        // Slow arm → hand the receiver to `spawn_result_collector` with a
        // BlockTimeout continuation handle; the bridge is the sole delivery
        // channel (collector skips the map write when `detached` is Some).
        //
        // Cancel-during-handoff (Risk R2): the select! arm atomically either
        // consumes the oneshot result (fast path) or hands off the receiver
        // to the collector (slow path) — never both. `handle_cancel_delegation`
        // routes through `CancellationControl`, which signals the orchestrator
        // rather than touching our oneshot, so a cancel arriving between the
        // inline-window tick and the collector spawn races against the
        // orchestrator's own cancellation drain, not against this handler.
        //
        // INV-ASYNC-7: no mutex guards are held across any `.await` point
        // inside the arms below — `active_delegations.lock()` is scoped to a
        // single `.remove()` call in the fast arm.
        let mut rx = rx;
        let inline_wait = self.inline_wait;
        tokio::select! {
            biased;
            res = &mut rx => {
                let result = match res {
                    Ok(r) => r,
                    Err(_) => DelegationResult {
                        status: DelegationStatus::Failed {
                            error: "Orchestrator disconnected".into(),
                        },
                        diff: None,
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                        artifact: None,
                    },
                };
                self.active_delegations
                    .lock()
                    .await
                    .remove(&request_id);
                let result_json = match serde_json::to_value(&result) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("Failed to serialize result: {e}"),
                        )
                    }
                };
                let payload = json!({
                    "status": "completed",
                    "delegation_id": request_id,
                    "result": result_json,
                    "continuation_will_fire": false,
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                // Response shape (spec §8.3): stringified JSON is the source
                // of truth; a one-line human-readable shadow is prefixed for
                // legacy pattern-matchers.
                let shadow = format!(
                    "Delegation to '{agent}' completed inline (delegation_id={request_id}).",
                );
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("{shadow}\n{payload_text}")
                        }]
                    }),
                )
            }
            _ = tokio::time::sleep(inline_wait) => {
                info!(
                    agent = %agent,
                    request_id = %request_id,
                    inline_wait_ms = inline_wait.as_millis() as u64,
                    "Delegation inline window expired — detaching via continuation bridge"
                );
                Self::spawn_result_collector(
                    &self.task_tracker,
                    request_id.clone(),
                    rx,
                    Arc::clone(&self.active_delegations),
                    Arc::clone(&self.completed_delegations),
                    Some(DetachedCompletionHandle {
                        ctx: Arc::clone(&self.continuation_ctx),
                        source_kind: DetachedSourceKind::BlockTimeout,
                    }),
                );
                let payload = json!({
                    "status": "pending",
                    "delegation_id": request_id,
                    "continuation_will_fire": true,
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                let shadow = format!(
                    "Delegation to '{agent}' is running in the background (delegation_id={request_id}); \
                     a continuation will fire when the worker completes.",
                );
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("{shadow}\n{payload_text}")
                        }]
                    }),
                )
            }
        }
    }

    async fn handle_delegate_parallel(&self, id: Value, args: Value) -> JsonRpcResponse {
        if let Some(batch_plan) = args.get("delegation_plan") {
            tracing::info!(
                batch_plan = %batch_plan,
                "delegate_parallel received batch-level delegation_plan (not propagated into per-task requests)",
            );
        }

        if let Err(e) = validate_parallel_args(&args) {
            return JsonRpcResponse::invalid_params(id, e);
        }

        let skeletons = match parse_parallel_tasks(&args, &self.brain_session_id) {
            Ok(s) => s,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        // Per-task dispatch, mirroring the single-worker handler. Each task
        // gets its own biased select! (fast arm: `&mut rx`, slow arm:
        // `sleep(inline_wait)`). This preserves INV-ASYNC-6 (the batch
        // response is a `Value::Array` of length N with mixed completed /
        // pending entries; detached tasks funnel through the continuation
        // bridge, never through `completed_delegations`).
        let inline_wait = self.inline_wait;
        let mut results: Vec<Value> = Vec::with_capacity(skeletons.len());

        for mut skeleton in skeletons {
            let request_id = skeleton.id.clone();
            let agent = skeleton.agent.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            skeleton.respond_to = tx;

            info!(agent = %agent, request_id = %request_id, "Sending parallel delegation request");

            if let Err(_e) = self.delegation_tx.send(skeleton).await {
                error!("Failed to send parallel delegation request");
                return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
            }

            self.active_delegations
                .lock()
                .await
                .insert(request_id.clone());

            let mut rx = rx;
            // Cancel-during-handoff (Risk R2): see `handle_delegate_to_worker`
            // — the select! arm atomically either consumes the result or
            // hands off the receiver; it does not do both.
            let entry: Value = tokio::select! {
                biased;
                res = &mut rx => {
                    let result = match res {
                        Ok(r) => r,
                        Err(_) => DelegationResult {
                            status: DelegationStatus::Failed {
                                error: "Orchestrator disconnected".into(),
                            },
                            diff: None,
                            diff_summary: None,
                            summary: None,
                            estimated_cost_usd: 0.0,
                            worker_branch: None,
                            artifact: None,
                        },
                    };
                    self.active_delegations
                        .lock()
                        .await
                        .remove(&request_id);
                    let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
                    json!({
                        "status": "completed",
                        "delegation_id": request_id,
                        "agent": agent,
                        "result": result_json,
                        "continuation_will_fire": false,
                    })
                }
                _ = tokio::time::sleep(inline_wait) => {
                    Self::spawn_result_collector(
                        &self.task_tracker,
                        request_id.clone(),
                        rx,
                        Arc::clone(&self.active_delegations),
                        Arc::clone(&self.completed_delegations),
                        Some(DetachedCompletionHandle {
                            ctx: Arc::clone(&self.continuation_ctx),
                            source_kind: DetachedSourceKind::BlockTimeout,
                        }),
                    );
                    json!({
                        "status": "pending",
                        "delegation_id": request_id,
                        "agent": agent,
                        "continuation_will_fire": true,
                    })
                }
            };
            results.push(entry);
        }

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&Value::Array(results.clone()))
                        .unwrap_or_else(|_| Value::Array(results).to_string())
                }]
            }),
        )
    }

    async fn handle_check_delegation_status(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        self.evict_stale_completions().await;

        // Completed — return and remove.
        let completed = {
            let mut map = self.completed_delegations.lock().await;
            map.retain(|_, (_, ts)| ts.elapsed() < COMPLETED_TTL);
            map.remove(&delegation_id).map(|(r, _)| r)
        };
        if let Some(result) = completed {
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
        if self
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
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

        JsonRpcResponse::error(id, -32602, format!("Unknown delegation: {delegation_id}"))
    }

    async fn handle_cancel_delegation(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        // Already completed — return the result directly.
        if let Some((result, _ts)) = self
            .completed_delegations
            .lock()
            .await
            .remove(&delegation_id)
        {
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

        // Active — use the CancellationControl side-channel (INV-6).
        if self
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
            if let Some(ref cc) = self.cancellation_control {
                let outcome = cc.cancel(&delegation_id).await;
                info!(delegation_id = %delegation_id, ?outcome, "Cancellation requested via CancellationControl");
                match outcome {
                    CancelOutcome::Cancelled => {
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("Delegation {} cancelled", delegation_id)
                                }]
                            }),
                        );
                    }
                    CancelOutcome::NotFound => {
                        // Token was already removed (delegation completed between
                        // the active_delegations check and the cancel call).
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("Delegation {} already completed", delegation_id)
                                }]
                            }),
                        );
                    }
                }
            } else {
                return JsonRpcResponse::internal_error(
                    id,
                    "cancel_delegation: no cancellation control wired",
                );
            }
        }

        JsonRpcResponse::error(id, -32602, format!("Unknown delegation: {delegation_id}"))
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
                let text =
                    serde_json::to_string_pretty(&issue).unwrap_or_else(|_| format!("{issue:?}"));
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
            status: args
                .get("status")
                .and_then(|v| v.as_str())
                .map(String::from),
            assignee: args
                .get("assignee")
                .and_then(|v| v.as_str())
                .map(String::from),
            priority_min: args
                .get("priority_min")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            priority_max: args
                .get("priority_max")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            issue_type: args
                .get("issue_type")
                .and_then(|v| v.as_str())
                .map(String::from),
            text_search: args
                .get("text_search")
                .and_then(|v| v.as_str())
                .map(String::from),
            limit: Some(
                args.get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .min(100) as usize,
            ),
            labels,
            since: None,
        };

        match pm.list_issues(filter).await {
            Ok(issues) => {
                let text =
                    serde_json::to_string_pretty(&issues).unwrap_or_else(|_| format!("{issues:?}"));
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
            status: args
                .get("status")
                .and_then(|v| v.as_str())
                .map(String::from),
            comment: args
                .get("comment")
                .and_then(|v| v.as_str())
                .map(String::from),
            priority: args
                .get("priority")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            assignee: args
                .get("assignee")
                .and_then(|v| v.as_str())
                .map(String::from),
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

    async fn handle_create_issue(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let title = match args.get("title").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'title'"),
        };

        let labels: Vec<String> = args
            .get("labels")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let depends_on: Vec<String> = args
            .get("depends_on")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let params = spur_pm::IssueCreate {
            title,
            description: args
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            issue_type: args.get("type").and_then(|v| v.as_str()).map(String::from),
            priority: args
                .get("priority")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32),
            labels,
            parent: args
                .get("parent")
                .and_then(|v| v.as_str())
                .map(String::from),
            assignee: args
                .get("assignee")
                .and_then(|v| v.as_str())
                .map(String::from),
            estimate_minutes: args
                .get("estimate")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
            depends_on,
        };

        match pm.create_issue(params).await {
            Ok(issue_id) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Issue created: {issue_id}")
                    }]
                }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("create_issue failed: {e}")),
        }
    }

    async fn handle_add_dependency(&self, id: Value, args: Value) -> JsonRpcResponse {
        let pm = match self.pm_service.as_ref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "No issue tracker configured"),
        };
        let issue_id = match args.get("issue_id").and_then(|v| v.as_str()) {
            Some(i) => i.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(id, "Missing required field 'issue_id'")
            }
        };
        let depends_on_id = match args.get("depends_on_id").and_then(|v| v.as_str()) {
            Some(d) => d.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'depends_on_id'",
                )
            }
        };

        match pm.add_dependency(&issue_id, &depends_on_id).await {
            Ok(()) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Dependency added: {issue_id} depends on {depends_on_id}")
                    }]
                }),
            ),
            Err(e) => JsonRpcResponse::internal_error(id, format!("add_dependency failed: {e}")),
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
            base_branch: args
                .get("base_branch")
                .and_then(|v| v.as_str())
                .map(String::from),
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

    // ─── Graph analysis handlers (bv robot protocol) ───────────────

    /// Helper: get the bv analyzer or return an MCP error.
    #[allow(clippy::result_large_err)]
    fn require_analyzer(&self, id: &Value) -> Result<&spur_pm::BvAdapter, JsonRpcResponse> {
        let pm = self.pm_service.as_ref().ok_or_else(|| {
            JsonRpcResponse::internal_error(id.clone(), "No PM service configured")
        })?;
        pm.analyzer().ok_or_else(|| {
            JsonRpcResponse::internal_error(
                id.clone(),
                "Graph analysis not available. Install bv: brew install dicklesworthstone/tap/bv",
            )
        })
    }

    async fn handle_graph_triage(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let label = args.get("label").and_then(|v| v.as_str());
        match bv.triage(label).await {
            Ok(report) => {
                let text = serde_json::to_string_pretty(&report.raw)
                    .unwrap_or_else(|_| report.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_triage failed: {e}")),
        }
    }

    async fn handle_graph_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let label = args.get("label").and_then(|v| v.as_str());
        match bv.plan(label).await {
            Ok(plan) => {
                let text = serde_json::to_string_pretty(&plan.raw)
                    .unwrap_or_else(|_| plan.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_plan failed: {e}")),
        }
    }

    async fn handle_graph_insights(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let label = args.get("label").and_then(|v| v.as_str());
        match bv.insights(label).await {
            Ok(insights) => {
                let text = serde_json::to_string_pretty(&insights.raw)
                    .unwrap_or_else(|_| insights.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_insights failed: {e}")),
        }
    }

    async fn handle_graph_alerts(&self, id: Value, _args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        match bv.alerts().await {
            Ok(report) => {
                let text = serde_json::to_string_pretty(&report.raw)
                    .unwrap_or_else(|_| report.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_alerts failed: {e}")),
        }
    }

    async fn handle_graph_subgraph(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bv = match self.require_analyzer(&id) {
            Ok(bv) => bv,
            Err(resp) => return resp,
        };
        let root_id = match args.get("root_id").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'root_id'"),
        };
        let depth = args.get("depth").and_then(|v| v.as_u64()).map(|d| d as u32);
        let format = args.get("format").and_then(|v| v.as_str());
        match bv.subgraph(root_id, depth, format).await {
            Ok(graph) => {
                let text = serde_json::to_string_pretty(&graph.raw)
                    .unwrap_or_else(|_| graph.raw.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => JsonRpcResponse::internal_error(id, format!("graph_subgraph failed: {e}")),
        }
    }

    // ─── Plan execution handlers ──────────────────────────────────

    async fn handle_submit_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let tasks_val = match args.get("tasks").and_then(|v| v.as_array()) {
            Some(t) => t.clone(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'tasks'"),
        };

        let tasks: Vec<crate::plan::PlanTask> = match tasks_val
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(t) => t,
            Err(e) => {
                return JsonRpcResponse::invalid_params(id, format!("Invalid task format: {e}"))
            }
        };

        if let Err(e) = crate::plan::validate_plan(&tasks) {
            return JsonRpcResponse::invalid_params(id, e);
        }

        // ─── Persist-as-epic extraction (T2.1) ─────────────────────────
        let persist_as_epic = args
            .get("persist_as_epic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let epic_title = args
            .get("epic_title")
            .and_then(|v| v.as_str())
            .map(String::from);
        let epic_body = args
            .get("epic_body")
            .and_then(|v| v.as_str())
            .map(String::from);

        if persist_as_epic {
            if epic_title
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
            {
                return JsonRpcResponse::invalid_params(
                    id,
                    "submit_plan: epic_title is required when persist_as_epic is true",
                );
            }
            let pm_source = self.pm_service.as_deref().map(|p| p.source_str());
            if pm_source != Some("beads") {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!(
                        "submit_plan: persist_as_epic requires a beads PM backend (configured backend: {})",
                        pm_source.unwrap_or("none"),
                    ),
                );
            }
        }
        let plan_id = uuid::Uuid::new_v4().to_string();

        // Build the beads epic subgraph before spawning the executor so
        // any creation error is surfaced synchronously.
        let epic_subgraph: Option<EpicSubgraph> = if persist_as_epic {
            let pm = self
                .pm_service
                .as_deref()
                .expect("gate ensures pm is beads");
            let title = epic_title.as_deref().expect("gate ensures non-empty title");
            match build_epic_subgraph(pm, &plan_id, title, epic_body.as_deref(), &tasks).await {
                Ok(sg) => {
                    info!(
                        plan_id = %plan_id,
                        epic_id = %sg.epic_id,
                        children = sg.task_map.len(),
                        "submit_plan: beads epic subgraph created"
                    );
                    Some(sg)
                }
                Err(e) => {
                    error!(plan_id = %plan_id, "build_epic_subgraph failed: {e}");
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!("submit_plan: failed to persist plan as beads epic: {e}"),
                    );
                }
            }
        } else {
            None
        };

        let entries: Vec<crate::plan::PlanTaskEntry> = tasks
            .into_iter()
            .map(|spec| crate::plan::PlanTaskEntry {
                spec,
                status: crate::plan::PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
            })
            .collect();

        let task_count = entries.len();
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id.clone(),
            epic_id: epic_subgraph.as_ref().map(|sg| sg.epic_id.clone()),
        };
        let state = Arc::new(tokio::sync::Mutex::new(state));

        self.active_plans
            .lock()
            .await
            .insert(plan_id.clone(), Arc::clone(&state));

        // Spawn the plan executor.
        let delegation_tx = self.delegation_tx.clone();
        let plan_sink = self.event_sink.clone();
        self.task_tracker
            .spawn(crate::plan::run_plan(state, delegation_tx, plan_sink));

        info!(plan_id = %plan_id, tasks = task_count, "Plan submitted");

        let response_text = if let Some(sg) = &epic_subgraph {
            let task_map_json =
                serde_json::to_string(&sg.task_map).unwrap_or_else(|_| "{}".to_string());
            format!(
                "Plan submitted: {task_count} tasks.\n\
                 plan_id: {plan_id}\n\
                 epic_id: {epic_id} (beads)\n\
                 task_map: {task_map_json}\n\
                 Poll with get_plan_status to monitor progress.",
                epic_id = sg.epic_id,
            )
        } else {
            format!(
                "Plan submitted: {task_count} tasks. plan_id: {plan_id}\n\
                 Poll with get_plan_status to monitor progress."
            )
        };

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": response_text
                }]
            }),
        )
    }

    async fn handle_execute_epic(&self, id: Value, args: Value) -> JsonRpcResponse {
        // 1. Extract required epic_id.
        let epic_id = match args.get("epic_id").and_then(|v| v.as_str()) {
            Some(e) => e.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "missing required field: epic_id"),
        };
        let default_agent = args
            .get("default_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // 2. Require PmService.
        // Unit-tested via integration-level fixtures only; the PmService gate is
        // the first check in handle_execute_epic and its error message matches
        // this literal: "beads (PmService) is not configured — cannot execute epic".
        let pm = match self.pm_service.as_deref() {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    "beads (PmService) is not configured — cannot execute epic",
                )
            }
        };

        // Sentinel value used to reserve a registry slot while the PmService
        // fetch is in flight. Concurrent callers that see this value return an
        // "already in progress" error instead of racing into double-dispatch.
        const PENDING_SENTINEL: &str = "__pending__";

        // 3. Idempotency + reservation: under a single lock acquisition,
        //    either return the existing non-terminal plan, reserve the slot
        //    with a sentinel (and fall through to the fetch), or clear a
        //    stale/terminal entry and reserve.
        {
            let mut registry = self.plan_registry.lock().await;
            match registry.by_epic.get(&epic_id).cloned() {
                Some(ref existing) if existing == PENDING_SENTINEL => {
                    // A concurrent call is already in the fetch/derive phase.
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!(
                            "execute_epic for epic '{epic_id}' is already in progress — \
                             wait for it to complete and call get_plan_status"
                        ),
                    );
                }
                Some(existing_plan_id) => {
                    // Check if the existing plan is still non-terminal.
                    // Release the registry lock before acquiring active_plans
                    // to maintain consistent lock ordering (active_plans is
                    // never acquired while holding plan_registry).
                    drop(registry);
                    let plan_arc = {
                        let plans = self.active_plans.lock().await;
                        plans.get(&existing_plan_id).cloned()
                    };
                    if let Some(arc) = plan_arc {
                        let state = arc.lock().await;
                        let status_val = crate::plan::build_plan_status(&existing_plan_id, &state);
                        let overall = status_val
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        if !crate::plan::is_terminal_plan_status(overall) {
                            // Return existing plan status.
                            let mut resp_val = status_val;
                            if let serde_json::Value::Object(ref mut m) = resp_val {
                                m.insert("epic_id".into(), serde_json::json!(epic_id));
                                m.insert(
                                    "next_action".into(),
                                    serde_json::json!(
                                        "Plan already active for this epic. \
                                         Poll with get_plan_status(plan_id) to monitor progress."
                                    ),
                                );
                            }
                            let text = serde_json::to_string_pretty(&resp_val)
                                .unwrap_or_else(|_| resp_val.to_string());
                            return JsonRpcResponse::success(
                                id,
                                json!({ "content": [{ "type": "text", "text": text }] }),
                            );
                        }
                        // Terminal plan — fall through to start a fresh one.
                        // Re-acquire the registry lock to insert the sentinel.
                    }
                    // Plan not found in active_plans (evicted or never inserted)
                    // or was terminal — reserve the slot now.
                    self.plan_registry
                        .lock()
                        .await
                        .by_epic
                        .insert(epic_id.clone(), PENDING_SENTINEL.into());
                }
                None => {
                    // No entry at all — reserve the slot.
                    registry
                        .by_epic
                        .insert(epic_id.clone(), PENDING_SENTINEL.into());
                }
            }
        }

        // 4. Derive the plan from the epic subgraph via PmService.
        let known_agent_names: Vec<String> = self.workers.iter().map(|w| w.name.clone()).collect();
        let known_agents_refs: Vec<&str> = known_agent_names.iter().map(String::as_str).collect();

        let derived = match crate::plan::derive_epic_plan(
            pm,
            &epic_id,
            default_agent.as_deref(),
            &known_agents_refs,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                // Clear the sentinel so callers can retry after fixing the issue.
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::error(id, -32000, e);
            }
        };

        // 5. Build PlanState and spawn the plan — mirrors handle_submit_plan exactly.
        let plan_id = uuid::Uuid::new_v4().to_string();
        let entries: Vec<crate::plan::PlanTaskEntry> = derived
            .plan_tasks
            .into_iter()
            .map(|spec| crate::plan::PlanTaskEntry {
                spec,
                status: crate::plan::PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
            })
            .collect();

        let task_count = entries.len();
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id.clone(),
            epic_id: None, // TODO follow-up: set to Some(epic_id_arg.clone()) so review_task can correlate
        };
        let state = Arc::new(tokio::sync::Mutex::new(state));

        // Keep a clone of the Arc to build the initial status response.
        let state_for_status = Arc::clone(&state);

        // Insert into active_plans first (no registry lock held here).
        self.active_plans
            .lock()
            .await
            .insert(plan_id.clone(), Arc::clone(&state));

        // Replace the sentinel with the real plan_id now that dispatch is
        // committed. active_plans lock is already released above, so these
        // two locks are never held simultaneously.
        self.plan_registry
            .lock()
            .await
            .by_epic
            .insert(epic_id.clone(), plan_id.clone());

        // Spawn the plan executor.
        // tokio_util 0.7 TaskTracker::spawn returns JoinHandle directly (not
        // Option), but it will panic if the underlying Tokio runtime has shut
        // down. Guard with is_closed() so a shutting-down orchestrator rolls
        // back instead of leaving a zombie plan in active_plans + registry.
        if self.task_tracker.is_closed() {
            // Roll back: remove the active_plans entry we just inserted.
            {
                let mut plans = self.active_plans.lock().await;
                plans.remove(&plan_id);
            }
            // Roll back: remove the registry entry (real plan_id, not sentinel).
            {
                let mut reg = self.plan_registry.lock().await;
                reg.by_epic.remove(&epic_id);
            }
            return JsonRpcResponse::error(
                id,
                -32000,
                "orchestrator shutting down — execute_epic aborted",
            );
        }
        let delegation_tx = self.delegation_tx.clone();
        let plan_sink = self.event_sink.clone();
        self.task_tracker
            .spawn(crate::plan::run_plan(state, delegation_tx, plan_sink));

        info!(
            plan_id = %plan_id,
            epic_id = %epic_id,
            tasks = task_count,
            "Epic plan submitted"
        );

        // 6. Build response: plan status + epic metadata.
        let status_val = {
            let st = state_for_status.lock().await;
            crate::plan::build_plan_status(&plan_id, &st)
        };

        let derived_info = json!({
            "task_count": task_count,
            "edge_count": derived.edge_count,
            "agents": derived.agent_counts,
            "warnings": derived.warnings,
        });

        let mut resp_val = status_val;
        if let serde_json::Value::Object(ref mut m) = resp_val {
            m.insert("epic_id".into(), serde_json::json!(epic_id));
            m.insert("derived".into(), derived_info);
        }

        let text = serde_json::to_string_pretty(&resp_val).unwrap_or_else(|_| resp_val.to_string());

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    async fn handle_get_plan_status(&self, id: Value, args: Value) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'plan_id'"),
        };

        // Clone the Arc before releasing the outer lock so we don't hold
        // active_plans while awaiting the inner plan lock — prevents
        // blocking concurrent submit_plan calls.
        let plan_arc = {
            let plans = self.active_plans.lock().await;
            plans.get(&plan_id).cloned()
        };

        let plan_state = match plan_arc {
            Some(s) => s,
            None => {
                return JsonRpcResponse::invalid_params(id, format!("Unknown plan_id: '{plan_id}'"))
            }
        };

        let state = plan_state.lock().await;
        let status = crate::plan::build_plan_status(&plan_id, &state);
        let text = serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string());

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    async fn handle_get_task_diff(&self, args: &serde_json::Value) -> Result<String, String> {
        let plan_id = args["plan_id"]
            .as_str()
            .ok_or("missing plan_id")?
            .to_string();
        let task_id = args["task_id"]
            .as_str()
            .ok_or("missing task_id")?
            .to_string();
        let attempt = args["attempt"].as_u64().map(|n| n as u32);

        let plan_arc = {
            let plans = self.active_plans.lock().await;
            plans
                .get(&plan_id)
                .cloned()
                .ok_or_else(|| format!("unknown plan '{plan_id}'"))?
        };

        let state = plan_arc.lock().await;
        let entry = state
            .tasks
            .iter()
            .find(|t| t.spec.task_id == task_id)
            .ok_or_else(|| format!("unknown task '{task_id}' in plan '{plan_id}'"))?;

        match &entry.status {
            crate::plan::PlanTaskStatus::Pending | crate::plan::PlanTaskStatus::Ready => {
                return Err(format!("task '{task_id}' has not been dispatched yet"));
            }
            crate::plan::PlanTaskStatus::Dispatched { .. } => {
                return Err(format!(
                    "task '{task_id}' is still running — diff not available yet"
                ));
            }
            _ => {}
        }

        // If attempt specified and differs from current, look up historical attempt.
        if let Some(want_attempt) = attempt {
            if want_attempt != entry.attempt {
                let Some(rec) = entry.history.iter().find(|r| r.attempt == want_attempt) else {
                    return Err(format!(
                        "task '{task_id}' has no attempt {want_attempt} (current: {}, history: {} entries)",
                        entry.attempt,
                        entry.history.len()
                    ));
                };
                let mut resp = serde_json::Map::new();
                resp.insert("task_id".into(), json!(task_id));
                resp.insert("agent".into(), json!(entry.spec.agent));
                resp.insert("attempt".into(), json!(want_attempt));
                resp.insert("status".into(), json!("historical"));
                resp.insert("task_description".into(), json!(entry.spec.task));
                if let Some(ref id) = entry.spec.issue_id {
                    resp.insert("issue_id".into(), json!(id));
                }
                if let Some(ref b) = rec.worker_branch {
                    resp.insert("worker_branch".into(), json!(b));
                }
                if let Some(ref s) = rec.summary {
                    resp.insert("summary".into(), json!(s));
                }
                if let Some(ref d) = rec.diff_summary {
                    resp.insert(
                        "diff_summary".into(),
                        serde_json::to_value(d).unwrap_or_default(),
                    );
                }
                resp.insert("feedback".into(), json!(rec.feedback));
                resp.insert(
                    "note".into(),
                    json!("Historical attempt — full diff text not stored. Inspect git: `git show <worker_branch>`."),
                );
                return serde_json::to_string_pretty(&serde_json::Value::Object(resp))
                    .map_err(|e| e.to_string());
            }
        }

        let mut resp = serde_json::Map::new();
        resp.insert("task_id".into(), json!(task_id));
        resp.insert("agent".into(), json!(entry.spec.agent));
        resp.insert("task_description".into(), json!(entry.spec.task));
        if let Some(ref issue_id) = entry.spec.issue_id {
            resp.insert("issue_id".into(), json!(issue_id));
        }

        let status_str = match &entry.status {
            crate::plan::PlanTaskStatus::AwaitingReview { .. } => "awaiting_review",
            crate::plan::PlanTaskStatus::Approved { .. } => "approved",
            crate::plan::PlanTaskStatus::Rejected { .. } => "rejected",
            crate::plan::PlanTaskStatus::Failed { .. } => "failed",
            _ => "unknown",
        };
        resp.insert("status".into(), json!(status_str));

        if let Some(ref branch) = entry.worker_branch {
            resp.insert("worker_branch".into(), json!(branch));
        }
        if let Some(ref result) = entry.result {
            for (k, v) in crate::plan::build_task_diff_fields(result) {
                resp.insert(k, v);
            }
        }

        serde_json::to_string_pretty(&serde_json::Value::Object(resp)).map_err(|e| e.to_string())
    }

    async fn handle_review_task(&self, args: &serde_json::Value) -> Result<String, String> {
        let plan_id = args["plan_id"]
            .as_str()
            .ok_or("missing plan_id")?
            .to_string();
        let task_id = args["task_id"]
            .as_str()
            .ok_or("missing task_id")?
            .to_string();
        let decision = args["decision"].as_str().ok_or("missing decision")?;
        let feedback = args["feedback"].as_str();

        let plan_arc = {
            let plans = self.active_plans.lock().await;
            plans
                .get(&plan_id)
                .cloned()
                .ok_or_else(|| format!("unknown plan '{plan_id}'"))?
        };

        let sink: Option<&dyn crate::events::McpEventSink> = self.event_sink.as_deref();

        // INV-5: use handle_review_task so the plan lock is dropped before
        // pm.update_issue() is called.  The pm_service field stores a concrete
        // Arc<PmService>; deref as &dyn PmLike for the call.
        let pm_like: Option<&dyn crate::plan::PmLike> = self
            .pm_service
            .as_deref()
            .map(|s| s as &dyn crate::plan::PmLike);

        let result = crate::plan::handle_review_task(
            plan_arc,
            &plan_id,
            &task_id,
            decision,
            feedback,
            pm_like,
            sink,
            Some(&self.delegation_tx),
            Some(&self.task_tracker),
        )
        .await?;

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }

    async fn handle_delegate_async(&self, id: Value, args: Value) -> JsonRpcResponse {
        let bad_params = |message| JsonRpcResponse::invalid_params(id.clone(), message);
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
        let delegation_plan = match parse_delegation_plan(&args).map_err(bad_params) {
            Ok(plan) => plan,
            Err(response) => return response,
        };
        let issue_id = args
            .get("issue_id")
            .and_then(|v| v.as_str())
            .map(String::from);

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

        // Build the detached handle so the result-collector can route the
        // completion back into the orchestrator ingress via the `on_complete`
        // callback wired in spur-core.
        let detached = Some(DetachedCompletionHandle {
            ctx: Arc::clone(&self.continuation_ctx),
            source_kind: DetachedSourceKind::AsyncRequested,
        });

        Self::spawn_result_collector(
            &self.task_tracker,
            request_id.clone(),
            rx,
            Arc::clone(&self.active_delegations),
            Arc::clone(&self.completed_delegations),
            detached,
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
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        self.evict_stale_completions().await;

        // Already completed — return immediately.
        if let Some((result, _ts)) = self
            .completed_delegations
            .lock()
            .await
            .remove(&delegation_id)
        {
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
        if !self
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
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

            if let Some((result, _ts)) = self
                .completed_delegations
                .lock()
                .await
                .remove(&delegation_id)
            {
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

impl ServerHandler for McpCallbackServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Use these tools to delegate work, inspect plan status, and interact with the configured project management backend."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();

        let mut implementation = Implementation::default();
        implementation.name = "spur-mcp".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.rmcp_tool(name)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.rmcp_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = request.name.to_string();
        let params = json!({
            "name": tool_name,
            "arguments": request.arguments.map(Value::Object).unwrap_or_else(|| json!({})),
        });
        let response = self.handle_tool_call(Value::Null, params).await;
        Self::call_tool_result_from_legacy_response(response, &tool_name)
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

#[cfg(test)]
mod cancel_delegation_tests {
    use spur_acp::{CancelOutcome, CancellationControl};

    /// INV-6: CancellationControl.cancel returns Cancelled the first time
    /// and NotFound on a second call (token was removed on first cancel).
    #[tokio::test]
    async fn cancel_returns_cancelled_then_not_found() {
        let cc = CancellationControl::new();
        let token = cc.register("req-1".into()).await;

        assert!(!token.is_cancelled(), "token should not be cancelled yet");

        let outcome = cc.cancel("req-1").await;
        assert_eq!(outcome, CancelOutcome::Cancelled);
        assert!(
            token.is_cancelled(),
            "token must be cancelled after cancel()"
        );

        // Second cancel: token was removed, so NotFound.
        let outcome2 = cc.cancel("req-1").await;
        assert_eq!(outcome2, CancelOutcome::NotFound);
    }

    /// INV-6: cancel on an unknown id returns NotFound.
    #[tokio::test]
    async fn cancel_unknown_id_returns_not_found() {
        let cc = CancellationControl::new();
        let outcome = cc.cancel("no-such-id").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }

    /// INV-6: remove() cleans up without cancelling the token.
    #[tokio::test]
    async fn remove_cleans_up_without_cancelling() {
        let cc = CancellationControl::new();
        let token = cc.register("req-2".into()).await;
        cc.remove("req-2").await;
        assert!(!token.is_cancelled(), "remove must not cancel the token");
        // After remove, cancel returns NotFound.
        let outcome = cc.cancel("req-2").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }
}
