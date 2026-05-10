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

    pub(crate) async fn handle_execute_epic(&self, id: Value, args: Value) -> JsonRpcResponse {
        // 1. Extract required epic_id.
        let epic_id = match args.get("epic_id").and_then(|v| v.as_str()) {
            Some(e) => e.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "missing required field: epic_id"),
        };
        let default_agent = args
            .get("default_agent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // 2. Require PmService.
        // Unit-tested via integration-level fixtures only; the PmService gate is
        // the first check in handle_execute_epic and its error message matches
        // this literal: "beads (PmService) is not configured — cannot execute epic".
        let pm = match self.pm_service.as_deref() {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    "beads (PmService) is not configured — cannot execute epic",
                )
            }
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }

        let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;

        // Owner-classification gate. Refuses takeover (OwnedByOther) and
        // ambiguous owner labels before reserving the registry slot. Unowned
        // (claim) and OwnedByCurrent (re-issue) proceed. The
        // PlanOwnershipTransferred audit branch downstream stays intact as
        // defense-in-depth for a future force-reclaim path.
        // Fail fast on PM fetch error so a transient beads outage cannot
        // silently bypass the gate (mirrors check_plan_owner_for_op).
        let epic_issue = match pm.get_issue(&epic_id).await {
            Ok(issue) => issue,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32603,
                    format!("execute_epic: failed to load epic {epic_id}: {error}"),
                );
            }
        };
        if let Some(existing_plan_id) = persisted_plan_epic_plan_id(&epic_issue) {
            return JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "execute_epic: epic {epic_id} is already a persisted plan epic for plan {existing_plan_id}; use claim/start/resume plan instead"
                ),
            );
        }
        match crate::plan::ownership::classify_owner(
            &epic_issue.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::Unowned
            | crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {}
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                return JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "execute_epic: epic {epic_id} is owned by {owner}; active handoff is not implemented in MVP"
                    ),
                );
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                return JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "execute_epic: epic {epic_id} has ambiguous owner labels: {}",
                        owners.join(", ")
                    ),
                );
            }
        }

        // Sentinel value used to reserve a registry slot while the PmService
        // fetch is in flight. Concurrent callers that see this value return an
        // "already in progress" error instead of racing into double-dispatch.
        const PENDING_SENTINEL: &str = "__pending__";

        // 3. Idempotency + reservation: under a single lock acquisition,
        //    either return the existing non-terminal plan, reserve the slot
        //    with a sentinel (and fall through to the fetch), or clear a
        //    stale/terminal entry and reserve.
        {
            let mut registry = self.plan_registry.lock().await;
            match registry.by_epic.get(&epic_id).cloned() {
                Some(ref existing) if existing == PENDING_SENTINEL => {
                    // A concurrent call is already in the fetch/derive phase.
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!(
                            "execute_epic for epic '{epic_id}' is already in progress — \
                             wait for it to complete and call get_plan_status"
                        ),
                    );
                }
                Some(existing_plan_id) => {
                    // Check if the existing plan is still non-terminal.
                    // Persisted plans must be reprojected here so stale cache
                    // state cannot block a legitimate rerun.
                    drop(registry);
                    let plan_arc = self.load_or_project_plan(&existing_plan_id).await.ok();
                    if let Some(arc) = plan_arc {
                        let state = arc.lock().await;
                        let status_val = crate::plan::build_plan_status(&existing_plan_id, &state);
                        let overall = status_val
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        if !crate::plan::is_terminal_plan_status(overall) {
                            // Return existing plan status.
                            let mut resp_val = status_val;
                            if let serde_json::Value::Object(ref mut m) = resp_val {
                                m.insert("epic_id".into(), serde_json::json!(epic_id));
                                m.insert(
                                    "next_action".into(),
                                    serde_json::json!(
                                        "Plan already active for this epic. \
                                         Poll with get_plan_status(plan_id) to monitor progress."
                                    ),
                                );
                            }
                            let text = serde_json::to_string_pretty(&resp_val)
                                .unwrap_or_else(|_| resp_val.to_string());
                            return JsonRpcResponse::success(
                                id,
                                json!({ "content": [{ "type": "text", "text": text }] }),
                            );
                        }
                        // Terminal plan — fall through to start a fresh one.
                        // Re-acquire the registry lock to insert the sentinel.
                    }
                    // Plan not found in active_plans (evicted or never inserted)
                    // or was terminal — reserve the slot now.
                    self.plan_registry
                        .lock()
                        .await
                        .by_epic
                        .insert(epic_id.clone(), PENDING_SENTINEL.into());
                }
                None => {
                    // No entry at all — reserve the slot.
                    registry
                        .by_epic
                        .insert(epic_id.clone(), PENDING_SENTINEL.into());
                }
            }
        }

        match self.nonterminal_plan_status_for_epic(pm, &epic_id).await {
            Ok(Some((existing_plan_id, status_val))) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                self.plan_registry
                    .lock()
                    .await
                    .by_epic
                    .insert(epic_id.clone(), existing_plan_id);

                let mut resp_val = status_val;
                if let serde_json::Value::Object(ref mut m) = resp_val {
                    m.insert("epic_id".into(), serde_json::json!(epic_id));
                    m.insert(
                        "next_action".into(),
                        serde_json::json!(
                            "Plan already active for this epic. \
                             Poll with get_plan_status(plan_id) to monitor progress."
                        ),
                    );
                }
                let text = serde_json::to_string_pretty(&resp_val)
                    .unwrap_or_else(|_| resp_val.to_string());
                return JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                );
            }
            Ok(None) => {}
            Err(error) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(id, format!("execute_epic: {error}"));
            }
        }

        match self
            .current_brain_active_owned_plan(pm, None, Some(&epic_id))
            .await
        {
            Ok(Some(active)) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "execute_epic: current brain session already owns active plan {} (epic {}); finish it before executing epic {epic_id}",
                        active.plan_id, active.epic_id
                    ),
                );
            }
            Ok(None) => {}
            Err(error) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(id, format!("execute_epic: {error}"));
            }
        }

        // 4. Derive the plan from the epic subgraph via PmService.
        let known_agent_names: Vec<String> = self.workers.iter().map(|w| w.name.clone()).collect();
        let known_agents_refs: Vec<&str> = known_agent_names.iter().map(String::as_str).collect();

        let derived = match crate::plan::derive_epic_plan(
            pm,
            self.feature_gate.as_ref(),
            &epic_id,
            default_agent.as_deref(),
            &known_agents_refs,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => {
                // Clear the sentinel so callers can retry after fixing the issue.
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::error(id, -32000, e);
            }
        };

        // 5. Build PlanState and spawn the plan — mirrors handle_submit_plan exactly.
        let plan_id = uuid::Uuid::new_v4().to_string();
        let entries: Vec<crate::plan::PlanTaskEntry> = derived
            .plan_tasks
            .into_iter()
            .map(|spec| crate::plan::PlanTaskEntry {
                spec,
                status: crate::plan::PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            })
            .collect();

        let task_count = entries.len();
        let base_snapshot = match resolve_plan_base(self.repo_root.as_ref(), None).await {
            Ok(snapshot) => snapshot,
            Err(e) => {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(id, e);
            }
        };
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id().clone(),
            base_snapshot_branch: base_snapshot.branch,
            base_snapshot_oid: base_snapshot.oid,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some(epic_id.clone()),
        };
        let state = Arc::new(tokio::sync::Mutex::new(state));

        // Keep a clone of the Arc to build the initial status response.
        let state_for_status = Arc::clone(&state);

        let (task_scope, base_snapshot_branch, base_snapshot_oid) = {
            let state = state_for_status.lock().await;
            let task_scope = state
                .tasks
                .iter()
                .filter_map(|entry| {
                    entry.spec.issue_id.as_ref().map(|issue_id| {
                        (
                            issue_id.clone(),
                            entry.spec.task_id.clone(),
                            entry.spec.agent.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            (
                task_scope,
                state.base_snapshot_branch.clone(),
                state.base_snapshot_oid.clone(),
            )
        };

        let mut rollback_updates: Vec<(String, spur_pm::IssueUpdate)> = Vec::new();
        let mut prior_owner_match: Option<crate::plan::ownership::PlanOwnerMatch> = None;
        if let Ok(epic_issue) = pm.get_issue(&epic_id).await {
            prior_owner_match = Some(crate::plan::ownership::classify_owner(
                &epic_issue.labels,
                self.brain_session_id().as_session_id(),
            ));
            let mut remove_labels = Vec::new();
            let owner_label =
                crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
            for label in &epic_issue.labels {
                if crate::plan::labels::parse_plan_id(label).is_some()
                    || crate::plan::labels::parse_agent(label).is_some()
                    || crate::plan::labels::parse_plan_owner(label).is_some()
                {
                    remove_labels.push(label.clone());
                }
            }
            let add_labels = vec![
                crate::plan::labels::plan_id(&plan_id),
                crate::plan::labels::PLAN_COMPLETE.to_string(),
                owner_label,
            ];
            filter_remove_labels(&mut remove_labels, &add_labels);
            let update = spur_pm::IssueUpdate {
                add_labels,
                remove_labels,
                ..Default::default()
            };
            if let Err(error) = apply_issue_update(pm, &epic_id, update.clone()).await {
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(
                    id,
                    format!("failed to persist execute_epic labels on epic: {error}"),
                );
            }
            rollback_updates.push((epic_id.clone(), invert_label_update(&update)));
        }

        for (issue_id, task_id, agent_name) in &task_scope {
            let issue = match pm.get_issue(issue_id).await {
                Ok(issue) => issue,
                Err(error) => {
                    self.plan_registry.lock().await.by_epic.remove(&epic_id);
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("failed to fetch execute_epic task '{issue_id}': {error}"),
                    );
                }
            };
            let update = replace_task_execution_labels(&issue, &plan_id, task_id, agent_name);
            if let Err(error) = apply_issue_update(pm, issue_id, update.clone()).await {
                let mut compensations = vec![(issue_id.clone(), invert_label_update(&update))];
                compensations.extend(rollback_updates.iter().rev().cloned());
                for (rollback_issue_id, rollback_update) in compensations {
                    if let Err(rollback_error) =
                        apply_issue_update(pm, &rollback_issue_id, rollback_update).await
                    {
                        tracing::warn!(
                            issue_id = %rollback_issue_id,
                            "failed to roll back execute_epic scope after task persist failure: {rollback_error}"
                        );
                    }
                }
                self.plan_registry.lock().await.by_epic.remove(&epic_id);
                return JsonRpcResponse::internal_error(
                    id,
                    format!("failed to persist execute_epic labels on task '{issue_id}': {error}"),
                );
            }
            rollback_updates.push((issue_id.clone(), invert_label_update(&update)));
        }

        if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
            return JsonRpcResponse::mcp_error(id, error);
        }
        if let Some(adv) = pm.advanced() {
            let task_map = task_scope
                .iter()
                .map(|(issue_id, task_id, _)| (task_id.clone(), issue_id.clone()))
                .collect();
            let sg = EpicSubgraph {
                epic_id: epic_id.clone(),
                task_map,
            };
            emit_plan_submit_audit(
                adv,
                &plan_id,
                &sg,
                PlanSubmitAuditContext {
                    base_snapshot_branch: base_snapshot_branch.as_deref(),
                    base_snapshot_oid: base_snapshot_oid.as_deref(),
                    execution_mode: Some("execute_epic"),
                    brain_session_id: Some(self.brain_session_id().as_session_id()),
                    explicit_base: None,
                },
            )
            .await;

            if let Some(prior) = prior_owner_match.as_ref() {
                let audit = match prior {
                    crate::plan::ownership::PlanOwnerMatch::Unowned
                    | crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => Some(
                        crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
                            plan_id: plan_id.clone(),
                            owner: self.brain_session_id().to_string(),
                            token: uuid::Uuid::new_v4().to_string(),
                            reason: "execute_epic".to_string(),
                        },
                    ),
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => Some(
                        crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipTransferred {
                            plan_id: plan_id.clone(),
                            from: owner.clone(),
                            to: self.brain_session_id().to_string(),
                            mode: "execute_epic".to_string(),
                            previous_token: String::new(),
                            new_token: uuid::Uuid::new_v4().to_string(),
                        },
                    ),
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        tracing::warn!(
                            target: "spur.audit.emit_skip",
                            epic_id = %epic_id,
                            plan_id = %plan_id,
                            owners = ?owners,
                            "execute_epic: prior epic labels ambiguous; skipping ownership audit emission"
                        );
                        None
                    }
                };
                if let Some(audit) = audit {
                    let kind_str = audit.kind_str();
                    let body = crate::plan::audit_sentinel::encode_comment(&audit);
                    if let Err(e) = adv.add_comment(&epic_id, &body).await {
                        tracing::warn!(
                            target: "spur.audit.emit_failure",
                            kind = kind_str,
                            epic_id = %epic_id,
                            plan_id = %plan_id,
                            "execute_epic ownership audit comment emission failed (owner label is persisted; audit missing): {e}"
                        );
                    }
                }
            }
        }

        // Insert into active_plans first (no registry lock held here).
        self.active_plans.lock().await.insert(
            plan_id.clone(),
            CachedPlan::new(Arc::clone(&state), unknown_beads_version()),
        );

        // Replace the sentinel with the real plan_id now that dispatch is
        // committed. active_plans lock is already released above, so these
        // two locks are never held simultaneously.
        self.plan_registry
            .lock()
            .await
            .by_epic
            .insert(epic_id.clone(), plan_id.clone());

        if self.task_tracker.is_closed() {
            // Roll back: remove the active_plans entry we just inserted.
            {
                let mut plans = self.active_plans.lock().await;
                plans.remove(&plan_id);
            }
            // Roll back: remove the registry entry (real plan_id, not sentinel).
            {
                let mut reg = self.plan_registry.lock().await;
                reg.by_epic.remove(&epic_id);
            }
            return JsonRpcResponse::error(
                id,
                -32000,
                "orchestrator shutting down — execute_epic aborted",
            );
        }

        {
            let state = state.lock().await;
            crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &state);
        }
        self.fast_forward_reconciler();

        info!(
            plan_id = %plan_id,
            epic_id = %epic_id,
            tasks = task_count,
            "Epic plan submitted"
        );

        // 6. Build response: plan status + epic metadata.
        let status_val = {
            let st = state_for_status.lock().await;
            crate::plan::build_plan_status(&plan_id, &st)
        };

        let derived_info = json!({
            "task_count": task_count,
            "edge_count": derived.edge_count,
            "agents": derived.agent_counts,
            "warnings": derived.warnings,
        });

        let mut resp_val = status_val;
        if let serde_json::Value::Object(ref mut m) = resp_val {
            m.insert("epic_id".into(), serde_json::json!(epic_id));
            m.insert("derived".into(), derived_info);
        }

        let text = serde_json::to_string_pretty(&resp_val).unwrap_or_else(|_| resp_val.to_string());

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
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
mod plan_truncate_and_restart_tests {
    use super::*;
    use crate::plan::PmLike;
    use serde_json::json;
    use spur_acp::{BrainSessionId, SessionId};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn no_op_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    async fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        super::run_git_capture(repo, None, args)
            .await
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"))
    }

    async fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-q", "-b", "main"]).await;
        run_git(dir.path(), &["config", "user.email", "test@spur"]).await;
        run_git(dir.path(), &["config", "user.name", "spur-test"]).await;
        std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
        run_git(dir.path(), &["add", "README.md"]).await;
        run_git(dir.path(), &["commit", "-q", "-m", "seed"]).await;
        dir
    }

    async fn commit_file_on_branch(
        repo: &std::path::Path,
        branch: &str,
        base: &str,
        path: &str,
        content: &str,
    ) -> String {
        run_git(repo, &["checkout", "-q", "-B", branch, base]).await;
        std::fs::write(repo.join(path), content).expect("write file");
        run_git(repo, &["add", path]).await;
        run_git(repo, &["commit", "-q", "-m", &format!("write {path}")]).await;
        let tip = run_git(repo, &["rev-parse", "--verify", "HEAD"]).await;
        run_git(repo, &["checkout", "-q", "main"]).await;
        tip
    }

    fn entry_for(
        task_id: &str,
        deps: &[&str],
        status: crate::plan::PlanTaskStatus,
    ) -> crate::plan::PlanTaskEntry {
        crate::plan::PlanTaskEntry {
            spec: crate::plan::PlanTask {
                task_id: task_id.into(),
                agent: "codex".into(),
                task: format!("task {task_id}"),
                depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
                issue_id: Some(format!("bd-{task_id}")),
                context_files: Vec::new(),
            },
            status,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: None,
            dispatched_base_oid: None,
        }
    }

    fn approved_entry(
        task_id: &str,
        deps: &[&str],
        worker_branch: &str,
        dispatched_base_oid: &str,
    ) -> crate::plan::PlanTaskEntry {
        let mut entry = entry_for(
            task_id,
            deps,
            crate::plan::PlanTaskStatus::Approved { summary: None },
        );
        entry.worker_branch = Some(worker_branch.to_string());
        entry.dispatched_base_oid = Some(dispatched_base_oid.to_string());
        entry
    }

    fn plan_with(
        plan_id: &str,
        entries: Vec<crate::plan::PlanTaskEntry>,
    ) -> crate::plan::PlanState {
        crate::plan::PlanState {
            plan_id: plan_id.into(),
            tasks: entries,
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            base_snapshot_branch: Some("main".into()),
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }
    }

    async fn persist_plan_fixture_to_mock_pm(
        mock_pm: &crate::plan::test_util::MockPm,
        plan: &crate::plan::PlanState,
    ) {
        let epic_id = plan.epic_id.as_deref().expect("fixture epic id");
        crate::plan::PmLike::update_issue(
            mock_pm,
            epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![
                    crate::plan::labels::plan_owner("brain"),
                    crate::plan::labels::PLAN_COMPLETE.to_string(),
                ],
                ..Default::default()
            },
        )
        .await
        .expect("mark mock epic as complete");

        let mut issue_by_task = std::collections::HashMap::new();
        for entry in &plan.tasks {
            let depends_on = entry
                .spec
                .depends_on
                .iter()
                .map(|dep| {
                    issue_by_task
                        .get(dep)
                        .cloned()
                        .unwrap_or_else(|| panic!("dependency {dep} must be persisted first"))
                })
                .collect();
            let issue_id = crate::plan::PmLike::create_issue(
                mock_pm,
                spur_pm::IssueCreate {
                    title: format!("Task {}", entry.spec.task_id),
                    description: Some(entry.spec.task.clone()),
                    issue_type: Some("task".to_string()),
                    priority: Some(2),
                    labels: vec![
                        crate::plan::labels::plan_id(&plan.plan_id),
                        crate::plan::labels::plan_task_id(&entry.spec.task_id),
                        crate::plan::labels::agent(&entry.spec.agent),
                    ],
                    parent: Some(epic_id.to_string()),
                    assignee: None,
                    estimate_minutes: None,
                    depends_on,
                },
            )
            .await
            .expect("create mock task issue");
            issue_by_task.insert(entry.spec.task_id.clone(), issue_id.clone());

            let adv = crate::plan::PmLike::advanced(mock_pm).expect("mock advanced PM");
            match &entry.status {
                crate::plan::PlanTaskStatus::Approved { summary } => {
                    let delegation_id = format!("del-{}", entry.spec.task_id);
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                                delegation_id: delegation_id.clone(),
                                worker: entry.spec.agent.clone(),
                                attempt: entry.attempt,
                            },
                        ),
                    )
                    .await
                    .expect("seed dispatch audit");
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                                delegation_id: delegation_id.clone(),
                                completion_state:
                                    crate::plan::audit_sentinel::CompletionState::AwaitingReview,
                                superseded: false,
                                worker_branch: entry.worker_branch.clone(),
                                result_summary: summary.clone(),
                                artifact_uri: None,
                                dispatched_base_oid: entry.dispatched_base_oid.clone(),
                            },
                        ),
                    )
                    .await
                    .expect("seed completion audit");
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Approval {
                                delegation_id,
                            },
                        ),
                    )
                    .await
                    .expect("seed approval audit");
                    crate::plan::PmLike::update_issue(
                        mock_pm,
                        &issue_id,
                        spur_pm::IssueUpdate {
                            status: Some(crate::plan::PmLike::closed_status(mock_pm).to_string()),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("close approved mock task");
                }
                crate::plan::PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files } => {
                    let reason = serde_json::to_string(&serde_json::json!({
                        "dep_task_id": dep_task_id,
                        "files": files,
                    }))
                    .expect("signal reason json");
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Signal {
                                signal_id: uuid::Uuid::new_v4().to_string(),
                                delegation_id: String::new(),
                                kind: "integration-conflict".to_string(),
                                severity: 1.0,
                                reason,
                            },
                        ),
                    )
                    .await
                    .expect("seed conflict signal audit");
                    crate::plan::PmLike::update_issue(
                        mock_pm,
                        &issue_id,
                        spur_pm::IssueUpdate {
                            add_labels: vec![
                                crate::plan::labels::SIGNAL_LABEL_INTEGRATION_CONFLICT.to_string(),
                            ],
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("label setup conflict");
                }
                _ => {}
            }
        }
    }

    async fn new_server_with_mock_pm(
        repo: &std::path::Path,
    ) -> (
        Arc<McpCallbackServer>,
        DelegationChannel,
        Arc<crate::plan::test_util::MockPm>,
    ) {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let mock_pm = crate::plan::test_util::MockPm::new().arc();
        let (mut server, channel) = McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            pro_feature_gate(),
        );
        server.__test_set_pm_like(mock_pm.clone() as Arc<dyn crate::plan::PmLike>);
        server.set_repo_root(repo.to_path_buf());
        server.set_reconciler_enabled(true, Some(Arc::new(tokio::sync::Notify::new())));
        let server = Arc::new(server);
        Arc::clone(&server)
            .enable_reconciler()
            .await
            .expect("enable mock reconciler");
        (server, channel, mock_pm)
    }

    fn output_json(response: serde_json::Value) -> serde_json::Value {
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected success response, got {response}"));
        serde_json::from_str(text).expect("response text is JSON")
    }

    #[tokio::test]
    pub(crate) async fn handle_plan_truncate_and_restart_happy_path() {
        let dir = init_repo().await;
        let base_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        commit_file_on_branch(dir.path(), "spur/test-task-a", "main", "a.txt", "task A\n").await;

        let (server, mut _channel, mock_pm) = new_server_with_mock_pm(dir.path()).await;
        let parent_epic_id = mock_pm
            .create_issue(spur_pm::IssueCreate {
                title: "Parent Recovery Epic".to_string(),
                description: Some("parent body".to_string()),
                issue_type: Some("epic".to_string()),
                labels: vec![crate::plan::labels::plan_id("recover-plan")],
                ..Default::default()
            })
            .await
            .expect("create parent epic");
        let mut parent_plan = plan_with(
            "recover-plan",
            vec![
                approved_entry("A", &[], "spur/test-task-a", &base_oid),
                entry_for(
                    "B",
                    &["A"],
                    crate::plan::PlanTaskStatus::BlockedOnSetupConflict {
                        dep_task_id: "A".into(),
                        files: vec!["a.txt".into()],
                    },
                ),
                entry_for("C", &["B"], crate::plan::PlanTaskStatus::Pending),
            ],
        );
        parent_plan.epic_id = Some(parent_epic_id.clone());
        persist_plan_fixture_to_mock_pm(&mock_pm, &parent_plan).await;

        let response = server
            .__test_call_tool(
                "plan_truncate_and_restart",
                json!({
                    "plan_id": "recover-plan",
                    "blocked_task_id": "B",
                }),
            )
            .await;
        let output = output_json(response);
        assert_eq!(output["staging_branch"], "spur/plan-staging/recover-plan");
        assert_eq!(output["superseded_task_ids"], json!(["B", "C"]));
        assert_eq!(output["conflict"], serde_json::Value::Null);
        let new_plan_id = output["new_plan_id"].as_str().expect("new_plan_id");

        assert_eq!(
            run_git(
                dir.path(),
                &["show", "spur/plan-staging/recover-plan:a.txt"],
            )
            .await,
            "task A"
        );

        let original = server
            .active_plans
            .lock()
            .await
            .get("recover-plan")
            .cloned()
            .expect("original plan");
        let original = original.state.lock().await;
        assert!(matches!(
            original.tasks[1].status,
            crate::plan::PlanTaskStatus::Superseded { .. }
        ));
        assert!(matches!(
            original.tasks[2].status,
            crate::plan::PlanTaskStatus::Superseded { .. }
        ));
        drop(original);

        let restarted = server
            .active_plans
            .lock()
            .await
            .get(new_plan_id)
            .cloned()
            .expect("new plan");
        let restarted = restarted.state.lock().await;
        assert!(
            restarted
                .base_snapshot_branch
                .as_deref()
                .is_some_and(|branch| branch.starts_with("spur/brain-snapshot-")),
            "expected explicit branch base to be captured as snapshot branch, got {:?}",
            restarted.base_snapshot_branch
        );
        let staging_oid = run_git(
            dir.path(),
            &["rev-parse", "--verify", "spur/plan-staging/recover-plan"],
        )
        .await;
        assert_eq!(
            restarted.base_snapshot_oid.as_deref(),
            Some(staging_oid.as_str())
        );
        let restarted_ids: Vec<&str> = restarted
            .tasks
            .iter()
            .map(|entry| entry.spec.task_id.as_str())
            .collect();
        assert_eq!(restarted_ids, vec!["B", "C"]);
        assert_eq!(restarted.tasks[0].spec.depends_on, Vec::<String>::new());
        assert_eq!(restarted.tasks[1].spec.depends_on, vec!["B".to_string()]);
        let restarted_epic_id = restarted.epic_id.clone().expect("child epic id");
        drop(restarted);

        let child_epic = mock_pm.issue(&restarted_epic_id).await;
        assert_eq!(
            child_epic.title,
            "Parent Recovery Epic (spur/plan-staging/recover-plan)"
        );
        assert!(
            child_epic.blocked_by.contains(&parent_epic_id),
            "child epic should be linked to parent epic: {child_epic:?}"
        );
        assert!(
            child_epic
                .labels
                .contains(&crate::plan::labels::PLAN_COMPLETE.to_string()),
            "child epic labels: {:?}",
            child_epic.labels
        );
        let child_issues = mock_pm
            .issues()
            .await
            .into_iter()
            .filter(|issue| {
                issue.issue_type.as_deref() == Some("task")
                    && issue.labels.iter().any(|label| {
                        crate::plan::labels::parse_plan_id(label).as_deref() == Some(new_plan_id)
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(child_issues.len(), 2, "{child_issues:?}");
        let b_issue = child_issues
            .iter()
            .find(|issue| issue.title.contains("B"))
            .expect("B child");
        let c_issue = child_issues
            .iter()
            .find(|issue| issue.title.contains("C"))
            .expect("C child");
        assert!(b_issue.blocked_by.contains(&restarted_epic_id));
        assert!(c_issue.blocked_by.contains(&restarted_epic_id));
        assert!(
            c_issue.blocked_by.contains(&b_issue.id),
            "C should depend on B via beads edge: {c_issue:?}"
        );
        assert!(
            mock_pm.audit_seq().await >= 2,
            "ownership and submit audit comments should be persisted"
        );
    }

    #[tokio::test]
    pub(crate) async fn handle_plan_truncate_and_restart_returns_conflict_when_cherry_pick_fails() {
        let dir = init_repo().await;
        std::fs::write(dir.path().join("conflict.txt"), "base\n").expect("write base");
        run_git(dir.path(), &["add", "conflict.txt"]).await;
        run_git(dir.path(), &["commit", "-q", "-m", "conflict base"]).await;
        let base_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        commit_file_on_branch(
            dir.path(),
            "spur/test-task-a",
            "main",
            "conflict.txt",
            "task A\n",
        )
        .await;
        commit_file_on_branch(
            dir.path(),
            "spur/test-task-b",
            "main",
            "conflict.txt",
            "task B\n",
        )
        .await;

        let (server, _channel, mock_pm) = new_server_with_mock_pm(dir.path()).await;
        let parent_epic_id = mock_pm
            .create_issue(spur_pm::IssueCreate {
                title: "Conflict Parent Epic".to_string(),
                issue_type: Some("epic".to_string()),
                labels: vec![crate::plan::labels::plan_id("conflict-plan")],
                ..Default::default()
            })
            .await
            .expect("create parent epic");
        let mut parent_plan = plan_with(
            "conflict-plan",
            vec![
                approved_entry("A", &[], "spur/test-task-a", &base_oid),
                approved_entry("B", &[], "spur/test-task-b", &base_oid),
                entry_for(
                    "C",
                    &["A", "B"],
                    crate::plan::PlanTaskStatus::BlockedOnSetupConflict {
                        dep_task_id: "B".into(),
                        files: vec!["conflict.txt".into()],
                    },
                ),
            ],
        );
        parent_plan.epic_id = Some(parent_epic_id);
        persist_plan_fixture_to_mock_pm(&mock_pm, &parent_plan).await;

        let response = server
            .__test_call_tool(
                "plan_truncate_and_restart",
                json!({
                    "plan_id": "conflict-plan",
                    "blocked_task_id": "C",
                }),
            )
            .await;
        let output = output_json(response);
        assert_eq!(output["conflict"]["dep_task_id"], "B");
        assert!(output["conflict"]["files"]
            .as_array()
            .expect("conflict files")
            .iter()
            .any(|file| file == "conflict.txt"));
        assert_eq!(output["superseded_task_ids"], json!(["C"]));
        assert!(output["new_plan_id"].as_str().is_some());
        assert_eq!(
            run_git(
                dir.path(),
                &["show", "spur/plan-staging/conflict-plan:conflict.txt"],
            )
            .await,
            "task A"
        );
    }
}

#[cfg(test)]
mod reconciler_fast_forward_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Notify;

    #[tokio::test]
    async fn notify_fast_forward_wakes_waiter() {
        let notify = Arc::new(Notify::new());
        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        super::notify_fast_forward(&Some(Arc::clone(&notify)));

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("waiter must wake")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn fast_forward_reconciler_uses_configured_notify() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let notify = Arc::new(Notify::new());
        server.set_reconciler_enabled(true, Some(Arc::clone(&notify)));

        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        server.fast_forward_reconciler();

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("fast-forward must wake the configured reconciler channel")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn fast_forward_reconciler_uses_default_notify_when_enabled_without_config() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        server.set_reconciler_enabled(true, None);
        let notify = server
            .reconciler_fast_forward
            .as_ref()
            .cloned()
            .expect("default fast-forward notify should be allocated");

        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        server.fast_forward_reconciler();

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("fast-forward must wake the default reconciler channel")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn load_or_project_plan_rejects_ephemeral_cache_without_epic() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let plan = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: Vec::new(),
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }));
        server.active_plans.lock().await.insert(
            "plan-1".into(),
            super::CachedPlan::new(Arc::clone(&plan), super::unknown_beads_version()),
        );

        let error = server
            .load_or_project_plan("plan-1")
            .await
            .expect_err("ephemeral cache entry without durable epic must not load");
        assert_eq!(error, "unknown plan 'plan-1'");
    }

    #[test]
    fn discover_plan_ids_collects_unique_prefix_values() {
        let issues = vec![
            spur_pm::IssueSummary {
                id: "bd-1".into(),
                source: spur_pm::PmSource::Beads,
                title: "Epic A".into(),
                status: "open".into(),
                labels: vec![
                    crate::plan::labels::plan_id("plan-1"),
                    crate::plan::labels::PLAN_COMPLETE.to_string(),
                ],
                url: "beads://bd-1".into(),
                priority: Some(2),
                issue_type: Some("epic".into()),
                assignee: None,
            },
            spur_pm::IssueSummary {
                id: "bd-2".into(),
                source: spur_pm::PmSource::Beads,
                title: "Epic B".into(),
                status: "open".into(),
                labels: vec![
                    crate::plan::labels::plan_id("plan-2"),
                    crate::plan::labels::plan_id("plan-1"),
                ],
                url: "beads://bd-2".into(),
                priority: Some(2),
                issue_type: Some("epic".into()),
                assignee: None,
            },
        ];

        let plan_ids = super::discover_plan_ids(&issues);
        assert_eq!(plan_ids, vec!["plan-1".to_string()]);
    }

    #[test]
    fn mutation_orphan_ids_require_terminal_companion_breadcrumb() {
        use crate::plan::audit_sentinel::AuditSentinelKind;

        let audits = vec![
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-1".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-1".into()),
                trigger_task_id: "bd-1".into(),
            },
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-2".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-2".into()),
                trigger_task_id: "bd-1".into(),
            },
            AuditSentinelKind::MutationCommit {
                mutation_id: "mut-2".into(),
                children_created: vec!["bd-2".into()],
                op_tags: vec!["split_task".into()],
                affected_task_ids: vec!["bd-1".into(), "bd-2".into()],
            },
        ];

        assert_eq!(
            super::mutation_orphan_ids(&audits),
            vec!["mut-1".to_string()]
        );
    }

    #[test]
    fn execution_label_replacement_removes_old_plan_and_agent_labels() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Task".into(),
            body: "Body".into(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("old-plan"),
                crate::plan::labels::agent("old-agent"),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let update = super::replace_execution_labels(&issue, "new-plan", "codex");
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::plan_id("new-plan")));
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::agent("codex")));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::plan_id("old-plan")));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::agent("old-agent")));
    }

    /// Regression for bd-19od: when an issue already carries the correct
    /// `spur:agent:<name>` and/or `spur:plan-id:<id>` label, the same string
    /// must NOT appear in both `add_labels` and `remove_labels`. The beads
    /// CLI processes adds before removes, so the duplicate would strip the
    /// label we just (idempotently) added.
    #[test]
    fn execution_label_replacement_does_not_strip_already_correct_agent_label() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Task".into(),
            body: "Body".into(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("plan-1"),
                crate::plan::labels::agent("claude-code"),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let update = super::replace_execution_labels(&issue, "plan-1", "claude-code");
        let agent_label = crate::plan::labels::agent("claude-code");
        let plan_label = crate::plan::labels::plan_id("plan-1");
        assert!(
            update.add_labels.contains(&agent_label),
            "add_labels must include the target agent label: {:?}",
            update.add_labels
        );
        assert!(
            !update.remove_labels.contains(&agent_label),
            "remove_labels must NOT contain the agent label that we are also adding: {:?}",
            update.remove_labels
        );
        assert!(
            !update.remove_labels.contains(&plan_label),
            "remove_labels must NOT contain the plan-id label that we are also adding: {:?}",
            update.remove_labels
        );

        let task_update =
            super::replace_task_execution_labels(&issue, "plan-1", "t1", "claude-code");
        assert!(
            !task_update.remove_labels.contains(&agent_label),
            "replace_task_execution_labels must also filter the agent label: {:?}",
            task_update.remove_labels
        );
        assert!(
            !task_update.remove_labels.contains(&plan_label),
            "replace_task_execution_labels must also filter the plan-id label: {:?}",
            task_update.remove_labels
        );
    }

    #[test]
    fn persisted_plan_epic_blocks_execute_epic_relabeling() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Persisted plan epic".into(),
            body: String::new(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("plan-1"),
                crate::plan::labels::PLAN_COMPLETE.to_string(),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("epic".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(super::persisted_plan_epic_plan_id(&issue), Some("plan-1"));
    }

    #[test]
    fn ordinary_epic_can_still_be_executed() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Product epic".into(),
            body: String::new(),
            status: "open".into(),
            labels: vec![],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("epic".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(super::persisted_plan_epic_plan_id(&issue), None);
    }

    #[tokio::test]
    async fn install_projected_plan_replaces_stale_cache_entry() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        let stale = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: Vec::new(),
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }));
        server.active_plans.lock().await.insert(
            "plan-1".into(),
            super::CachedPlan::new(Arc::clone(&stale), super::unknown_beads_version()),
        );

        let fresh = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![crate::plan::PlanTaskEntry {
                spec: crate::plan::PlanTask {
                    task_id: "t1".into(),
                    agent: "codex".into(),
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    context_files: Vec::new(),
                },
                status: crate::plan::PlanTaskStatus::Ready,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: session_id.clone(),
            base_snapshot_branch: Some("refs/heads/main".into()),
            base_snapshot_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        server.install_projected_plan(fresh, false).await;
        let loaded = server
            .active_plans
            .lock()
            .await
            .get("plan-1")
            .cloned()
            .expect("cached plan");
        assert_eq!(loaded.state.lock().await.tasks.len(), 1);
    }

    #[tokio::test]
    async fn reclaim_persisted_plans_hydrates_empty_cache() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");
        pm.update_issue(
            &subgraph.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::plan_owner(
                    &session_id.as_session_id().0,
                )],
                ..Default::default()
            },
        )
        .await
        .expect("stamp owner label");

        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            feature_gate,
        );
        assert!(server.active_plans.lock().await.is_empty());

        server
            .reclaim_persisted_plans_on_startup(pm)
            .await
            .expect("reclaim persisted plans");
        assert!(!server.active_plans.lock().await.is_empty());
    }

    #[tokio::test]
    async fn reclaim_replaces_existing_cache_entry_instead_of_merging() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        server.active_plans.lock().await.insert(
            "plan-1".into(),
            super::CachedPlan::new(
                Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
                    plan_id: "plan-1".into(),
                    tasks: Vec::new(),
                    brain_session_id: session_id.clone(),
                    base_snapshot_branch: None,
                    base_snapshot_oid: None,
                    merge_state: crate::plan::PlanMergeState::NotStarted,
                    epic_id: None,
                })),
                super::unknown_beads_version(),
            ),
        );

        let fresh_plan = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![crate::plan::PlanTaskEntry {
                spec: crate::plan::PlanTask {
                    task_id: "t1".into(),
                    agent: "codex".into(),
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    context_files: Vec::new(),
                },
                status: crate::plan::PlanTaskStatus::Ready,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };
        let replacement_plan = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![
                crate::plan::PlanTaskEntry {
                    spec: crate::plan::PlanTask {
                        task_id: "t1".into(),
                        agent: "codex".into(),
                        task: "Task".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-1".into()),
                        context_files: Vec::new(),
                    },
                    status: crate::plan::PlanTaskStatus::Ready,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                    last_delegation_id: None,
                    dispatched_base_oid: None,
                },
                crate::plan::PlanTaskEntry {
                    spec: crate::plan::PlanTask {
                        task_id: "t2".into(),
                        agent: "codex".into(),
                        task: "Task 2".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-2".into()),
                        context_files: Vec::new(),
                    },
                    status: crate::plan::PlanTaskStatus::Pending,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                    last_delegation_id: None,
                    dispatched_base_oid: None,
                },
            ],
            brain_session_id: session_id,
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        server.install_projected_plan(fresh_plan, false).await;
        server.install_projected_plan(replacement_plan, false).await;
        let cached = server
            .active_plans
            .lock()
            .await
            .get("plan-1")
            .cloned()
            .expect("cached");
        assert_eq!(cached.state.lock().await.tasks.len(), 2);
    }

    #[tokio::test]
    async fn detector_skips_reclaim_when_all_epics_have_rev1_metadata() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let sg = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");

        // Emit PlanSubmit audit so the epic carries rev1 bootstrap metadata.
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced");
        crate::emit_plan_submit_audit(
            adv,
            "plan-1",
            &sg,
            crate::PlanSubmitAuditContext {
                base_snapshot_branch: Some("main"),
                base_snapshot_oid: Some("abc123"),
                execution_mode: Some("test"),
                brain_session_id: None,
                explicit_base: None,
            },
        )
        .await;

        // The detector must report that no legacy reclaim is needed.
        let needs_reclaim =
            super::any_open_epic_lacks_rev1_metadata(pm.as_ref(), feature_gate.as_ref())
                .await
                .expect("detector query");
        assert!(
            !needs_reclaim,
            "detector must skip reclaim when all epics have rev1 metadata"
        );
    }

    #[tokio::test]
    async fn detector_reclaims_when_plan_submit_lacks_bootstrap_metadata() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let sg = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");

        // Emit PlanSubmit audit WITHOUT base snapshot metadata.
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced");
        crate::emit_plan_submit_audit(adv, "plan-1", &sg, crate::PlanSubmitAuditContext::default())
            .await;

        // The detector must report that legacy reclaim is still needed.
        let needs_reclaim =
            super::any_open_epic_lacks_rev1_metadata(pm.as_ref(), feature_gate.as_ref())
                .await
                .expect("detector query");
        assert!(
            needs_reclaim,
            "detector must reclaim when PlanSubmit lacks rev1 bootstrap metadata"
        );
    }

    #[test]
    fn legacy_reclaim_needed_when_rev1_bootstrap_metadata_is_missing() {
        assert!(super::legacy_reclaim_needed(false));
    }

    #[test]
    fn legacy_reclaim_skipped_when_rev1_bootstrap_metadata_exists() {
        assert!(!super::legacy_reclaim_needed(true));
    }
}
