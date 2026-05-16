use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use spur_pm::{IssueFilter, IssueUpdate, PmService};
use tokio::sync::Mutex;

use crate::outcome_materializer::OutcomeMaterializer;
use crate::plan::outcomes::OutcomeStore;
use crate::plan::PlanState;

#[derive(Debug, Clone)]
pub struct WorkerCallContext {
    pub delegation_id: String,
    pub brain_session_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum McpHandlerError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("upstream PM failure: {0}")]
    UpstreamPm(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl McpHandlerError {
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => -32602,
            Self::NotFound(_) => -32004,
            Self::Unauthorized(_) => -32001,
            Self::UpstreamPm(_) => -32603,
            Self::Internal(_) => -32603,
        }
    }

    pub fn to_jsonrpc_response(&self, id: Value) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": self.json_rpc_code(),
                "message": self.to_string(),
            }
        })
    }
}

impl From<McpHandlerError> for rmcp::ErrorData {
    fn from(value: McpHandlerError) -> Self {
        rmcp::ErrorData::new(
            rmcp::model::ErrorCode(value.json_rpc_code()),
            value.to_string(),
            None,
        )
    }
}

pub async fn get_issue(
    pm: &PmService,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let issue_id = args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing required field 'id'".into()))?;

    let issue = pm
        .get_issue(issue_id)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    serde_json::to_value(issue)
        .map_err(|e| McpHandlerError::Internal(format!("failed to serialize issue: {e}")))
}

pub async fn list_issues(
    pm: &PmService,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
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
        offset: None,
        labels,
        since: None,
        include_closed: false,
    };

    let issues = pm
        .list_issues(filter)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    serde_json::to_value(issues)
        .map_err(|e| McpHandlerError::Internal(format!("failed to serialize issues: {e}")))
}

pub async fn update_issue(
    pm: &PmService,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let id = args
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing 'id'".into()))?;

    let comment = args
        .get("comment")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let add_labels: Vec<String> = args
        .get("add_labels")
        .and_then(serde_json::Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let remove_labels: Vec<String> = args
        .get("remove_labels")
        .and_then(serde_json::Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let status = args
        .get("status")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let priority = args
        .get("priority")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);

    let assignee = args
        .get("assignee")
        .and_then(serde_json::Value::as_str)
        .map(String::from);

    let update = IssueUpdate {
        status,
        comment,
        add_labels,
        remove_labels,
        priority,
        assignee,
        body: None,
        external_ref: None,
        source_system: None,
        source_repo: None,
    };

    pm.update_issue(id, update)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    Ok(serde_json::json!({ "ok": true }))
}

/// Abstracts `McpCallbackServer::load_or_project_plan` so freestanding
/// handlers can resolve a plan without depending on the full server.
///
/// Kept `dyn`-compatible (no generic methods, no `Self: Sized`) so handlers
/// can take `&dyn PlanResolver`. Reused by Task 11 (`get_task_diff`).
#[async_trait]
pub trait PlanResolver: Send + Sync {
    async fn load_or_project_plan(&self, plan_id: &str) -> Result<Arc<Mutex<PlanState>>, String>;
}

pub async fn get_plan_status(
    plan_resolver: &dyn PlanResolver,
    reconciler_outcomes: &Mutex<OutcomeStore>,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    let plan_id = args
        .get("plan_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'plan_id'".into()))?
        .to_string();

    let plan_state = plan_resolver
        .load_or_project_plan(&plan_id)
        .await
        .map_err(|_| McpHandlerError::InvalidParams(format!("Unknown plan_id: '{plan_id}'")))?;

    let state = plan_state.lock().await;
    let mut status = crate::plan::build_plan_status(&plan_id, &state);
    let outcomes = reconciler_outcomes.lock().await;
    if let serde_json::Value::Object(ref mut fields) = status {
        fields.insert(
            "recent_outcomes".into(),
            serde_json::json!(outcomes.recent_outcomes(&plan_id)),
        );
        fields.insert(
            "stuck_tasks".into(),
            serde_json::json!(outcomes.stuck_tasks_for_plan(&plan_id)),
        );
    }
    Ok(status)
}

