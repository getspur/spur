use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::outcome_materializer::OutcomeMaterializer;
use crate::server::McpCallbackServer;
use crate::server::{
    dispatch_error_response, new_attempt_tracker, spawn_result_collector, DetachedCompletionHandle,
    DetachedContinuationCtx, DetachedSourceKind, JsonRpcResponse, WorkerInfo, COMPLETED_TTL,
};
use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::{json, Value};
use spur_acp::domain::{DelegationResult, DelegationStatus};
use spur_acp::{BrainSessionId, CancelOutcome, CancellationControl, DelegationId, SessionId};
use spur_blob_store::OutcomeStore;
use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};
use tokio::sync::{mpsc, OnceCell};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[cfg(test)]
use crate::server::{build_detached_continuation, community_feature_gate};
use crate::DelegationRequest;

#[cfg(test)]
const PRODUCER_MAX_FIELD_BYTES: usize = 8192;

#[derive(Clone)]
pub struct DelegationMcpDeps {
    delegation_tx: mpsc::Sender<DelegationRequest>,
    workers: Vec<WorkerInfo>,
    brain_session_id: Arc<OnceCell<BrainSessionId>>,
    active_delegations: Arc<tokio::sync::Mutex<HashSet<DelegationId>>>,
    completed_delegations:
        Arc<tokio::sync::Mutex<HashMap<DelegationId, (DelegationResult, tokio::time::Instant)>>>,
    task_tracker: TaskTracker,
    cancellation_control: Option<CancellationControl>,
    continuation_ctx: Arc<DetachedContinuationCtx>,
    materializer: OutcomeMaterializer,
    outcome_store: Arc<dyn OutcomeStore>,
    inline_wait: Duration,
    retiring: Arc<AtomicBool>,
    cancel_token: CancellationToken,
    event_sink: Option<Arc<dyn spur_mcp::McpEventSink>>,
}

impl DelegationMcpDeps {
    pub fn from_server(server: &McpCallbackServer) -> Self {
        Self {
            delegation_tx: server.delegation_sender(),
            workers: server.workers_snapshot(),
            brain_session_id: server.brain_session_id_cell(),
            active_delegations: server.active_delegations_handle(),
            completed_delegations: server.completed_delegations_handle(),
            task_tracker: server.task_tracker_handle(),
            cancellation_control: server.cancellation_control_handle(),
            continuation_ctx: server.continuation_ctx_handle(),
            materializer: server.outcome_materializer(),
            outcome_store: server.outcome_store_handle(),
            inline_wait: server.inline_wait_duration(),
            retiring: server.retiring_flag(),
            cancel_token: server.cancel_token(),
            event_sink: server.event_sink_handle(),
        }
    }

    pub fn catalog_only() -> Self {
        let (delegation_tx, _rx) = mpsc::channel(1);
        let brain_session_id = Arc::new(OnceCell::new());
        let _ = brain_session_id.set(BrainSessionId::new(SessionId("catalog".into())));
        let outcome_store: Arc<dyn OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());

        Self {
            delegation_tx,
            workers: Vec::new(),
            brain_session_id,
            active_delegations: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            completed_delegations: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            task_tracker: TaskTracker::new(),
            cancellation_control: None,
            continuation_ctx: Arc::new(DetachedContinuationCtx {
                on_complete: Arc::new(|_, _| Box::pin(async {})),
            }),
            materializer: OutcomeMaterializer::new(Arc::clone(&outcome_store)),
            outcome_store,
            inline_wait: Duration::from_millis(0),
            retiring: Arc::new(AtomicBool::new(false)),
            cancel_token: CancellationToken::new(),
            event_sink: None,
        }
    }
}

pub struct DelegationMcpModule {
    deps: DelegationMcpDeps,
}

impl DelegationMcpModule {
    pub fn new(deps: DelegationMcpDeps) -> Self {
        Self { deps }
    }

    fn brain_session_id(&self) -> &BrainSessionId {
        self.deps
            .brain_session_id
            .get()
            .expect("brain_session_id must be set before delegation MCP handlers dispatch")
    }

    fn ensure_accepting_delegations(
        &self,
    ) -> std::result::Result<(), spur_acp::DelegationDispatchError> {
        if self.deps.retiring.load(Ordering::SeqCst) {
            Err(spur_acp::DelegationDispatchError::SessionRetiring)
        } else {
            Ok(())
        }
    }

