use std::sync::Arc;

use crate::handlers::{McpHandlerError, WorkerCallContext};
use crate::worker_server::WorkerSignalSink;
use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::{json, Value};
use spur_acp::SpurEventBody;
use spur_license::{FeatureGate, FeatureKey};
use spur_mcp::events::McpEventSink;
use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};
use spur_pm::PmService;

#[derive(Clone)]
pub struct SignalMcpDeps {
    pub pm_service: Option<Arc<PmService>>,
    pub event_sink: Option<Arc<dyn McpEventSink>>,
    pub feature_gate: Arc<FeatureGate>,
}

pub struct SignalMcpModule {
    deps: SignalMcpDeps,
}

impl SignalMcpModule {
    pub fn new(deps: SignalMcpDeps) -> Self {
        Self { deps }
    }
}

pub struct WorkerSignalMcpToolModule {
    deps: SignalMcpDeps,
}

impl WorkerSignalMcpToolModule {
    pub fn new(deps: SignalMcpDeps) -> Self {
        Self { deps }
    }

    async fn report_signal_inner(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        let pm = self
            .deps
            .pm_service
            .as_ref()
            .ok_or_else(|| McpHandlerError::Internal("No issue tracker configured".into()))?;
        report_signal(pm, self.deps.feature_gate.as_ref(), ctx, args).await
    }

    async fn report_progress_inner(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        let sink = self.deps.event_sink.as_deref().ok_or_else(|| {
            McpHandlerError::Internal("report_progress: event sink not configured".into())
        })?;
        report_progress(sink, ctx, args).await
    }
}

#[async_trait]
impl WorkerSignalSink for WorkerSignalMcpToolModule {
    async fn report_signal(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        self.report_signal_inner(ctx, args).await
    }

    async fn report_progress(
        &self,
        ctx: &WorkerCallContext,
        args: Value,
    ) -> Result<Value, McpHandlerError> {
        self.report_progress_inner(ctx, args).await
    }
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        report_signal_def(),
        report_progress_def(),
        super::worker_evidence::tool_definition(),
    ]
}

fn report_signal_def() -> ToolDefinition {
    ToolDefinition {
        name: "report_signal".into(),
        description: "Worker-facing. Record a typed WorkerSignal on a task. Brain-side watcher will inspect and may mutate the plan.".into(),
        input_schema: Value::Object(report_signal_input_schema()),
    }
}

/// Shared by the catalog and RMCP router. The discriminated requirements
/// mirror worker-emittable `WorkerSignal` variants without requiring fields
/// belonging to another kind. Use `anyOf`: worker providers reject `oneOf`
/// and `allOf`, and the common object schema keeps scalar fields discoverable.
pub(crate) fn report_signal_input_schema() -> serde_json::Map<String, Value> {
    json!({
            "type": "object",
            "required": ["task_id", "signal"],
            "properties": {
                "task_id": { "type": "string" },
                "signal": {
                    "type": "object",
                    "required": ["kind", "signal_id"],
                    "anyOf": [
                        {
                            "properties": { "kind": { "enum": ["scope_drift", "blocked", "risk"] } },
                            "required": ["severity", "reason"]
                        },
                        {
                            "properties": { "kind": { "enum": ["escalate", "mark_noop"] } },
                            "required": ["reason"]
                        },
                        {
                            "properties": { "kind": { "enum": ["retry_exhausted"] } },
                            "required": ["task_id", "attempt", "last_error"]
                        }
                    ],
                    "properties": {
                        "kind": { "type": "string", "enum": ["scope_drift", "retry_exhausted", "blocked", "risk", "escalate", "mark_noop"] },
                        "signal_id": { "type": "string", "format": "uuid" },
                        "severity": { "type": "number", "minimum": 0, "maximum": 1 },
                        "reason": { "type": "string" },
                        "estimated_subtasks": { "type": "integer", "minimum": 0, "maximum": 255 },
                        "task_id": { "type": "string" },
                        "attempt": { "type": "integer", "minimum": 0 },
                        "last_error": { "type": "string" }
                    }
                }
            }
        })
        .as_object()
        .expect("report_signal input schema is an object")
        .clone()
}