/// Phase 2 Task 10: freestanding `fetch_outcome_artifact` handler.
///
/// SECURITY INVARIANT: the lookup `OutcomeKey` is built from
/// `ctx.brain_session_id`, NEVER from a caller-supplied parameter — this is
/// what enforces cross-session isolation (formerly server.rs:2997-3001).
///
/// A miss in the underlying store is reported as `Unauthorized` rather than
/// `NotFound` so that a caller in session A cannot probe whether a given
/// `(delegation_id, attempt)` exists in session B.
pub async fn fetch_outcome_artifact(
    materializer: &OutcomeMaterializer,
    outcome_store: &dyn spur_blob_store::OutcomeStore,
    ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    use spur_acp::DelegationId;
    use spur_blob_store::Section;

    let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s.into(),
        _ => {
            return Err(McpHandlerError::InvalidParams(
                "Missing or empty 'delegation_id'".into(),
            ));
        }
    };

    let section_str = args
        .get("section")
        .and_then(|v| v.as_str())
        .unwrap_or("full");
    let section = match section_str {
        "full" => Section::Full,
        "status_only" => Section::StatusOnly,
        "summary" => Section::Summary,
        "diff_only" => Section::DiffOnly,
        other => {
            return Err(McpHandlerError::InvalidParams(format!(
                "Unknown section '{other}'. Must be one of: status_only, summary, diff_only, full."
            )));
        }
    };

    // Distinguish missing (use latest) from invalid (reject). Without this
    // split, a non-numeric or negative `attempt` value would silently fall
    // through to the missing-arg fallback and return data for a different
    // attempt than the brain asked for.
    let attempt = match args.get("attempt") {
        None | Some(serde_json::Value::Null) => materializer
            .latest_attempt(&delegation_id)
            .await
            .unwrap_or(1),
        Some(v) => match v.as_u64() {
            Some(n) if (1..=u32::MAX as u64).contains(&n) => n as u32,
            _ => {
                return Err(McpHandlerError::InvalidParams(
                    "Invalid 'attempt': must be u32 >= 1".into(),
                ));
            }
        },
    };

    let key = spur_acp::domain::outcome::OutcomeKey {
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId(
            ctx.brain_session_id.clone(),
        )),
        delegation_id: delegation_id.clone(),
        attempt,
    };

    let start = std::time::Instant::now();
    let content = match outcome_store.get(&key, Some(Section::Full)).await {
        Ok(content) => content,
        Err(spur_blob_store::StoreError::NotFound(_)) => {
            tracing::warn!(
                target: "spur.metrics.outcome_fetch_not_found",
                ?key,
                section = section_str,
                "outcome not found; reported as Unauthorized to prevent cross-session probing"
            );
            return Err(McpHandlerError::Unauthorized(format!(
                "Outcome artifact not accessible for delegation_id={delegation_id} attempt={attempt}"
            )));
        }
        Err(spur_blob_store::StoreError::Unauthorized { requested, actual }) => {
            tracing::warn!(
                target: "spur.metrics.outcome_fetch_unauthorized",
                ?requested,
                ?actual,
                "cross-session fetch rejected"
            );
            return Err(McpHandlerError::Unauthorized(
                "cross-session outcome read forbidden".into(),
            ));
        }
        Err(error) => {
            return Err(McpHandlerError::Internal(format!(
                "OutcomeStore::get failed: {error}"
            )));
        }
    };

    let projected_text =
        crate::server::project_section(&content.bytes, section, &key).map_err(|error| {
            McpHandlerError::Internal(format!("Section projection failed: {error}"))
        })?;

    tracing::info!(
        target: "spur.metrics.outcome_fetched",
        ?key,
        section = section_str,
        byte_size = projected_text.len() as u64,
        latency_ms = start.elapsed().as_millis() as u64,
    );

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": projected_text }]
    }))
}

