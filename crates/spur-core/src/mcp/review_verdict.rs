use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde::Deserialize;
use serde_json::{json, Value};
use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use crate::handlers::{McpHandlerError, WorkerCallContext};
use crate::plan::audit_sentinel::{
    encode_comment, AuditSentinelKind, CompletionState, SystemReviewDecision,
};
use crate::plan::PmLike;

pub struct ReviewVerdictMcpModule;

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "submit_review_verdict".into(),
        description: "Reviewer-only. Submit one authenticated durable verdict for the current maker completion bound to this reviewer delegation.".into(),
        input_schema: json!({
            "type": "object",
            "required": ["target_issue_id", "decision", "feedback", "evidence"],
            "properties": {
                "target_issue_id": { "type": "string" },
                "decision": {
                    "type": "string",
                    "enum": ["approve", "request_changes"]
                },
                "feedback": { "type": "string", "minLength": 1 },
                "evidence": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1 },
                    "minItems": 1
                }
            }
        }),
    }
}

#[async_trait]
impl ToolModule for ReviewVerdictMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![tool_definition()]
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        _args: Value,
    ) -> Result<ToolResponse, McpError> {
        if name != "submit_review_verdict" {
            return Err(McpError::new(
                ErrorCode(-32601),
                format!("Unknown tool: {name}"),
                None,
            ));
        }
        let id = ctx.request_id.cloned().unwrap_or(Value::Null);
        Err(McpError::new(
            ErrorCode(-32001),
            "submit_review_verdict requires an authenticated worker transport",
            Some(json!({ "request_id": id })),
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecisionArg {
    Approve,
    RequestChanges,
}

#[derive(Debug, Deserialize)]
struct SubmitReviewVerdictArgs {
    target_issue_id: String,
    decision: DecisionArg,
    feedback: String,
    evidence: Vec<String>,
}

fn unauthorized(message: impl Into<String>) -> McpHandlerError {
    McpHandlerError::Unauthorized(format!("submit_review_verdict: {}", message.into()))
}

fn verdict_lock(
    target_issue_id: &str,
    reviewer_delegation_id: &str,
) -> &'static tokio::sync::Mutex<()> {
    const STRIPES: usize = 64;
    static LOCKS: OnceLock<Vec<tokio::sync::Mutex<()>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| (0..STRIPES).map(|_| tokio::sync::Mutex::new(())).collect());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    target_issue_id.hash(&mut hasher);
    reviewer_delegation_id.hash(&mut hasher);
    &locks[hasher.finish() as usize % STRIPES]
}

pub async fn submit_review_verdict(
    pm: &dyn PmLike,
    ctx: &WorkerCallContext,
    args: Value,
) -> Result<Value, McpHandlerError> {
    let args: SubmitReviewVerdictArgs = serde_json::from_value(args)
        .map_err(|error| McpHandlerError::InvalidParams(format!("invalid args: {error}")))?;
    if args.target_issue_id.trim().is_empty() {
        return Err(McpHandlerError::InvalidParams(
            "target_issue_id must not be empty".into(),
        ));
    }
    if args.feedback.trim().is_empty() {
        return Err(McpHandlerError::InvalidParams(
            "feedback must not be empty".into(),
        ));
    }
    if args.evidence.is_empty() || args.evidence.iter().any(|item| item.trim().is_empty()) {
        return Err(McpHandlerError::InvalidParams(
            "evidence must contain at least one non-empty item".into(),
        ));
    }
    let _verdict_guard = verdict_lock(&args.target_issue_id, &ctx.delegation_id)
        .lock()
        .await;

    let advanced = pm.advanced().ok_or_else(|| {
        McpHandlerError::Internal("submit_review_verdict requires beads backend".into())
    })?;
    let comments = advanced
        .list_comments(&args.target_issue_id)
        .await
        .map_err(|error| McpHandlerError::UpstreamPm(error.to_string()))?;
    let audits =
        crate::plan::projector::collect_sorted_audits_for_issue(&args.target_issue_id, comments)
            .map_err(|error| {
                McpHandlerError::Internal(format!(
                    "cannot authorize verdict from target audits: {error}"
                ))
            })?;

    let dispatch = audits
        .iter()
        .rev()
        .find_map(|audit| match audit {
            AuditSentinelKind::SystemReviewDispatch {
                plan_id,
                task_id,
                attempt,
                maker_delegation_id,
                reviewer_delegation_id,
                review_issue_id,
            } => Some((
                plan_id,
                task_id,
                attempt,
                maker_delegation_id,
                reviewer_delegation_id,
                review_issue_id,
            )),
            _ => None,
        })
        .ok_or_else(|| unauthorized("target has no pending system review dispatch"))?;

    let (plan_id, task_id, attempt, maker_id, reviewer_id, review_issue_id) = dispatch;
    if reviewer_id != &ctx.delegation_id {
        return Err(unauthorized(format!(
            "delegation {} is not the current reviewer",
            ctx.delegation_id
        )));
    }

    let decision = match args.decision {
        DecisionArg::Approve => SystemReviewDecision::Approve,
        DecisionArg::RequestChanges => SystemReviewDecision::RequestChanges,
    };
    let verdict = AuditSentinelKind::SystemReviewVerdict {
        maker_delegation_id: maker_id.clone(),
        reviewer_delegation_id: reviewer_id.clone(),
        review_issue_id: review_issue_id.clone(),
        decision,
        feedback: args.feedback,
        evidence: args.evidence,
    };

    if let Some(existing) = audits.iter().rev().find(|audit| {
        matches!(
            audit,
            AuditSentinelKind::SystemReviewVerdict {
                maker_delegation_id,
                reviewer_delegation_id,
                review_issue_id: existing_review_issue_id,
                ..
            } if maker_delegation_id == maker_id
                && reviewer_delegation_id == reviewer_id
                && existing_review_issue_id == review_issue_id
        )
    }) {
        if existing == &verdict {
            return Ok(json!({
                "ok": true,
                "idempotent": true,
                "plan_id": plan_id,
                "task_id": task_id,
                "attempt": attempt,
                "target_issue_id": args.target_issue_id,
                "review_issue_id": review_issue_id,
                "reviewer_delegation_id": reviewer_id,
            }));
        }
        return Err(unauthorized(
            "reviewer already submitted a conflicting verdict",
        ));
    }

    let target = pm
        .get_issue(&args.target_issue_id)
        .await
        .map_err(|error| McpHandlerError::UpstreamPm(error.to_string()))?;
    if target.status == pm.closed_status() {
        return Err(unauthorized("target task is already terminal"));
    }

    let current_completion = audits.iter().rev().find_map(|audit| match audit {
        AuditSentinelKind::Completion {
            delegation_id,
            completion_state,
            superseded,
            worker_branch,
            ..
        } => Some((delegation_id, completion_state, superseded, worker_branch)),
        _ => None,
    });
    let Some((completion_id, completion_state, superseded, worker_branch)) = current_completion
    else {
        return Err(unauthorized("target has no maker completion"));
    };
    if completion_id != maker_id
        || !matches!(completion_state, CompletionState::AwaitingReview)
        || *superseded
        || worker_branch.as_deref().is_none_or(str::is_empty)
    {
        return Err(unauthorized("bound maker completion is no longer current"));
    }

    let companion = pm
        .get_issue(review_issue_id)
        .await
        .map_err(|error| McpHandlerError::UpstreamPm(error.to_string()))?;
    let expected_labels = [
        crate::plan::labels::SYSTEM_REVIEW.to_string(),
        crate::plan::labels::review_target(&args.target_issue_id),
        crate::plan::labels::review_maker_delegation(maker_id),
        crate::plan::labels::review_reviewer_delegation(reviewer_id),
    ];
    if companion.status == pm.closed_status()
        || expected_labels
            .iter()
            .any(|label| !companion.labels.contains(label))
    {
        return Err(unauthorized("review companion is no longer live or bound"));
    }

    advanced
        .add_comment(&args.target_issue_id, &encode_comment(&verdict))
        .await
        .map_err(|error| McpHandlerError::UpstreamPm(error.to_string()))?;

    Ok(json!({
        "ok": true,
        "idempotent": false,
        "plan_id": plan_id,
        "task_id": task_id,
        "attempt": attempt,
        "target_issue_id": args.target_issue_id,
        "review_issue_id": review_issue_id,
        "reviewer_delegation_id": reviewer_id,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::handlers::{McpHandlerError, WorkerCallContext};
    use crate::plan::audit_sentinel::{
        encode_comment, parse_comment, AuditSentinelKind, CompletionState, SystemReviewDecision,
    };
    use crate::plan::test_util::MockPm;
    use crate::plan::PmLike;

    struct ReversedCommentsPm {
        inner: std::sync::Arc<MockPm>,
    }

    #[async_trait::async_trait]
    impl PmLike for ReversedCommentsPm {
        async fn get_issue(&self, id: &str) -> anyhow::Result<spur_pm::Issue> {
            self.inner.get_issue(id).await
        }

        async fn list_issues(
            &self,
            filter: spur_pm::IssueFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            self.inner.list_issues(filter).await
        }

        async fn create_issue(&self, params: spur_pm::IssueCreate) -> anyhow::Result<String> {
            self.inner.create_issue(params).await
        }

        async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
            self.inner.update_issue(id, update).await
        }

        fn closed_status(&self) -> &str {
            self.inner.closed_status()
        }

        fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
            Some(self)
        }
    }

    #[async_trait::async_trait]
    impl spur_pm::BeadsAdvanced for ReversedCommentsPm {
        async fn list_ready(
            &self,
            filter: spur_pm::ReadyFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            spur_pm::BeadsAdvanced::list_ready(self.inner.as_ref(), filter).await
        }

        async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
            let mut comments =
                spur_pm::BeadsAdvanced::list_comments(self.inner.as_ref(), issue_id).await?;
            comments.reverse();
            Ok(comments)
        }

        async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<String> {
            spur_pm::BeadsAdvanced::add_comment(self.inner.as_ref(), issue_id, body).await
        }

        async fn remove_dependency(
            &self,
            issue_id: &str,
            depends_on_id: &str,
        ) -> anyhow::Result<()> {
            spur_pm::BeadsAdvanced::remove_dependency(self.inner.as_ref(), issue_id, depends_on_id)
                .await
        }

        async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
            spur_pm::BeadsAdvanced::dep_cycles(self.inner.as_ref()).await
        }
    }

    async fn create_issue(pm: &MockPm, title: &str, labels: Vec<String>) -> String {
        pm.create_issue(spur_pm::IssueCreate {
            title: title.into(),
            description: Some(format!("{title} fixture")),
            issue_type: Some("task".into()),
            labels,
            ..Default::default()
        })
        .await
        .expect("create fixture issue")
    }

    async fn add_audit(pm: &MockPm, issue_id: &str, audit: AuditSentinelKind) {
        pm.advanced()
            .expect("beads advanced")
            .add_comment(issue_id, &encode_comment(&audit))
            .await
            .expect("append audit");
    }

    async fn seed_bound_review(
        pm: &MockPm,
        maker_delegation_id: &str,
        reviewer_delegation_id: &str,
    ) -> (String, String) {
        let target_issue_id = create_issue(
            pm,
            "maker target",
            vec![
                crate::plan::labels::plan_id("P1"),
                crate::plan::labels::plan_task_id("T1"),
            ],
        )
        .await;
        let review_issue_id = create_issue(
            pm,
            "independent review",
            vec![
                crate::plan::labels::SYSTEM_REVIEW.into(),
                crate::plan::labels::review_target(&target_issue_id),
                crate::plan::labels::review_maker_delegation(maker_delegation_id),
                crate::plan::labels::review_reviewer_delegation(reviewer_delegation_id),
            ],
        )
        .await;
        add_audit(
            pm,
            &target_issue_id,
            AuditSentinelKind::Completion {
                delegation_id: maker_delegation_id.into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker/maker".into()),
                result_summary: Some("maker completed".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
                estimated_cost_micros: None,
            },
        )
        .await;
        add_audit(
            pm,
            &target_issue_id,
            AuditSentinelKind::SystemReviewDispatch {
                plan_id: "P1".into(),
                task_id: "T1".into(),
                attempt: 1,
                maker_delegation_id: maker_delegation_id.into(),
                reviewer_delegation_id: reviewer_delegation_id.into(),
                review_issue_id: review_issue_id.clone(),
            },
        )
        .await;
        (target_issue_id, review_issue_id)
    }

    fn context(delegation_id: &str) -> WorkerCallContext {
        WorkerCallContext {
            delegation_id: delegation_id.into(),
            brain_session_id: crate::plan::loops::LOOP_RUNTIME_OWNER_ID.into(),
        }
    }

    fn verdict_args(target_issue_id: &str, decision: &str) -> serde_json::Value {
        json!({
            "target_issue_id": target_issue_id,
            "decision": decision,
            "feedback": "reviewed acceptance criteria and current maker diff",
            "evidence": ["get_task_diff P1/T1 inspected"]
        })
    }

    #[tokio::test]
    async fn recorded_reviewer_can_submit_approve() {
        let pm = MockPm::new();
        let (target_issue_id, _) = seed_bound_review(&pm, "maker-1", "reviewer-1").await;

        let response = super::submit_review_verdict(
            &pm,
            &context("reviewer-1"),
            verdict_args(&target_issue_id, "approve"),
        )
        .await
        .expect("bound reviewer verdict");

        assert_eq!(response["ok"], true);
        let verdicts = pm
            .comments(&target_issue_id)
            .await
            .into_iter()
            .filter_map(|comment| parse_comment(&comment.body))
            .filter_map(Result::ok)
            .filter(|audit| matches!(audit, AuditSentinelKind::SystemReviewVerdict { .. }))
            .collect::<Vec<_>>();
        assert_eq!(verdicts.len(), 1);
        assert!(matches!(
            &verdicts[0],
            AuditSentinelKind::SystemReviewVerdict {
                decision: SystemReviewDecision::Approve,
                reviewer_delegation_id,
                ..
            } if reviewer_delegation_id == "reviewer-1"
        ));
    }

    #[tokio::test]
    async fn maker_delegation_is_unauthorized_to_submit_verdict() {
        let pm = MockPm::new();
        let (target_issue_id, _) = seed_bound_review(&pm, "maker-1", "reviewer-1").await;

        let error = super::submit_review_verdict(
            &pm,
            &context("maker-1"),
            verdict_args(&target_issue_id, "approve"),
        )
        .await
        .expect_err("maker must not verdict its own work");

        assert!(matches!(error, McpHandlerError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn stale_reviewer_is_unauthorized_after_newer_dispatch() {
        let pm = MockPm::new();
        let (target_issue_id, _) = seed_bound_review(&pm, "maker-1", "reviewer-1").await;
        let newer_review_issue = create_issue(
            &pm,
            "new independent review",
            vec![
                crate::plan::labels::SYSTEM_REVIEW.into(),
                crate::plan::labels::review_target(&target_issue_id),
                crate::plan::labels::review_maker_delegation("maker-2"),
                crate::plan::labels::review_reviewer_delegation("reviewer-2"),
            ],
        )
        .await;
        add_audit(
            &pm,
            &target_issue_id,
            AuditSentinelKind::Completion {
                delegation_id: "maker-2".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker/maker-2".into()),
                result_summary: Some("new maker completion".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
                estimated_cost_micros: None,
            },
        )
        .await;
        add_audit(
            &pm,
            &target_issue_id,
            AuditSentinelKind::SystemReviewDispatch {
                plan_id: "P1".into(),
                task_id: "T1".into(),
                attempt: 2,
                maker_delegation_id: "maker-2".into(),
                reviewer_delegation_id: "reviewer-2".into(),
                review_issue_id: newer_review_issue,
            },
        )
        .await;

        let error = super::submit_review_verdict(
            &pm,
            &context("reviewer-1"),
            verdict_args(&target_issue_id, "approve"),
        )
        .await
        .expect_err("stale reviewer must be fenced");

        assert!(matches!(error, McpHandlerError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn identical_verdict_is_idempotent_but_conflicting_verdict_is_rejected() {
        let pm = MockPm::new();
        let (target_issue_id, _) = seed_bound_review(&pm, "maker-1", "reviewer-1").await;
        let ctx = context("reviewer-1");
        let approve = verdict_args(&target_issue_id, "approve");

        super::submit_review_verdict(&pm, &ctx, approve.clone())
            .await
            .expect("first verdict");
        let repeated = super::submit_review_verdict(&pm, &ctx, approve)
            .await
            .expect("identical verdict is idempotent");
        assert_eq!(repeated["idempotent"], true);

        let error = super::submit_review_verdict(
            &pm,
            &ctx,
            verdict_args(&target_issue_id, "request_changes"),
        )
        .await
        .expect_err("conflicting verdict must fail closed");
        assert!(matches!(error, McpHandlerError::Unauthorized(_)));

        let verdict_count = pm
            .comments(&target_issue_id)
            .await
            .into_iter()
            .filter_map(|comment| parse_comment(&comment.body))
            .filter_map(Result::ok)
            .filter(|audit| matches!(audit, AuditSentinelKind::SystemReviewVerdict { .. }))
            .count();
        assert_eq!(verdict_count, 1);
    }

    #[tokio::test]
    async fn identical_verdict_retry_succeeds_after_target_and_companion_advance() {
        let pm = MockPm::new();
        let (target_issue_id, review_issue_id) =
            seed_bound_review(&pm, "maker-1", "reviewer-1").await;
        let ctx = context("reviewer-1");
        let approve = verdict_args(&target_issue_id, "approve");
        super::submit_review_verdict(&pm, &ctx, approve.clone())
            .await
            .expect("first verdict committed");
        pm.update_issue(
            &target_issue_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("advance target");
        pm.update_issue(
            &review_issue_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close review companion");

        let repeated = super::submit_review_verdict(&pm, &ctx, approve)
            .await
            .expect("response-loss retry must be idempotent after reconciliation");
        assert_eq!(repeated["idempotent"], true);

        let conflict = super::submit_review_verdict(
            &pm,
            &ctx,
            verdict_args(&target_issue_id, "request_changes"),
        )
        .await
        .expect_err("a conflicting retry must remain rejected after reconciliation");
        assert!(matches!(conflict, McpHandlerError::Unauthorized(_)));
    }

    #[tokio::test]
    async fn verdict_authorization_uses_stable_audit_order_not_comment_return_order() {
        let inner = MockPm::new().arc();
        let (target_issue_id, _) = seed_bound_review(&inner, "maker-1", "reviewer-1").await;
        let current_review_issue = create_issue(
            inner.as_ref(),
            "current independent review",
            vec![
                crate::plan::labels::SYSTEM_REVIEW.into(),
                crate::plan::labels::review_target(&target_issue_id),
                crate::plan::labels::review_maker_delegation("maker-2"),
                crate::plan::labels::review_reviewer_delegation("reviewer-2"),
            ],
        )
        .await;
        add_audit(
            inner.as_ref(),
            &target_issue_id,
            AuditSentinelKind::Completion {
                delegation_id: "maker-2".into(),
                completion_state: CompletionState::AwaitingReview,
                superseded: false,
                worker_branch: Some("spur/worker/maker-2".into()),
                result_summary: Some("current maker completed".into()),
                artifact_uri: None,
                dispatched_base_oid: None,
                estimated_cost_micros: None,
            },
        )
        .await;
        add_audit(
            inner.as_ref(),
            &target_issue_id,
            AuditSentinelKind::SystemReviewDispatch {
                plan_id: "P1".into(),
                task_id: "T1".into(),
                attempt: 2,
                maker_delegation_id: "maker-2".into(),
                reviewer_delegation_id: "reviewer-2".into(),
                review_issue_id: current_review_issue,
            },
        )
        .await;
        let reversed = ReversedCommentsPm { inner };

        let response = super::submit_review_verdict(
            &reversed,
            &context("reviewer-2"),
            verdict_args(&target_issue_id, "approve"),
        )
        .await
        .expect("latest durable dispatch must win regardless of comment return order");

        assert_eq!(response["attempt"], 2);
        assert_eq!(response["reviewer_delegation_id"], "reviewer-2");
    }

    #[test]
    fn worker_tool_schema_requires_non_empty_review_evidence() {
        let definition = super::tool_definition();
        assert_eq!(definition.name, "submit_review_verdict");
        assert_eq!(
            definition.input_schema["required"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            definition.input_schema["properties"]["feedback"]["minLength"],
            1
        );
        assert_eq!(
            definition.input_schema["properties"]["evidence"]["minItems"],
            1
        );
        assert_eq!(
            definition.input_schema["properties"]["decision"]["enum"],
            json!(["approve", "request_changes"])
        );
    }
}
