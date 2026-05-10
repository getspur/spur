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
            .find_map(|label| crate::plan::labels::parse_delegation_id(label))
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
        );
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
mod cancel_delegation_tests {
    use spur_acp::{CancelOutcome, CancellationControl};

    /// INV-6: CancellationControl.cancel returns Cancelled the first time
    /// and NotFound on a second call (token was removed on first cancel).
    #[tokio::test]
    async fn cancel_returns_cancelled_then_not_found() {
        let cc = CancellationControl::new();
        let token = cc.register("req-1".into()).await;

        assert!(!token.is_cancelled(), "token should not be cancelled yet");

        let outcome = cc.cancel("req-1").await;
        assert_eq!(outcome, CancelOutcome::Cancelled);
        assert!(
            token.is_cancelled(),
            "token must be cancelled after cancel()"
        );

        // Second cancel: token was removed, so NotFound.
        let outcome2 = cc.cancel("req-1").await;
        assert_eq!(outcome2, CancelOutcome::NotFound);
    }

    /// INV-6: cancel on an unknown id returns NotFound.
    #[tokio::test]
    async fn cancel_unknown_id_returns_not_found() {
        let cc = CancellationControl::new();
        let outcome = cc.cancel("no-such-id").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }

    /// INV-6: remove() cleans up without cancelling the token.
    #[tokio::test]
    async fn remove_cleans_up_without_cancelling() {
        let cc = CancellationControl::new();
        let token = cc.register("req-2".into()).await;
        cc.remove("req-2").await;
        assert!(!token.is_cancelled(), "remove must not cancel the token");
        // After remove, cancel returns NotFound.
        let outcome = cc.cancel("req-2").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }
}

#[cfg(test)]
mod retirement_state_tests {
    use std::future::pending;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use spur_acp::{BrainSessionId, SessionId};
    use tokio::sync::Notify;

    fn no_op_ctx() -> super::DetachedContinuationCtx {
        super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn test_server_mark_retiring_rejects_new_delegations() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        server.mark_retiring();

        let single = server
            .__test_call_delegate_to_worker("codex", "should reject")
            .await;
        assert_eq!(single["error"]["message"], "SessionRetiring");

        let parallel = server
            .__test_call_delegate_parallel(vec![("codex", "parallel should reject")])
            .await;
        assert_eq!(parallel["error"]["message"], "SessionRetiring");
    }

    #[tokio::test]
    async fn test_server_cancel_in_flight_signals_token() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        assert!(
            !server.cancel_token.is_cancelled(),
            "fresh servers must start with an active cancellation token"
        );

        server.cancel_in_flight_workers();

        assert!(
            server.cancel_token.is_cancelled(),
            "cancel_in_flight_workers must signal the shared cancellation token"
        );
    }

    #[tokio::test]
    async fn test_server_force_abort_idempotent() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(Notify::new());

        *server.root_handle.lock().unwrap() = Some(tokio::spawn({
            let dropped = Arc::clone(&dropped);
            let started = Arc::clone(&started);
            async move {
                let _flag = DropFlag(dropped);
                started.notify_one();
                pending::<()>().await;
            }
        }));

        started.notified().await;
        server.force_abort();
        server.force_abort();
        tokio::time::timeout(Duration::from_millis(200), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("force_abort should eventually abort the stored root task");

        assert!(
            dropped.load(Ordering::SeqCst),
            "force_abort must abort the stored root task"
        );
        assert!(
            server.root_handle.lock().unwrap().is_none(),
            "force_abort must take the root handle so repeated calls stay idempotent"
        );
    }

    #[tokio::test]
    async fn test_server_force_abort_after_shutdown_partial_progress() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let server = Arc::new(server);

        let release = Arc::new(Notify::new());
        server.task_tracker.spawn({
            let release = Arc::clone(&release);
            async move {
                release.notified().await;
            }
        });

        let dropped = Arc::new(AtomicBool::new(false));
        *server.root_handle.lock().unwrap() = Some(tokio::spawn({
            let dropped = Arc::clone(&dropped);
            async move {
                let _flag = DropFlag(dropped);
                pending::<()>().await;
            }
        }));

        let shutdown = tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                server.shutdown().await;
            }
        });

        tokio::task::yield_now().await;
        server.force_abort();
        release.notify_waiters();

        tokio::time::timeout(Duration::from_millis(200), shutdown)
            .await
            .expect("shutdown should complete once tracked work finishes")
            .expect("shutdown task must not panic");

        assert!(
            dropped.load(Ordering::SeqCst),
            "force_abort must still abort the root task after shutdown has already started"
        );
    }
}