/// Phase 2 Task 11: freestanding `get_task_diff` handler — the heaviest
/// extraction in this phase.
///
/// Mechanically transcribed from `McpCallbackServer::handle_get_task_diff`.
/// `pm` and `repo_root` are `Option` so cached-result reads (where
/// `entry.result == Some(_)`) succeed even when the brain has no PmService /
/// repo_root configured — matching the pre-refactor behavior. Each path that
/// actually needs them performs its own `.ok_or_else(...)` check inside the
/// recovery branch.
///
/// String errors from the helper functions are mapped to `McpHandlerError`
/// by prefix: `"not licensed"` → `Unauthorized` (preserves the -32001 wire
/// code on feature-gate denials), everything else → `Internal`.
pub async fn get_task_diff(
    pm: Option<&PmService>,
    feature_gate: &spur_license::FeatureGate,
    repo_root: Option<&std::path::Path>,
    plan_resolver: &dyn PlanResolver,
    _ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    use serde_json::json;

    let plan_id = args
        .get("plan_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing plan_id".into()))?
        .to_string();
    let task_id = args
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing task_id".into()))?
        .to_string();
    let attempt = args
        .get("attempt")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    let plan_arc = plan_resolver
        .load_or_project_plan(&plan_id)
        .await
        .map_err(McpHandlerError::InvalidParams)?;

    let (
        current_attempt,
        history,
        agent,
        task_description,
        issue_id,
        status_str,
        status_summary,
        worker_branch,
        dispatched_base_oid,
        result,
        epic_id,
        base_snapshot_branch,
        base_snapshot_oid,
    ) = {
        let state = plan_arc.lock().await;
        let entry = state
            .tasks
            .iter()
            .find(|t| t.spec.task_id == task_id)
            .ok_or_else(|| {
                McpHandlerError::InvalidParams(format!(
                    "unknown task '{task_id}' in plan '{plan_id}'"
                ))
            })?;

        match &entry.status {
            crate::plan::PlanTaskStatus::Pending | crate::plan::PlanTaskStatus::Ready => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "task '{task_id}' has not been dispatched yet"
                )));
            }
            crate::plan::PlanTaskStatus::Dispatched { .. } => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "task '{task_id}' is still running — diff not available yet"
                )));
            }
            crate::plan::PlanTaskStatus::AwaitingReview { .. }
            | crate::plan::PlanTaskStatus::Approved { .. }
            | crate::plan::PlanTaskStatus::Rejected { .. }
            | crate::plan::PlanTaskStatus::Failed { .. }
            | crate::plan::PlanTaskStatus::EscalatedToBrain { .. }
            | crate::plan::PlanTaskStatus::Cancelled { .. }
            | crate::plan::PlanTaskStatus::Superseded { .. }
            | crate::plan::PlanTaskStatus::BlockedOnSetupConflict { .. } => {}
        }

        let status_str = match &entry.status {
            crate::plan::PlanTaskStatus::AwaitingReview { .. } => "awaiting_review",
            crate::plan::PlanTaskStatus::Approved { .. } => "approved",
            crate::plan::PlanTaskStatus::Rejected { .. } => "rejected",
            crate::plan::PlanTaskStatus::Failed { .. } => "failed",
            crate::plan::PlanTaskStatus::EscalatedToBrain { .. } => "escalated",
            crate::plan::PlanTaskStatus::Cancelled { .. } => "cancelled",
            crate::plan::PlanTaskStatus::Superseded { .. } => "superseded",
            crate::plan::PlanTaskStatus::BlockedOnSetupConflict { .. } => {
                "blocked_on_setup_conflict"
            }
            crate::plan::PlanTaskStatus::Pending
            | crate::plan::PlanTaskStatus::Ready
            | crate::plan::PlanTaskStatus::Dispatched { .. } => "unknown",
        }
        .to_string();
        let status_summary = match &entry.status {
            crate::plan::PlanTaskStatus::AwaitingReview { summary }
            | crate::plan::PlanTaskStatus::Approved { summary } => summary.clone(),
            crate::plan::PlanTaskStatus::Pending
            | crate::plan::PlanTaskStatus::Ready
            | crate::plan::PlanTaskStatus::Dispatched { .. }
            | crate::plan::PlanTaskStatus::Rejected { .. }
            | crate::plan::PlanTaskStatus::Failed { .. }
            | crate::plan::PlanTaskStatus::EscalatedToBrain { .. }
            | crate::plan::PlanTaskStatus::Cancelled { .. }
            | crate::plan::PlanTaskStatus::Superseded { .. }
            | crate::plan::PlanTaskStatus::BlockedOnSetupConflict { .. } => None,
        };

        (
            entry.attempt,
            entry.history.clone(),
            entry.spec.agent.clone(),
            entry.spec.task.clone(),
            entry.spec.issue_id.clone(),
            status_str,
            status_summary,
            entry.worker_branch.clone(),
            entry.dispatched_base_oid.clone(),
            entry.result.clone(),
            state.epic_id.clone(),
            state.base_snapshot_branch.clone(),
            state.base_snapshot_oid.clone(),
        )
    };

    // If attempt specified and differs from current, look up historical attempt.
    if let Some(want_attempt) = attempt {
        if want_attempt != current_attempt {
            let historical_attempts = if history.is_empty() {
                if let (Some(pm), Some(issue_id)) = (pm, issue_id.as_deref()) {
                    crate::server::reconstruct_historical_attempts(
                        pm,
                        feature_gate,
                        issue_id,
                        current_attempt,
                    )
                    .await
                    .map_err(map_string_error)?
                } else {
                    Vec::new()
                }
            } else {
                history.clone()
            };
            let Some(rec) = historical_attempts
                .iter()
                .find(|r| r.attempt == want_attempt)
            else {
                return Err(McpHandlerError::InvalidParams(format!(
                    "task '{task_id}' has no attempt {want_attempt} (current: {}, history: {} entries)",
                    current_attempt,
                    historical_attempts.len()
                )));
            };
            let mut resp = serde_json::Map::new();
            resp.insert("task_id".into(), json!(task_id));
            resp.insert("agent".into(), json!(agent));
            resp.insert("attempt".into(), json!(want_attempt));
            resp.insert("status".into(), json!("historical"));
            resp.insert("task_description".into(), json!(task_description));
            if let Some(ref id) = issue_id {
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
            return Ok(serde_json::Value::Object(resp));
        }
    }

    let mut resp = serde_json::Map::new();
    resp.insert("task_id".into(), json!(task_id));
    resp.insert("agent".into(), json!(agent));
    resp.insert("task_description".into(), json!(task_description));
    if let Some(ref issue_id) = issue_id {
        resp.insert("issue_id".into(), json!(issue_id));
    }
    resp.insert("status".into(), json!(status_str));

    if let Some(ref branch) = worker_branch {
        resp.insert("worker_branch".into(), json!(branch));
    }
    if let Some(ref summary) = status_summary {
        resp.insert("summary".into(), json!(summary));
    }
    if let Some(ref result) = result {
        for (k, v) in crate::plan::build_task_diff_fields(result) {
            resp.insert(k, v);
        }
    } else if let (Some(pm), Some(epic_id), Some(issue_id)) =
        (pm, epic_id.as_deref(), issue_id.as_deref())
    {
        let bootstrap =
            crate::server::read_persisted_plan_bootstrap(pm, feature_gate, &plan_id, epic_id)
                .await
                .ok();
        let completion = crate::server::read_latest_task_completion(pm, feature_gate, issue_id)
            .await
            .map_err(map_string_error)?;
        let recovered_worker_branch = completion
            .as_ref()
            .and_then(|record| record.worker_branch.clone())
            .or(worker_branch);

        if let Some(recovered_worker_branch) = recovered_worker_branch {
            let base_ref = if let Some(dispatched_base_oid) = dispatched_base_oid {
                dispatched_base_oid
            } else {
                tracing::warn!(
                    plan_id = %plan_id,
                    task_id = %task_id,
                    worker_branch = %recovered_worker_branch,
                    "get_task_diff falling back to base snapshot range because task has no dispatched_base_oid"
                );
                bootstrap
                    .as_ref()
                    .and_then(crate::server::PersistedPlanBootstrap::preferred_base_ref)
                    .map(str::to_string)
                    .or(base_snapshot_oid)
                    .or(base_snapshot_branch)
                    .ok_or_else(|| {
                        McpHandlerError::Internal(format!(
                            "plan '{plan_id}' has no captured base snapshot; latest diff unavailable"
                        ))
                    })?
            };
            let repo_root = repo_root.ok_or_else(|| {
                McpHandlerError::Internal(
                    "Repository root not configured; get_task_diff cannot reconstruct persisted diffs"
                        .to_string(),
                )
            })?;
            let diff = crate::server::diff_text_from_branches(
                repo_root,
                &base_ref,
                &recovered_worker_branch,
            )
            .await
            .map_err(map_string_error)?;
            resp.insert("worker_branch".into(), json!(recovered_worker_branch));
            resp.insert("diff".into(), json!(diff));
            if let Some(summary) = completion.and_then(|record| record.summary) {
                resp.insert("summary".into(), json!(summary));
            }
        }
    }

    Ok(serde_json::Value::Object(resp))
}

