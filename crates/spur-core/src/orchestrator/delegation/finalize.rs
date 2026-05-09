use super::*;

/// Common terminal-arm helper: emits `DelegationCompleted` and
/// constructs the `DelegationResult`. Centralizing this makes the
/// "every terminal emits DelegationCompleted" invariant locally
/// verifiable (one call site per terminal arm in `execute_delegation`).
///
/// Phase 5 / Task 27: routes the `DelegationCompleted` emit through
/// [`flush_then_emit_completed`] so the per-delegation worker-MCP
/// read-tool audit aggregator drains and the
/// `WorkerMcpDelegationSummary` event lands BEFORE
/// `DelegationCompleted`. The same helper is used by the abort-path
/// branches in `handle_delegations` so the ordering invariant is
/// preserved on every terminal exit.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn finalize(
    funnel: &crate::event_funnel::FunnelHandle,
    worker_mcp_servers: &Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>>,
    pm_service: Option<&Arc<PmService>>,
    brain_session_id: &spur_acp::BrainSessionId,
    delegation_id: &str,
    issue_id: Option<&str>,
    worker_session: SessionId,
    final_status: DelegationStatus,
    diff: Option<String>,
    diff_summary: Option<spur_acp::DiffSummary>,
    summary: Option<String>,
    total_cost: f64,
    worker_branch: Option<String>,
    artifact: Option<spur_acp::WorkerArtifact>,
) -> DelegationResult {
    flush_then_emit_completed(
        funnel,
        worker_mcp_servers,
        pm_service,
        brain_session_id,
        delegation_id,
        issue_id,
        worker_session,
        &final_status,
    )
    .await;
    DelegationResult {
        status: final_status,
        diff,
        diff_summary,
        summary,
        estimated_cost_usd: total_cost,
        worker_branch,
        artifact,
    }
}

/// Map a terminal [`DelegationStatus`] onto the audit-trail outcome
/// string forwarded to `WorkerMcpServer::flush_delegation`. Four-way:
/// `"success"` (clean approval), `"cancelled"` and `"rejected"` (clean
/// terminations preserved in the audit trail), and `"error"` (every
/// other terminal failure mode — Failed, Timeout, TimedOut, Conflict,
/// SetupFailed, Modified is treated as success-with-caveat).
fn outcome_for_status(status: &DelegationStatus) -> &'static str {
    match status {
        DelegationStatus::Success | DelegationStatus::Modified { .. } => "success",
        DelegationStatus::Cancelled { .. } => "cancelled",
        DelegationStatus::Rejected { .. } => "rejected",
        // `DelegationStatus` is `#[non_exhaustive]`; map every other
        // current and future variant onto "error" so a future
        // success-like variant doesn't silently get summarised here
        // — that audit-trail bug is harder to spot than this default.
        _ => "error",
    }
}

/// Phase 5 / Task 27 — shared helper invoked by `finalize` and the
/// abort-path branches in `handle_delegations` so every terminal exit
/// uses the same flush-then-complete ordering. Drains the
/// per-delegation worker-MCP audit aggregate (which emits the
/// `WorkerMcpDelegationSummary` funnel event), then emits
/// `DelegationCompleted`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn flush_then_emit_completed(
    funnel: &crate::event_funnel::FunnelHandle,
    worker_mcp_servers: &Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>>,
    pm_service: Option<&Arc<PmService>>,
    brain_session_id: &spur_acp::BrainSessionId,
    delegation_id: &str,
    issue_id: Option<&str>,
    worker_session: SessionId,
    final_status: &DelegationStatus,
) {
    flush_worker_mcp_audits(
        worker_mcp_servers,
        pm_service,
        brain_session_id,
        delegation_id,
        issue_id,
        final_status,
    )
    .await;
    funnel.emit(SpurEventBody::DelegationCompleted {
        worker_session,
        status: final_status.clone(),
    });
}