#[cfg(test)]
mod continuation_producer_tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use chrono::Utc;
    use spur_acp::domain::artifact::{ArtifactKind as WorkerArtifactKind, WorkerArtifact};
    use spur_acp::domain::continuation::ArtifactKind as ContinuationArtifactKind;
    use spur_acp::domain::events::{DiffSummary, SpurEventBody};
    use spur_acp::domain::{
        BrainContinuation, ContinuationSource, DelegationResult, DelegationStatus,
    };
    use spur_acp::{DelegationId, SessionId};
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::TaskTracker;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<SpurEventBody>>,
    }

    impl crate::events::McpEventSink for RecordingSink {
        fn emit(&self, event: SpurEventBody) {
            self.events.lock().unwrap().push(event);
        }
    }

    async fn capture_continuation(
        delegation_id: DelegationId,
        result: DelegationResult,
        attempt: u32,
        brain_session: SessionId,
        event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    ) -> BrainContinuation {
        let tracker = TaskTracker::new();
        let active = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        let completed = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let captured = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let captured_for_ctx = Arc::clone(&captured);
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let materializer = super::OutcomeMaterializer::new(store);

        let detached = Some(super::DetachedCompletionHandle {
            ctx: Arc::new(super::DetachedContinuationCtx {
                on_complete: Arc::new(move |cont, _worker_session| {
                    let captured = Arc::clone(&captured_for_ctx);
                    Box::pin(async move {
                        captured.lock().await.push(cont);
                    })
                }),
            }),
            source_kind: super::DetachedSourceKind::BlockTimeout,
            attempt_tracker: Arc::new(AtomicU32::new(attempt)),
            brain_session,
            event_sink,
            materializer,
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        super::McpCallbackServer::spawn_result_collector(
            &tracker,
            delegation_id,
            rx,
            CancellationToken::new(),
            active,
            completed,
            detached,
        );

        tx.send(result).expect("send continuation result");
        tracker.close();
        tracker.wait().await;

        let captured = captured.lock().await;
        assert_eq!(
            captured.len(),
            1,
            "collector should emit exactly one continuation"
        );
        captured[0].clone()
    }

    fn success_result(
        summary: Option<String>,
        diff_summary: Option<DiffSummary>,
        artifact: Option<WorkerArtifact>,
    ) -> DelegationResult {
        DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary,
            summary,
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-test".into()),
            artifact,
        }
    }

    #[tokio::test]
    async fn build_detached_continuation_populates_artifact_id_via_materializer() {
        use spur_blob_store::MemoryOutcomeStore;

        let store: Arc<dyn spur_blob_store::OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = crate::outcome_materializer::OutcomeMaterializer::new(store);
        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-x".into()),
            artifact: None,
        };
        let delegation_id = DelegationId::from("deadbeef-1111-2222-3333-444455556666");
        let brain_session = SessionId("550e8400-e29b-41d4-a716-446655440000".into());

        let cont = super::build_detached_continuation(
            &delegation_id,
            &result,
            spur_acp::domain::ContinuationSource::BlockTimeout,
            1,
            brain_session,
            None,
            &mat,
        )
        .await;
        assert!(
            cont.payload.artifact_id.is_some(),
            "Phase 3 wires artifact_id"
        );
    }

    #[tokio::test]
    async fn test_producer_materializes_oversized_summary_with_fetch_hint() {
        let delegation_id: DelegationId = "del-oversized".into();
        let sink = Arc::new(RecordingSink::default());
        let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
        let original_summary = "x".repeat(super::PRODUCER_MAX_FIELD_BYTES + 64);

        let continuation = capture_continuation(
            delegation_id.clone(),
            success_result(Some(original_summary.clone()), None, None),
            1,
            SessionId("brain".into()),
            Some(sink_obj),
        )
        .await;

        let clipped = continuation
            .payload
            .summary
            .as_ref()
            .expect("summary should still be present after clipping");
        assert!(
            clipped.len() <= super::PRODUCER_MAX_FIELD_BYTES,
            "clipped summary must stay within the producer byte budget"
        );
        assert!(
            clipped.ends_with('…'),
            "clipped summary should carry the ellipsis marker"
        );
        assert!(
            continuation.payload.artifact_id.is_some(),
            "full result should be fetchable from the outcome store"
        );
        assert!(
            continuation
                .payload
                .fetch_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("Summary truncated")),
            "fetch hint should tell the brain that the summary was clipped"
        );

        assert!(
            sink.events.lock().unwrap().is_empty(),
            "primary materializer path persists the full result instead of emitting a truncation event"
        );
    }

    #[tokio::test]
    async fn test_producer_diff_summary_handled() {
        let sink = Arc::new(RecordingSink::default());
        let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
        let diff_summary = DiffSummary {
            files_changed: 2,
            insertions: 8,
            deletions: 3,
            files: vec!["src/main.rs".into(), "src/lib.rs".into()],
        };

        let continuation = capture_continuation(
            "del-diff-summary".into(),
            success_result(Some("ok".into()), Some(diff_summary.clone()), None),
            1,
            SessionId("brain".into()),
            Some(sink_obj),
        )
        .await;

        assert_eq!(continuation.payload.diff_summary, Some(diff_summary));
        assert!(
            sink.events.lock().unwrap().is_empty(),
            "structured diff_summary should not emit truncation events when no string field is clipped"
        );
    }

    #[tokio::test]
    async fn test_continuation_construction_brain_session_attempt_created_at() {
        let delegation_id: DelegationId = "del-cont-1".into();
        let brain_session = SessionId("brain-session-7".into());
        let before_wall = Utc::now();
        let before_mono = Instant::now();

        let continuation = capture_continuation(
            delegation_id.clone(),
            success_result(
                Some("done".into()),
                None,
                Some(WorkerArtifact {
                    object_ref: "refs/spur/artifacts/abc123".into(),
                    blob_sha: "0".repeat(40),
                    size_bytes: 1_234,
                    kind: WorkerArtifactKind::Diagnostic,
                }),
            ),
            7,
            brain_session.clone(),
            None,
        )
        .await;

        let after_mono = Instant::now();
        let after_wall = Utc::now();

        assert_eq!(continuation.delegation_id, delegation_id);
        assert_eq!(continuation.attempt, 7);
        assert_eq!(continuation.brain_session, brain_session);
        assert_eq!(continuation.source, ContinuationSource::BlockTimeout);
        assert!(continuation.created_at_wall >= before_wall);
        assert!(continuation.created_at_wall <= after_wall);
        assert!(continuation.created_at_mono >= before_mono);
        assert!(continuation.created_at_mono <= after_mono);

        let artifact_ref = continuation
            .payload
            .artifact_ref
            .as_ref()
            .expect("worker artifacts should map to continuation artifact refs");
        assert_eq!(
            artifact_ref.kind,
            ContinuationArtifactKind::Other("worker_artifact".into())
        );
        assert_eq!(artifact_ref.uri, "spur://artifact/del-cont-1");
        assert_eq!(artifact_ref.byte_size, 1_234);
        assert_eq!(
            artifact_ref.sha256.as_deref(),
            Some("0".repeat(40).as_str())
        );
        assert_eq!(
            artifact_ref.git_object_ref.as_deref(),
            Some("refs/spur/artifacts/abc123")
        );
        assert_eq!(
            artifact_ref.git_blob_sha.as_deref(),
            Some("0".repeat(40).as_str())
        );
    }
}