fn map_string_error(error: String) -> McpHandlerError {
    if error.starts_with("not licensed") {
        McpHandlerError::Unauthorized(error)
    } else {
        McpHandlerError::Internal(error)
    }
}

/// Worker-facing handler for the `report_signal` MCP tool.
pub async fn report_signal(
    pm: &PmService,
    feature_gate: &spur_license::FeatureGate,
    ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    use serde_json::json;

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

    // Defense-in-depth: the MCP tool schema declares `kind` enum, but harness
    // input-schema enforcement is best-effort across MCP runtimes. Reject
    // non-worker-emittable variants explicitly so a hallucinating worker
    // cannot spoof brain-side signals (e.g., PotentialClobber, which carries
    // OIDs the worker has no authority over).
    //
    // bd-2m2u Phase 2e — `RetryExhausted` is now a worker-emittable kind so
    // the autonomous recovery proposer (option B) can be triggered without
    // going through a forged comment. `PotentialClobber` remains brain-only.
    if !matches!(
        args.signal,
        WorkerSignal::ScopeDrift { .. }
            | WorkerSignal::RetryExhausted { .. }
            | WorkerSignal::MarkNoop { .. }
    ) {
        return Err(McpHandlerError::InvalidParams(format!(
            "report_signal: only worker-emittable signal kinds are accepted; got {}",
            args.signal.kind_label()
        )));
    }

    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate,
    )
    .map_err(|error| McpHandlerError::Unauthorized(crate::server::feature_error_message(error)))?;

    let adv = pm
        .advanced()
        .ok_or_else(|| McpHandlerError::Internal("report_signal requires beads backend".into()))?;

    let issue = pm
        .get_issue(&args.task_id)
        .await
        .map_err(|e| McpHandlerError::UpstreamPm(format!("{e}")))?;

    let signal_id = args.signal.signal_id().to_string();

    // Beads persists a compressed status vocabulary — SPUR's nine-state
    // PlanTaskStatus terminals (Approved, Failed, Cancelled, Superseded) all
    // project to the beads `closed` status. Rejected stays `open` (retry-
    // eligible). Using the beads closed-status predicate correctly models
    // every SPUR terminal without matching vocabulary that beads never emits.
    // Fine-grain which-terminal is recoverable from audit comments + labels
    // (spur:superseded-by:*, Approval/Rejection sentinels) at consumer time.
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
        WorkerSignal::MarkNoop { reason, .. } => {
            (0.0, reason.clone(), args.signal.kind_label().to_string())
        }
    };

    // Emit the audit sentinel BEFORE operational writes so the decision-at-
    // decision-time is immutably recorded. Beads has no transactions; if the
    // task closes between our terminal check above and subsequent writes, the
    // watcher's status-at-tick-time filter (signal_watcher.rs) still enforces
    // I3 at consumption. Ordering here makes partial failures auditable and
    // matches the late-path ordering (audit-before-label) for consistency.
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