    async fn evict_stale_completions(&self) {
        self.deps
            .completed_delegations
            .lock()
            .await
            .retain(|_, (_, ts)| ts.elapsed() < COMPLETED_TTL);
    }

    async fn handle_delegate_to_worker(&self, id: Value, args: Value) -> JsonRpcResponse {
        if let Err(error) = self.ensure_accepting_delegations() {
            return dispatch_error_response(error, id);
        }
        let parsed: crate::tool_schemas::DelegateToWorkerInput =
            match serde_json::from_value(args.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::invalid_params(id, format!("Invalid arguments: {e}"))
                }
            };

        let request_id = DelegationId::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let attempt_tracker = new_attempt_tracker();

        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: parsed.agent.clone(),
            task: parsed.task,
            context_files: parsed.context_files.unwrap_or_default(),
            prior_branch_for_reuse: None,
            respond_to: tx,
            brain_session_id: self.brain_session_id().clone(),
            delegation_plan: parsed.delegation_plan,
            issue_id: parsed.issue_id,
            base: parsed.base,
            dispatched_base_oid_tx: None,
            attempt_tracker: Arc::clone(&attempt_tracker),
            enable_worker_mcp: parsed.enable_worker_mcp,
        };

        tracing::info!(agent = %parsed.agent, request_id = %request_id, "Sending delegation request");

        if self.deps.delegation_tx.send(delegation).await.is_err() {
            tracing::error!("Failed to send delegation request");
            return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
        }

        self.deps
            .active_delegations
            .lock()
            .await
            .insert(request_id.clone());

        let mut rx = rx;
        let inline_wait = self.deps.inline_wait;
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
                self.deps
                    .active_delegations
                    .lock()
                    .await
                    .remove(&request_id);
                let attempt = attempt_tracker.load(Ordering::SeqCst);
                let continuation = self.deps.materializer
                    .materialize(
                        result.clone(),
                        request_id.clone(),
                        attempt,
                        self.brain_session_id().clone(),
                        spur_acp::domain::ContinuationSource::Inline,
                        self.deps.event_sink.as_ref(),
                    )
                    .await;
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
                    "artifact_id": continuation.payload.artifact_id,
                    "continuation_will_fire": false,
                    "description": format!(
                        "Delegation to '{agent}' completed inline (delegation_id={request_id}).",
                        agent = parsed.agent
                    ),
                    "result": result_json,
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": payload_text
                        }]
                    }),
                )
            }
            _ = tokio::time::sleep(inline_wait) => {
                tracing::info!(
                    agent = %parsed.agent,
                    request_id = %request_id,
                    inline_wait_ms = inline_wait.as_millis() as u64,
                    "Delegation inline window expired — detaching via continuation bridge"
                );
                spawn_result_collector(
                    &self.deps.task_tracker,
                    request_id.clone(),
                    rx,
                    self.deps.cancel_token.child_token(),
                    Arc::clone(&self.deps.active_delegations),
                    Arc::clone(&self.deps.completed_delegations),
                    Some(DetachedCompletionHandle {
                        ctx: Arc::clone(&self.deps.continuation_ctx),
                        source_kind: DetachedSourceKind::BlockTimeout,
                        attempt_tracker,
                        brain_session: self.brain_session_id().as_session_id().clone(),
                        event_sink: self.deps.event_sink.clone(),
                        materializer: self.deps.materializer.clone(),
                    }),
                );
                let payload = json!({
                    "status": "pending",
                    "delegation_id": request_id,
                    "continuation_will_fire": true,
                    "description": format!(
                        "Delegation to '{agent}' is running in the background \
                         (delegation_id={request_id}). A continuation event will \
                         fire automatically when the worker completes. Do NOT call \
                         check_delegation_status — you will be re-prompted automatically.",
                        agent = parsed.agent
                    ),
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": payload_text
                        }]
                    }),
                )
            }
        }
    }

    async fn handle_delegate_parallel(&self, id: Value, args: Value) -> JsonRpcResponse {
        if let Err(error) = self.ensure_accepting_delegations() {
            return dispatch_error_response(error, id);
        }
        if let Some(batch_plan) = args.get("delegation_plan") {
            tracing::info!(
                batch_plan = %batch_plan,
                "delegate_parallel received batch-level delegation_plan (not propagated into per-task requests)",
            );
        }

        if let Err(e) = crate::server::validate_parallel_args(&args) {
            return JsonRpcResponse::invalid_params(id, e);
        }

        let skeletons = match crate::server::parse_parallel_tasks(&args, self.brain_session_id()) {
            Ok(s) => s,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        let inline_wait = self.deps.inline_wait;
        let task_count = skeletons.len();
        let mut dispatched = Vec::with_capacity(task_count);

        for (idx, mut skeleton) in skeletons.into_iter().enumerate() {
            let request_id = skeleton.id.clone();
            let agent = skeleton.agent.clone();
            let attempt_tracker = Arc::clone(&skeleton.attempt_tracker);
            let (tx, rx) = tokio::sync::oneshot::channel();
            skeleton.respond_to = tx;

            tracing::info!(agent = %agent, request_id = %request_id, "Sending parallel delegation request");

            if self.deps.delegation_tx.send(skeleton).await.is_err() {
                tracing::error!("Failed to send parallel delegation request");
                return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
            }

            self.deps
                .active_delegations
                .lock()
                .await
                .insert(request_id.clone());
            dispatched.push((idx, request_id, agent, rx, attempt_tracker));
        }

        let mut waits = JoinSet::new();
        for (idx, request_id, agent, rx, attempt_tracker) in dispatched {
            let active_delegations = Arc::clone(&self.deps.active_delegations);
            let completed_delegations = Arc::clone(&self.deps.completed_delegations);
            let continuation_ctx = Arc::clone(&self.deps.continuation_ctx);
            let task_tracker = self.deps.task_tracker.clone();
            let cancel_token = self.deps.cancel_token.child_token();
            let event_sink = self.deps.event_sink.clone();
            let brain_session_id = self.brain_session_id().clone();
            let brain_session = brain_session_id.as_session_id().clone();
            let materializer = self.deps.materializer.clone();
            waits.spawn(async move {
                let mut rx = rx;
                let entry = tokio::select! {
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
                        active_delegations
                            .lock()
                            .await
                            .remove(&request_id);
                        let attempt = attempt_tracker.load(Ordering::SeqCst);
                        let continuation = materializer
                            .materialize(
                                result.clone(),
                                request_id.clone(),
                                attempt,
                                brain_session_id,
                                spur_acp::domain::ContinuationSource::Inline,
                                event_sink.as_ref(),
                            )
                            .await;
                        let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
                        json!({
                            "status": "completed",
                            "delegation_id": request_id,
                            "artifact_id": continuation.payload.artifact_id,
                            "agent": agent,
                            "continuation_will_fire": false,
                            "description": format!(
                                "Delegation to '{agent}' completed inline (delegation_id={request_id})."
                            ),
                            "result": result_json,
                        })
                    }
                    _ = tokio::time::sleep(inline_wait) => {
                        spawn_result_collector(
                            &task_tracker,
                            request_id.clone(),
                            rx,
                            cancel_token,
                            active_delegations,
                            completed_delegations,
                            Some(DetachedCompletionHandle {
                                ctx: continuation_ctx,
                                source_kind: DetachedSourceKind::BlockTimeout,
                                attempt_tracker,
                                brain_session,
                                event_sink,
                                materializer,
                            }),
                        );
                        json!({
                            "status": "pending",
                            "delegation_id": request_id,
                            "agent": agent,
                            "continuation_will_fire": true,
                            "description": format!(
                                "Delegation to '{agent}' is running in the background \
                                 (delegation_id={request_id}). A continuation event will \
                                 fire automatically when the worker completes. Do NOT call \
                                 check_delegation_status — you will be re-prompted automatically."
                            ),
                        })
                    }
                };
                (idx, entry)
            });
        }

        let mut results = vec![Value::Null; task_count];
        while let Some(join_result) = waits.join_next().await {
            let (idx, entry) = match join_result {
                Ok(result) => result,
                Err(e) => {
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("Parallel delegation waiter failed: {e}"),
                    )
                }
            };
            results[idx] = entry;
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
        let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.into(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        self.evict_stale_completions().await;

        let completed = {
            let mut map = self.deps.completed_delegations.lock().await;
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

        if self
            .deps
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

    async fn handle_fetch_outcome_artifact(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::fetch_outcome_artifact(
            &self.deps.materializer,
            self.deps.outcome_store.as_ref(),
            &ctx,
            args,
        )
        .await
        {
            Ok(value) => JsonRpcResponse::success(id, value),
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("fetch_outcome_artifact failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    async fn handle_cancel_delegation(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.into(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        if let Some((result, _ts)) = self
            .deps
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

        if self
            .deps
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
            if let Some(ref cc) = self.deps.cancellation_control {
                let outcome = cc
                    .cancel_with_reason(delegation_id.as_str(), "brain requested cancel".into())
                    .await;
                tracing::info!(delegation_id = %delegation_id, ?outcome, "Cancellation requested via CancellationControl");
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
        let workers_json = serde_json::to_value(&self.deps.workers).unwrap_or(json!([]));
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
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        delegate_to_worker_def(),
        delegate_parallel_def(),
        check_delegation_status_def(),
        fetch_outcome_artifact_def(),
        cancel_delegation_def(),
        list_available_workers_def(),
    ]
}

#[async_trait]
impl ToolModule for DelegationMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let id = ctx.request_id.cloned().unwrap_or(Value::Null);
        let response = match name {
            "delegate_to_worker" => self.handle_delegate_to_worker(id, args).await,
            "delegate_parallel" => self.handle_delegate_parallel(id, args).await,
            "check_delegation_status" => self.handle_check_delegation_status(id, args).await,
            "fetch_outcome_artifact" => self.handle_fetch_outcome_artifact(id, args).await,
            "cancel_delegation" => self.handle_cancel_delegation(id, args).await,
            "list_available_workers" => self.handle_list_available_workers(id).await,
            other => {
                return Err(McpError::new(
                    ErrorCode(-32601),
                    format!("Unknown tool: {other}"),
                    None,
                ))
            }
        };
        Ok(ToolResponse::from_json_rpc(response))
    }
}

fn delegate_to_worker_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_to_worker".into(),
        description: "Delegate a task to a worker agent. Returns inline if the worker finishes within the inline-wait window (configurable via `delegation.inline_wait_ms`; default 0). Otherwise returns `{status: \"pending\", delegation_id}` and you will be re-prompted automatically when the worker completes — you do not need to poll. Pass a `delegation_plan` parameter (at minimum `{chosen, rationale}`; more for multi-step work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT. `enable_worker_mcp` defaults to on — the worker receives the curated worker MCP server unless you pass `false`. `enable_worker_progress` defaults to off; opt in for progress events. Use `list_available_workers` when routing is ambiguous.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::DelegateToWorkerInput>(),
    }
}

fn delegate_parallel_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_parallel".into(),
        description: "Delegate multiple tasks in parallel. Returns a response array of length N; each element is either an inline result or `{status: \"pending\", delegation_id}` with an automatic re-prompt when that task completes. Each task's per-task `delegation_plan` documents structured reasoning for reviewer mismatch checks. Per-task `enable_worker_mcp` defaults to on — each worker receives the curated worker MCP server unless explicitly set to `false`. `enable_worker_progress` defaults to off; opt in per task for progress events. Subtasks MUST be independent — no shared state, no sequential data dependencies. If unsure, use `delegate_to_worker` serially.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::DelegateParallelInput>(),
    }
}

