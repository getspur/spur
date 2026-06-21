use super::*;

/// Plan/reconciler orchestration handles used by the plan engine.
///
/// Stage 2 keeps the engine in `spur-mcp`, but moves the receiver away from
/// `McpCallbackServer` so the state bundle can travel with the engine in the
/// later `spur-core` relocation.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct PlanMcpDeps {
    pub(crate) active_plans: Arc<tokio::sync::Mutex<HashMap<String, CachedPlan>>>,
    pub(crate) plan_registry: Arc<tokio::sync::Mutex<crate::plan::PlanRegistry>>,
    pub(crate) plan_claim_lock: Arc<tokio::sync::Mutex<()>>,
    pub(crate) reconciler_outcomes: Arc<tokio::sync::Mutex<crate::plan::outcomes::OutcomeStore>>,
    pub(crate) pm_service: Option<Arc<PmService>>,
    pub(crate) pm_service_like: Option<Arc<dyn crate::plan::PmLike>>,
    pub(crate) feature_gate: Arc<spur_license::FeatureGate>,
    pub(crate) continuation_ctx: Arc<DetachedContinuationCtx>,
    pub(crate) materializer: OutcomeMaterializer,
    pub(crate) outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    pub(crate) event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    pub(crate) repo_root: Option<std::path::PathBuf>,
    pub(crate) versioned_cache_serve: bool,
    pub(crate) nonadvisory_review_writes: bool,
    pub(crate) dispatch_lease_duration: std::time::Duration,
    pub(crate) auto_merge_approved_plans: bool,
    pub(crate) plan_pending_grace: std::time::Duration,
    pub(crate) reconciler_enabled: bool,
    pub(crate) brain_session_id: Arc<OnceCell<spur_acp::BrainSessionId>>,
    pub(crate) version_churn_epic_for_test: Arc<tokio::sync::Mutex<Option<String>>>,
}

impl PlanMcpDeps {
    pub(crate) fn from_server(server: &McpCallbackServer) -> Self {
        Self {
            active_plans: Arc::clone(&server.active_plans),
            plan_registry: Arc::clone(&server.plan_registry),
            plan_claim_lock: Arc::clone(&server.active_plan_claim_lock),
            reconciler_outcomes: Arc::clone(&server.reconciler_outcomes),
            pm_service: server.pm_service.clone(),
            pm_service_like: server.pm_service_like.clone(),
            feature_gate: Arc::clone(&server.feature_gate),
            continuation_ctx: Arc::clone(&server.continuation_ctx),
            materializer: server.materializer.clone(),
            outcome_store: Arc::clone(&server.outcome_store),
            event_sink: server.event_sink.clone(),
            repo_root: server.repo_root.clone(),
            versioned_cache_serve: server.versioned_cache_serve,
            nonadvisory_review_writes: server.nonadvisory_review_writes,
            dispatch_lease_duration: server.dispatch_lease_duration,
            auto_merge_approved_plans: server.auto_merge_approved_plans,
            plan_pending_grace: server.plan_pending_grace,
            reconciler_enabled: server.reconciler_enabled,
            brain_session_id: Arc::clone(&server.brain_session_id),
            version_churn_epic_for_test: Arc::clone(&server.version_churn_epic_for_test),
        }
    }

    pub(crate) fn brain_session_id(&self) -> &spur_acp::BrainSessionId {
        self.brain_session_id
            .get()
            .expect("brain_session_id must be set before MCP handlers dispatch")
    }

    pub(crate) fn submit_plan_substrate_pm(&self) -> Option<&dyn crate::plan::PmLike> {
        if let Some(pm) = self.pm_service_like.as_deref() {
            return Some(pm);
        }
        self.pm_service
            .as_deref()
            .map(|pm| pm as &dyn crate::plan::PmLike)
    }