/// Phase 2 Task 13: fire-and-forget `report_progress` handler.
///
/// Worker streams a free-form status `message` (required) plus an optional
/// `percent` to the brain. The handler emits exactly one
/// `SpurEventBody::WorkerReportProgress` via the injected
/// [`McpEventSink`](crate::events::McpEventSink) and returns a tiny `{ok:true}`
/// acknowledgement — the side effect IS the event.
///
/// Distinct from the orchestrator-synthesized `WorkerProgress` variant
/// (executor-scoped, structured `name`/`u8 pct` milestone). This variant is
/// delegation-scoped, free-form text, and intentionally does NOT clamp or
/// validate the `percent` numeric range — consumers (TUI / dashboards) decide.
///
/// Defense-in-depth: even though the MCP tool schema declares the args shape,
/// `serde_json::from_value` is used so a malformed payload from a hallucinating
/// worker becomes `InvalidParams` (-32602) rather than a silent emission.
pub async fn report_progress(
    sink: &dyn crate::events::McpEventSink,
    ctx: &WorkerCallContext,
    args: serde_json::Value,
) -> Result<serde_json::Value, McpHandlerError> {
    use spur_acp::SpurEventBody;

    #[derive(serde::Deserialize)]
    struct Args {
        message: String,
        #[serde(default)]
        percent: Option<f64>,
    }

    let Args { message, percent } = serde_json::from_value(args)
        .map_err(|e| McpHandlerError::InvalidParams(format!("invalid args: {e}")))?;

    // Dual-gate gate 2: drop on full bus rather than block. The return
    // value is ignored because the contract is fire-and-forget.
    let _ = sink.try_emit(SpurEventBody::WorkerReportProgress {
        delegation_id: ctx.delegation_id.clone(),
        message,
        percent,
    });

    Ok(serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_call_context_construction() {
        let ctx = WorkerCallContext {
            delegation_id: "d-1".into(),
            brain_session_id: "b-1".into(),
        };
        assert_eq!(ctx.delegation_id, "d-1");
    }

    #[test]
    fn handler_error_to_jsonrpc_response() {
        let err = McpHandlerError::InvalidParams("missing field 'id'".into());
        let resp = err.to_jsonrpc_response(serde_json::json!(7));
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("-32602"));
        assert!(s.contains("missing field"));
    }

    #[test]
    fn upstream_pm_failure_maps_to_internal_error() {
        let err = McpHandlerError::UpstreamPm("503 service unavailable".into());
        let resp = err.to_jsonrpc_response(serde_json::json!(1));
        assert!(serde_json::to_string(&resp).unwrap().contains("-32603"));
    }
}
