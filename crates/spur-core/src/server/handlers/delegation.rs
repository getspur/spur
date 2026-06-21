use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
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
        delegation_id: DelegationId,
        rx: tokio::sync::oneshot::Receiver<DelegationResult>,
        cancel_token: CancellationToken,
        active: Arc<tokio::sync::Mutex<HashSet<DelegationId>>>,
        completed: Arc<
            tokio::sync::Mutex<HashMap<DelegationId, (DelegationResult, tokio::time::Instant)>>,
        >,
        detached: Option<DetachedCompletionHandle>,
    ) {
        crate::server::spawn_result_collector(
            tracker,
            delegation_id,
            rx,
            cancel_token,
            active,
            completed,
            detached,
        );
    }

    // ─── Tool handlers ────────────────────────────────────────────────

    pub(crate) async fn handle_recover_orphaned_dispatch(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let parsed: crate::tool_schemas::RecoverOrphanedDispatchInput =
            match serde_json::from_value(args) {
                Ok(parsed) => parsed,
                Err(error) => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        format!("recover_orphaned_dispatch: invalid arguments: {error}"),
                    )
                }
            };

        match self
            .recover_orphaned_dispatch_with_branch(
                &parsed.issue_id,
                &parsed.worker_branch,
                &parsed.dispatched_base_oid,
            )
            .await
        {
            Ok(message) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": message }] }),
            ),
            Err(error) => JsonRpcResponse::internal_error(id, error),
        }
    }

    pub(crate) async fn recover_orphaned_dispatch_with_branch(
        &self,
        issue_id: &str,
        worker_branch: &str,
        dispatched_base_oid: &str,
    ) -> Result<String, String> {
        let pm = self
            .pm_service
            .clone()
            .ok_or_else(|| "recover_orphaned_dispatch: no PM service configured".to_string())?;
        let repo_root = self
            .repo_root
            .as_deref()
            .ok_or_else(|| "recover_orphaned_dispatch: repo_root is not configured".to_string())?;

        let issue = pm.get_issue(issue_id).await.map_err(|error| {
            format!("recover_orphaned_dispatch: get_issue({issue_id}) failed: {error}")
        })?;
        if issue.status != "open" {
            return Err(format!(
                "recover_orphaned_dispatch: issue {issue_id} status is '{}', expected 'open'",
                issue.status
            ));
        }

        let delegation_label = issue
            .labels
            .iter()
            .find_map(|label| {
                crate::plan::labels::parse_delegation_id(label)
                    .or_else(|| label.strip_prefix("delegation-id:"))
            })
            .map(str::to_string);

        require_feature(
            FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(feature_error_message)?;
        let adv = pm.advanced().ok_or_else(|| {
            "recover_orphaned_dispatch: dispatch recovery requires beads backend".to_string()
        })?;
        let audits = crate::plan::projector::collect_sorted_audits_for_issue(
            issue_id,
            adv.list_comments(issue_id).await.map_err(|error| {
                format!("recover_orphaned_dispatch: list_comments({issue_id}) failed: {error}")
            })?,
        )
        .map_err(|error| {
            format!("recover_orphaned_dispatch: parse comments({issue_id}) failed: {error}")
        })?;
        let delegation_audit = crate::plan::projector::current_delegation_from_audits(&audits);
        let has_ready_label =
            crate::plan::projector::has_ready_for_review_label_compat(&issue.labels);
        let awaiting_review_audit = crate::plan::projector::awaiting_review_from_audits(&audits);
        let awaiting_review_completion_delegation =
            audits.iter().rev().find_map(|audit| match audit {
                crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                    delegation_id,
                    completion_state: crate::plan::audit_sentinel::CompletionState::AwaitingReview,
                    ..
                } => Some(delegation_id.clone()),
                _ => None,
            });
        let delegation_id = match (
            delegation_audit,
            delegation_label,
            awaiting_review_completion_delegation,
        ) {
            (Some(audit_id), Some(label_id), _) if audit_id == label_id => audit_id,
            (Some(audit_id), Some(_), _) => {
                crate::plan::projector::emit_label_audit_drift(
                    "delegation-id",
                    "mismatch",
                    issue_id,
                );
                audit_id
            }
            (Some(audit_id), None, _) => {
                crate::plan::projector::emit_label_audit_drift(
                    "delegation-id",
                    "audit_only",
                    issue_id,
                );
                audit_id
            }
            (None, Some(label_id), Some(completed_id))
                if awaiting_review_audit && label_id == completed_id =>
            {
                completed_id
            }
            (None, None, Some(completed_id)) if awaiting_review_audit => completed_id,
            (None, Some(_label_id), _) => {
                crate::plan::projector::emit_label_audit_drift(
                    "delegation-id",
                    "label_only",
                    issue_id,
                );
                return Err(format!(
                    "recover_orphaned_dispatch: issue {issue_id} has delegation-id label but no audit attestation"
                ));
            }
            (None, None, _) => {
                return Err(format!(
                    "recover_orphaned_dispatch: issue {issue_id} is missing spur:delegation-id:<id> label"
                ));
            }
        };
        let plan_id = issue
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_plan_id(label))
            .map(str::to_string)
            .ok_or_else(|| {
                format!(
                    "recover_orphaned_dispatch: issue {issue_id} is missing spur:plan-id:<id> label (orphan recovery is only supported for tasks dispatched via submit_plan/execute_epic; ad-hoc delegate_to_worker tasks are not supported)"
                )
            })?;
        let task_id = issue
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_plan_task_id(label))
            .unwrap_or_else(|| issue_id.to_string())
            .to_string();
        if has_ready_label && !awaiting_review_audit {
            crate::plan::projector::emit_label_audit_drift(
                "ready-for-review",
                "label_only",
                issue_id,
            );
        }
        if awaiting_review_audit {
            if !has_ready_label {
                crate::plan::projector::emit_label_audit_drift(
                    "ready-for-review",
                    "audit_only",
                    issue_id,
                );
            }
            let (attempt, _) = crate::plan::projector::project_attempt_facts(&audits);
            let (_, worker_branch, summary, _, _) =
                crate::plan::projector::latest_completion_facts(&audits).ok_or_else(|| {
                    format!(
                        "recover_orphaned_dispatch: issue {issue_id} is already awaiting review but has no latest completion facts"
                    )
                })?;
            crate::server::replay_awaiting_review_continuation(
                self.event_sink.as_ref(),
                self.continuation_ctx.as_ref(),
                &self.materializer,
                self.brain_session_id(),
                crate::server::AwaitingReviewReplay {
                    plan_id,
                    task_id,
                    delegation_id,
                    attempt,
                    summary,
                    worker_branch,
                },
            )
            .await;
            return Ok("Task already awaiting review - continuation re-emitted.".to_string());
        }
        let dispatched_base_oid = issue
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_dispatched_base_oid(label))
            .unwrap_or(dispatched_base_oid);
        if audits.iter().any(|audit| {
            matches!(
                audit,
                crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                    delegation_id: completed,
                    ..
                } if completed == &delegation_id
            )
        }) {
            return Err(format!(
                "recover_orphaned_dispatch: delegation {delegation_id} already has a completion audit"
            ));
        }

        let tip_oid = resolve_worker_branch_tip_oid(repo_root, worker_branch).await?;
        let commit_count = count_commits_between(repo_root, dispatched_base_oid, &tip_oid)
            .await
            .map_err(|e| {
                format!(
                    "recover_orphaned_dispatch: G-Strict validation failed (base={dispatched_base_oid}): {e}"
                )
            })?;
        if commit_count != 1 {
            return Err(format!(
                "recover_orphaned_dispatch: worker branch {worker_branch} has {commit_count} commits in {dispatched_base_oid}..{tip_oid}; expected exactly 1"
            ));
        }

        let (attempt, _) = crate::plan::projector::project_attempt_facts(&audits);

        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: Some(worker_branch.to_string()),
            artifact: None,
        };

        let deferred = crate::plan::persist_system_completion_and_notify(
            pm.as_ref(),
            issue_id,
            self.feature_gate.as_ref(),
            &plan_id,
            &delegation_id,
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            &self.reconciler_fast_forward,
            &result,
            self.brain_session_id(),
            attempt,
            &self.materializer,
            Some(dispatched_base_oid.to_string()),
            Some(repo_root.to_path_buf()),
            Some(&task_id),
        )
        .await
        .map_err(|error| {
            format!("recover_orphaned_dispatch: failed to persist completion: {error}")
        })?;

        match crate::plan::projector::project_plan_from_beads(
            pm.as_ref(),
            &plan_id,
            self.feature_gate.as_ref(),
        )
        .await
        {
            Ok(projected) => self.install_projected_plan(projected, true).await,
            Err(error) => tracing::warn!(
                plan_id = %plan_id,
                issue_id = %issue_id,
                "recover_orphaned_dispatch: failed to refresh projected plan after recovery: {error}"
            ),
        }

        if let Some(deferred) = deferred {
            deferred
                .deliver(self.event_sink.as_deref(), self.continuation_ctx.as_ref())
                .await;
        }

        Ok("Task promoted to AwaitingReview. Call review_task to approve or reject.".to_string())
    }
}
