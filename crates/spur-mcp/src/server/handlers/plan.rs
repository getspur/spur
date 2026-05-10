use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
    pub(crate) async fn handle_merge_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'plan_id'"),
        };

        if let Err((code, message)) = self.check_plan_owner_for_op(&plan_id, "merge_plan").await {
            return JsonRpcResponse::error(id, code, message);
        }

        match self.merge_plan_impl(&plan_id).await {
            Ok(merge_state) => {
                let plan_arc = match self.load_or_project_plan(&plan_id).await {
                    Ok(p) => p,
                    Err(e) => return JsonRpcResponse::invalid_params(id, e),
                };
                {
                    let mut state = plan_arc.lock().await;
                    state.merge_state = merge_state;
                }
                let state = plan_arc.lock().await;
                let status = crate::plan::build_plan_status(&plan_id, &state);
                let text =
                    serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not fully approved yet") || msg.contains("Unknown plan_id") {
                    JsonRpcResponse::invalid_params(id, msg)
                } else {
                    JsonRpcResponse::internal_error(id, msg)
                }
            }
        }
    }

    /// Public bridge for orchestrator/TUI: invoke `resume_plan` and reduce the
    /// JSON-RPC response to a simple Result. Error message is verbatim from the
    /// MCP tool's `JsonRpcError.message`.
    pub async fn call_resume_plan(&self, plan_id: &str) -> Result<(), String> {
        let args = serde_json::json!({ "plan_id": plan_id });
        let resp = self
            .handle_resume_plan_with(serde_json::Value::Null, args, false)
            .await;
        match resp.error {
            Some(e) => Err(e.message),
            None => Ok(()),
        }
    }

    /// Public bridge for orchestrator/TUI: claim ownership for a persisted plan
    /// without starting dispatch. The pending gate keeps the reconciler from
    /// observing ready work until `call_resume_plan` explicitly starts it.
    pub async fn call_claim_plan(&self, plan_id: &str) -> Result<(), String> {
        let pm = self
            .pm_service
            .as_deref()
            .ok_or_else(|| "claim_plan requires PM service".to_string())?;

        let epic_summary =
            find_plan_epic(pm, self.feature_gate.as_ref(), plan_id, "claim_plan").await?;
        let epic_id = epic_summary.id.clone();
        let epic = pm
            .get_issue(&epic_id)
            .await
            .map_err(|error| format!("claim_plan: failed to load epic {epic_id}: {error}"))?;

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                if let Some(active) = self
                    .current_brain_active_owned_plan(pm, Some(plan_id), Some(&epic_id))
                    .await?
                {
                    return Err(format!(
                        "claim_plan: current brain session already owns active plan {} (epic {}); cannot claim different active plan {plan_id}",
                        active.plan_id, active.epic_id
                    ));
                }
                Ok(())
            }
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => Err(format!(
                "claim_plan: plan {plan_id} is owned by {owner}; active handoff is not supported"
            )),
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => Err(format!(
                "claim_plan: plan {plan_id} has ambiguous owner labels: {}",
                owners.join(", ")
            )),
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;
                if let Some(active) = self.current_brain_active_owned_plan(pm, None, None).await? {
                    return Err(format!(
                        "claim_plan: current brain session already owns active plan {} (epic {}); finish it before claiming plan {plan_id}",
                        active.plan_id, active.epic_id
                    ));
                }

                let owner_label =
                    crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
                apply_issue_update(
                    pm,
                    &epic_id,
                    IssueUpdate {
                        add_labels: vec![
                            owner_label.clone(),
                            crate::plan::labels::PLAN_PENDING.to_string(),
                        ],
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| format!("claim_plan: failed to claim plan: {error}"))?;

                let epic = pm.get_issue(&epic_id).await.map_err(|error| {
                    format!("claim_plan: failed to reload claimed epic {epic_id}: {error}")
                })?;
                match crate::plan::ownership::classify_owner(
                    &epic.labels,
                    self.brain_session_id().as_session_id(),
                ) {
                    crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => Ok(()),
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                        let _ = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await;
                        Err(format!(
                            "claim_plan: failed to claim plan {plan_id}; plan is owned by {owner}"
                        ))
                    }
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        let _ = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await;
                        Err(format!(
                            "claim_plan: failed to claim plan {plan_id}; ambiguous owner labels: {}",
                            owners.join(", ")
                        ))
                    }
                    crate::plan::ownership::PlanOwnerMatch::Unowned => Err(format!(
                        "claim_plan: failed to claim plan {plan_id}; plan remains unowned"
                    )),
                }
            }
        }
    }

    /// Public bridge for orchestrator/TUI: project a persisted plan and emit a
    /// `PlanSnapshotUpdated` without claiming ownership or changing plan state.
    pub async fn call_inspect_plan(&self, plan_id: &str) -> Result<(), String> {
        let plan = self.load_or_project_plan(plan_id).await?;
        let state = plan.lock().await;
        crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &state);
        Ok(())
    }

    pub(crate) async fn handle_resume_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_resume_plan_with(id, args, true).await
    }

    pub(crate) async fn handle_resume_plan_with(
        &self,
        id: Value,
        args: Value,
        allow_claim_unowned: bool,
    ) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|value| value.as_str()) {
            Some(plan_id) => plan_id,
            None => return JsonRpcResponse::invalid_params(id, "resume_plan: missing plan_id"),
        };
        let pm = match self.pm_service.as_deref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "resume_plan requires PM service"),
        };

        let epic_summary =
            match find_plan_epic(pm, self.feature_gate.as_ref(), plan_id, "resume_plan").await {
                Ok(epic) => epic,
                Err(error) => {
                    return if error.contains("plan not found") {
                        JsonRpcResponse::error(id, -32004, error)
                    } else if error.contains("ambiguous plan lookup") {
                        JsonRpcResponse::error(id, -32009, error)
                    } else {
                        JsonRpcResponse::internal_error(id, error)
                    }
                }
            };
        let epic_id = epic_summary.id.clone();
        let epic = match pm.get_issue(&epic_id).await {
            Ok(epic) => epic,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("resume_plan: failed to load epic {epic_id}: {error}"),
                )
            }
        };

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                match self.is_projected_plan_nonterminal(plan_id).await {
                    Ok(true) => {
                        match self
                            .current_brain_active_owned_plan(pm, Some(plan_id), Some(&epic_id))
                            .await
                        {
                            Ok(Some(active)) => {
                                return JsonRpcResponse::error(
                                    id,
                                    -32009,
                                    format!(
                                        "resume_plan: current brain session already owns active plan {} (epic {}); cannot resume different active plan {plan_id}",
                                        active.plan_id, active.epic_id
                                    ),
                                );
                            }
                            Ok(None) => {}
                            Err(error) => {
                                return JsonRpcResponse::internal_error(
                                    id,
                                    format!("resume_plan: {error}"),
                                )
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: failed to project plan {plan_id}: {error}"),
                        )
                    }
                }
                let was_pending = epic
                    .labels
                    .iter()
                    .any(|label| label == crate::plan::labels::PLAN_PENDING);
                if was_pending {
                    if let Err(error) = apply_issue_update(
                        pm,
                        &epic_id,
                        IssueUpdate {
                            add_labels: vec![crate::plan::labels::PLAN_COMPLETE.to_string()],
                            remove_labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: failed to start plan: {error}"),
                        );
                    }
                }
                self.fast_forward_reconciler();
                let result = json!({
                    "status": if was_pending { "started" } else { "already_owner" },
                    "plan_id": plan_id,
                    "epic_id": epic_id,
                });
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "resume_plan: plan {plan_id} is owned by {owner}; active handoff is not supported"
                    ),
                )
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "resume_plan: plan {plan_id} has ambiguous owner labels: {}",
                    owners.join(", ")
                ),
            ),
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                if !allow_claim_unowned {
                    return JsonRpcResponse::error(
                        id,
                        -32009,
                        format!("resume_plan: plan {plan_id} is unowned; claim it before starting"),
                    );
                }
                let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;
                match self.current_brain_active_owned_plan(pm, None, None).await {
                    Ok(Some(active)) => {
                        return JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: current brain session already owns active plan {} (epic {}); finish it before claiming plan {plan_id}",
                                active.plan_id, active.epic_id
                            ),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: {error}"),
                        )
                    }
                }
                let owner_label =
                    crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
                if let Err(error) = apply_issue_update(
                    pm,
                    &epic_id,
                    IssueUpdate {
                        add_labels: vec![owner_label.clone()],
                        ..Default::default()
                    },
                )
                .await
                {
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("resume_plan: failed to claim plan: {error}"),
                    );
                }
                self.fast_forward_reconciler();

                let epic = match pm.get_issue(&epic_id).await {
                    Ok(epic) => epic,
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!(
                                "resume_plan: failed to reload claimed epic {epic_id}: {error}"
                            ),
                        )
                    }
                };
                match crate::plan::ownership::classify_owner(
                    &epic.labels,
                    self.brain_session_id().as_session_id(),
                ) {
                    crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                        let result = json!({
                            "status": "claimed",
                            "plan_id": plan_id,
                            "epic_id": epic_id,
                        });
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        JsonRpcResponse::success(
                            id,
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                        if let Err(error) = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await
                        {
                            return JsonRpcResponse::internal_error(
                                id,
                                format!(
                                    "resume_plan: failed to clean up contested owner claim for plan {plan_id}: {error}"
                                ),
                            );
                        }
                        JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: failed to claim plan {plan_id}; plan is owned by {owner}"
                            ),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        if let Err(error) = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await
                        {
                            return JsonRpcResponse::internal_error(
                                id,
                                format!(
                                    "resume_plan: failed to clean up contested owner claim for plan {plan_id}: {error}"
                                ),
                            );
                        }
                        JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: failed to claim plan {plan_id}; ambiguous owner labels: {}",
                                owners.join(", ")
                            ),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::Unowned => JsonRpcResponse::error(
                        id,
                        -32009,
                        format!("resume_plan: failed to claim plan {plan_id}; plan remains unowned"),
                    ),
                }
            }
        }
    }

    /// Operator-initiated force-reclaim. Removes any existing
    /// `spur:plan-owner:*` labels from the plan's epic and stamps the current
    /// brain. Records a `PlanForceReclaimed` audit sentinel with the prior
    /// owner (or `None` if Unowned) and an optional operator-supplied reason.
    /// Refuses unless `confirm: true` is passed.
    pub(crate) async fn handle_force_reclaim_plan(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|v| v.as_str()) {
            Some(plan_id) => plan_id,
            None => {
                return JsonRpcResponse::invalid_params(id, "force_reclaim_plan: missing plan_id")
            }
        };
        let confirm = args
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !confirm {
            return JsonRpcResponse::invalid_params(
                id,
                "force_reclaim_plan: missing or false `confirm`. This tool clobbers any \
                 concurrent owner brain's in-flight state and is intended only for stuck or \
                 dead owners. Re-invoke with `confirm: true` to acknowledge.",
            );
        }
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let pm = match self.pm_service.as_deref() {
            Some(pm) => pm,
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "force_reclaim_plan requires PM service",
                )
            }
        };

        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }

        let epics = match pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(10),
                ..Default::default()
            })
            .await
        {
            Ok(epics) => epics,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("force_reclaim_plan: failed to find plan: {error}"),
                )
            }
        };
        let Some(epic_summary) = epics.first() else {
            return JsonRpcResponse::error(
                id,
                -32004,
                format!("force_reclaim_plan: plan not found: {plan_id}"),
            );
        };
        if epics.len() > 1 {
            let epic_ids = epics
                .iter()
                .map(|epic| epic.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "force_reclaim_plan: ambiguous plan lookup for {plan_id}; multiple epics matched: {epic_ids}"
                ),
            );
        }
        let epic_id = epic_summary.id.clone();
        let epic = match pm.get_issue(&epic_id).await {
            Ok(epic) => epic,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("force_reclaim_plan: failed to load epic {epic_id}: {error}"),
                )
            }
        };

        // Capture prior owner(s) for the audit sentinel BEFORE rewriting labels.
        // The single-owner case yields `Some("<owner>")`; the rare ambiguous
        // multi-owner case preserves the comma-joined list verbatim so operators
        // can see what was clobbered. Unowned → `None`.
        let prior_owners: Vec<String> = epic
            .labels
            .iter()
            .filter_map(|label| {
                crate::plan::labels::parse_plan_owner(label).map(|owner| owner.to_string())
            })
            .collect();
        let prior_owner = match prior_owners.len() {
            0 => None,
            1 => Some(prior_owners[0].clone()),
            _ => Some(prior_owners.join(",")),
        };

        let new_owner_label =
            crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
        let mut remove_labels: Vec<String> = epic
            .labels
            .iter()
            .filter(|label| crate::plan::labels::parse_plan_owner(label).is_some())
            .cloned()
            .collect();
        let add_labels = vec![new_owner_label.clone()];
        filter_remove_labels(&mut remove_labels, &add_labels);

        if let Err(error) = apply_issue_update(
            pm,
            &epic_id,
            IssueUpdate {
                add_labels,
                remove_labels,
                ..Default::default()
            },
        )
        .await
        {
            return JsonRpcResponse::internal_error(
                id,
                format!(
                    "force_reclaim_plan: failed to write owner labels on epic {epic_id}: {error}"
                ),
            );
        }
        self.fast_forward_reconciler();

        let new_owner = self.brain_session_id().to_string();
        let token = uuid::Uuid::new_v4().to_string();

        if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
            return JsonRpcResponse::mcp_error(id, error);
        }
        if let Some(adv) = pm.advanced() {
            let audit = crate::plan::audit_sentinel::AuditSentinelKind::PlanForceReclaimed {
                plan_id: plan_id.to_string(),
                prior_owner: prior_owner.clone(),
                new_owner: new_owner.clone(),
                token: token.clone(),
                reason: reason.clone(),
            };
            let body = crate::plan::audit_sentinel::encode_comment(&audit);
            if let Err(e) = adv.add_comment(&epic_id, &body).await {
                tracing::warn!(
                    target: "spur.audit.emit_failure",
                    kind = "plan_force_reclaimed",
                    epic_id = %epic_id,
                    plan_id = %plan_id,
                    "PlanForceReclaimed audit comment emission failed (owner label is persisted; audit missing): {e}"
                );
            }
        }

        let result = json!({
            "prior_owner": prior_owner,
            "new_owner": new_owner,
            "audit_token": token,
        });
        let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    // ─── Plan execution handlers ──────────────────────────────────

    async fn submit_plan_as_epic_internal(
        &self,
        mut input: SubmitPlanAsEpicInput,
    ) -> Result<SubmitPlanAsEpicResult, String> {
        if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
            return Err(feature_error_message(error));
        }
        let pm = self
            .submit_plan_substrate_pm()
            .ok_or_else(|| "submit_plan: persist_as_epic requires a beads PM backend (configured backend: none)".to_string())?;
        if pm.source_str() != "beads" {
            return Err(format!(
                "submit_plan: persist_as_epic requires a beads PM backend (configured backend: {})",
                pm.source_str()
            ));
        }

        let auto_serialized = match input.precomputed_auto_serialized {
            Some(overlaps) => overlaps,
            None => crate::plan::submit_plan_normalize_tasks(&mut input.tasks)?,
        };

        let plan_id = uuid::Uuid::new_v4().to_string();
        let owner_label =
            crate::plan::labels::plan_owner(&input.brain_session_id.as_session_id().0);
        let epic_title = match input
            .epic_title
            .take()
            .map(|title| title.trim().to_string())
        {
            Some(title) if !title.is_empty() => title,
            _ => {
                let parent_epic_id = input.parent_epic_id.as_deref().ok_or_else(|| {
                    "submit_plan: epic_title is required when persist_as_epic is true".to_string()
                })?;
                let parent = pm.get_issue(parent_epic_id).await.map_err(|error| {
                    format!("submit_plan: failed to load parent epic {parent_epic_id}: {error}")
                })?;
                let branch = match input.base.as_ref() {
                    Some(crate::tools::BaseTarget::Branch { name }) => name.as_str(),
                    _ => "unspecified base",
                };
                format!("{} ({branch})", parent.title)
            }
        };

        let epic_subgraph = build_epic_subgraph_with_activation_labels(
            pm,
            self.feature_gate.as_ref(),
            &plan_id,
            &epic_title,
            input.epic_body.as_deref(),
            &input.tasks,
            input.parent_epic_id.as_deref(),
            vec![owner_label],
        )
        .await?;

        if let Some(adv) = pm.advanced() {
            let audit = crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
                plan_id: plan_id.clone(),
                owner: input.brain_session_id.to_string(),
                token: uuid::Uuid::new_v4().to_string(),
                reason: input.execution_mode.to_string(),
            };
            let body = crate::plan::audit_sentinel::encode_comment(&audit);
            if let Err(e) = adv.add_comment(&epic_subgraph.epic_id, &body).await {
                tracing::warn!(
                    target: "spur.audit.emit_failure",
                    kind = "plan_ownership_acquired",
                    epic_id = %epic_subgraph.epic_id,
                    plan_id = %plan_id,
                    "PlanOwnershipAcquired audit comment emission failed (owner label is persisted; audit missing): {e}"
                );
            }
        }

        let entries = build_entries_with_task_map(input.tasks, Some(&epic_subgraph.task_map));
        let task_count = entries.len();
        let base_snapshot = resolve_plan_base(self.repo_root.as_ref(), input.base.as_ref()).await?;
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: input.brain_session_id.clone(),
            base_snapshot_branch: base_snapshot.branch,
            base_snapshot_oid: base_snapshot.oid,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some(epic_subgraph.epic_id.clone()),
        };
        let state = Arc::new(tokio::sync::Mutex::new(state));

        if let Some(adv) = pm.advanced() {
            let (base_snapshot_branch, base_snapshot_oid) = {
                let state = state.lock().await;
                (
                    state.base_snapshot_branch.clone(),
                    state.base_snapshot_oid.clone(),
                )
            };
            emit_plan_submit_audit(
                adv,
                &plan_id,
                &epic_subgraph,
                PlanSubmitAuditContext {
                    base_snapshot_branch: base_snapshot_branch.as_deref(),
                    base_snapshot_oid: base_snapshot_oid.as_deref(),
                    execution_mode: Some(input.execution_mode),
                    brain_session_id: Some(input.brain_session_id.as_session_id()),
                    explicit_base: input.base.as_ref(),
                },
            )
            .await;
        }

        self.active_plans.lock().await.insert(
            plan_id.clone(),
            CachedPlan::new(Arc::clone(&state), unknown_beads_version()),
        );

        {
            let state = state.lock().await;
            crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &state);
        }
        self.fast_forward_reconciler();

        Ok(SubmitPlanAsEpicResult {
            plan_id,
            task_count,
            auto_serialized,
            epic_subgraph,
        })
    }

    pub(crate) async fn handle_submit_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let client_idempotency_key = match args.get("client_idempotency_key") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_str() {
                Some(key) => {
                    let key = key.trim();
                    if key.is_empty() {
                        return JsonRpcResponse::invalid_params(
                            id,
                            "submit_plan: client_idempotency_key must be non-empty",
                        );
                    }
                    Some(key.to_string())
                }
                None => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        "submit_plan: client_idempotency_key must be a string",
                    )
                }
            },
        };

        if let Some(key) = client_idempotency_key.as_deref() {
            if self.pm_service.as_deref().map(|p| p.source_str()) == Some("beads") {
                let pm = self
                    .pm_service
                    .as_deref()
                    .expect("source check ensures pm exists");
                match crate::submit_plan_dedup::lookup(pm, key).await {
                    Ok(Some(hit)) => {
                        info!(
                            plan_id = %hit.plan_id,
                            dedup_issue_id = %hit.issue_id,
                            "submit_plan: client idempotency key hit"
                        );
                        let response_text = format!(
                            "Plan submitted: idempotency key hit.\n\
                             plan_id: {}\n\
                             Existing persisted plan was returned; no new beads epic was created.",
                            hit.plan_id
                        );
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "continuation_will_fire": true,
                                "auto_serialized": [],
                                "content": [{
                                    "type": "text",
                                    "text": response_text
                                }]
                            }),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        error!("submit_plan: failed to resolve client idempotency key: {error}");
                        return JsonRpcResponse::error(
                            id,
                            -32000,
                            format!(
                                "submit_plan: failed to resolve client idempotency key: {error}"
                            ),
                        );
                    }
                }
            }
        }

        let tasks_val = match args.get("tasks").and_then(|v| v.as_array()) {
            Some(t) => t.clone(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'tasks'"),
        };

        let mut tasks: Vec<crate::plan::PlanTask> = match tasks_val
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(t) => t,
            Err(e) => {
                return JsonRpcResponse::invalid_params(id, format!("Invalid task format: {e}"))
            }
        };

        let auto_serialized = match crate::plan::submit_plan_normalize_tasks(&mut tasks) {
            Ok(overlaps) => overlaps,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        if args.get("persist_as_epic").and_then(|v| v.as_bool()) == Some(false) {
            return JsonRpcResponse::invalid_params(id, PERSIST_AS_EPIC_FALSE_REMOVED_MESSAGE);
        }
        let epic_title = args
            .get("epic_title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(String::from)
            .or_else(|| {
                tasks
                    .first()
                    .map(|task| task.task.trim().chars().take(60).collect::<String>())
                    .filter(|title| !title.is_empty())
            });
        let epic_body = args
            .get("epic_body")
            .and_then(|v| v.as_str())
            .map(String::from);

        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        if epic_title.is_none() {
            return JsonRpcResponse::invalid_params(
                id,
                "submit_plan: cannot derive epic_title - provide epic_title or a non-whitespace tasks[0].task",
            );
        }
        let pm_source = self.submit_plan_substrate_pm().map(|p| p.source_str());
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

        // Parse optional explicit base. Tolerant: `BaseTarget`'s manual
        // Deserialize accepts both `{"kind":...}` and JSON-stringified-object.
        let explicit_base: Option<crate::tools::BaseTarget> = match args.get("base") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => match serde_json::from_value::<crate::tools::BaseTarget>(v.clone()) {
                Ok(target) => Some(target),
                Err(e) => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        format!("submit_plan: invalid 'base' parameter: {e}"),
                    );
                }
            },
        };

        let submitted = match self
            .submit_plan_as_epic_internal(SubmitPlanAsEpicInput {
                tasks,
                base: explicit_base,
                parent_epic_id: None,
                epic_title,
                epic_body,
                brain_session_id: self.brain_session_id().clone(),
                execution_mode: "submit_plan",
                precomputed_auto_serialized: Some(auto_serialized),
            })
            .await
        {
            Ok(submitted) => submitted,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("submit_plan: failed to persist plan as beads epic: {error}"),
                );
            }
        };

        if let Some(key) = client_idempotency_key.as_deref() {
            let pm = self
                .pm_service
                .as_deref()
                .expect("submit_plan persistent path has pm_service");
            if let Err(error) = crate::submit_plan_dedup::record(pm, key, &submitted.plan_id).await
            {
                error!(
                    plan_id = %submitted.plan_id,
                    "submit_plan: failed to record client idempotency key: {error}"
                );
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("submit_plan: failed to record client idempotency key: {error}"),
                );
            }
        }

        info!(plan_id = %submitted.plan_id, tasks = submitted.task_count, "Plan submitted");

        let task_map_json = serde_json::to_string(&submitted.epic_subgraph.task_map)
            .unwrap_or_else(|_| "{}".to_string());
        let response_text = format!(
            "Plan submitted: {task_count} tasks.\n\
             plan_id: {plan_id}\n\
             epic_id: {epic_id} (beads)\n\
             task_map: {task_map_json}\n\
             A continuation will fire on each per-task failure/awaiting-review and on plan completion. \
             Polling get_plan_status remains available as a safety net.",
            task_count = submitted.task_count,
            plan_id = submitted.plan_id,
            epic_id = submitted.epic_subgraph.epic_id,
        );

        let response_text = if submitted.auto_serialized.is_empty() {
            response_text
        } else {
            let edges: Vec<String> = submitted
                .auto_serialized
                .iter()
                .map(|o| {
                    format!(
                        "  {} → {} (shared: {})",
                        o.from,
                        o.to,
                        o.shared_files.join(", ")
                    )
                })
                .collect();
            format!(
                "{response_text}\n\nAuto-serialized {} sibling pair(s) with overlapping context_files:\n{}",
                submitted.auto_serialized.len(),
                edges.join("\n")
            )
        };

        JsonRpcResponse::success(
            id,
            json!({
                "continuation_will_fire": true,
                "auto_serialized": submitted.auto_serialized,
                "content": [{
                    "type": "text",
                    "text": response_text
                }]
            }),
        )
    }

    pub(crate) async fn handle_get_plan_status(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::get_plan_status(self, &self.reconciler_outcomes, &ctx, args).await {
            Ok(status) => {
                let text =
                    serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
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
                JsonRpcResponse::internal_error(id, format!("get_plan_status failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    pub(crate) async fn handle_get_reconciler_status(&self, id: Value) -> JsonRpcResponse {
        let status = self.reconciler_outcomes.lock().await.reconciler_status();
        let text = match serde_json::to_string_pretty(&status) {
            Ok(text) => text,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("failed to serialize reconciler status: {error}"),
                )
            }
        };

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    pub(crate) async fn handle_get_task_diff(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::get_task_diff(
            self.pm_service.as_deref(),
            self.feature_gate.as_ref(),
            self.repo_root.as_deref(),
            self,
            &ctx,
            args,
        )
        .await
        {
            Ok(value) => {
                let text = match serde_json::to_string_pretty(&value) {
                    Ok(t) => t,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("get_task_diff response serialization failed: {e}"),
                        )
                    }
                };
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
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
                JsonRpcResponse::internal_error(id, format!("get_task_diff failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    /// bd-2m2u Phase 2c — `submit_plan_mutation` MCP entry. Builds a
    /// `MutationBatch` from the request payload, runs cycle detection +
    /// rollback via the generic executor, and clears `signal:escalated` labels
    /// on success.
    ///
    /// bd-2m2u Phase 2d — emits `PlanMutationApplied` on success so observers
    /// (TUI, brain, dashboards) follow the recovery without parsing the audit
    /// log directly.
    pub(crate) async fn handle_submit_plan_mutation(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        let pm = match self.pm_service.clone() {
            Some(pm) => pm,
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "submit_plan_mutation: no PM service configured",
                )
            }
        };

        let trigger_task_id = match args.get("trigger_task_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "submit_plan_mutation: trigger_task_id is required",
                )
            }
        };
        let ops_value = match args.get("ops").cloned() {
            Some(v) => v,
            None => {
                return JsonRpcResponse::invalid_params(id, "submit_plan_mutation: ops required")
            }
        };
        let ops: Vec<crate::plan::mutation::PlanMutationOp> =
            match serde_json::from_value(ops_value) {
                Ok(v) => v,
                Err(e) => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        format!("submit_plan_mutation: invalid ops: {e}"),
                    )
                }
            };
        let mutation_id = args
            .get("mutation_id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .unwrap_or_else(uuid::Uuid::new_v4);

        // bd-2m2u Phase 2d — snapshot the op tags + plan_id BEFORE moving
        // the ops into the executor, so we can emit `PlanMutationApplied`
        // on success. Plan id is recovered from the trigger task's beads
        // labels (best effort — observability only).
        let op_tags: Vec<String> = ops
            .iter()
            .map(|op| crate::plan::mutation::op_tag_for(op).to_string())
            .collect();
        let trigger_task_id_for_event = trigger_task_id.clone();

        match crate::plan::mutation_executor::submit_plan_mutation(
            pm.clone(),
            Arc::clone(&self.feature_gate),
            mutation_id,
            trigger_task_id,
            ops,
        )
        .await
        {
            Ok(result) => {
                if let Some(sink) = self.event_sink.as_deref() {
                    let plan_id =
                        derive_plan_id_from_trigger_issue(pm.as_ref(), &trigger_task_id_for_event)
                            .await
                            .unwrap_or_default();
                    sink.emit(spur_acp::SpurEventBody::PlanMutationApplied {
                        plan_id,
                        mutation_id: result.mutation_id.clone(),
                        trigger_task_id: trigger_task_id_for_event,
                        op_tags,
                        affected_task_ids: result.affected_task_ids.clone(),
                    });
                }
                let payload = json!({
                    "mutation_id": result.mutation_id,
                    "children_created": result.children_created,
                    "affected_task_ids": result.affected_task_ids,
                });
                let text =
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(error) => JsonRpcResponse::internal_error(
                id,
                format!("submit_plan_mutation failed: {error:#}"),
            ),
        }
    }

    pub(crate) async fn handle_report_progress(&self, id: Value, args: Value) -> JsonRpcResponse {
        let sink = match self.event_sink.as_deref() {
            Some(sink) => sink,
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "report_progress: event sink not configured",
                )
            }
        };
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::report_progress(sink, &ctx, args).await {
            Ok(value) => {
                let text =
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
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
                JsonRpcResponse::internal_error(id, format!("report_progress failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    pub(crate) async fn handle_preview_task_base(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::PreviewTaskBaseInput = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => return JsonRpcResponse::invalid_params(id, error.to_string()),
        };

        match self.preview_task_base_impl(input).await {
            Ok(output) => match serde_json::to_string_pretty(&output) {
                Ok(text) => JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                ),
                Err(error) => JsonRpcResponse::internal_error(
                    id,
                    format!("failed to serialize preview_task_base response: {error}"),
                ),
            },
            Err(error) => {
                let message = error.to_string();
                if message.starts_with("unknown plan")
                    || message.starts_with("Unknown task_id")
                    || message.contains("Unknown plan_id")
                {
                    JsonRpcResponse::invalid_params(id, message)
                } else {
                    JsonRpcResponse::internal_error(id, message)
                }
            }
        }
    }

    pub(crate) async fn handle_plan_truncate_and_restart(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let input: crate::tool_schemas::PlanTruncateAndRestartInput =
            match serde_json::from_value(args) {
                Ok(input) => input,
                Err(error) => return JsonRpcResponse::invalid_params(id, error.to_string()),
            };

        let repo_root = match self.repo_root.as_ref() {
            Some(root) => root.clone(),
            None => {
                return JsonRpcResponse::internal_error(id, "Repository root not configured");
            }
        };

        let plan_arc = match self.load_or_project_plan(&input.plan_id).await {
            Ok(plan) => plan,
            Err(error) => return JsonRpcResponse::invalid_params(id, error),
        };

        let snapshot = {
            let state = plan_arc.lock().await;
            crate::plan::PlanState {
                plan_id: state.plan_id.clone(),
                tasks: state.tasks.clone(),
                brain_session_id: state.brain_session_id.clone(),
                base_snapshot_branch: state.base_snapshot_branch.clone(),
                base_snapshot_oid: state.base_snapshot_oid.clone(),
                merge_state: state.merge_state.clone(),
                epic_id: state.epic_id.clone(),
            }
        };

        if snapshot.epic_id.is_none() {
            return JsonRpcResponse::invalid_params(
                id,
                "plan_truncate_and_restart requires a persisted parent plan; the supplied plan has no epic_id",
            );
        }

        if !snapshot
            .tasks
            .iter()
            .any(|entry| entry.spec.task_id == input.blocked_task_id)
        {
            return JsonRpcResponse::invalid_params(
                id,
                format!(
                    "Unknown blocked_task_id '{}' in plan '{}'",
                    input.blocked_task_id, input.plan_id
                ),
            );
        }

        let build = match crate::plan::staging::build_staging_branch(&snapshot, &repo_root).await {
            Ok(build) => build,
            Err(error) => return JsonRpcResponse::internal_error(id, error.to_string()),
        };

        let (new_tasks, superseded_task_ids) = crate::plan::staging::shape_new_plan(&snapshot);
        let superseded_set: HashSet<&str> =
            superseded_task_ids.iter().map(String::as_str).collect();

        {
            let mut state = plan_arc.lock().await;
            let mutation_id = uuid::Uuid::new_v4().to_string();
            for entry in state.tasks.iter_mut() {
                if superseded_set.contains(entry.spec.task_id.as_str()) {
                    entry.status = crate::plan::PlanTaskStatus::Superseded {
                        mutation_id: mutation_id.clone(),
                        by: Vec::new(),
                    };
                }
            }
        }

        let submitted = match self
            .submit_plan_as_epic_internal(SubmitPlanAsEpicInput {
                tasks: new_tasks,
                base: Some(crate::tools::BaseTarget::Branch {
                    name: build.branch.clone(),
                }),
                parent_epic_id: snapshot.epic_id.clone(),
                epic_title: None,
                epic_body: Some(format!(
                    "Child recovery plan for `{}` rooted at `{}` after `{}` blocked.",
                    snapshot.plan_id, build.branch, input.blocked_task_id
                )),
                brain_session_id: snapshot.brain_session_id.clone(),
                execution_mode: "plan_truncate_and_restart",
                precomputed_auto_serialized: None,
            })
            .await
        {
            Ok(submitted) => submitted,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("new plan submission failed: {error}"),
                );
            }
        };

        let output = crate::tool_schemas::PlanTruncateAndRestartOutput {
            staging_branch: build.branch,
            superseded_task_ids,
            new_plan_id: submitted.plan_id,
            conflict: build.conflict,
        };

        match serde_json::to_string_pretty(&output) {
            Ok(text) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": text }] }),
            ),
            Err(error) => JsonRpcResponse::internal_error(
                id,
                format!("failed to serialize plan_truncate_and_restart response: {error}"),
            ),
        }
    }

    pub(crate) async fn handle_review_task(
        &self,
        args: &serde_json::Value,
    ) -> Result<String, String> {
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

        let plan_arc = self.load_or_project_plan(&plan_id).await?;

        let sink: Option<&dyn crate::events::McpEventSink> = self.event_sink.as_deref();

        // INV-5: use handle_review_task so the plan lock is dropped before
        // pm.update_issue() is called. Tests may install a MockPm via the
        // substrate abstraction, so route review writes through the same PM
        // surface as the reconciler.
        let pm_arc: Option<std::sync::Arc<dyn crate::plan::PmLike>> = self.reconciler_pm();

        let write_mode = if self.nonadvisory_review_writes {
            crate::plan::ReviewWriteMode::NonAdvisory
        } else {
            crate::plan::ReviewWriteMode::Advisory
        };

        let result = crate::plan::handle_review_task_with_write_mode(
            Arc::clone(&plan_arc),
            &plan_id,
            &task_id,
            decision,
            feedback,
            pm_arc,
            sink,
            Some(&self.delegation_tx),
            Some(&self.task_tracker),
            Arc::clone(&self.feature_gate),
            write_mode,
        )
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if matches!(write_mode, crate::plan::ReviewWriteMode::NonAdvisory) {
                    self.active_plans.lock().await.remove(&plan_id);
                }
                return Err(error);
            }
        };
        let mut result = result;

        if decision == "approve" {
            let clobber_report = self
                .run_clobber_detector_for_review(&plan_arc, &task_id)
                .await?;
            if !clobber_report.signals.is_empty() {
                if let serde_json::Value::Object(ref mut m) = result {
                    m.insert(
                        "signals".into(),
                        serde_json::to_value(&clobber_report.signals).unwrap_or(json!(null)),
                    );
                }
            }
            for warning in clobber_report.warnings {
                append_review_warning(&mut result, warning);
            }
        }

        if let Some(sink) = self.event_sink.as_deref() {
            let projected = self.load_or_project_plan(&plan_id).await?;
            let state = projected.lock().await;
            crate::plan::snapshot::emit_plan_snapshot(Some(sink), &state);
        }

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
include!("plan_tests.rs");
