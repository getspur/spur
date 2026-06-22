use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
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
}