/// Flush the per-delegation worker-MCP audit aggregator before the
/// orchestrator emits `DelegationCompleted`. No-op when the brain has
/// no worker MCP server (delegation dispatched without
/// `enable_worker_mcp`).
///
/// Phase 5 / Task 27. On flush failure: log a warning at
/// `target: "spur.worker_mcp.audit"` AND emit a
/// `WorkerMcpSubkind::FlushFailed` audit sentinel as a beads comment
/// when `pm_service` and `issue_id` are both available, then continue.
/// The `WorkerMcpDelegationSummary` event is emitted by
/// `flush_delegation` itself even on channel-closed errors.
async fn flush_worker_mcp_audits(
    worker_mcp_servers: &Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>>,
    pm_service: Option<&Arc<PmService>>,
    brain_session_id: &spur_acp::BrainSessionId,
    delegation_id: &str,
    issue_id: Option<&str>,
    status: &DelegationStatus,
) {
    let server = match worker_mcp_servers.get(brain_session_id) {
        Some(entry) => Arc::clone(entry.value()),
        None => return,
    };
    let outcome = outcome_for_status(status);
    let Err(error) = server.flush_delegation(delegation_id, outcome).await else {
        return;
    };
    tracing::warn!(
        target: "spur.worker_mcp.audit",
        delegation_id = %delegation_id,
        brain_session_id = %brain_session_id,
        outcome = %outcome,
        error = %error,
        "flush_delegation failed; emitting DelegationCompleted anyway"
    );
    emit_flush_failed_audit_sentinel(pm_service, delegation_id, issue_id, &error).await;
}

/// Emit a `[[spur-audit v1]] worker-mcp/flush-failed` sentinel as a
/// beads comment on the delegation's target issue. Best-effort —
/// missing `pm_service`, missing `issue_id`, advanced unsupported by
/// the active backend, or a beads write failure all degrade silently
/// to a tracing warning. Mirrors the timeout/error handling of
/// `emit_worker_write_audit_inner` so a stuck PM can't stall the
/// terminal `DelegationCompleted` emission.
async fn emit_flush_failed_audit_sentinel(
    pm_service: Option<&Arc<PmService>>,
    delegation_id: &str,
    issue_id: Option<&str>,
    error: &spur_mcp::worker_server::FlushDelegationError,
) {
    let (Some(pm), Some(issue_id)) = (pm_service, issue_id) else {
        return;
    };
    let Some(adv) = pm.advanced() else {
        return;
    };
    let kind = spur_mcp::plan::audit_sentinel::AuditSentinelKind::WorkerMcp {
        delegation_id: delegation_id.to_string(),
        subkind: spur_mcp::plan::audit_sentinel::WorkerMcpSubkind::FlushFailed,
        tool_name: None,
        target_issue_id: Some(issue_id.to_string()),
        error: Some(error.to_string()),
    };
    let body = spur_mcp::plan::audit_sentinel::encode_comment(&kind);
    let timeout = std::time::Duration::from_secs(2);
    match tokio::time::timeout(timeout, adv.add_comment(issue_id, &body)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!(
                target: "spur.worker_mcp.audit",
                delegation_id = %delegation_id,
                issue_id = %issue_id,
                "FlushFailed audit comment emission failed: {e}"
            );
        }
        Err(_) => {
            tracing::warn!(
                target: "spur.worker_mcp.audit",
                delegation_id = %delegation_id,
                issue_id = %issue_id,
                "FlushFailed audit comment emission timed out"
            );
        }
    }
}

#[cfg(test)]
mod flush_ordering_tests {
    //! Phase 5 / Task 27 ordering invariant: when a delegation completes
    //! with a registered worker MCP server, the orchestrator MUST emit
    //! `WorkerMcpDelegationSummary` before `DelegationCompleted` on the
    //! event funnel — including on abort-path branches in
    //! `handle_delegations`. Both the happy `finalize` exit and the
    //! cancellation branches share the `flush_then_emit_completed`
    //! helper, so a single ordering test for each branch type is
    //! sufficient to lock the invariant.
    //!
    //! Outcome-string mapping is also locked here so future tweaks to
    //! `outcome_for_status` can't silently collapse `Cancelled`/`Rejected`
    //! into `"error"` again.
    use super::*;

    use spur_acp::SpurEventBody;
    use spur_license::policy::PolicyResolver;
    use spur_license::FeatureGate;
    use spur_mcp::handlers::PlanResolver;
    use spur_mcp::plan::PlanState;
    use spur_mcp::worker_server::{DelegationContext, WorkerMcpDeps, WorkerMcpServer};
    use spur_mcp::McpEventSink;
    use std::time::Duration;