fn report_progress_def() -> ToolDefinition {
    ToolDefinition {
        name: "report_progress".into(),
        description: "Worker-facing fire-and-forget progress emission. Sends a free-form `message` (and optional `percent`) to the brain as a `WorkerReportProgress` event. The handler returns `{ok: true}` on accept; the side effect IS the event. No PM writes, no audit sentinel - distinct from `report_signal` (which persists). Workers stream rich progress text without minting structured milestone names. Consumers (TUI / dashboards) decide how to render `percent` (no clamping).".into(),
        input_schema: json!({
            "type": "object",
            "required": ["message"],
            "properties": {
                "message": { "type": "string" },
                "percent": { "type": "number" }
            }
        }),
    }
}

#[async_trait]
impl ToolModule for SignalMcpModule {
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
        let brain_session_id = ctx
            .brain_session_id
            .map(|id| id.as_session_id().0.clone())
            .unwrap_or_default();
        let worker_ctx = WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id,
        };
        let worker_module = WorkerSignalMcpToolModule::new(self.deps.clone());

        let result = match name {
            "report_audit" => {
                return Err(McpError::new(
                    ErrorCode(-32001),
                    "report_audit requires an authenticated worker transport",
                    None,
                ))
            }
            "report_signal" => worker_module
                .report_signal(&worker_ctx, args)
                .await
                .map_err(|error| handler_error_to_mcp_error("report_signal", error))?,
            "report_progress" => worker_module
                .report_progress(&worker_ctx, args)
                .await
                .map_err(|error| handler_error_to_mcp_error("report_progress", error))?,
            other => {
                return Err(McpError::new(
                    ErrorCode(-32601),
                    format!("Unknown tool: {other}"),
                    None,
                ))
            }
        };

        Ok(ToolResponse::json_text(id, result))
    }
}

fn handler_error_to_mcp_error(tool_name: &str, error: McpHandlerError) -> McpError {
    match error {
        McpHandlerError::InvalidParams(message) => McpError::new(ErrorCode(-32602), message, None),
        McpHandlerError::NotFound(message) => McpError::new(ErrorCode(-32004), message, None),
        McpHandlerError::Unauthorized(message) => McpError::new(ErrorCode(-32001), message, None),
        McpHandlerError::UpstreamPm(message) => {
            McpError::internal_error(format!("{tool_name} failed: {message}"), None)
        }
        McpHandlerError::Internal(message) => McpError::internal_error(message, None),
    }
}

