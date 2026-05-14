use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
    pub(crate) fn ensure_accepting_delegations(
        &self,
    ) -> std::result::Result<(), DelegationDispatchError> {
        if self.retiring.load(Ordering::SeqCst) {
            Err(DelegationDispatchError::SessionRetiring)
        } else {
            Ok(())
        }
    }

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
        tracker.spawn(async move {
            let result = tokio::select! {
                res = rx => match res {
                    Ok(r) => r,
                    Err(_) => DelegationResult {
                        status: DelegationStatus::Failed {
                            error: "Orchestrator disconnected".into(),
                        },
                        diff: None,
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                        artifact: None,
                    },
                },
                _ = cancel_token.cancelled() => DelegationResult {
                    status: DelegationStatus::Cancelled {
                        reason: "Brain session retiring".into(),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                },
            };
            active.lock().await.remove(&delegation_id);

            // INV-ASYNC-2 (source_kind-gated): the continuation bridge is the
            // SOLE delivery channel for `BlockTimeout` (the async-first
            // path — `delegate_to_worker` auto-reprompt). Writing to the
            // map on that path would let `check_delegation_status` redeliver
            // what the brain already received as a continuation turn — the
            // double-delivery failure mode closed by INV-ASYNC-1.
            //
            // Phase 4: the `AsyncRequested` source_kind is retained only as a
            // legacy-debugging affordance — the `delegate_async` /
            // `wait_delegation` RPCs that drove it were removed in this
            // phase. In production nothing constructs `AsyncRequested`
            // handles, so the map write below is effectively dead code
            // unless a test/injection harness wires one explicitly.
            //
            // `detached = None` is retained as a fallback for unit
            // tests exercising the collector directly.
            let keep_map_entry = match &detached {
                None => true,
                Some(h) => matches!(h.source_kind, DetachedSourceKind::AsyncRequested),
            };
            if keep_map_entry {
                completed.lock().await.insert(
                    delegation_id.clone(),
                    (result.clone(), tokio::time::Instant::now()),
                );
            }

            if let Some(h) = detached {
                let source = if matches!(result.status, DelegationStatus::Cancelled { .. }) {
                    spur_acp::domain::ContinuationSource::Cancelled
                } else {
                    match h.source_kind {
                        DetachedSourceKind::AsyncRequested => {
                            spur_acp::domain::ContinuationSource::AsyncRequested
                        }
                        DetachedSourceKind::BlockTimeout => {
                            spur_acp::domain::ContinuationSource::BlockTimeout
                        }
                    }
                };

                let DetachedCompletionHandle {
                    ctx,
                    attempt_tracker,
                    brain_session,
                    event_sink,
                    materializer,
                    ..
                } = h;
                let attempt = attempt_tracker.load(Ordering::SeqCst);
                let cont = build_detached_continuation(
                    &delegation_id,
                    &result,
                    source,
                    attempt,
                    brain_session,
                    event_sink.as_ref(),
                    &materializer,
                )
                .await;
                // Route the completion back to the orchestrator ingress via
                // the injected callback (wired in spur-core to avoid a
                // circular dependency). The delegation_id is used as a
                // worker_session proxy for the DelegationCompleted UI event.
                (ctx.on_complete)(cont, delegation_id.clone().into()).await;
            }
        });
    }

    /// Remove completed delegation results older than `COMPLETED_TTL`.
    /// Called lazily from polling handlers to bound memory growth.
    pub(crate) async fn evict_stale_completions(&self) {
        self.completed_delegations
            .lock()
            .await
            .retain(|_, (_, ts)| ts.elapsed() < COMPLETED_TTL);
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

        let delegation_id = issue
            .labels
            .iter()
            .find_map(|label| {
                crate::plan::labels::parse_delegation_id(label)
                    .or_else(|| label.strip_prefix("delegation-id:"))
            })
            .map(str::to_string)
            .ok_or_else(|| {
                format!(
                    "recover_orphaned_dispatch: issue {issue_id} is missing spur:delegation-id:<id> label"
                )
            })?;

        if crate::plan::projector::has_ready_for_review_label_compat(&issue.labels) {
            return Err(format!(
                "recover_orphaned_dispatch: issue {issue_id} already has ready-for-review label"
            ));
        }
        let dispatched_base_oid = issue
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_dispatched_base_oid(label))
            .unwrap_or(dispatched_base_oid);

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
            .unwrap_or_else(|| issue_id.to_string());
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

    pub(crate) async fn handle_delegate_to_worker(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        if let Err(error) = self.ensure_accepting_delegations() {
            return dispatch_error_response(error, id);
        }
        let parsed: crate::tool_schemas::DelegateToWorkerInput =
            match serde_json::from_value(args.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return JsonRpcResponse::invalid_params(id, format!("Invalid arguments: {e}"))
                }
            };

        let request_id = DelegationId::new();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let attempt_tracker = new_attempt_tracker();

        let delegation = DelegationRequest {
            id: request_id.clone(),
            agent: parsed.agent.clone(),
            task: parsed.task,
            context_files: parsed.context_files.unwrap_or_default(),
            prior_branch_for_reuse: None,
            respond_to: tx,
            brain_session_id: self.brain_session_id().clone(),
            delegation_plan: parsed.delegation_plan,
            issue_id: parsed.issue_id,
            base: parsed.base,
            dispatched_base_oid_tx: None,
            attempt_tracker: Arc::clone(&attempt_tracker),
            enable_worker_mcp: parsed.enable_worker_mcp,
        };

        info!(agent = %parsed.agent, request_id = %request_id, "Sending delegation request");

        if let Err(_e) = self.delegation_tx.send(delegation).await {
            error!("Failed to send delegation request");
            return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
        }

        self.active_delegations
            .lock()
            .await
            .insert(request_id.clone());

        // Phase 1c (INV-ASYNC-1/2/3/7): biased select! over
        //   (fast arm: &mut rx), (slow arm: sleep(inline_wait))
        // Fast arm → return inline completion, drain active_delegations, no
        // collector spawn, no map write.
        // Slow arm → hand the receiver to `spawn_result_collector` with a
        // BlockTimeout continuation handle; the bridge is the sole delivery
        // channel (collector skips the map write when `detached` is Some).
        //
        // Cancel-during-handoff (Risk R2): the select! arm atomically either
        // consumes the oneshot result (fast path) or hands off the receiver
        // to the collector (slow path) — never both. `handle_cancel_delegation`
        // routes through `CancellationControl`, which signals the orchestrator
        // rather than touching our oneshot, so a cancel arriving between the
        // inline-window tick and the collector spawn races against the
        // orchestrator's own cancellation drain, not against this handler.
        //
        // INV-ASYNC-7: no mutex guards are held across any `.await` point
        // inside the arms below — `active_delegations.lock()` is scoped to a
        // single `.remove()` call in the fast arm.
        let mut rx = rx;
        let inline_wait = self.inline_wait;
        tokio::select! {
            biased;
            res = &mut rx => {
                let result = match res {
                    Ok(r) => r,
                    Err(_) => DelegationResult {
                        status: DelegationStatus::Failed {
                            error: "Orchestrator disconnected".into(),
                        },
                        diff: None,
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                        artifact: None,
                    },
                };
                self.active_delegations
                    .lock()
                    .await
                    .remove(&request_id);
                let result_json = match serde_json::to_value(&result) {
                    Ok(v) => v,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("Failed to serialize result: {e}"),
                        )
                    }
                };
                // Response shape (spec §8.3, post-review): content[0].text is
                // PURE JSON so brains can `json.loads(text)` without stripping
                // a leading shadow sentence. Human-readable context lives in
                // the `description` field.
                let payload = json!({
                    "status": "completed",
                    "delegation_id": request_id,
                    "continuation_will_fire": false,
                    "description": format!(
                        "Delegation to '{agent}' completed inline (delegation_id={request_id}).",
                        agent = parsed.agent
                    ),
                    "result": result_json,
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": payload_text
                        }]
                    }),
                )
            }
            _ = tokio::time::sleep(inline_wait) => {
                info!(
                    agent = %parsed.agent,
                    request_id = %request_id,
                    inline_wait_ms = inline_wait.as_millis() as u64,
                    "Delegation inline window expired — detaching via continuation bridge"
                );
                Self::spawn_result_collector(
                    &self.task_tracker,
                    request_id.clone(),
                    rx,
                    self.cancel_token.child_token(),
                    Arc::clone(&self.active_delegations),
                    Arc::clone(&self.completed_delegations),
                    Some(DetachedCompletionHandle {
                        ctx: Arc::clone(&self.continuation_ctx),
                        source_kind: DetachedSourceKind::BlockTimeout,
                        attempt_tracker,
                        brain_session: self.brain_session_id().as_session_id().clone(),
                        event_sink: self.event_sink.clone(),
                        materializer: self.materializer.clone(),
                    }),
                );
                let payload = json!({
                    "status": "pending",
                    "delegation_id": request_id,
                    "continuation_will_fire": true,
                    "description": format!(
                        "Delegation to '{agent}' is running in the background \
                         (delegation_id={request_id}). A continuation event will \
                         fire automatically when the worker completes. Do NOT call \
                         check_delegation_status — you will be re-prompted automatically.",
                        agent = parsed.agent
                    ),
                });
                let payload_text = serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": payload_text
                        }]
                    }),
                )
            }
        }
    }

    pub(crate) async fn handle_delegate_parallel(&self, id: Value, args: Value) -> JsonRpcResponse {
        if let Err(error) = self.ensure_accepting_delegations() {
            return dispatch_error_response(error, id);
        }
        if let Some(batch_plan) = args.get("delegation_plan") {
            tracing::info!(
                batch_plan = %batch_plan,
                "delegate_parallel received batch-level delegation_plan (not propagated into per-task requests)",
            );
        }

        if let Err(e) = validate_parallel_args(&args) {
            return JsonRpcResponse::invalid_params(id, e);
        }

        let skeletons = match parse_parallel_tasks(&args, self.brain_session_id()) {
            Ok(s) => s,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        // Phase 2 (INV-ASYNC-6): split the batch into
        //   (1) dispatch: send every delegation request up front and capture
        //       `(idx, request_id, agent, rx)` for later waiting
        //   (2) concurrent await: run one biased `select!` per task in a
        //       `JoinSet`
        //   (3) aggregation: place each `(idx, Value)` back into a fixed
        //       response vector so the output order matches the input order.
        //
        // This preserves the single-worker fast/slow-arm semantics while
        // removing the Phase 1c serial-dispatch regression where task N+1
        // could not even be sent until task N finished its inline wait.
        let inline_wait = self.inline_wait;
        let task_count = skeletons.len();
        let mut dispatched = Vec::with_capacity(task_count);

        for (idx, mut skeleton) in skeletons.into_iter().enumerate() {
            let request_id = skeleton.id.clone();
            let agent = skeleton.agent.clone();
            let attempt_tracker = Arc::clone(&skeleton.attempt_tracker);
            let (tx, rx) = tokio::sync::oneshot::channel();
            skeleton.respond_to = tx;

            info!(agent = %agent, request_id = %request_id, "Sending parallel delegation request");

            if let Err(_e) = self.delegation_tx.send(skeleton).await {
                error!("Failed to send parallel delegation request");
                return JsonRpcResponse::internal_error(id, "Failed to send delegation request");
            }

            self.active_delegations
                .lock()
                .await
                .insert(request_id.clone());
            dispatched.push((idx, request_id, agent, rx, attempt_tracker));
        }

        let mut waits = JoinSet::new();
        for (idx, request_id, agent, rx, attempt_tracker) in dispatched {
            let active_delegations = Arc::clone(&self.active_delegations);
            let completed_delegations = Arc::clone(&self.completed_delegations);
            let continuation_ctx = Arc::clone(&self.continuation_ctx);
            let task_tracker = self.task_tracker.clone();
            let cancel_token = self.cancel_token.child_token();
            let event_sink = self.event_sink.clone();
            let brain_session = self.brain_session_id().as_session_id().clone();
            let materializer = self.materializer.clone();
            waits.spawn(async move {
                let mut rx = rx;
                // Cancel-during-handoff (Risk R2): see
                // `handle_delegate_to_worker` — the select! arm atomically
                // either consumes the result or hands off the receiver; it
                // does not do both.
                let entry = tokio::select! {
                    biased;
                    res = &mut rx => {
                        let result = match res {
                            Ok(r) => r,
                            Err(_) => DelegationResult {
                                status: DelegationStatus::Failed {
                                    error: "Orchestrator disconnected".into(),
                                },
                                diff: None,
                                diff_summary: None,
                                summary: None,
                                estimated_cost_usd: 0.0,
                                worker_branch: None,
                                artifact: None,
                            },
                        };
                        active_delegations
                            .lock()
                            .await
                            .remove(&request_id);
                        let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
                        json!({
                            "status": "completed",
                            "delegation_id": request_id,
                            "agent": agent,
                            "continuation_will_fire": false,
                            "description": format!(
                                "Delegation to '{agent}' completed inline (delegation_id={request_id})."
                            ),
                            "result": result_json,
                        })
                    }
                    _ = tokio::time::sleep(inline_wait) => {
                        Self::spawn_result_collector(
                            &task_tracker,
                            request_id.clone(),
                            rx,
                            cancel_token,
                            active_delegations,
                            completed_delegations,
                            Some(DetachedCompletionHandle {
                                ctx: continuation_ctx,
                                source_kind: DetachedSourceKind::BlockTimeout,
                                attempt_tracker,
                                brain_session,
                                event_sink,
                                materializer,
                            }),
                        );
                        json!({
                            "status": "pending",
                            "delegation_id": request_id,
                            "agent": agent,
                            "continuation_will_fire": true,
                            "description": format!(
                                "Delegation to '{agent}' is running in the background \
                                 (delegation_id={request_id}). A continuation event will \
                                 fire automatically when the worker completes. Do NOT call \
                                 check_delegation_status — you will be re-prompted automatically."
                            ),
                        })
                    }
                };
                (idx, entry)
            });
        }

        let mut results = vec![Value::Null; task_count];
        while let Some(join_result) = waits.join_next().await {
            let (idx, entry) = match join_result {
                Ok(result) => result,
                Err(e) => {
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("Parallel delegation waiter failed: {e}"),
                    )
                }
            };
            results[idx] = entry;
        }

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&Value::Array(results.clone()))
                        .unwrap_or_else(|_| Value::Array(results).to_string())
                }]
            }),
        )
    }

    pub(crate) async fn handle_check_delegation_status(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.into(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        self.evict_stale_completions().await;

        // Completed — return and remove.
        let completed = {
            let mut map = self.completed_delegations.lock().await;
            map.retain(|_, (_, ts)| ts.elapsed() < COMPLETED_TTL);
            map.remove(&delegation_id).map(|(r, _)| r)
        };
        if let Some(result) = completed {
            let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result_json)
                            .unwrap_or_else(|_| result_json.to_string())
                    }]
                }),
            );
        }

        // Still running.
        if self
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": json!({"status": "running", "delegation_id": delegation_id}).to_string()
                    }]
                }),
            );
        }

        JsonRpcResponse::error(id, -32602, format!("Unknown delegation: {delegation_id}"))
    }

    pub(crate) async fn handle_fetch_outcome_artifact(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        match crate::handlers::fetch_outcome_artifact(
            &self.materializer,
            self.outcome_store.as_ref(),
            &ctx,
            args,
        )
        .await
        {
            Ok(value) => JsonRpcResponse::success(id, value),
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
                JsonRpcResponse::internal_error(id, format!("fetch_outcome_artifact failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    pub(crate) async fn handle_cancel_delegation(&self, id: Value, args: Value) -> JsonRpcResponse {
        let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(d) => d.into(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "Missing required field 'delegation_id'",
                )
            }
        };

        // Already completed — return the result directly.
        if let Some((result, _ts)) = self
            .completed_delegations
            .lock()
            .await
            .remove(&delegation_id)
        {
            let result_json = serde_json::to_value(&result).unwrap_or(json!(null));
            return JsonRpcResponse::success(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&result_json)
                            .unwrap_or_else(|_| result_json.to_string())
                    }]
                }),
            );
        }

        // Active — use the CancellationControl side-channel (INV-6).
        if self
            .active_delegations
            .lock()
            .await
            .contains(&delegation_id)
        {
            if let Some(ref cc) = self.cancellation_control {
                let outcome = cc
                    .cancel_with_reason(delegation_id.as_str(), "brain requested cancel".into())
                    .await;
                info!(delegation_id = %delegation_id, ?outcome, "Cancellation requested via CancellationControl");
                match outcome {
                    CancelOutcome::Cancelled => {
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("Delegation {} cancelled", delegation_id)
                                }]
                            }),
                        );
                    }
                    CancelOutcome::NotFound => {
                        // Token was already removed (delegation completed between
                        // the active_delegations check and the cancel call).
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("Delegation {} already completed", delegation_id)
                                }]
                            }),
                        );
                    }
                }
            } else {
                return JsonRpcResponse::internal_error(
                    id,
                    "cancel_delegation: no cancellation control wired",
                );
            }
        }

        JsonRpcResponse::error(id, -32602, format!("Unknown delegation: {delegation_id}"))
    }

    pub(crate) async fn handle_list_available_workers(&self, id: Value) -> JsonRpcResponse {
        let workers_json = serde_json::to_value(&self.workers).unwrap_or(json!([]));
        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&workers_json)
                        .unwrap_or_else(|_| workers_json.to_string())
                }]
            }),
        )
    }
}

#[cfg(test)]
include!("delegation_tests.rs");