    struct NullPlanResolver;

    #[async_trait::async_trait]
    impl PlanResolver for NullPlanResolver {
        async fn load_or_project_plan(
            &self,
            plan_id: &str,
        ) -> Result<Arc<tokio::sync::Mutex<PlanState>>, String> {
            Err(format!("test resolver: unknown plan_id '{plan_id}'"))
        }
    }

    async fn build_pm_service(repo: &std::path::Path) -> Arc<spur_pm::PmService> {
        let beads = spur_pm::test_workspace::TestBeadsWorkspace::init();
        let beads_dir = repo.join(".beads");
        std::fs::create_dir_all(&beads_dir).expect("create .beads");
        beads.copy_db_to(&beads_dir);
        Arc::new(
            spur_pm::PmService::try_new(None, true, false, repo, None)
                .await
                .expect("PmService::try_new")
                .expect("expected Some(PmService)"),
        )
    }

    fn make_funnel_sink(funnel: &crate::event_funnel::FunnelHandle) -> Arc<dyn McpEventSink> {
        Arc::new(funnel.clone())
    }

    /// Build a `WorkerMcpServer` registered for the test brain session,
    /// pre-registering `delegation_id` so the per-delegation summary
    /// guard exists.
    async fn make_registered_server(
        funnel: &crate::event_funnel::FunnelHandle,
        repo: &std::path::Path,
        brain_session_id: &spur_acp::BrainSessionId,
        delegation_id: &str,
    ) -> Arc<WorkerMcpServer> {
        let pm = build_pm_service(repo).await;
        let deps = WorkerMcpDeps {
            pm_service: pm,
            feature_gate: Arc::new(FeatureGate::new(PolicyResolver::embedded())),
            funnel: make_funnel_sink(funnel),
            plan_resolver: Arc::new(NullPlanResolver),
            reconciler_outcomes: Arc::new(tokio::sync::Mutex::new(
                spur_mcp::plan::outcomes::OutcomeStore::default(),
            )),
            outcome_store: Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            repo_root: None,
        };
        let server = WorkerMcpServer::start(brain_session_id.to_string(), deps)
            .await
            .expect("worker MCP server starts");
        server.register_delegation(
            delegation_id.to_string(),
            DelegationContext {
                enable_worker_progress: false,
            },
        );
        server
    }