/// Worker-facing handler for the `report_signal` MCP tool.
pub async fn report_signal(
    pm: &PmService,
    feature_gate: &FeatureGate,
    ctx: &WorkerCallContext,
    args: Value,
) -> Result<Value, McpHandlerError> {
    use crate::plan::audit_sentinel::{encode_comment as audit_encode, AuditSentinelKind};
    use crate::plan::labels;
    use crate::plan::signals::{encode_comment as signal_encode, WorkerSignal};

    #[derive(serde::Deserialize)]
    struct Args {
        task_id: String,
        signal: WorkerSignal,
    }

    let args: Args = serde_json::from_value(args)
        .map_err(|e| McpHandlerError::InvalidParams(format!("invalid args: {e}")))?;

    if !matches!(
        args.signal,
        WorkerSignal::ScopeDrift { .. }
            | WorkerSignal::RetryExhausted { .. }
            | WorkerSignal::Escalate { .. }
            | WorkerSignal::MarkNoop { .. }
            | WorkerSignal::Blocked { .. }
            | WorkerSignal::Risk { .. }
    ) {
        return Err(McpHandlerError::InvalidParams(format!(
            "report_signal: only worker-emittable signal kinds are accepted; got {}",
            args.signal.kind_label()
        )));
    }

    if !feature_gate.has(FeatureKey::PM_PRO_BEADS_ADVANCED) {
        return Err(McpHandlerError::Unauthorized(format!(
            "not licensed for feature {}",
            FeatureKey::PM_PRO_BEADS_ADVANCED.as_str()
        )));
    }
    let _target_guard = crate::plan::system_review_target_lock(&args.task_id)
        .lock()
        .await;

    let adv = pm
        .advanced()
        .ok_or_else(|| McpHandlerError::Internal("report_signal requires beads backend".into()))?;

    let issue = pm
        .get_issue(&args.task_id)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    let signal_id = args.signal.signal_id().to_string();

    // Brain-side legacy calls retain their late-signal path. Worker calls
    // must prove the target's current durable dispatch before any write.
    let comments = if ctx.delegation_id.is_empty() {
        adv.list_comments(&args.task_id)
            .await
            .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?
    } else {
        super::worker_evidence::authorize_worker_task(pm, ctx, &args.task_id).await?
    };

    if issue.status.as_str() == pm.closed_status() {
        adv.add_comment(
            &args.task_id,
            &audit_encode(&AuditSentinelKind::LateSignal {
                signal_id: signal_id.clone(),
                terminal_status: issue.status.clone(),
            }),
        )
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

        pm.update_issue(
            &args.task_id,
            spur_pm::IssueUpdate {
                add_labels: vec![labels::SIGNAL_LATE_ARRIVAL.to_string()],
                ..Default::default()
            },
        )
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

        return Ok(json!({
            "recorded": true,
            "signal_id": signal_id,
            "late": true,
        }));
    }

    let (severity, reason, kind_label) = match &args.signal {
        WorkerSignal::ScopeDrift {
            severity, reason, ..
        }
        | WorkerSignal::Blocked {
            severity, reason, ..
        }
        | WorkerSignal::Risk {
            severity, reason, ..
        } => (
            *severity,
            reason.clone(),
            args.signal.kind_label().to_string(),
        ),
        WorkerSignal::PotentialClobber { .. } => {
            (0.0, String::new(), args.signal.kind_label().to_string())
        }
        WorkerSignal::RetryExhausted { .. } => {
            (0.0, String::new(), args.signal.kind_label().to_string())
        }
        WorkerSignal::Escalate { reason, .. } => {
            (0.0, reason.clone(), args.signal.kind_label().to_string())
        }
        WorkerSignal::MarkNoop { reason, .. } => {
            (0.0, reason.clone(), args.signal.kind_label().to_string())
        }
    };

    if !severity.is_finite() || !(0.0..=1.0).contains(&severity) {
        return Err(McpHandlerError::InvalidParams(
            "signal severity must be between 0 and 1".into(),
        ));
    }
    if matches!(
        &args.signal,
        WorkerSignal::ScopeDrift { .. }
            | WorkerSignal::Blocked { .. }
            | WorkerSignal::Risk { .. }
            | WorkerSignal::Escalate { .. }
            | WorkerSignal::MarkNoop { .. }
    ) && reason.trim().is_empty()
    {
        return Err(McpHandlerError::InvalidParams(
            "signal reason must not be empty".into(),
        ));
    }
    let audit = AuditSentinelKind::Signal {
        signal_id: signal_id.clone(),
        delegation_id: ctx.delegation_id.clone(),
        kind: kind_label.clone(),
        severity,
        reason,
    };
    let mut has_audit = false;
    let mut has_signal = false;
    for comment in &comments {
        if let Some(Ok(existing @ AuditSentinelKind::Signal { .. })) =
            crate::plan::audit_sentinel::parse_comment(&comment.body)
        {
            if let AuditSentinelKind::Signal { signal_id: id, .. } = &existing {
                if id == &signal_id {
                    if existing != audit {
                        return Err(McpHandlerError::InvalidParams(
                            "signal_id already has a different audit".into(),
                        ));
                    }
                    has_audit = true;
                }
            }
        }
        if let Some(Ok(existing)) = crate::plan::signals::parse_comment(&comment.body) {
            if existing.signal_id() == args.signal.signal_id() {
                if existing != args.signal {
                    return Err(McpHandlerError::InvalidParams(
                        "signal_id already has different content".into(),
                    ));
                }
                has_signal = true;
            }
        }
    }
    // Repair partially persisted calls on retry, without duplicating durable
    // comments. Success is returned only after the discovery label is saved.
    if !has_audit {
        adv.add_comment(&args.task_id, &audit_encode(&audit))
            .await
            .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?;
    }

    if !has_signal {
        adv.add_comment(&args.task_id, &signal_encode(&args.signal))
            .await
            .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;
    }

    pm.update_issue(
        &args.task_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::signal_kind(&kind_label)],
            ..Default::default()
        },
    )
    .await
    .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    Ok(json!({
        "recorded": true,
        "signal_id": signal_id,
        "late": false,
        "idempotent": has_audit && has_signal,
    }))
}

pub async fn report_progress(
    sink: &dyn McpEventSink,
    ctx: &WorkerCallContext,
    args: Value,
) -> Result<Value, McpHandlerError> {
    #[derive(serde::Deserialize)]
    struct Args {
        message: String,
        #[serde(default)]
        percent: Option<f64>,
    }

    let Args { message, percent } = serde_json::from_value(args)
        .map_err(|e| McpHandlerError::InvalidParams(format!("invalid args: {e}")))?;

    let _ = sink.try_emit(SpurEventBody::WorkerReportProgress {
        delegation_id: ctx.delegation_id.clone(),
        message,
        percent,
    });

    Ok(json!({ "ok": true }))
}