fn check_delegation_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "check_delegation_status".into(),
        description: "Non-blocking status query for a delegation. Returns the result if finished, or `{\"status\":\"running\"}`. Primarily a debugging affordance — brains are re-prompted automatically when delegations complete and normally do not need to call this.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id to check"
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

pub(crate) fn fetch_outcome_artifact_def() -> ToolDefinition {
    ToolDefinition {
        name: "fetch_outcome_artifact".into(),
        description: "Fetch the side-channel artifact (full or sectioned) for a completed delegation. Use when continuation.payload.artifact_id is Some(_) and you need fuller context. Sections let you pick what to fetch: pass 'status_only' for just status fields (~100B), 'summary' for the inline summary, 'diff_only' for full diff text, or 'full' for the entire DelegationResult JSON.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id whose artifact you want to fetch."
                },
                "attempt": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional attempt number. Default: latest known attempt for this delegation. Pin a specific attempt for forensic queries on retried delegations."
                },
                "section": {
                    "type": "string",
                    "enum": ["status_only", "summary", "diff_only", "full"],
                    "default": "full",
                    "description": "Which section to fetch."
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

fn cancel_delegation_def() -> ToolDefinition {
    ToolDefinition {
        name: "cancel_delegation".into(),
        description: "Request cancellation of a running delegation. If the delegation already completed, returns its result. Otherwise forwards the cancellation to the orchestrator and returns its response.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id to cancel"
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

fn list_available_workers_def() -> ToolDefinition {
    ToolDefinition {
        name: "list_available_workers".into(),
        description: "Returns tier, description, good_for, avoid_for, output_shape, and cost_tier for each worker. Call when the system-prompt one-liner is insufficient.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

#[cfg(test)]
include!("delegation_tests.rs");