    /// Drain the funnel test channel until both sentinel bodies have
    /// arrived (or the deadline elapses). The funnel forwards via a
    /// spawned task so events are NOT in `body_rx` synchronously after
    /// `emit`.
    async fn drain_until_pair(
        body_rx: &mut tokio::sync::mpsc::UnboundedReceiver<SpurEventBody>,
    ) -> Vec<SpurEventBody> {
        let mut events: Vec<SpurEventBody> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let summary_seen = events
                .iter()
                .any(|e| matches!(e, SpurEventBody::WorkerMcpDelegationSummary { .. }));
            let completed_seen = events
                .iter()
                .any(|e| matches!(e, SpurEventBody::DelegationCompleted { .. }));
            if summary_seen && completed_seen {
                break;
            }
            match tokio::time::timeout_at(deadline, body_rx.recv()).await {
                Ok(Some(body)) => events.push(body),
                Ok(None) | Err(_) => break,
            }
        }
        events
    }

    fn assert_summary_precedes_completed(events: &[SpurEventBody]) {
        let summary_pos = events
            .iter()
            .position(|e| matches!(e, SpurEventBody::WorkerMcpDelegationSummary { .. }))
            .unwrap_or_else(|| panic!("missing summary; events: {events:?}"));
        let completed_pos = events
            .iter()
            .position(|e| matches!(e, SpurEventBody::DelegationCompleted { .. }))
            .unwrap_or_else(|| panic!("missing DelegationCompleted; events: {events:?}"));
        assert!(
            summary_pos < completed_pos,
            "WorkerMcpDelegationSummary must precede DelegationCompleted; events: {events:?}"
        );
    }

    /// `finalize` (the happy-path terminal helper) must drive
    /// `WorkerMcpDelegationSummary` ahead of `DelegationCompleted`
    /// whenever a worker-MCP server is registered for the brain session.
    #[tokio::test]
    async fn finalize_emits_summary_before_delegation_completed() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (funnel, mut body_rx) = crate::event_funnel::test_channel();
        let brain_session_id = spur_acp::BrainSessionId::new(spur_acp::types::SessionId::new());
        let delegation_id = "d-flush-ordering-finalize";

        let server =
            make_registered_server(&funnel, dir.path(), &brain_session_id, delegation_id).await;

        let servers: Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>> =
            Arc::new(DashMap::new());
        servers.insert(brain_session_id.clone(), Arc::clone(&server));

        let _ = finalize(
            &funnel,
            &servers,
            None, // no pm_service in this test
            &brain_session_id,
            delegation_id,
            None,
            spur_acp::types::SessionId::new(),
            DelegationStatus::Success,
            None,
            None,
            None,
            0.0,
            None,
            None,
        )
        .await;

        let events = drain_until_pair(&mut body_rx).await;
        assert_summary_precedes_completed(&events);
    }

    /// Abort-path ordering: `flush_then_emit_completed` (used by the
    /// `handle_delegations` cancellation branches) must emit the summary
    /// before `DelegationCompleted` even when the delegation never
    /// reached `execute_delegation`. Also asserts no panic when the
    /// delegation_guards lock is taken alongside the summary drop.
    #[tokio::test]
    async fn abort_path_emits_summary_before_delegation_completed() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (funnel, mut body_rx) = crate::event_funnel::test_channel();
        let brain_session_id = spur_acp::BrainSessionId::new(spur_acp::types::SessionId::new());
        let delegation_id = "d-flush-ordering-abort";

        let server =
            make_registered_server(&funnel, dir.path(), &brain_session_id, delegation_id).await;

        let servers: Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>> =
            Arc::new(DashMap::new());
        servers.insert(brain_session_id.clone(), Arc::clone(&server));

        let cancelled_status = DelegationStatus::Cancelled {
            reason: "test abort".into(),
        };

        flush_then_emit_completed(
            &funnel,
            &servers,
            None,
            &brain_session_id,
            delegation_id,
            None,
            spur_acp::types::SessionId(delegation_id.to_string()),
            &cancelled_status,
        )
        .await;

        let events = drain_until_pair(&mut body_rx).await;
        assert_summary_precedes_completed(&events);

        // The Cancelled status must not bump the summary's `errors`
        // count — clean termination is not a per-delegation error.
        // Post-br-8gw the WorkerMcpDelegationSummary uses `errors: u64`
        // (per-call + delegation-level) instead of a single `outcome`
        // string, so the contract here is `errors == 0` for a clean
        // Cancelled exit with no per-call errors.
        let errors = events
            .iter()
            .find_map(|e| {
                if let SpurEventBody::WorkerMcpDelegationSummary { errors, .. } = e {
                    Some(*errors)
                } else {
                    None
                }
            })
            .expect("summary present");
        assert_eq!(
            errors, 0,
            "Cancelled does not bump the per-delegation summary's `errors`; events: {events:?}"
        );
    }

    /// Outcome-string contract: `outcome_for_status` must keep the
    /// 4-way mapping `success` / `cancelled` / `rejected` / `error` so
    /// the audit-trail outcome forwarded to `flush_delegation` retains
    /// the clean-termination distinction. Locks the regression that
    /// previously collapsed everything-not-Success to `"error"`.
    #[test]
    fn outcome_for_status_is_four_way() {
        assert_eq!(outcome_for_status(&DelegationStatus::Success), "success");
        assert_eq!(
            outcome_for_status(&DelegationStatus::Modified {
                reviewer_note: "lgtm with caveat".into(),
            }),
            "success"
        );
        assert_eq!(
            outcome_for_status(&DelegationStatus::Cancelled {
                reason: "operator abort".into(),
            }),
            "cancelled"
        );
        assert_eq!(
            outcome_for_status(&DelegationStatus::Rejected {
                reason: "missing tests".into(),
            }),
            "rejected"
        );
        assert_eq!(
            outcome_for_status(&DelegationStatus::Failed {
                error: "exit 1".into(),
            }),
            "error"
        );
        assert_eq!(outcome_for_status(&DelegationStatus::Timeout), "error");
        assert_eq!(
            outcome_for_status(&DelegationStatus::Conflict { files: vec![] }),
            "error"
        );
    }
}
