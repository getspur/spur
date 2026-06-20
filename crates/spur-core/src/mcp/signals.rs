use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::{json, Value};
use spur_acp::SpurEventBody;
use spur_license::{FeatureGate, FeatureKey};
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::{McpHandlerError, WorkerCallContext};
use spur_mcp::worker_server::WorkerSignalSink;
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
    vec![report_signal_def(), report_progress_def()]
}

fn report_signal_def() -> ToolDefinition {
    ToolDefinition {
        name: "report_signal".into(),
        description: "Worker-facing. Record a typed WorkerSignal on a task. Brain-side watcher will inspect and may mutate the plan.".into(),
        input_schema: json!({
            "type": "object",
            "required": ["task_id", "signal"],
            "properties": {
                "task_id": { "type": "string" },
                "signal": {
                    "type": "object",
                    "required": ["kind", "signal_id"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["scope_drift", "retry_exhausted"] },
                        "signal_id": { "type": "string", "format": "uuid" },
                        "severity": { "type": "number", "minimum": 0, "maximum": 1 },
                        "reason": { "type": "string" },
                        "estimated_subtasks": { "type": ["integer", "null"], "minimum": 1 },
                        "task_id": { "type": "string" },
                        "attempt": { "type": "integer", "minimum": 0 },
                        "last_error": { "type": "string" }
                    }
                }
            }
        }),
    }
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
                "percent": { "type": ["number", "null"] }
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
    use spur_mcp::plan::audit_sentinel::{encode_comment as audit_encode, AuditSentinelKind};
    use spur_mcp::plan::labels;
    use spur_mcp::plan::signals::{encode_comment as signal_encode, WorkerSignal};

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

    let adv = pm
        .advanced()
        .ok_or_else(|| McpHandlerError::Internal("report_signal requires beads backend".into()))?;

    let issue = pm
        .get_issue(&args.task_id)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    let signal_id = args.signal.signal_id().to_string();

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
        _ => (0.0, String::new(), args.signal.kind_label().to_string()),
    };

    adv.add_comment(
        &args.task_id,
        &audit_encode(&AuditSentinelKind::Signal {
            signal_id: signal_id.clone(),
            delegation_id: ctx.delegation_id.clone(),
            kind: kind_label.clone(),
            severity,
            reason,
        }),
    )
    .await
    .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    adv.add_comment(&args.task_id, &signal_encode(&args.signal))
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

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