    pub(crate) async fn check_plan_owner_for_op(
        &self,
        plan_id: &str,
        op_name: &str,
    ) -> Result<(), (i64, String)> {
        let Some(pm) = self.pm_service.as_deref() else {
            return Ok(());
        };

        let epics = pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(10),
                ..Default::default()
            })
            .await
            .map_err(|error| (-32603, format!("{op_name}: failed to find plan: {error}")))?;

        let Some(epic_summary) = epics.first() else {
            return Err((-32004, format!("{op_name}: plan not found: {plan_id}")));
        };
        if epics.len() > 1 {
            let epic_ids = epics
                .iter()
                .map(|epic| epic.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err((
                -32009,
                format!(
                    "{op_name}: ambiguous plan lookup for {plan_id}; multiple epics matched: {epic_ids}"
                ),
            ));
        }
        let epic_id = epic_summary.id.clone();
        let epic = pm.get_issue(&epic_id).await.map_err(|error| {
            (
                -32603,
                format!("{op_name}: failed to load epic {epic_id}: {error}"),
            )
        })?;

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => Ok(()),
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => Err((
                -32009,
                format!(
                    "{op_name}: plan {plan_id} is owned by {owner}; active handoff is not implemented in MVP"
                ),
            )),
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => Err((
                -32009,
                format!(
                    "{op_name}: plan {plan_id} has ambiguous owner labels: {}",
                    owners.join(", ")
                ),
            )),
            crate::plan::ownership::PlanOwnerMatch::Unowned => Err((
                -32009,
                format!(
                    "{op_name}: plan {plan_id} is unowned; claim it via execute_epic or resume_plan first"
                ),
            )),
        }
    }

    pub(crate) async fn projected_plan_status(
        &self,
        plan_id: &str,
    ) -> Result<serde_json::Value, String> {
        let plan_arc = self.load_or_project_plan(plan_id).await?;
        let state = plan_arc.lock().await;
        Ok(crate::plan::build_plan_status(plan_id, &state))
    }

    pub(crate) async fn is_projected_plan_nonterminal(
        &self,
        plan_id: &str,
    ) -> Result<bool, String> {
        let status = self.projected_plan_status(plan_id).await?;
        let overall = status
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        Ok(!crate::plan::is_terminal_plan_status(overall))
    }

    /// Single-active-plan-per-brain quota check. Layered ON TOP of plan-scoped
    /// ownership: this assumes ownership labels are already maintained correctly
    /// (per main's plan-scoped system) and enforces that any one brain holds at
    /// most one non-terminal owned plan at a time.
    pub(crate) async fn current_brain_active_owned_plan(
        &self,
        pm: &dyn crate::plan::PmLike,
        exempt_plan_id: Option<&str>,
        exempt_epic_id: Option<&str>,
    ) -> Result<Option<ActiveOwnedPlan>, String> {
        let owner_label =
            crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
        let epics = pm
            .list_issues(IssueFilter {
                labels: vec![owner_label],
                status: Some("open".to_string()),
                issue_type: Some("epic".to_string()),
                include_closed: false,
                limit: Some(10_000),
                ..Default::default()
            })
            .await
            .map_err(|error| format!("failed to scan active owned plans: {error}"))?;

        for epic_summary in epics {
            let epic_id = epic_summary.id;
            let epic = pm
                .get_issue(&epic_id)
                .await
                .map_err(|error| format!("failed to load owned plan epic {epic_id}: {error}"))?;
            let plan_ids = epic
                .labels
                .iter()
                .filter_map(|label| crate::plan::labels::parse_plan_id(label))
                .collect::<HashSet<_>>();

            for plan_id in plan_ids {
                if exempt_plan_id == Some(plan_id) || exempt_epic_id == Some(epic_id.as_str()) {
                    continue;
                }
                if self.is_projected_plan_nonterminal(plan_id).await? {
                    return Ok(Some(ActiveOwnedPlan {
                        plan_id: plan_id.to_string(),
                        epic_id: epic_id.clone(),
                    }));
                }
            }
        }

        Ok(None)
    }

    pub(crate) async fn nonterminal_plan_status_for_epic(
        &self,
        pm: &dyn crate::plan::PmLike,
        epic_id: &str,
    ) -> Result<Option<(String, serde_json::Value)>, String> {
        let epic = pm
            .get_issue(epic_id)
            .await
            .map_err(|error| format!("failed to load epic {epic_id}: {error}"))?;
        let plan_ids = epic
            .labels
            .iter()
            .filter_map(|label| crate::plan::labels::parse_plan_id(label))
            .collect::<HashSet<_>>();
        let mut active = Vec::new();

        for plan_id in plan_ids {
            let status = self.projected_plan_status(plan_id).await?;
            let overall = status
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            if !crate::plan::is_terminal_plan_status(overall) {
                active.push((plan_id.to_string(), status));
            }
        }

        match active.len() {
            0 => Ok(None),
            1 => Ok(active.into_iter().next()),
            _ => Err(format!(
                "epic {epic_id} has multiple nonterminal plans: {}",
                active
                    .iter()
                    .map(|(plan_id, _)| plan_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    pub(crate) async fn install_projected_plan(
        &self,
        projected: crate::plan::PlanState,
        emit_snapshot: bool,
    ) {
        let plan_id = projected.plan_id.clone();
        if let Some(epic_id) = projected.epic_id.clone() {
            self.plan_registry
                .lock()
                .await
                .by_epic
                .insert(epic_id, plan_id.clone());
        }
        if emit_snapshot {
            crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &projected);
        }
        self.install_projected_plan_with_version(projected, unknown_beads_version())
            .await;
    }

    pub(crate) async fn install_projected_plan_with_version(
        &self,
        projected: crate::plan::PlanState,
        beads_version: BeadsVersion,
    ) -> Arc<tokio::sync::Mutex<crate::plan::PlanState>> {
        let plan_id = projected.plan_id.clone();
        if let Some(epic_id) = projected.epic_id.clone() {
            self.plan_registry
                .lock()
                .await
                .by_epic
                .insert(epic_id, plan_id.clone());
        }
        let state = Arc::new(tokio::sync::Mutex::new(projected));
        self.active_plans
            .lock()
            .await
            .insert(plan_id, CachedPlan::new(Arc::clone(&state), beads_version));
        state
    }

    pub(crate) async fn refresh_unversioned_cached_plan(
        &self,
        plan_id: &str,
    ) -> Result<crate::handlers::ResolvedPlanState, String> {
        let pm = self
            .submit_plan_substrate_pm()
            .ok_or_else(|| format!("unknown plan '{plan_id}'"))?;
        let projected = crate::plan::projector::project_plan_from_beads(
            pm,
            plan_id,
            self.feature_gate.as_ref(),
        )
        .await
        .map_err(|error| format!("unknown plan '{plan_id}': {error}"))?;
        let state = self
            .install_projected_plan_with_version(projected, unknown_beads_version())
            .await;
        Ok(crate::handlers::ResolvedPlanState {
            state,
            freshness: crate::handlers::PlanStateFreshness::Projection,
        })
    }

    pub(crate) async fn maybe_churn_beads_version_for_test(
        &self,
        epic_id: &str,
    ) -> Result<(), String> {
        let churn_epic = self.version_churn_epic_for_test.lock().await.clone();
        if churn_epic.as_deref() != Some(epic_id) {
            return Ok(());
        }
        let pm = self
            .pm_service
            .as_deref()
            .ok_or_else(|| "test version churn requires PM service".to_string())?;
        require_feature(
            FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )
        .map_err(feature_error_message)?;
        let advanced = pm
            .advanced()
            .ok_or_else(|| "test version churn requires beads advanced backend".to_string())?;
        advanced
            .add_comment(
                epic_id,
                &crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
                        plan_id: "test-version-churn".into(),
                        owner: "test".into(),
                        token: uuid::Uuid::new_v4().to_string(),
                        reason: "versioned-cache-retry-bound".into(),
                    },
                ),
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("test version churn failed: {error}"))
    }

    pub(crate) async fn beads_version_for_epic(
        &self,
        epic_id: &str,
    ) -> Result<BeadsVersion, String> {
        self.maybe_churn_beads_version_for_test(epic_id).await?;
        let pm = self.pm_service.as_deref().ok_or_else(|| {
            format!("beads version unavailable for epic '{epic_id}': PM service not configured")
        })?;
        Self::derive_beads_version(pm, self.feature_gate.as_ref(), epic_id).await
    }

    pub(crate) async fn derive_beads_version(
        pm: &spur_pm::PmService,
        feature_gate: &spur_license::FeatureGate,
        epic_id: &str,
    ) -> Result<BeadsVersion, String> {
        require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate,
        )
        .map_err(feature_error_message)?;
        let adv = pm
            .advanced()
            .ok_or_else(|| "beads version derivation requires beads backend".to_string())?;
        let comments = adv
            .list_comments(epic_id)
            .await
            .map_err(|error| format!("list_comments({epic_id}) failed: {error}"))?;
        let epic_issue = pm
            .get_issue(epic_id)
            .await
            .map_err(|error| format!("get_issue({epic_id}) failed: {error}"))?;
        let plan_id = epic_issue
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_plan_id(label));
        let Some(plan_id) = plan_id else {
            return Ok(BeadsVersion::AuditSeq(
                crate::plan::projector::sort_projection_comments(comments)
                    .into_iter()
                    .filter(|comment| {
                        comment
                            .body
                            .starts_with(crate::plan::audit_sentinel::SENTINEL_PREFIX)
                    })
                    .count() as u64,
            ));
        };

        // Option B (content-addressed): derive a cache token from the sorted
        // set of plan-scoped audit comment IDs. This avoids additive-count
        // collisions across plan restarts and aligns issue discovery with the
        // projector (scan by `spur:plan-id:<id>` label).
        let mut summary_by_id = HashMap::new();
        for status in [
            Some("open".to_string()),
            Some("in_progress".to_string()),
            Some(pm.closed_status().to_string()),
        ] {
            for summary in pm
                .list_issues(IssueFilter {
                    labels: vec![crate::plan::labels::plan_id(plan_id)],
                    status,
                    limit: Some(1_000),
                    ..Default::default()
                })
                .await
                .map_err(|error| format!("list_issues(plan={plan_id}) failed: {error}"))?
            {
                summary_by_id.insert(summary.id.clone(), summary);
            }
        }
        let mut issue_ids: Vec<String> = summary_by_id.into_keys().collect();
        issue_ids.sort();

        let comments_by_issue =
            futures::future::try_join_all(issue_ids.iter().map(|issue_id| async move {
                adv.list_comments(issue_id)
                    .await
                    .map(|comments| (issue_id.clone(), comments))
            }))
            .await
            .map_err(|error| format!("list_comments(plan={plan_id}) failed: {error}"))?;

        let mut audit_keys = Vec::new();
        for (issue_id, comments) in comments_by_issue {
            for comment in crate::plan::projector::sort_projection_comments(comments) {
                if !comment
                    .body
                    .starts_with(crate::plan::audit_sentinel::SENTINEL_PREFIX)
                {
                    continue;
                }
                if let Some(Err(error)) = crate::plan::audit_sentinel::parse_comment(&comment.body)
                {
                    tracing::warn!(
                        %plan_id,
                        %issue_id,
                        comment_id = %comment.id,
                        %error,
                        "malformed audit sentinel included in beads version hash"
                    );
                }
                audit_keys.push((issue_id.clone(), comment.id));
            }
        }
        audit_keys.sort();

        let mut hasher = Sha256::new();
        for (issue_id, comment_id) in audit_keys {
            hasher.update(issue_id.as_bytes());
            hasher.update([0_u8]);
            hasher.update(comment_id.as_bytes());
            hasher.update([0_u8]);
        }
        let digest = hasher.finalize();
        let mut hash = [0_u8; 32];
        hash.copy_from_slice(&digest);
        Ok(BeadsVersion::ContentHash(hash))
    }

    pub(crate) async fn project_plan_from_beads_with_stable_version(
        &self,
        pm: &spur_pm::PmService,
        plan_id: &str,
    ) -> Result<(crate::plan::PlanState, BeadsVersion), String> {
        let epic = find_plan_epic(
            pm,
            self.feature_gate.as_ref(),
            plan_id,
            "load_or_project_plan",
        )
        .await?;

        for (attempt, backoff) in VERSIONED_PLAN_CACHE_BACKOFFS.iter().enumerate() {
            let before_version = self.beads_version_for_epic(&epic.id).await?;
            let projected = crate::plan::projector::project_plan_from_beads(
                pm,
                plan_id,
                self.feature_gate.as_ref(),
            )
            .await
            .map_err(|error| format!("unknown plan '{plan_id}': {error}"))?;
            let after_version = self.beads_version_for_epic(&epic.id).await?;
            if before_version == after_version {
                return Ok((projected, after_version));
            }

            tracing::debug!(
                %plan_id,
                epic_id = %epic.id,
                attempt = attempt + 1,
                before_version = ?before_version,
                after_version = ?after_version,
                "persisted plan changed during projection; retrying"
            );

            if attempt + 1 < VERSIONED_PLAN_CACHE_MAX_ATTEMPTS {
                tokio::time::sleep(*backoff).await;
            }
        }

        Err(format!(
            "load_or_project_plan: plan '{plan_id}' changed during projection after {VERSIONED_PLAN_CACHE_MAX_ATTEMPTS} attempts"
        ))
    }

    pub(crate) async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::plan::PlanState>>, String> {
        Ok(self
            .load_or_project_plan_with_freshness(plan_id)
            .await?
            .state)
    }

    pub(crate) async fn load_or_project_plan_with_freshness(
        &self,
        plan_id: &str,
    ) -> Result<crate::handlers::ResolvedPlanState, String> {
        let cached = self.active_plans.lock().await.get(plan_id).cloned();
        if let Some(existing) = cached.clone() {
            let (epic_id, has_nonterminal_tasks) = {
                let state = existing.state.lock().await;
                (
                    state.epic_id.clone(),
                    state.tasks.iter().any(|task| !task.status.is_terminal()),
                )
            };
            if self.versioned_cache_serve {
                if let Some(epic_id) = epic_id {
                    let current_version = self.beads_version_for_epic(&epic_id).await?;
                    if current_version == existing.beads_version {
                        return Ok(crate::handlers::ResolvedPlanState {
                            state: existing.state,
                            freshness: crate::handlers::PlanStateFreshness::Cache {
                                beads_version_verified: true,
                                cached_age_ms: existing.cached_at.elapsed().as_millis() as u64,
                            },
                        });
                    }
                    tracing::debug!(
                        %plan_id,
                        %epic_id,
                        cached_age_ms = existing.cached_at.elapsed().as_millis(),
                        cached_version = ?existing.beads_version,
                        current_version = ?current_version,
                        "persisted plan cache version mismatch; re-projecting from beads"
                    );
                }
            } else {
                if epic_id.is_some()
                    && has_nonterminal_tasks
                    && existing.cached_at.elapsed() >= UNVERSIONED_PLAN_CACHE_REFRESH_AFTER
                {
                    match tokio::time::timeout(
                        UNVERSIONED_PLAN_CACHE_INLINE_REFRESH_TIMEOUT,
                        self.refresh_unversioned_cached_plan(plan_id),
                    )
                    .await
                    {
                        Ok(Ok(resolved)) => return Ok(resolved),
                        Ok(Err(error)) => {
                            tracing::warn!(
                                %plan_id,
                                %error,
                                "stale unversioned plan cache refresh failed; serving cache"
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                %plan_id,
                                timeout_ms = UNVERSIONED_PLAN_CACHE_INLINE_REFRESH_TIMEOUT
                                    .as_millis(),
                                "stale unversioned plan cache refresh timed out; serving cache"
                            );
                        }
                    }
                }
                return Ok(crate::handlers::ResolvedPlanState {
                    state: existing.state,
                    freshness: crate::handlers::PlanStateFreshness::Cache {
                        beads_version_verified: false,
                        cached_age_ms: existing.cached_at.elapsed().as_millis() as u64,
                    },
                });
            }
        }

        let pm = self
            .submit_plan_substrate_pm()
            .ok_or_else(|| format!("unknown plan '{plan_id}'"))?;
        if self.versioned_cache_serve {
            if let Some(pm_service) = self.pm_service.as_deref() {
                let (projected, beads_version) = self
                    .project_plan_from_beads_with_stable_version(pm_service, plan_id)
                    .await?;
                let state = self
                    .install_projected_plan_with_version(projected, beads_version)
                    .await;
                return Ok(crate::handlers::ResolvedPlanState {
                    state,
                    freshness: crate::handlers::PlanStateFreshness::Projection,
                });
            }
        }

        let projected = crate::plan::projector::project_plan_from_beads(
            pm,
            plan_id,
            self.feature_gate.as_ref(),
        )
        .await
        .map_err(|error| format!("unknown plan '{plan_id}': {error}"))?;
        let state = self
            .install_projected_plan_with_version(projected, unknown_beads_version())
            .await;
        Ok(crate::handlers::ResolvedPlanState {
            state,
            freshness: crate::handlers::PlanStateFreshness::Projection,
        })
    }

    pub(crate) async fn merge_plan_impl(
        &self,
        plan_id: &str,
    ) -> anyhow::Result<crate::plan::PlanMergeState> {
        let repo_root = match self.repo_root.as_ref() {
            Some(root) => root.clone(),
            None => anyhow::bail!("Repository root not configured; merge_plan is unavailable"),
        };

        let had_cached_entry = self.active_plans.lock().await.contains_key(plan_id);
        let plan_arc = match self.load_or_project_plan(plan_id).await {
            Ok(plan_arc) => plan_arc,
            Err(_) => anyhow::bail!("Unknown plan_id: '{plan_id}'"),
        };

        let (base_snapshot_branch, base_snapshot_oid, tasks, merge_state, epic_id) = {
            let state = plan_arc.lock().await;
            let status = crate::plan::build_plan_status(plan_id, &state);
            let ready = status
                .get("ready_to_merge")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !ready {
                anyhow::bail!("plan '{plan_id}' is not fully approved yet");
            }
            (
                state.base_snapshot_branch.clone(),
                state.base_snapshot_oid.clone(),
                state.tasks.clone(),
                state.merge_state.clone(),
                state.epic_id.clone(),
            )
        };

        if !matches!(merge_state, crate::plan::PlanMergeState::NotStarted) {
            return Ok(merge_state);
        }

        let persisted_bootstrap = if !had_cached_entry {
            match (self.pm_service.as_deref(), epic_id.as_deref()) {
                (Some(pm), Some(epic_id)) => {
                    match read_persisted_plan_bootstrap(
                        pm,
                        self.feature_gate.as_ref(),
                        plan_id,
                        epic_id,
                    )
                    .await
                    {
                        Ok(bootstrap) => Some(bootstrap),
                        Err(error) => anyhow::bail!(error),
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        let base_snapshot_ref = match persisted_bootstrap
            .as_ref()
            .and_then(PersistedPlanBootstrap::preferred_base_ref)
            .map(str::to_string)
            .or(base_snapshot_oid)
            .or(base_snapshot_branch)
        {
            Some(reference) => reference,
            None => anyhow::bail!(
                "plan '{plan_id}' has no captured base snapshot; resubmit the plan before calling merge_plan"
            ),
        };

        let task_specs: Vec<crate::plan::PlanTask> =
            tasks.iter().map(|entry| entry.spec.clone()).collect();
        let order = topological_order(&task_specs).map_err(|e| anyhow::anyhow!(e))?;

        let mut ordered_branches = Vec::with_capacity(order.len());
        for idx in order {
            let entry = &tasks[idx];
            if !matches!(entry.status, crate::plan::PlanTaskStatus::Approved { .. }) {
                anyhow::bail!(
                    "plan '{plan_id}' became non-approved while merge_plan was preparing task '{}'",
                    entry.spec.task_id
                );
            }
            let Some(worker_branch) = entry.worker_branch.clone() else {
                anyhow::bail!(
                    "approved task '{}' has no worker_branch; cannot integrate plan",
                    entry.spec.task_id
                );
            };
            ordered_branches.push((entry.spec.task_id.clone(), worker_branch));
        }

        let merge_branch = format!(
            "spur/plan-merge-{plan_id}-{}",
            uuid::Uuid::new_v4().simple()
        );

        let merge_state = match integrate_plan_branches(
            &repo_root,
            &base_snapshot_ref,
            &merge_branch,
            &ordered_branches,
        )
        .await
        {
            Ok(state) => state,
            Err(error) => crate::plan::PlanMergeState::Failed { error },
        };
        let merged_successfully =
            matches!(merge_state, crate::plan::PlanMergeState::Succeeded { .. });

        {
            let mut state = plan_arc.lock().await;
            state.merge_state = merge_state.clone();
        }

        if merged_successfully {
            if let (Some(pm), Some(epic_id)) = (self.pm_service.as_ref(), epic_id.as_deref()) {
                if let Err(error) = apply_issue_update(
                    pm.as_ref(),
                    epic_id,
                    spur_pm::IssueUpdate {
                        remove_labels: vec![crate::plan::labels::INTEGRATION_PENDING.to_string()],
                        ..Default::default()
                    },
                )
                .await
                {
                    tracing::warn!(
                        plan_id = %plan_id,
                        epic_id = %epic_id,
                        "failed to clear integration-pending on epic after merge: {error}"
                    );
                }
            }
        }

        Ok(merge_state)
    }

    pub(crate) async fn create_pr_impl(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.pm_service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No PR service configured"))?
            .create_pr(params)
            .await
    }
}

#[async_trait::async_trait]
impl crate::handlers::PlanResolver for PlanMcpDeps {
    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::plan::PlanState>>, String> {
        PlanMcpDeps::load_or_project_plan(self, plan_id).await
    }

    async fn load_or_project_plan_with_freshness(
        &self,
        plan_id: &str,
    ) -> Result<crate::handlers::ResolvedPlanState, String> {
        PlanMcpDeps::load_or_project_plan_with_freshness(self, plan_id).await
    }
}

#[async_trait::async_trait]
impl crate::plan::reconciler::ReconcilerAutomation for PlanMcpDeps {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState> {
        self.merge_plan_impl(plan_id).await
    }

    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.create_pr_impl(params).await
    }
}

impl McpCallbackServer {
    pub(crate) fn plan_mcp_deps(&self) -> PlanMcpDeps {
        PlanMcpDeps::from_server(self)
    }
}