#[cfg(test)]
mod fetch_outcome_artifact_tests {
    //! End-to-end tests for the `fetch_outcome_artifact` MCP tool.
    //!
    //! Seeds the outcome store with serialized `DelegationResult` blobs,
    //! then calls the JSON-RPC tool dispatcher and asserts the section
    //! projection returned to the brain.

    use super::{DetachedContinuationCtx, McpCallbackServer};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use spur_acp::domain::{ContinuationSource, DelegationResult, DelegationStatus, OutcomeKey};
    use spur_acp::{BrainSessionId, DelegationId, SessionId};
    use spur_blob_store::{ContentType, OutcomeMetadata, OutcomeStore};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn init_git_repo(path: &Path) {
        let init = tokio::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(path)
            .output()
            .await
            .expect("git init must run");
        assert!(init.status.success(), "git init failed: {init:?}");

        for kv in &[("user.email", "test@example.com"), ("user.name", "test")] {
            let out = tokio::process::Command::new("git")
                .args(["config", kv.0, kv.1])
                .current_dir(path)
                .output()
                .await
                .expect("git config must run");
            assert!(out.status.success(), "git config {} failed", kv.0);
        }
    }

    fn no_op_continuation_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_cont, _worker_session| Box::pin(async {})),
        }
    }

    async fn build_test_server(repo_root: &Path, session_id: &str) -> McpCallbackServer {
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let outcome_store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        build_test_server_with_store(repo_root, brain_session, outcome_store).await
    }

    async fn build_test_server_with_store(
        repo_root: &Path,
        brain_session: BrainSessionId,
        outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    ) -> McpCallbackServer {
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&brain_session),
            None,
            None,
            no_op_continuation_ctx(),
            outcome_store,
            super::community_feature_gate(),
        );
        server.set_repo_root(repo_root.to_path_buf());
        server
    }

    fn sha256_hex(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write;
            write!(&mut hex, "{byte:02x}").expect("hex write infallible");
        }
        hex
    }

    fn outcome_metadata(content: &[u8]) -> OutcomeMetadata {
        OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: ContentType::Json,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha256_hex(content),
        }
    }

    async fn put_outcome(
        store: &Arc<dyn OutcomeStore>,
        brain_session: &BrainSessionId,
        delegation_id: DelegationId,
        attempt: u32,
        result: &DelegationResult,
    ) {
        let bytes = serde_json::to_vec(result).expect("serialize result");
        let metadata = outcome_metadata(&bytes);
        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id,
            attempt,
        };
        store
            .put(&key, &bytes, &metadata)
            .await
            .expect("put outcome");
    }

    fn success_result(summary: &str, diff: &str, cost: f64) -> DelegationResult {
        DelegationResult {
            status: DelegationStatus::Success,
            summary: Some(summary.into()),
            diff: Some(diff.into()),
            diff_summary: None,
            estimated_cost_usd: cost,
            worker_branch: None,
            artifact: None,
        }
    }

    fn dispatch_args(name: &str, args: Value) -> Value {
        json!({ "name": name, "arguments": args })
    }

    fn response_text(response: &super::JsonRpcResponse) -> &str {
        response.result.as_ref().expect("expected success response")["content"][0]["text"]
            .as_str()
            .expect("text content")
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_persisted_blob_text() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-1111-2222-3333-444455556666".into();
        let result = success_result("ok", "line one\nline two\n", 0.0);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_id.as_str() }),
                ),
            )
            .await;

        let text = response_text(&response);
        let parsed: DelegationResult = serde_json::from_str(text).expect("full result json");
        assert_eq!(parsed.summary.as_deref(), Some("ok"));
        assert_eq!(parsed.diff.as_deref(), Some("line one\nline two\n"));
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_status_only_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-status-only".into();
        let result = success_result("summary must stay out", "diff must stay out", 1.25);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "status_only"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["attempt"], 1);
        assert_eq!(projected["brain_session"], session_id);
        assert_eq!(projected["estimated_cost_micros"], 1_250_000);
        assert!(projected.get("summary").is_none());
        assert!(projected.get("diff").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_summary_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-summary".into();
        let result = success_result("summary included", "diff must stay out", 0.5);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "summary"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["attempt"], 1);
        assert_eq!(projected["brain_session"], session_id);
        assert_eq!(projected["summary"], "summary included");
        assert_eq!(projected["estimated_cost_micros"], 500_000);
        assert!(projected.get("diff").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_diff_only_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-diff-only".into();
        let result = success_result("summary must stay out", "diff included", 0.25);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "diff_only"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["diff"], "diff included");
        assert!(projected.get("diff_summary").is_some());
        assert!(projected.get("summary").is_none());
        assert!(projected.get("attempt").is_none());
        assert!(projected.get("estimated_cost_micros").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_pins_specific_attempt() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-attempts".into();
        server
            .materializer
            .materialize(
                success_result("attempt one", "diff one", 0.0),
                delegation_id.clone(),
                1,
                brain_session.clone(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        server
            .materializer
            .materialize(
                success_result("attempt two", "diff two", 0.0),
                delegation_id.clone(),
                2,
                brain_session.clone(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;

        let latest_response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let latest: Value = serde_json::from_str(response_text(&latest_response)).expect("json");
        assert_eq!(latest["attempt"], 2);
        assert_eq!(latest["summary"], "attempt two");

        let pinned_response = server
            .handle_tool_call(
                Value::Number(2.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "attempt": 1,
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let pinned: Value = serde_json::from_str(response_text(&pinned_response)).expect("json");
        assert_eq!(pinned["attempt"], 1);
        assert_eq!(pinned["summary"], "attempt one");
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_invalid_attempt_arg() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        for invalid in [json!(-1), json!("two"), json!(0), json!(false)] {
            let response = server
                .handle_tool_call(
                    Value::Number(1.into()),
                    dispatch_args(
                        "fetch_outcome_artifact",
                        json!({
                            "delegation_id": "deadbeef-1111-2222-3333-444455556666",
                            "attempt": invalid,
                        }),
                    ),
                )
                .await;
            let error = response
                .error
                .as_ref()
                .unwrap_or_else(|| panic!("expected InvalidParams for attempt={invalid:?}"));
            assert_eq!(error.code, -32602);
            assert!(
                error.message.contains("Invalid 'attempt'"),
                "expected attempt rejection, got: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_internal_error_on_corrupted_blob() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        // Seed the store with bytes that ARE valid UTF-8 but NOT a valid
        // DelegationResult — exercises ProjectionError::InvalidResult on
        // a non-Full projection.
        let delegation_id: DelegationId = "deadbeef-1111-2222-3333-444455556666".into();
        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id: delegation_id.clone(),
            attempt: 1,
        };
        let bytes = b"not a delegation result";
        let metadata = outcome_metadata(bytes);
        store.put(&key, bytes, &metadata).await.expect("put");

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "attempt": 1,
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let error = response
            .error
            .as_ref()
            .expect("expected InternalError on corrupted blob");
        assert_eq!(error.code, -32603, "InternalError JSON-RPC code");
        assert!(
            error.message.to_lowercase().contains("projection")
                || error.message.contains("DelegationResult"),
            "expected projection-error context: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_clean_error_for_unknown_delegation() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": "nonexistent-delegation-id" }),
                ),
            )
            .await;

        let error = response.error.as_ref().expect("expected error response");
        // Phase 2 Task 10: a missing artifact is reported as Unauthorized
        // rather than NotFound so that a caller cannot probe whether a given
        // (delegation_id, attempt) exists in another brain session.
        assert_eq!(error.code, -32001);
        assert!(
            error.message.contains("not accessible"),
            "error message must mention not-accessible: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_unknown_section_cleanly() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": "any-id",
                        "section": "not_a_section"
                    }),
                ),
            )
            .await;

        let error = response
            .error
            .as_ref()
            .expect("expected InvalidParams error");
        assert_eq!(error.code, -32602, "InvalidParams JSON-RPC code");
        assert!(
            error
                .message
                .contains("Must be one of: status_only, summary, diff_only, full"),
            "unknown sections must be rejected cleanly: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_empty_delegation_id() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args("fetch_outcome_artifact", json!({ "delegation_id": "" })),
            )
            .await;

        let error = response.error.as_ref().expect("expected error response");
        assert_eq!(error.code, -32602, "InvalidParams JSON-RPC code");
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_completed_delegations_are_per_session() {
        // Two MCP servers share the same store, but each binds fetches to
        // its own brain_session_id. Server B asks for the same delegation_id
        // under its session and must not see Server A's outcome.
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_a_id = "550e8400-e29b-41d4-a716-446655440000";
        let session_b_id = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
        let brain_session_a = BrainSessionId::new(SessionId(session_a_id.into()));
        let brain_session_b = BrainSessionId::new(SessionId(session_b_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());

        let server_a =
            build_test_server_with_store(td.path(), brain_session_a.clone(), store.clone()).await;
        let server_b =
            build_test_server_with_store(td.path(), brain_session_b, store.clone()).await;

        let delegation_a: DelegationId = "delegation-belonging-to-a".into();
        let result_a = success_result("secret stdout for session A", "secret diff", 0.0);
        put_outcome(&store, &brain_session_a, delegation_a.clone(), 1, &result_a).await;

        // Server A can fetch its own delegation.
        let resp_a = server_a
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_a.as_str() }),
                ),
            )
            .await;
        let text = response_text(&resp_a);
        let parsed: DelegationResult = serde_json::from_str(text).expect("full result");
        assert_eq!(
            parsed.summary.as_deref(),
            Some("secret stdout for session A")
        );

        // Server B fetches under its own brain_session_id and is denied as
        // Unauthorized — the store-miss is deliberately indistinguishable
        // from a "different session" miss to prevent cross-session probing.
        let resp_b = server_b
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_a.as_str() }),
                ),
            )
            .await;
        let err = resp_b.error.as_ref().expect("server B must error");
        assert_eq!(err.code, -32001);
        assert!(
            err.message.contains("not accessible"),
            "Server B must not expose Server A's delegations: {err:?}"
        );
    }
}
