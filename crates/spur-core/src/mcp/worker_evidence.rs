//! Authenticated, append-only worker evidence. This is not a lifecycle API.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use spur_mcp::ToolDefinition;

use crate::handlers::{McpHandlerError, WorkerCallContext};
use crate::plan::audit_sentinel::{encode_comment, parse_comment, AuditSentinelKind};
use crate::plan::{labels, projector, PmLike};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportAuditArgs {
    task_id: String,
    audit_id: String,
    message: String,
    evidence: BTreeMap<String, Value>,
}

pub(crate) fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "report_audit".into(),
        description: "Worker-only. Append durable evidence (including complete PRE/POST solver receipts) to your currently dispatched Beads issue. Use a UUID audit_id; retry the same ID and payload after transport failures. Conflicting reuse is refused. Returns comment_id and the complete audit reloaded from Beads; repeat the same request to verify persistence without duplicating it. Does not change status, labels, approval, or plan state. Use report_signal for blockers and risks; update_issue remains brain-only.".into(),
        input_schema: json!({
            "type": "object", "additionalProperties": false,
            "required": ["task_id", "audit_id", "message", "evidence"],
            "properties": {
                "task_id": {"type": "string", "minLength": 1},
                "audit_id": {"type": "string", "format": "uuid"},
                "message": {"type": "string", "minLength": 1},
                "evidence": {"type": "object", "additionalProperties": true, "minProperties": 1}
            }
        }),
    }
}

fn denied(reason: &str) -> McpHandlerError {
    McpHandlerError::Unauthorized(format!("worker evidence: {reason}"))
}

/// Caller holds system_review_target_lock. Context must come from the worker
/// transport, which authenticates the brain and checks the live registration.
/// Require both durable dispatch and its current label; neither stale audit
/// history nor a guessed issue ID grants write authority.
pub(crate) async fn authorize_worker_task(
    pm: &dyn PmLike,
    ctx: &WorkerCallContext,
    task_id: &str,
) -> Result<Vec<spur_pm::Comment>, McpHandlerError> {
    if ctx.delegation_id.is_empty() || ctx.brain_session_id.is_empty() {
        return Err(denied("authenticated worker context required"));
    }
    let issue = pm
        .get_issue(task_id)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?;
    if issue.status == pm.closed_status()
        || issue.status == "tombstone"
        || issue
            .labels
            .iter()
            .any(|label| label.starts_with("spur:superseded-by:"))
    {
        return Err(denied("target is terminal or superseded"));
    }
    let delegations: Vec<_> = issue
        .labels
        .iter()
        .filter_map(|label| labels::parse_delegation_id(label))
        .collect();
    if delegations != [ctx.delegation_id.as_str()] {
        return Err(denied("target is not assigned to this delegation"));
    }
    let advanced = pm
        .advanced()
        .ok_or_else(|| McpHandlerError::Internal("worker evidence requires Beads".into()))?;
    let comments = advanced
        .list_comments(task_id)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?;
    let audits = projector::collect_sorted_audits_for_issue(task_id, comments.clone())
        .map_err(|e| McpHandlerError::Internal(format!("cannot authorize worker evidence: {e}")))?;
    if projector::current_delegation_from_audits(&audits).as_deref()
        != Some(ctx.delegation_id.as_str())
    {
        return Err(denied("durable dispatch is no longer current"));
    }
    Ok(comments)
}

pub(crate) async fn report_audit(
    pm: &dyn PmLike,
    ctx: &WorkerCallContext,
    args: Value,
) -> Result<Value, McpHandlerError> {
    let args: ReportAuditArgs = serde_json::from_value(args)
        .map_err(|e| McpHandlerError::InvalidParams(format!("invalid audit: {e}")))?;
    let audit_id = uuid::Uuid::parse_str(&args.audit_id)
        .map_err(|e| McpHandlerError::InvalidParams(format!("audit_id must be a UUID: {e}")))?
        .to_string();
    if args.task_id.trim().is_empty() || args.message.trim().is_empty() || args.evidence.is_empty()
    {
        return Err(McpHandlerError::InvalidParams(
            "task_id, message and evidence must not be empty".into(),
        ));
    }
    let _target_guard = crate::plan::system_review_target_lock(&args.task_id)
        .lock()
        .await;
    let comments = authorize_worker_task(pm, ctx, &args.task_id).await?;
    let audit = AuditSentinelKind::WorkerEvidence {
        audit_id: audit_id.clone(),
        delegation_id: ctx.delegation_id.clone(),
        brain_session_id: ctx.brain_session_id.clone(),
        message: args.message,
        evidence: json!(args.evidence),
    };
    let mut existing_id = None;
    for comment in comments {
        if let Some(Ok(existing @ AuditSentinelKind::WorkerEvidence { .. })) =
            parse_comment(&comment.body)
        {
            if let AuditSentinelKind::WorkerEvidence {
                audit_id: existing_key,
                ..
            } = &existing
            {
                if existing_key != &audit_id {
                    continue;
                }
            }
            if existing != audit {
                return Err(McpHandlerError::InvalidParams(
                    "audit_id already has different evidence".into(),
                ));
            }
            existing_id = Some(comment.id);
        }
    }
    let idempotent = existing_id.is_some();
    let comment_id = match existing_id {
        Some(id) => id,
        None => pm
            .advanced()
            .expect("authorized Beads backend")
            .add_comment(&args.task_id, &encode_comment(&audit))
            .await
            .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?,
    };
    // The generic worker get_issue surface does not include comments. Supply
    // the complete reloaded record here so persistence can actually be
    // verified inside the worker, including after a lost first response.
    let persisted = pm.advanced().expect("authorized Beads backend")
        .list_comments(&args.task_id).await
        .map_err(|e| McpHandlerError::UpstreamPm(e.to_string()))?
        .into_iter().find(|comment| comment.id == comment_id)
        .and_then(|comment| parse_comment(&comment.body).and_then(Result::ok))
        .filter(|stored| stored == &audit)
        .ok_or_else(|| McpHandlerError::UpstreamPm("persisted audit could not be reloaded exactly; retry the same audit_id and payload".into()))?;
    Ok(
        json!({"recorded": true, "idempotent": idempotent, "comment_id": comment_id,
        "audit_id": audit_id, "task_id": args.task_id, "delegation_id": ctx.delegation_id, "audit": persisted}),
    )
}
