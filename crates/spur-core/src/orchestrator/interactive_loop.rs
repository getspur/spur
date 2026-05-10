use super::*;

fn take_rendered_batch(
    drained_batch: &mut Option<crate::scheduler::DrainedBatch>,
    render_outcome: &mut Option<crate::continuation_bridge::RenderOutcome>,
) -> Option<(
    crate::scheduler::DrainedBatch,
    crate::continuation_bridge::RenderOutcome,
)> {
    drained_batch.take().map(|batch| {
        let outcome = render_outcome
            .take()
            .expect("drained batch must carry render outcome");
        (batch, outcome)
    })
}

fn dropped_terminal_from_render_outcome(
    outcome: &crate::continuation_bridge::RenderOutcome,
) -> Vec<(
    spur_acp::domain::DelegationKey,
    spur_acp::domain::DropReason,
)> {
    outcome
        .dropped_oversized
        .iter()
        .map(|(key, bytes)| {
            (
                key.clone(),
                spur_acp::domain::DropReason::OversizedSingleItem {
                    continuation_bytes: *bytes,
                    budget_bytes: crate::continuation_bridge::MERGE_BUDGET_DEFAULT_BYTES,
                },
            )
        })
        .collect()
}

fn commit_rendered_batch(
    scheduler: &mut crate::scheduler::BrainScheduler,
    batch: crate::scheduler::DrainedBatch,
    outcome: crate::continuation_bridge::RenderOutcome,
) {
    let dropped_terminal = dropped_terminal_from_render_outcome(&outcome);
    let spilled_with_reason = Some(
        outcome
            .deferred_spill
            .into_iter()
            .map(|(continuation, reason)| {
                (spur_acp::domain::DelegationKey::from(&continuation), reason)
            })
            .collect(),
    );
    scheduler.commit_partial(
        batch,
        outcome.delivered_keys,
        dropped_terminal,
        spilled_with_reason,
    );
}

impl Orchestrator {
    /// Run an interactive session: multi-turn loop that accepts user input
    /// between brain turns. Used by `spur watch`.
    pub async fn run_interactive(
        mut self,
        mut user_input_rx: mpsc::Receiver<InteractiveInput>,
        brain_override: Option<String>,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        overflow_continuations: crate::continuation_bridge::OverflowBuf,
    ) -> Result<()> {
        let mut brain: Option<BrainSession> = None;
        let mut scheduler = crate::scheduler::BrainScheduler::new(
            None, // active_session set when first brain spawns
            Arc::new(self.funnel.clone()),
        );
        // Pre-connected (initialized) agent connection, ready for create_brain_session
        // or load_brain_session without re-running connect_brain.
        let mut agent_connection: Option<ActiveConnection> = None;

        let mut reconnect_failures: std::collections::VecDeque<std::time::Instant> =
            std::collections::VecDeque::new();
        const RECONNECT_CIRCUIT_LIMIT: usize = 3;
        const RECONNECT_CIRCUIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

        // Startup: parallel-fetch issues + graph alerts for TUI display.
        if let Some(pm) = &self.pm_service {
            refresh_pm_state(pm, &self.funnel, None, false).await;
        }

        // Startup guidance: surface actionable install hints for missing PM tools.
        if let Some(warning) = startup_beads_warning(
            &self.config,
            self.feature_gate.as_deref(),
            self.repo_root.join(".beads").is_dir(),
            self.pm_service.is_some(),
            binary_on_path("br"),
        ) {
            self.funnel.emit(SpurEventBody::IssueCommandError {
                operation: "startup".into(),
                error: render_beads_startup_warning(warning).into(),
                id: None,
            });
        } else if let Some(pm) = &self.pm_service {
            if pm.analyzer().is_none() {
                self.funnel.emit(SpurEventBody::IssueCommandError {
                    operation: "startup".into(),
                    error: "graph analysis disabled (beads database unavailable)".into(),
                    id: None,
                });
            }
        }

        loop {
            // ── (a) Drain overflow buffer so scheduler sees fresh state ──
            {
                let mut over = overflow_continuations.lock().await;
                while let Some((_sid, c)) = over.pop_front() {
                    scheduler.push_continuation(c);
                }
            }

            // ── (b) Ask scheduler what to do ────────────────────────────
            let now = std::time::Instant::now();
            let action = scheduler.next(now);

            // ── (c) Idle: recv next input and dispatch immediately ───────
            if let crate::scheduler::ScheduledAction::IdleUntil { deadline } = action {
                let raw = match deadline {
                    Some(deadline) => {
                        let deadline = tokio::time::Instant::from_std(deadline);
                        tokio::select! {
                            maybe = user_input_rx.recv() => match maybe {
                                Some(input) => input,
                                None => break,
                            },
                            _ = tokio::time::sleep_until(deadline) => continue,
                        }
                    }
                    None => match user_input_rx.recv().await {
                        Some(i) => i,
                        None => break, // channel closed — shutdown
                    },
                };

                match raw {
                    InteractiveInput::WarmConnect => {
                        if brain.is_some() || agent_connection.is_some() {
                            continue;
                        }

                        let target_brain = self.selected_brain_name(brain_override.as_deref());
                        self.emit(SpurEvent::now(SpurEventBody::BrainConnectStarted {
                            brain: target_brain.clone(),
                        }));

                        match self
                            .connect_brain(brain_override.as_deref(), permission_tx.clone())
                            .await
                        {
                            Ok((conn, brain_name, init_response)) => {
                                agent_connection = Some(ActiveConnection {
                                    transport: conn,
                                    brain_name: brain_name.clone(),
                                    attach_guard: None,
                                    fs_unsafe: false,
                                    init_response,
                                });
                                self.emit(SpurEvent::now(SpurEventBody::BrainConnected {
                                    brain: brain_name,
                                }));
                            }
                            Err(e) => {
                                let error_message = format_error_chain(&e);
                                error!(
                                    error = %error_message,
                                    brain = %target_brain,
                                    "Failed to warm-connect brain"
                                );
                                self.emit(SpurEvent::now(SpurEventBody::BrainConnectFailed {
                                    brain: target_brain,
                                    reason: error_message,
                                }));
                                if Self::is_auth_required_error(&e) {
                                    self.emit(SpurEvent::now(SpurEventBody::AuthRequired {
                                        session: SessionId(String::new()),
                                        message: Self::auth_required_banner(),
                                    }));
                                }
                            }
                        }
                        continue;
                    }
                    // Continuation — route to scheduler for next tick.
                    InteractiveInput::SystemContinuation { continuation, .. } => {
                        scheduler.push_continuation(continuation);
                        continue;
                    }
                    // Prompt-class — push to scheduler; will be dispatched next tick.
                    InteractiveInput::Message { .. } => {
                        scheduler.push_user(raw);
                        continue;
                    }
                    // NewSessionWithMessage — retire brain, then push Message to scheduler.
                    InteractiveInput::NewSessionWithMessage { blocks, interrupt } => {
                        self.retire_active_brain(
                            &mut brain,
                            &mut agent_connection,
                            &mut scheduler,
                            &overflow_continuations,
                            spur_acp::domain::events::BrainRetireReason::UserClear,
                            None,
                        )
                        .await;
                        if blocks.is_empty() {
                            info!("NewSessionWithMessage with empty blocks — spawn deferred to next Message");
                        } else {
                            scheduler.push_user(InteractiveInput::Message { blocks, interrupt });
                        }
                        continue;
                    }

                    // ── ListSessions ──────────────────────────────────────
                    InteractiveInput::ListSessions => {
                        let ActiveConnection {
                            transport: mut conn,
                            brain_name,
                            attach_guard,
                            fs_unsafe,
                            init_response,
                        } = match agent_connection.take() {
                            Some(existing) => existing,
                            None => {
                                match self
                                    .connect_brain(brain_override.as_deref(), permission_tx.clone())
                                    .await
                                {
                                    Ok((transport, brain_name, init_response)) => {
                                        ActiveConnection {
                                            transport,
                                            brain_name,
                                            attach_guard: None,
                                            fs_unsafe: false,
                                            init_response,
                                        }
                                    }
                                    Err(e) => {
                                        error!(error = %e, "Failed to connect brain for list_sessions");
                                        self.emit(SpurEvent::now(
                                            SpurEventBody::SessionsListError {
                                                message: e.to_string(),
                                            },
                                        ));
                                        continue;
                                    }
                                }
                            }
                        };

                        let sessions_result = match Self::list_sessions_from_rpc(
                            &mut *conn,
                            &self.repo_root,
                        )
                        .await
                        {
                            Ok(sessions) if !sessions.is_empty() => Ok(sessions),
                            Ok(_) => {
                                // RPC succeeded but returned empty — try disk fallback.
                                match self.registry.get(&brain_name) {
                                    Some(cfg) => Self::list_sessions_from_disk(cfg),
                                    None => Ok(Vec::new()),
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "list_sessions failed, trying filesystem fallback");
                                match self.registry.get(&brain_name) {
                                    Some(cfg) => Self::list_sessions_from_disk(cfg),
                                    None => Err(anyhow::anyhow!(
                                        "Agent '{}' not found in registry for disk fallback",
                                        brain_name
                                    )),
                                }
                            }
                        };

                        match sessions_result {
                            Ok(sessions) => {
                                let (brain_sessions, worker_sessions) =
                                    classify_sessions(sessions, &self.repo_root);
                                if !worker_sessions.is_empty() {
                                    debug!(
                                        count = worker_sessions.len(),
                                        "Worker sessions excluded from brain picker"
                                    );
                                }
                                self.emit(SpurEvent::now(SpurEventBody::SessionsListed {
                                    agent: brain_name.clone(),
                                    sessions: brain_sessions,
                                }));
                            }
                            Err(e) => {
                                error!(error = %e, "list_sessions failed (no fallback available)");
                                self.emit(SpurEvent::now(SpurEventBody::SessionsListError {
                                    message: e.to_string(),
                                }));
                            }
                        }

                        agent_connection = Some(ActiveConnection {
                            transport: conn,
                            brain_name,
                            attach_guard,
                            fs_unsafe,
                            init_response,
                        });
                    }

                    // ── ResumeSession ─────────────────────────────────────
                    InteractiveInput::ResumeSession { session_id } => {
                        self.retire_active_brain(
                            &mut brain,
                            &mut agent_connection,
                            &mut scheduler,
                            &overflow_continuations,
                            spur_acp::domain::events::BrainRetireReason::ResumeSwitch,
                            Some(SessionId(session_id.clone())),
                        )
                        .await;

                        let ActiveConnection {
                            transport: connection,
                            brain_name,
                            attach_guard,
                            fs_unsafe,
                            init_response,
                        } = match agent_connection.take() {
                            Some(existing) => existing,
                            None => {
                                // Emit BrainConnecting before attempting spawn so the
                                // UI can transition to a "connecting" loading state.
                                self.emit(SpurEvent::now(SpurEventBody::BrainConnecting {
                                    session: SessionId(session_id.clone()),
                                    brain_name: self.selected_brain_name(brain_override.as_deref()),
                                }));
                                match self
                                    .connect_brain(brain_override.as_deref(), permission_tx.clone())
                                    .await
                                {
                                    Ok((transport, brain_name, init_response)) => {
                                        ActiveConnection {
                                            transport,
                                            brain_name,
                                            attach_guard: None,
                                            fs_unsafe: false,
                                            init_response,
                                        }
                                    }
                                    Err(e) => {
                                        let error_message = format_error_chain(&e);
                                        error!(error = %error_message, "Failed to connect brain for resume");
                                        self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                            session: SessionId(session_id.clone()),
                                            message: error_message,
                                        }));
                                        continue;
                                    }
                                }
                            }
                        };

                        let original_session_id = session_id.clone();
                        let loading_session_id = spur_mcp::plan::labels::derive_brain_session_id(
                            &spur_acp::SessionId(session_id.clone()),
                        )
                        .as_session_id()
                        .clone();
                        // Emit SessionLoading before the RPC so the UI can show a
                        // "loading session" state while the brain retrieves history.
                        self.emit(SpurEvent::now(SpurEventBody::SessionLoading {
                            session: loading_session_id,
                        }));
                        match self
                            .load_brain_session(
                                connection,
                                brain_name,
                                permission_tx.clone(),
                                session_id,
                                false,
                                false,
                                attach_guard,
                                fs_unsafe,
                                init_response,
                            )
                            .await
                        {
                            Ok((session, mut history_stream, _load_outcome)) => {
                                let spur_id = session.spur_session_id.clone();
                                let mut history_count = 0usize;
                                while let Some(notification) = history_stream.next().await {
                                    history_count += 1;
                                    self.emit(SpurEvent::now(SpurEventBody::AgentNotification {
                                        session: spur_id.clone(),
                                        notification: Box::new(notification),
                                    }));
                                }

                                if history_count == 0 {
                                    let entries =
                                        Self::read_session_history_from_disk(&original_session_id);
                                    if !entries.is_empty() {
                                        info!(
                                            count = entries.len(),
                                            "Replaying conversation history from disk"
                                        );
                                        self.emit(SpurEvent::now(SpurEventBody::SessionHistory {
                                            session: spur_id.clone(),
                                            entries,
                                        }));
                                    }
                                }

                                brain = Some(session);
                                // Register the resumed session with the scheduler so
                                // future continuations target the correct session id.
                                // No eviction emission here — the note_session_swap(None)
                                // above already drained any stale continuations.
                                //
                                // MUST be `spur_session_id`, not `acp_session_id`: the
                                // scheduler's `push_continuation` compares against
                                // `BrainContinuation.brain_session`, which the MCP server
                                // stamps from `McpCallbackServer.brain_session_id` (the
                                // SPUR UUID). See
                                // tests/continuation_brain_session_wiring.rs.
                                if let Some(ref b) = brain {
                                    scheduler.note_session_swap(
                                        Some(b.spur_session_id.clone().into()),
                                        &overflow_continuations,
                                    );
                                }
                                // Session is fully loaded — history replayed, brain
                                // installed.  Emit SessionLoaded so the UI can
                                // transition out of the loading state.
                                self.emit(SpurEvent::now(SpurEventBody::SessionLoaded {
                                    session: spur_id.clone(),
                                }));
                                self.emit(SpurEvent::now(SpurEventBody::TurnComplete {
                                    session: spur_id,
                                }));
                            }
                            Err(LoadBrainSessionError::AlreadyAttached { acp_id, holder }) => {
                                self.emit(SpurEvent::now(SpurEventBody::SessionAttachRejected {
                                    acp_session_id: acp_id,
                                    holder,
                                    fs_unsafe: false,
                                }));
                            }
                            Err(LoadBrainSessionError::Other(e)) => {
                                let error_message = format_error_chain(&e);
                                error!(error = %error_message, "Failed to load brain session");
                                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                    session: SessionId(original_session_id.clone()),
                                    message: error_message,
                                }));
                            }
                        }
                    }

                    // ── VendorExec ────────────────────────────────────────
                    InteractiveInput::VendorExec {
                        session,
                        method,
                        mut params,
                    } => {
                        if let Some(b) = brain.as_mut() {
                            if let Some(obj) = params.as_object_mut() {
                                obj.insert("sessionId".into(), serde_json::json!(b.acp_session_id));
                            } else {
                                warn!(
                                    method = %method,
                                    "VendorExec params is not a JSON object; sessionId not injected"
                                );
                            }
                            let brain_name_for_log = b.brain_name.clone();
                            let call_result = b.connection.call_ext(&method, params).await;
                            match call_result {
                                Ok(resp) => {
                                    self.emit(SpurEvent::now(
                                        SpurEventBody::AgentExtNotification {
                                            session: session.clone(),
                                            method: format!("{}/response", method),
                                            params: resp,
                                        },
                                    ));
                                }
                                Err(e) => {
                                    warn!(
                                        brain = %brain_name_for_log,
                                        method = %method,
                                        error = %e,
                                        "vendor exec call failed"
                                    );
                                    if is_connection_death(&e) {
                                        if let Some(dead) = brain.take() {
                                            let reason =
                                                format!("vendor exec `{method}` died: {e}");
                                            if let Some(new_brain) = self
                                                .reconnect_with_events(
                                                    dead,
                                                    permission_tx.clone(),
                                                    brain_override.as_deref(),
                                                    reason,
                                                    &mut reconnect_failures,
                                                    RECONNECT_CIRCUIT_LIMIT,
                                                    RECONNECT_CIRCUIT_WINDOW,
                                                )
                                                .await
                                            {
                                                brain = Some(new_brain);
                                            }
                                        }
                                    } else {
                                        self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                            session,
                                            message: format!(
                                                "vendor exec `{}` failed: {}",
                                                method, e
                                            ),
                                        }));
                                    }
                                }
                            }
                        } else {
                            warn!(method = %method, "VendorExec received but no active brain session");
                        }
                    }

                    // ── SetSessionMode ────────────────────────────────────
                    InteractiveInput::SetSessionMode { mode_id } => {
                        if let Some(b) = brain.as_mut() {
                            let req = SetSessionModeRequest::new(
                                agent_client_protocol::schema::SessionId::new(
                                    b.acp_session_id.clone(),
                                ),
                                agent_client_protocol::schema::SessionModeId::new(
                                    std::sync::Arc::<str>::from(mode_id.as_str()),
                                ),
                            );
                            if let Err(e) = b.connection.set_session_mode(req).await {
                                warn!(
                                    brain = %b.brain_name,
                                    session_id = %b.spur_session_id,
                                    mode_id = %mode_id,
                                    error = %e,
                                    "set_session_mode failed"
                                );
                            }
                        } else {
                            warn!(
                                mode_id = %mode_id,
                                "SetSessionMode received but no active brain session"
                            );
                        }
                    }

                    // ── SetSessionConfigOption ───────────────────────────
                    InteractiveInput::SetSessionConfigOption { config_id, value } => {
                        if let Some(b) = brain.as_mut() {
                            let req =
                                agent_client_protocol::schema::SetSessionConfigOptionRequest::new(
                                    agent_client_protocol::schema::SessionId::new(
                                        b.acp_session_id.clone(),
                                    ),
                                    agent_client_protocol::schema::SessionConfigId::new(
                                        std::sync::Arc::<str>::from(config_id.as_str()),
                                    ),
                                    agent_client_protocol::schema::SessionConfigValueId::new(
                                        std::sync::Arc::<str>::from(value.as_str()),
                                    ),
                                );
                            match b.connection.set_session_config_option(req).await {
                                Ok(resp) => {
                                    self.replace_session_config_options(b, resp.config_options);
                                }
                                Err(e) => {
                                    warn!(
                                        brain = %b.brain_name,
                                        session_id = %b.spur_session_id,
                                        config_id = %config_id,
                                        value = %value,
                                        error = %e,
                                        "set_session_config_option failed"
                                    );
                                }
                            }
                        } else {
                            warn!(
                                config_id = %config_id,
                                value = %value,
                                "SetSessionConfigOption received but no active brain session"
                            );
                        }
                    }

                    // ── SetSessionModel (M9 F-C) ──────────────────────────
                    InteractiveInput::SetSessionModel { value } => {
                        if let Some(b) = brain.as_mut() {
                            if let Err(e) =
                                Orchestrator::dispatch_set_session_model(b, value.clone()).await
                            {
                                warn!(
                                    brain = %b.brain_name,
                                    session_id = %b.spur_session_id,
                                    value = %value,
                                    error = %e,
                                    "set_session_model failed"
                                );
                            }
                        } else {
                            warn!(
                                value = %value,
                                "SetSessionModel received but no active brain session"
                            );
                        }
                    }

                    // ── CancelStream (outside active turn) ────────────────
                    InteractiveInput::CancelStream { session } => {
                        tracing::debug!(
                            session = %session,
                            "CancelStream received outside active turn; dropping (no stream to cancel)"
                        );
                    }

                    // ── RefreshIssues ─────────────────────────────────────
                    InteractiveInput::RefreshIssues => {
                        if let Some(pm) = &self.pm_service {
                            refresh_pm_state(pm, &self.funnel, Some(1000), false).await;
                        } else {
                            self.funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "RefreshIssues".into(),
                                error: "No issue tracker configured".into(),
                                id: None,
                            });
                        }
                    }

                    // ── RefreshPlans ──────────────────────────────────────
                    InteractiveInput::RefreshPlans => {
                        if let Some(pm) = &self.pm_service {
                            let current_session = brain.as_ref().map(|b| &b.spur_session_id);
                            match load_plan_summaries(pm, current_session).await {
                                Ok(load) => {
                                    self.funnel.emit(SpurEventBody::PlansLoaded {
                                        plans: load.plans,
                                        warnings: load.warnings,
                                    });
                                }
                                Err(e) => {
                                    self.funnel.emit(SpurEventBody::PlanCommandError {
                                        operation: "RefreshPlans".into(),
                                        plan_id: None,
                                        error: e.to_string(),
                                    });
                                }
                            }
                        } else {
                            self.funnel.emit(SpurEventBody::PlanCommandError {
                                operation: "RefreshPlans".into(),
                                plan_id: None,
                                error: "No issue tracker configured".into(),
                            });
                        }
                    }

                    // ── ClaimPlan ─────────────────────────────────────────
                    InteractiveInput::ClaimPlan { plan_id } => {
                        let server = brain
                            .as_ref()
                            .and_then(|b| b.mcp_server.as_ref())
                            .map(Arc::clone);
                        if let Some(server) = server {
                            if let Err(error) = server.call_claim_plan(&plan_id).await {
                                self.funnel.emit(SpurEventBody::PlanCommandError {
                                    operation: "ClaimPlan".into(),
                                    plan_id: Some(plan_id),
                                    error,
                                });
                            } else if let Some(pm) = &self.pm_service {
                                let current_session = brain.as_ref().map(|b| &b.spur_session_id);
                                match load_plan_summaries(pm, current_session).await {
                                    Ok(load) => {
                                        self.funnel.emit(SpurEventBody::PlansLoaded {
                                            plans: load.plans,
                                            warnings: load.warnings,
                                        });
                                    }
                                    Err(error) => {
                                        self.funnel.emit(SpurEventBody::PlanCommandError {
                                            operation: "RefreshPlans".into(),
                                            plan_id: None,
                                            error: error.to_string(),
                                        });
                                    }
                                }
                            }
                        } else {
                            let error = if brain.is_some() {
                                "Brain session initializing - try again in a moment".into()
                            } else {
                                "No active brain session - start one to claim plans".into()
                            };
                            self.funnel.emit(SpurEventBody::PlanCommandError {
                                operation: "ClaimPlan".into(),
                                plan_id: Some(plan_id),
                                error,
                            });
                        }
                    }

                    // ── ResumePlan ────────────────────────────────────────
                    InteractiveInput::ResumePlan { plan_id } => {
                        let server = brain
                            .as_ref()
                            .and_then(|b| b.mcp_server.as_ref())
                            .map(Arc::clone);
                        if let Some(server) = server {
                            if let Err(error) = server.call_resume_plan(&plan_id).await {
                                self.funnel.emit(SpurEventBody::PlanCommandError {
                                    operation: "ResumePlan".into(),
                                    plan_id: Some(plan_id),
                                    error,
                                });
                            }
                            // On success, the reconciler emits PlanSnapshotUpdated downstream.
                        } else {
                            // Distinguish "no brain" from "brain mid-init" so the user knows
                            // whether to start a session or just wait a moment.
                            let error = if brain.is_some() {
                                "Brain session initializing — try again in a moment".into()
                            } else {
                                "No active brain session — start one to resume plans".into()
                            };
                            self.funnel.emit(SpurEventBody::PlanCommandError {
                                operation: "ResumePlan".into(),
                                plan_id: Some(plan_id),
                                error,
                            });
                        }
                    }

                    // ── InspectPlan ───────────────────────────────────────
                    InteractiveInput::InspectPlan { plan_id } => {
                        let server = brain
                            .as_ref()
                            .and_then(|b| b.mcp_server.as_ref())
                            .map(Arc::clone);
                        if let Some(server) = server {
                            if let Err(error) = server.call_inspect_plan(&plan_id).await {
                                self.funnel.emit(SpurEventBody::PlanCommandError {
                                    operation: "InspectPlan".into(),
                                    plan_id: Some(plan_id),
                                    error,
                                });
                            }
                        } else {
                            let error = if brain.is_some() {
                                "Brain session initializing - try again in a moment".into()
                            } else {
                                "No active brain session - start one to inspect plans".into()
                            };
                            self.funnel.emit(SpurEventBody::PlanCommandError {
                                operation: "InspectPlan".into(),
                                plan_id: Some(plan_id),
                                error,
                            });
                        }
                    }

                    // ── GetIssueDetail ────────────────────────────────────
                    InteractiveInput::GetIssueDetail { id } => {
                        tracing::debug!(
                            target: "issue_probe",
                            site = "orch_legacy_handler",
                            id = %id,
                            "GetIssueDetail handled via legacy user_rx path — TUI should be on data_rx",
                        );
                        // PROBE: issue_detail_latency
                        let handler_started = std::time::Instant::now();
                        tracing::info!(
                            target: "issue_probe",
                            site = "orch_handler_entry",
                            id = %id,
                            "GetIssueDetail entered run_interactive handler (idle path)",
                        );
                        if let Some(pm) = &self.pm_service {
                            let pm_call_started = std::time::Instant::now();
                            match pm.get_issue(&id).await {
                                Ok(issue) => {
                                    let pm_get_issue_ms =
                                        pm_call_started.elapsed().as_millis() as u64;
                                    tracing::info!(
                                        target: "issue_probe",
                                        site = "orch_pm_get_issue_ok",
                                        id = %id,
                                        pm_get_issue_ms = pm_get_issue_ms,
                                        total_handler_ms = handler_started.elapsed().as_millis() as u64,
                                        "pm.get_issue resolved",
                                    );
                                    self.funnel.emit(SpurEventBody::IssueDetailFetched {
                                        requested_id: id,
                                        issue: issue_to_detail_event(&issue),
                                    });
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        target: "issue_probe",
                                        site = "orch_pm_get_issue_err",
                                        id = %id,
                                        pm_get_issue_ms = pm_call_started.elapsed().as_millis() as u64,
                                        error = %e,
                                        "pm.get_issue failed",
                                    );
                                    self.funnel.emit(SpurEventBody::IssueCommandError {
                                        operation: "GetIssueDetail".into(),
                                        error: e.to_string(),
                                        id: Some(id),
                                    });
                                }
                            }
                        } else {
                            self.funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "GetIssueDetail".into(),
                                error: "No issue tracker configured".into(),
                                id: Some(id),
                            });
                        }
                    }

                    // ── GetIssueGraph ────────────────────────────────────
                    InteractiveInput::GetIssueGraph { id } => {
                        tracing::debug!(
                            target: "issue_probe",
                            site = "orch_legacy_handler",
                            id = %id,
                            "GetIssueGraph handled via legacy user_rx path — TUI should be on data_rx",
                        );
                        handle_get_issue_graph(self.pm_service.as_deref(), &self.funnel, id).await;
                    }

                    // ── UpdateIssue ───────────────────────────────────────
                    InteractiveInput::UpdateIssue { id, update } => {
                        if let Some(pm) = &self.pm_service {
                            match pm.update_issue(&id, update.clone()).await {
                                Ok(()) => {
                                    self.funnel.emit(SpurEventBody::IssueUpdated {
                                        source: pm.source_str().into(),
                                        id,
                                        status: update.status.clone(),
                                        assignee: update.assignee.clone(),
                                    });
                                }
                                Err(e) => {
                                    self.funnel.emit(SpurEventBody::IssueCommandError {
                                        operation: "UpdateIssue".into(),
                                        error: e.to_string(),
                                        id: None,
                                    });
                                }
                            }
                        } else {
                            self.funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "UpdateIssue".into(),
                                error: "No issue tracker configured".into(),
                                id: None,
                            });
                        }
                    }

                    // ── SubmitReview ──────────────────────────────────────
                    // Intentional no-op: spur-cli routes SubmitReview to the
                    // review_dispatcher_loop task, not to run_interactive.
                    InteractiveInput::SubmitReview { .. } => {}
                }

                // Done handling non-prompt variant — go back to top of loop.
                continue;
            }

            // ── (d) Scheduler returned a prompt action — fire the brain turn ──
            let mut user_input_opt: Option<InteractiveInput> = None;
            let mut drained_batch: Option<crate::scheduler::DrainedBatch> = None;
            let mut render_outcome: Option<crate::continuation_bridge::RenderOutcome> = None;

            // ── Build the blocks for this turn ─────────────────────────
            let prompt_blocks: Vec<ContentBlock> = match action {
                crate::scheduler::ScheduledAction::UserPrompt(user) => {
                    user_input_opt = Some(user);
                    match user_input_opt.as_ref() {
                        Some(InteractiveInput::Message { blocks, interrupt }) => {
                            if *interrupt {
                                strip_bang_prefix(blocks.clone())
                            } else {
                                blocks.clone()
                            }
                        }
                        Some(other) => {
                            // PROBE: issue_detail_latency
                            tracing::warn!(
                                target: "issue_probe",
                                site = "orch_scheduler_drop",
                                ?other,
                                "unexpected non-Message variant dequeued from scheduler; skipping turn"
                            );
                            continue;
                        }
                        None => unreachable!("user prompt must retain its input"),
                    }
                }
                crate::scheduler::ScheduledAction::MergedPrompt { user, batch } => {
                    user_input_opt = Some(user);
                    let base = match user_input_opt.as_ref() {
                        Some(InteractiveInput::Message { blocks, interrupt }) => {
                            if *interrupt {
                                strip_bang_prefix(blocks.clone())
                            } else {
                                blocks.clone()
                            }
                        }
                        Some(other) => {
                            tracing::warn!(
                                ?other,
                                "unexpected non-Message variant dequeued from scheduler; rolling back batch"
                            );
                            scheduler.rollback(batch, vec![]);
                            continue;
                        }
                        None => unreachable!("merged prompt must retain its input"),
                    };
                    let outcome = crate::continuation_bridge::render_merged_turn_with_spill_v2(
                        &base,
                        batch.items(),
                        crate::continuation_bridge::MERGE_BUDGET_DEFAULT_BYTES,
                    );
                    let blocks = outcome.blocks.clone();
                    drained_batch = Some(batch);
                    render_outcome = Some(outcome);
                    blocks
                }
                crate::scheduler::ScheduledAction::ContinuationPrompt(batch) => {
                    let outcome = crate::continuation_bridge::render_autonomous_turn_with_spill_v2(
                        batch.items(),
                        crate::continuation_bridge::MERGE_BUDGET_DEFAULT_BYTES,
                    );
                    let blocks = outcome.blocks.clone();
                    drained_batch = Some(batch);
                    render_outcome = Some(outcome);
                    blocks
                }
                crate::scheduler::ScheduledAction::IdleUntil { .. } => {
                    unreachable!("handled above")
                }
            };

            if !prompt_blocks.is_empty() || drained_batch.is_none() {
                // normal prompt path continues below
            } else {
                let (batch, outcome) = take_rendered_batch(&mut drained_batch, &mut render_outcome)
                    .expect("empty prompt still owns a batch");
                commit_rendered_batch(&mut scheduler, batch, outcome);
                continue;
            }

            // ── Lazy-spawn brain on first turn (or after crash) ─────────
            if brain.is_none() {
                let result = match agent_connection.take() {
                    Some(ActiveConnection {
                        transport: connection,
                        brain_name,
                        attach_guard,
                        fs_unsafe,
                        init_response,
                    }) => {
                        self.create_brain_session(
                            connection,
                            brain_name,
                            permission_tx.clone(),
                            attach_guard,
                            fs_unsafe,
                            init_response,
                        )
                        .await
                    }
                    None => {
                        self.spawn_brain_session(brain_override.as_deref(), permission_tx.clone())
                            .await
                    }
                };

                match result {
                    Ok(b) => {
                        // Wire the new session into the scheduler.
                        //
                        // The scheduler keys `push_continuation` on the SPUR
                        // session id (`spur_session_id`), NOT the ACP protocol
                        // session id (`acp_session_id`). These are distinct
                        // UUIDs — `spur_session_id` is SPUR-generated; the ACP
                        // agent returns its own session id from `new_session`.
                        // The MCP server stamps continuations with
                        // `spur_session_id` (see
                        // `McpCallbackServer.brain_session_id`), so we must
                        // seed the scheduler on the same id to avoid every
                        // detached continuation being dropped as StaleSession.
                        // Regression test: tests/continuation_brain_session_wiring.rs.
                        let new_sid = Some(b.spur_session_id.clone().into());
                        scheduler.note_session_swap(new_sid, &overflow_continuations);
                        brain = Some(b);
                    }
                    Err(e) => {
                        let error_message = format_error_chain(&e);
                        error!(error = %error_message, "Failed to spawn brain");
                        if Self::is_auth_required_error(&e) {
                            self.emit(SpurEvent::now(SpurEventBody::AuthRequired {
                                session: SessionId(String::new()),
                                message: Self::auth_required_banner(),
                            }));
                        } else {
                            self.emit(SpurEvent::now(SpurEventBody::BrainError {
                                session: SessionId::new(),
                                message: error_message,
                            }));
                        }
                        continue;
                    }
                }
            }
            let b = brain.as_mut().unwrap();

            // ── Send prompt ──────────────────────────────────────────────
            let prompt_request = PromptRequest::new(b.acp_session_id.clone(), prompt_blocks);
            let spur_sid_for_log = b.spur_session_id.clone();
            let continuations_count = render_outcome
                .as_ref()
                .map(|outcome| outcome.delivered_keys.len())
                .unwrap_or(0);

            let turn_kind = match (&user_input_opt, drained_batch.is_some()) {
                (Some(_), false) => "user_only",
                (Some(_), true) => "merged",
                (None, true) => "continuation_only",
                (None, false) => "empty_defensive",
            };
            tracing::debug!(
                continuation_probe = true,
                site = "D_prompt_dispatch",
                turn_kind = turn_kind,
                continuations = continuations_count,
                acp_session = %b.acp_session_id,
                spur_session = %spur_sid_for_log,
                "orchestrator: dispatching session/prompt"
            );
            // INV-C3 observable half: publish PromptDispatched on the funnel
            // BEFORE the transport call. Pairs with upstream DelegationCompleted
            // so subscribers can verify UI-before-model ordering via `seq`.
            // Emitted for every dispatch (including `user_only`) so the event
            // stream reflects every turn boundary.
            self.funnel.emit(SpurEventBody::PromptDispatched {
                session: spur_sid_for_log.clone(),
                turn_kind: turn_kind.to_string(),
                continuations_count,
            });

            let _turn_guard = TurnGuard::arm(scheduler.turn_flag());
            let prompt_started_at = std::time::Instant::now();
            let mut stream = match b.connection.prompt(prompt_request).await {
                Ok(s) => {
                    if let Some((batch, outcome)) =
                        take_rendered_batch(&mut drained_batch, &mut render_outcome)
                    {
                        commit_rendered_batch(&mut scheduler, batch, outcome);
                    }
                    s
                }
                Err(e) => {
                    if let Some((batch, outcome)) =
                        take_rendered_batch(&mut drained_batch, &mut render_outcome)
                    {
                        let dropped_terminal = dropped_terminal_from_render_outcome(&outcome);
                        scheduler.rollback(batch, dropped_terminal);
                    }
                    let error_message = format_error_chain(&e);
                    error!(error = %error_message, "Brain prompt failed");
                    if Self::is_auth_required_error(&e) {
                        self.emit(SpurEvent::now(SpurEventBody::AuthRequired {
                            session: spur_sid_for_log,
                            message: Self::auth_required_banner(),
                        }));
                        let mut dead = brain.take().expect("brain.as_mut() just held it");
                        dead.delegation_handle.abort();
                        if let Some(h) = dead.notification_pump_handle.take() {
                            h.abort();
                        }
                        self.self_held.remove(&spur_acp::BrainSessionId::from(
                            dead.spur_session_id.clone(),
                        ));
                        retire_brain_session(
                            &self.funnel,
                            &dead.spur_session_id,
                            &mut dead.mcp_server,
                            Some(&mut dead.mcp_guard),
                            &self.worker_mcp_servers,
                            &mut scheduler,
                            &overflow_continuations,
                            None,
                        )
                        .await;
                        let _ = dead.connection.shutdown().await;
                        continue;
                    }
                    if is_connection_death(&e) {
                        let dead = brain.take().expect("brain.as_mut() just held it");
                        let reason = format!("prompt died: {e}");
                        if let Some(new_brain) = self
                            .reconnect_with_events(
                                dead,
                                permission_tx.clone(),
                                brain_override.as_deref(),
                                reason,
                                &mut reconnect_failures,
                                RECONNECT_CIRCUIT_LIMIT,
                                RECONNECT_CIRCUIT_WINDOW,
                            )
                            .await
                        {
                            brain = Some(new_brain);
                        }
                        continue;
                    }
                    self.emit(SpurEvent::now(SpurEventBody::BrainError {
                        session: spur_sid_for_log,
                        message: error_message,
                    }));
                    let mut dead = brain.take().expect("brain.as_mut() just held it");
                    dead.delegation_handle.abort();
                    if let Some(h) = dead.notification_pump_handle.take() {
                        h.abort();
                    }
                    self.self_held.remove(&spur_acp::BrainSessionId::from(
                        dead.spur_session_id.clone(),
                    ));
                    retire_brain_session(
                        &self.funnel,
                        &dead.spur_session_id,
                        &mut dead.mcp_server,
                        Some(&mut dead.mcp_guard),
                        &self.worker_mcp_servers,
                        &mut scheduler,
                        &overflow_continuations,
                        None,
                    )
                    .await;
                    let _ = dead.connection.shutdown().await;
                    continue;
                }
            };

            // ── Stream output + check for interrupts ─────────────────────
            let mut cancel_deadline: Option<tokio::time::Instant> = None;
            let mut cancel_resolved = false;
            {
                let b = brain.as_mut().unwrap();

                loop {
                    tokio::select! {
                        item = stream.next() => {
                            match item {
                                Some(notification) => {
                                    let variant = match &notification.update {
                                        spur_acp::SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
                                        spur_acp::SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
                                        spur_acp::SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
                                        spur_acp::SessionUpdate::ToolCall(_) => "tool_call",
                                        spur_acp::SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
                                        spur_acp::SessionUpdate::Plan(_) => "plan",
                                        spur_acp::SessionUpdate::AvailableCommandsUpdate(_) => "available_commands_update",
                                        spur_acp::SessionUpdate::CurrentModeUpdate(_) => "current_mode_update",
                                        _ => "other",
                                    };
                                    let text_len = match &notification.update {
                                        spur_acp::SessionUpdate::AgentMessageChunk(c)
                                        | spur_acp::SessionUpdate::AgentThoughtChunk(c)
                                        | spur_acp::SessionUpdate::UserMessageChunk(c) => {
                                            match &c.content {
                                                spur_acp::ContentBlock::Text(tc) => tc.text.len(),
                                                _ => 0,
                                            }
                                        }
                                        _ => 0,
                                    };
                                    tracing::debug!(
                                        streaming_probe = true,
                                        site = "C_orchestrator_emit",
                                        variant = variant,
                                        text_len = text_len,
                                        since_prompt_ms = prompt_started_at.elapsed().as_millis() as u64,
                                        session = %b.spur_session_id,
                                        "orchestrator emitting AgentNotification"
                                    );
                                    self.emit(SpurEvent::now(SpurEventBody::AgentNotification {
                                        session: b.spur_session_id.clone(),
                                        notification: Box::new(notification),
                                    }));
                                }
                                None => break, // Turn complete
                            }
                        }
                        Some(queued) = user_input_rx.recv() => {
                            match queued {
                                InteractiveInput::Message { blocks: msg_blocks, interrupt: msg_interrupt } => {
                                    if msg_interrupt {
                                        let _ = b.connection.cancel(&b.acp_session_id).await;
                                        arm_cancel_deadline(&mut cancel_deadline);
                                    }
                                    let queued_blocks = if msg_interrupt {
                                        strip_bang_prefix(msg_blocks)
                                    } else {
                                        msg_blocks
                                    };
                                    scheduler.push_user(InteractiveInput::Message {
                                        blocks: queued_blocks,
                                        interrupt: false,
                                    });
                                }
                                InteractiveInput::CancelStream { session } => {
                                    let _ = session;
                                    let _ = b.connection.cancel(&b.acp_session_id).await;
                                    arm_cancel_deadline(&mut cancel_deadline);
                                }
                                InteractiveInput::SystemContinuation { continuation, .. } => {
                                    scheduler.push_continuation(continuation);
                                }
                                other => {
                                    // PROBE: issue_detail_latency
                                    // Non-prompt, non-cancel variants arriving mid-stream:
                                    // push to scheduler as user input so they run after the turn.
                                    // NOTE: when the scheduler later pops these as ScheduledAction::UserPrompt,
                                    // the non-Message arm (orchestrator.rs `unexpected non-Message variant
                                    // dequeued from scheduler; skipping turn`) silently drops them.
                                    let probe_label = match &other {
                                        InteractiveInput::RefreshIssues => {
                                            Some("RefreshIssues".to_string())
                                        }
                                        _ => None,
                                    };
                                    if let Some(label) = probe_label {
                                        tracing::warn!(
                                            target: "issue_probe",
                                            site = "orch_queued_during_stream",
                                            input = %label,
                                            "non-Message InteractiveInput queued mid-stream — will likely be dropped at scheduler dequeue",
                                        );
                                    }
                                    scheduler.push_user(other);
                                }
                            }
                        }
                        _ = async {
                            match cancel_deadline {
                                Some(deadline) => tokio::time::sleep_until(deadline).await,
                                None => futures::future::pending().await,
                            }
                        } => {
                            warn!("Cancel timeout — force-ending stream");
                            cancel_resolved = true;
                            break;
                        }
                    }
                }
            }

            // Fire the grace window if a cancel was ARMED during this turn, regardless
            // of whether the stream ended naturally or the deadline force-broke. Either
            // way the user just expressed "stop" intent; autonomous continuations should
            // pause briefly per G5.
            if cancel_resolved || cancel_deadline.is_some() {
                scheduler.note_cancel_resolved(std::time::Instant::now());
            }

            // Emit turn complete
            let b = brain.as_mut().unwrap();
            self.emit(SpurEvent::now(SpurEventBody::TurnComplete {
                session: b.spur_session_id.clone(),
            }));
        }

        // ── Cleanup ─────────────────────────────────────────────────────
        if brain.is_some() {
            self.shutdown_active_brain(
                &mut brain,
                &mut agent_connection,
                &mut scheduler,
                &overflow_continuations,
            )
            .await;
        }
        // Drop any pre-connected but unused connection.
        if let Some(ActiveConnection {
            transport: mut conn,
            ..
        }) = agent_connection.take()
        {
            let _ = conn.shutdown().await;
        }

        info!("Interactive session ended");
        Ok(())
    }
}

#[cfg(test)]
mod list_sessions_tests {
    use super::*;
    use agent_client_protocol::schema::{ListSessionsRequest, SessionInfo};
    use async_trait::async_trait;
    use futures::Stream;
    use std::collections::VecDeque;
    use std::pin::Pin;

    struct NonProgressingCursorConnection {
        calls: usize,
    }

    struct TwoPhaseConnection {
        requests: Vec<ListSessionsRequest>,
        responses: VecDeque<Vec<SessionInfo>>,
    }

    #[async_trait]
    impl AgentConnection for NonProgressingCursorConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::InitializeResponse> {
            unimplemented!("NonProgressingCursorConnection: initialize")
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<agent_client_protocol::schema::NewSessionResponse> {
            unimplemented!("NonProgressingCursorConnection: new_session")
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = spur_acp::SessionNotification> + Send>>>
        {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }

        async fn list_sessions(
            &mut self,
            request: ListSessionsRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::ListSessionsResponse> {
            assert!(
                request.cwd.as_deref() == Some(Path::new("/repo/spur")) || request.cwd.is_none()
            );
            assert!(
                request.cursor.is_none() || request.cursor.as_deref() == Some("same"),
                "unexpected cursor {:?}",
                request.cursor
            );
            self.calls += 1;

            Ok(
                agent_client_protocol::schema::ListSessionsResponse::new(vec![SessionInfo::new(
                    format!("session-{}", self.calls),
                    "/repo/spur",
                )])
                .next_cursor(Some("same".to_string())),
            )
        }
    }

    #[async_trait]
    impl AgentConnection for TwoPhaseConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::InitializeResponse> {
            unimplemented!("TwoPhaseConnection: initialize")
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<agent_client_protocol::schema::NewSessionResponse> {
            unimplemented!("TwoPhaseConnection: new_session")
        }

        async fn prompt(
            &mut self,
            _request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn Stream<Item = spur_acp::SessionNotification> + Send>>>
        {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }

        async fn list_sessions(
            &mut self,
            request: ListSessionsRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::ListSessionsResponse> {
            self.requests.push(request);
            let sessions = self.responses.pop_front().expect("queued response");
            Ok(agent_client_protocol::schema::ListSessionsResponse::new(
                sessions,
            ))
        }
    }

    #[tokio::test]
    async fn list_sessions_queries_repo_root_then_broad_and_merges_by_session_id() {
        let mut scoped = SessionInfo::new("root", "/repo/spur");
        scoped.title = Some("scoped title".into());
        let mut broad_duplicate = SessionInfo::new("root", "/repo/spur");
        broad_duplicate.title = Some("broad title".into());
        let worktree = SessionInfo::new("worker", "/repo/spur/.spur/worktrees/task-1");
        let outside = SessionInfo::new("outside", "/tmp/other");
        let mut conn = TwoPhaseConnection {
            requests: Vec::new(),
            responses: VecDeque::from([
                vec![scoped],
                vec![broad_duplicate, worktree.clone(), outside.clone()],
            ]),
        };

        let sessions = Orchestrator::list_sessions_from_rpc(&mut conn, Path::new("/repo/spur"))
            .await
            .expect("list sessions");

        assert_eq!(conn.requests.len(), 2);
        assert_eq!(
            conn.requests[0].cwd.as_deref(),
            Some(Path::new("/repo/spur"))
        );
        assert!(conn.requests[1].cwd.is_none());

        let ids: Vec<_> = sessions.iter().map(|s| s.session_id.0.as_ref()).collect();
        assert_eq!(ids, vec!["root", "worker", "outside"]);
        assert_eq!(sessions[0].title.as_deref(), Some("scoped title"));
    }

    #[tokio::test]
    async fn pagination_breaks_on_non_progressing_cursor() {
        let mut conn = NonProgressingCursorConnection { calls: 0 };

        let sessions = Orchestrator::list_sessions_from_rpc(&mut conn, Path::new("/repo/spur"))
            .await
            .expect("list sessions");

        assert_eq!(conn.calls, 4);
        assert!(conn.calls <= 6);
        assert_eq!(sessions.len(), 4);
    }
}

#[cfg(test)]
mod peer_mailbox_drain_tests {
    use super::delegation::peer_mailbox::drain_peer_acks_with_timeout;
    use crate::peer_mailbox::router::Acceptance;
    use crate::peer_mailbox::{
        prompt_builder::PeerPromptContextBuilder, InMemoryLedger, Limits, PeerMailboxBundle,
        PeerMailboxLedger, PeerMailboxRouter,
    };
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::peer_message::{
        LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
    };
    use spur_mcp::plan::scope_snapshot::PlanScopeSnapshot;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

    struct DrainFixture {
        bundle: PeerMailboxBundle,
        funnel: crate::event_funnel::FunnelHandle,
        snapshot: PlanScopeSnapshot,
        events: UnboundedReceiver<SpurEventBody>,
    }

    fn fixture(targets: &[&str]) -> DrainFixture {
        let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
        let (funnel, events) = crate::event_funnel::test_channel();
        let (reconciler_tx, _reconciler_rx) = unbounded_channel();
        let router = Arc::new(PeerMailboxRouter::new(
            ledger.clone(),
            funnel.clone(),
            reconciler_tx,
            Limits::default(),
        ));
        let bundle = PeerMailboxBundle {
            router,
            builder: Arc::new(PeerPromptContextBuilder::new(ledger.clone())),
            ledger,
            brain_session_id_slot: Arc::new(tokio::sync::RwLock::new(Some("bs".into()))),
        };

        let mut delegation_to_task = HashMap::new();
        delegation_to_task.insert(DelegationId("src".into()), "task-src".into());

        let mut peer_edges = HashSet::new();
        for target in targets {
            let task_id = format!("task-{target}");
            delegation_to_task.insert(DelegationId((*target).into()), task_id.clone());
            peer_edges.insert(("task-src".into(), task_id));
        }

        DrainFixture {
            bundle,
            funnel,
            snapshot: PlanScopeSnapshot {
                plan_version: 1,
                peer_edges,
                delegation_to_task,
                delegation_to_issue: HashMap::new(),
                superseded_tasks: HashSet::new(),
                terminal_tasks: HashSet::new(),
            },
            events,
        }
    }

    fn envelope(message_id: PeerMessageId, target: &DelegationId) -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id,
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: target.clone(),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Handoff,
            body: "ready for review".into(),
            sequence: 1,
        }
    }

    async fn accept_and_walk(
        fixture: &DrainFixture,
        message_id: PeerMessageId,
        target: &DelegationId,
        final_state: LedgerState,
    ) {
        match fixture
            .bundle
            .router
            .accept_or_reject("bs", envelope(message_id, target), &fixture.snapshot)
            .await
            .unwrap()
        {
            Acceptance::Created(_guard) => {}
            Acceptance::AlreadyAccepted => panic!("expected fresh peer message"),
        }

        fixture
            .bundle
            .ledger
            .transition(&message_id, LedgerState::Queued)
            .await
            .unwrap();
        fixture
            .bundle
            .ledger
            .transition(&message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();

        match final_state {
            LedgerState::DeliveredInflight => {}
            LedgerState::Delivered => {
                fixture
                    .bundle
                    .ledger
                    .transition(&message_id, LedgerState::Delivered)
                    .await
                    .unwrap();
            }
            other => panic!("unsupported drain test target state: {other:?}"),
        }
    }

    async fn spawn_drain(
        bundle: PeerMailboxBundle,
        target: DelegationId,
        quiet_window: Duration,
        max_total: Duration,
        brain_session_id: &'static str,
        funnel: crate::event_funnel::FunnelHandle,
        ack_rx: UnboundedReceiver<()>,
    ) -> tokio::task::JoinHandle<Duration> {
        let brain_session_id =
            spur_acp::BrainSessionId::new(spur_acp::types::SessionId(brain_session_id.into()));
        let start = tokio::time::Instant::now();
        let handle = tokio::spawn(async move {
            drain_peer_acks_with_timeout(
                &bundle,
                &target,
                quiet_window,
                max_total,
                &brain_session_id,
                &funnel,
                ack_rx,
            )
            .await;
            start.elapsed()
        });
        tokio::task::yield_now().await;
        handle
    }

    fn drain_events(events: &mut UnboundedReceiver<SpurEventBody>) -> Vec<SpurEventBody> {
        let mut out = Vec::new();
        while let Ok(event) = events.try_recv() {
            out.push(event);
        }
        out
    }

    fn ignored_timeout_events(
        events: &[SpurEventBody],
        message_id: PeerMessageId,
        target: &DelegationId,
    ) -> usize {
        ignored_events_with_reason(events, message_id, target, "drain_timeout")
    }

    fn ignored_events_with_reason(
        events: &[SpurEventBody],
        message_id: PeerMessageId,
        target: &DelegationId,
        expected_reason: &str,
    ) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    SpurEventBody::WorkerPeerMessageIgnored {
                        message_id: event_message_id,
                        target_delegation_id,
                        reason,
                        ..
                    } if *event_message_id == message_id
                        && target_delegation_id == target
                        && reason == expected_reason
                )
            })
            .count()
    }

    fn fixed_peer_message_id(suffix: u16) -> PeerMessageId {
        serde_json::from_str(&format!("\"00000000-0000-0000-0000-{suffix:012}\"")).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn drain_started_emits_with_candidates_at_start() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        for suffix in 800..803 {
            accept_and_walk(
                &fixture,
                fixed_peer_message_id(suffix),
                &target,
                LedgerState::Delivered,
            )
            .await;
        }

        let (ack_tx, ack_rx) = unbounded_channel();
        let _hold_open_until_timeout = ack_tx.clone();
        drop(ack_tx);

        let quiet_window = Duration::from_millis(100);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        tokio::time::advance(quiet_window).await;
        handle.await.unwrap();

        let events = drain_events(&mut fixture.events);
        let started_events: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if let SpurEventBody::WorkerPeerMessageDrainStarted {
                    brain_session_id,
                    target_delegation_id,
                    candidates_at_start,
                    cap_ms,
                    quiet_window_ms,
                } = event
                {
                    Some((
                        brain_session_id,
                        target_delegation_id,
                        *candidates_at_start,
                        *cap_ms,
                        *quiet_window_ms,
                    ))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(started_events.len(), 1);
        let (brain_session_id, event_target, candidates_at_start, cap_ms, quiet_window_ms) =
            started_events[0];
        assert_eq!(brain_session_id, "bs");
        assert_eq!(event_target, &target);
        assert_eq!(candidates_at_start, 3);
        assert_eq!(cap_ms, 60_000);
        assert_eq!(quiet_window_ms, 100);
    }

    #[tokio::test(start_paused = true)]
    async fn drain_timed_out_emits_when_quiet_window_exits_with_remaining() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id = fixed_peer_message_id(810);
        accept_and_walk(&fixture, message_id, &target, LedgerState::Delivered).await;

        let (ack_tx, ack_rx) = unbounded_channel();
        let _hold_open_until_timeout = ack_tx.clone();
        drop(ack_tx);

        let quiet_window = Duration::from_millis(100);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        tokio::time::advance(quiet_window).await;
        handle.await.unwrap();

        let events = drain_events(&mut fixture.events);
        let timeout_events: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if let SpurEventBody::WorkerPeerMessageDrainTimedOut {
                    brain_session_id,
                    target_delegation_id,
                    acks_received,
                    remaining_messages,
                    cap_ms,
                    quiet_window_ms,
                    actual_elapsed_ms,
                } = event
                {
                    Some((
                        brain_session_id,
                        target_delegation_id,
                        *acks_received,
                        *remaining_messages,
                        *cap_ms,
                        *quiet_window_ms,
                        *actual_elapsed_ms,
                    ))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(timeout_events.len(), 1);
        let (
            brain_session_id,
            event_target,
            acks_received,
            remaining_messages,
            cap_ms,
            quiet_window_ms,
            elapsed_ms,
        ) = timeout_events[0];
        assert_eq!(brain_session_id, "bs");
        assert_eq!(event_target, &target);
        assert_eq!(acks_received, 0);
        assert!(remaining_messages >= 1);
        assert_eq!(cap_ms, 60_000);
        assert_eq!(quiet_window_ms, 100);
        assert!(
            (100..=150).contains(&elapsed_ms),
            "actual_elapsed_ms: {elapsed_ms}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_timed_out_not_emitted_on_clean_exit() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());

        let (ack_tx, ack_rx) = unbounded_channel();
        let _hold_open_until_timeout = ack_tx.clone();
        drop(ack_tx);

        let quiet_window = Duration::from_millis(100);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target,
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        tokio::time::advance(quiet_window).await;
        handle.await.unwrap();

        let events = drain_events(&mut fixture.events);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    SpurEventBody::WorkerPeerMessageDrainTimedOut { .. }
                ))
                .count(),
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_cap_hit_emits_only_drain_capped_out() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id = fixed_peer_message_id(820);
        accept_and_walk(&fixture, message_id, &target, LedgerState::Delivered).await;

        let (_ack_tx, ack_rx) = unbounded_channel();
        let quiet_window = Duration::from_secs(10);
        let max_total = Duration::from_millis(100);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target,
            quiet_window,
            max_total,
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        tokio::time::advance(max_total).await;
        handle.await.unwrap();

        let events = drain_events(&mut fixture.events);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    SpurEventBody::WorkerPeerMessageDrainCappedOut { .. }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    SpurEventBody::WorkerPeerMessageDrainTimedOut { .. }
                ))
                .count(),
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_completes_after_quiet_window_with_no_acks() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000701\"").unwrap();
        accept_and_walk(&fixture, message_id, &target, LedgerState::Delivered).await;

        let (_ack_tx, ack_rx) = unbounded_channel();
        let quiet_window = Duration::from_millis(50);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;

        tokio::time::advance(quiet_window).await;
        let elapsed = handle.await.unwrap();

        assert!(elapsed >= quiet_window, "elapsed: {elapsed:?}");
        assert!(
            elapsed < quiet_window + Duration::from_millis(1),
            "elapsed: {elapsed:?}"
        );
        let entry = fixture.bundle.ledger.get(&message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Ignored);
        let events = drain_events(&mut fixture.events);
        assert_eq!(ignored_timeout_events(&events, message_id, &target), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn drain_resets_quiet_window_on_each_ack() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000702\"").unwrap();
        accept_and_walk(&fixture, message_id, &target, LedgerState::Delivered).await;

        let (ack_tx, ack_rx) = unbounded_channel();
        let heartbeat_tx = ack_tx.clone();
        let _hold_open_until_timeout = ack_tx.clone();
        drop(ack_tx);

        let quiet_window = Duration::from_secs(1);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;

        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(900)).await;
            heartbeat_tx.send(()).unwrap();
            tokio::task::yield_now().await;
            assert!(
                !handle.is_finished(),
                "drain finished before quiet window reset"
            );
        }
        drop(heartbeat_tx);

        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(
            !handle.is_finished(),
            "drain finished before the final quiet window elapsed"
        );
        tokio::time::advance(Duration::from_millis(1)).await;

        let elapsed = handle.await.unwrap();
        let expected = Duration::from_millis(4 * 900 + 1_000);
        assert!(
            elapsed >= expected && elapsed < expected + Duration::from_millis(1),
            "elapsed: {elapsed:?}, expected: {expected:?}"
        );

        let entry = fixture.bundle.ledger.get(&message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Ignored);
        let events = drain_events(&mut fixture.events);
        assert_eq!(ignored_timeout_events(&events, message_id, &target), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn drain_returns_immediately_when_sender_drops_with_no_pending() {
        let fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let (ack_tx, ack_rx) = unbounded_channel();
        drop(ack_tx);

        let quiet_window = Duration::from_secs(1);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target,
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        let elapsed = handle.await.unwrap();

        assert!(
            elapsed < quiet_window,
            "closed sender should bypass quiet window, elapsed: {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_forces_delivered_inflight_messages_to_ignored_after_timeout() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000703\"").unwrap();
        accept_and_walk(
            &fixture,
            message_id,
            &target,
            LedgerState::DeliveredInflight,
        )
        .await;

        let (ack_tx, ack_rx) = unbounded_channel();
        let _hold_open_until_timeout = ack_tx.clone();
        drop(ack_tx);

        let quiet_window = Duration::from_millis(100);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        tokio::time::advance(quiet_window).await;
        let elapsed = handle.await.unwrap();

        assert!(elapsed >= quiet_window, "elapsed: {elapsed:?}");
        let entry = fixture.bundle.ledger.get(&message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Ignored);
        let events = drain_events(&mut fixture.events);
        assert_eq!(ignored_timeout_events(&events, message_id, &target), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn drain_late_ack_after_timeout_is_safely_swallowed() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000704\"").unwrap();
        accept_and_walk(&fixture, message_id, &target, LedgerState::Delivered).await;

        let (ack_tx, ack_rx) = unbounded_channel();
        let stale_ack_tx = ack_tx.clone();
        drop(ack_tx);

        let quiet_window = Duration::from_millis(100);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        tokio::time::advance(quiet_window).await;
        handle.await.unwrap();

        assert!(
            stale_ack_tx.send(()).is_err(),
            "late ack should be dropped after drain receiver exits"
        );
        let err = fixture
            .bundle
            .router
            .record_terminal("bs", &message_id, TerminalOutcome::Consumed)
            .await
            .unwrap_err();
        match err {
            crate::peer_mailbox::router::RouterError::Ledger(
                crate::peer_mailbox::ledger::LedgerError::InvalidTransition { from, to },
            ) => {
                assert!(crate::peer_mailbox::ledger::is_terminal(
                    LedgerState::Ignored
                ));
                assert_eq!(from, LedgerState::Ignored);
                assert_eq!(to, LedgerState::Consumed);
            }
            other => panic!("expected InvalidTransition with terminal Ignored from, got {other:?}"),
        }

        let entry = fixture.bundle.ledger.get(&message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Ignored);

        let events = drain_events(&mut fixture.events);
        assert_eq!(ignored_timeout_events(&events, message_id, &target), 1);
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                SpurEventBody::WorkerPeerMessageConsumed {
                    message_id: event_message_id,
                    ..
                } if *event_message_id == message_id
            )
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn drain_forces_only_delegations_target_messages_not_unrelated_messages() {
        let mut fixture = fixture(&["tgt-A", "tgt-B"]);
        let target_a = DelegationId("tgt-A".into());
        let target_b = DelegationId("tgt-B".into());
        let message_a: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000705\"").unwrap();
        let message_b: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000706\"").unwrap();
        accept_and_walk(&fixture, message_a, &target_a, LedgerState::Delivered).await;
        accept_and_walk(&fixture, message_b, &target_b, LedgerState::Delivered).await;

        let (ack_tx, ack_rx) = unbounded_channel();
        let _hold_open_until_timeout = ack_tx.clone();
        drop(ack_tx);

        let quiet_window = Duration::from_millis(100);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target_a.clone(),
            quiet_window,
            Duration::from_secs(60),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;
        tokio::time::advance(quiet_window).await;
        handle.await.unwrap();

        let entry_a = fixture.bundle.ledger.get(&message_a).await.unwrap();
        let entry_b = fixture.bundle.ledger.get(&message_b).await.unwrap();
        assert_eq!(entry_a.state, LedgerState::Ignored);
        assert_eq!(entry_b.state, LedgerState::Delivered);

        let events = drain_events(&mut fixture.events);
        assert_eq!(ignored_timeout_events(&events, message_a, &target_a), 1);
        assert_eq!(ignored_timeout_events(&events, message_b, &target_b), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn drain_hits_cap_under_ack_flood() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000707\"").unwrap();
        accept_and_walk(&fixture, message_id, &target, LedgerState::Delivered).await;

        let (ack_tx, ack_rx) = unbounded_channel();
        let quiet_window = Duration::from_secs(1);
        let max_total = Duration::from_secs(5);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            max_total,
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;

        for _ in 0..5 {
            ack_tx.send(()).unwrap();
            tokio::time::advance(Duration::from_millis(900)).await;
            tokio::task::yield_now().await;
            assert!(!handle.is_finished(), "drain finished before cap");
        }

        tokio::time::advance(Duration::from_millis(499)).await;
        tokio::task::yield_now().await;
        assert!(
            !handle.is_finished(),
            "drain finished before absolute cap elapsed"
        );
        tokio::time::advance(Duration::from_millis(1)).await;

        let elapsed = handle.await.unwrap();
        assert!(
            elapsed >= max_total && elapsed <= max_total + Duration::from_millis(50),
            "elapsed: {elapsed:?}"
        );

        let entry = fixture.bundle.ledger.get(&message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Ignored);
        let events = drain_events(&mut fixture.events);
        assert_eq!(
            ignored_events_with_reason(&events, message_id, &target, "drain_capped"),
            1
        );

        let cap_events: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if let SpurEventBody::WorkerPeerMessageDrainCappedOut {
                    brain_session_id,
                    target_delegation_id,
                    acks_received,
                    remaining_messages,
                    cap_ms,
                    actual_elapsed_ms,
                } = event
                {
                    Some((
                        brain_session_id,
                        target_delegation_id,
                        *acks_received,
                        *remaining_messages,
                        *cap_ms,
                        *actual_elapsed_ms,
                    ))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(cap_events.len(), 1);
        let (brain_session_id, event_target, acks_received, remaining, cap_ms, elapsed_ms) =
            cap_events[0];
        assert_eq!(brain_session_id, "bs");
        assert_eq!(event_target, &target);
        assert_eq!(acks_received, 5);
        assert_eq!(remaining, 1);
        assert_eq!(cap_ms, 5_000);
        assert!(
            (5_000..=5_050).contains(&elapsed_ms),
            "actual_elapsed_ms: {elapsed_ms}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_quiet_exit_under_normal_flow() {
        let mut fixture = fixture(&["tgt"]);
        let target = DelegationId("tgt".into());
        let message_id: PeerMessageId =
            serde_json::from_str("\"00000000-0000-0000-0000-000000000708\"").unwrap();
        accept_and_walk(&fixture, message_id, &target, LedgerState::Delivered).await;

        let (ack_tx, ack_rx) = unbounded_channel();
        let quiet_window = Duration::from_secs(1);
        let handle = spawn_drain(
            fixture.bundle.clone(),
            target.clone(),
            quiet_window,
            Duration::from_secs(10),
            "bs",
            fixture.funnel.clone(),
            ack_rx,
        )
        .await;

        ack_tx.send(()).unwrap();
        tokio::time::advance(Duration::from_millis(100)).await;
        ack_tx.send(()).unwrap();
        tokio::time::advance(Duration::from_millis(100)).await;
        ack_tx.send(()).unwrap();

        tokio::time::advance(Duration::from_millis(1_100)).await;
        handle.await.unwrap();

        let entry = fixture.bundle.ledger.get(&message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Ignored);
        let events = drain_events(&mut fixture.events);
        assert_eq!(ignored_timeout_events(&events, message_id, &target), 1);
        assert!(!events.iter().any(|event| {
            matches!(event, SpurEventBody::WorkerPeerMessageDrainCappedOut { .. })
        }));
    }
}

#[cfg(test)]
mod cancel_stream_variant_tests {
    use super::InteractiveInput;
    use spur_acp::SessionId;

    #[test]
    fn cancel_stream_variant_constructs() {
        let _ = InteractiveInput::CancelStream {
            session: SessionId("s".to_string()),
        };
    }
}

#[cfg(test)]
mod base_spec_dispatch_tests {
    use super::delegation::base_spec::{
        emit_dispatch_overlay_applied, extract_overlays, resolve_base_branch,
        snapshot_required_for_dispatch,
    };
    use spur_mcp::tools::{BaseSpec, BaseTarget, OverlayCommit};

    #[test]
    fn snapshot_needed_for_none_and_repo_main() {
        assert!(snapshot_required_for_dispatch(None));
        assert!(snapshot_required_for_dispatch(Some(&BaseSpec::RepoMain)));
        assert!(snapshot_required_for_dispatch(Some(
            &BaseSpec::WithOverlay {
                base: BaseTarget::RepoMain,
                overlays: vec![],
            }
        )));
    }

    #[test]
    fn snapshot_not_needed_for_branch_or_commit() {
        assert!(!snapshot_required_for_dispatch(Some(&BaseSpec::Branch {
            name: "x".into()
        })));
        assert!(!snapshot_required_for_dispatch(Some(&BaseSpec::Commit {
            oid: "abc".into()
        })));
        assert!(!snapshot_required_for_dispatch(Some(
            &BaseSpec::WithOverlay {
                base: BaseTarget::Branch { name: "x".into() },
                overlays: vec![],
            }
        )));
        assert!(!snapshot_required_for_dispatch(Some(
            &BaseSpec::WithOverlay {
                base: BaseTarget::Commit { oid: "abc".into() },
                overlays: vec![],
            }
        )));
    }

    #[test]
    fn resolve_base_branch_unwraps_with_overlay() {
        let spec = BaseSpec::WithOverlay {
            base: BaseTarget::Branch {
                name: "spur/plan-base-xyz".into(),
            },
            overlays: vec![],
        };

        assert_eq!(resolve_base_branch(&spec, "fallback"), "spur/plan-base-xyz");
    }

    #[test]
    fn resolve_base_branch_falls_back_for_repo_main() {
        let spec = BaseSpec::RepoMain;

        assert_eq!(
            resolve_base_branch(&spec, "spur/brain-snapshot-X"),
            "spur/brain-snapshot-X"
        );
    }

    #[test]
    fn extract_overlays_returns_empty_for_non_overlay() {
        assert!(extract_overlays(&BaseSpec::RepoMain).is_empty());
        assert!(extract_overlays(&BaseSpec::Branch { name: "x".into() }).is_empty());
        assert!(extract_overlays(&BaseSpec::Commit { oid: "abc".into() }).is_empty());
    }

    #[test]
    fn extract_overlays_returns_all_for_with_overlay() {
        let spec = BaseSpec::WithOverlay {
            base: BaseTarget::RepoMain,
            overlays: vec![
                OverlayCommit {
                    source_task_id: "T1".into(),
                    base_oid: "a".into(),
                    tip_oid: "b".into(),
                },
                OverlayCommit {
                    source_task_id: "T2".into(),
                    base_oid: "b".into(),
                    tip_oid: "c".into(),
                },
            ],
        };

        let overlays = extract_overlays(&spec);

        assert_eq!(overlays.len(), 2);
        assert_eq!(overlays[0].0, "T1");
        assert_eq!(overlays[1].0, "T2");
    }

    #[tokio::test]
    async fn dispatch_overlay_applied_event_includes_base_and_overlay_ids() {
        let spec = BaseSpec::WithOverlay {
            base: BaseTarget::Branch {
                name: "spur/plan-base".into(),
            },
            overlays: vec![
                OverlayCommit {
                    source_task_id: "T1".into(),
                    base_oid: "a".into(),
                    tip_oid: "b".into(),
                },
                OverlayCommit {
                    source_task_id: "T2".into(),
                    base_oid: "b".into(),
                    tip_oid: "c".into(),
                },
            ],
        };
        let overlays = extract_overlays(&spec);
        let (funnel, mut events) = crate::event_funnel::test_channel();

        emit_dispatch_overlay_applied(&funnel, "req-1", Some(&spec), "overlay-head", &overlays);

        match tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event channel closed")
        {
            spur_acp::SpurEventBody::DispatchOverlayApplied {
                request_id,
                base_spec,
                dispatched_base_oid,
                overlay_task_ids,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(base_spec["kind"], "with_overlay");
                assert_eq!(base_spec["overlays"][0]["source_task_id"], "T1");
                assert_eq!(dispatched_base_oid, "overlay-head");
                assert_eq!(overlay_task_ids, vec!["T1", "T2"]);
            }
            other => panic!("expected DispatchOverlayApplied, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod prompt_v1_tests {
    // --- Bundled skill content tests: verify SKILL.md bodies contain required keywords ---

    #[test]
    fn brain_delegation_skill_contains_dispatch_procedure() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .expect("bundled brain-delegation skill must exist");
        assert!(body.contains("When to delegate vs. do it yourself"));
        assert!(body.contains("Do it yourself when:"));
        assert!(body.contains("Delegate when:"));
        assert!(body.contains("specialist"));
        assert!(body.contains("avoid_for is a SOFT signal"));
    }

    #[test]
    fn brain_delegation_skill_contains_plan_requirement() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .unwrap();
        assert!(body.contains("delegation_plan"));
        assert!(body.contains("candidates"));
        assert!(body.contains("decomposition"));
        assert!(body.contains("minimum shape"));
        assert!(body.contains(">=2 subtasks OR >3 files"));
    }

    #[test]
    fn brain_delegation_skill_contains_task_structure() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .unwrap();
        assert!(body.contains("CONTEXT:"));
        assert!(body.contains("GOAL:"));
        assert!(body.contains("CONSTRAINTS:"));
        assert!(body.contains("EXPECTED OUTPUT"));
    }

    #[test]
    fn brain_delegation_skill_contains_canonical_example() {
        let body =
            crate::skills::load_skill("brain-delegation", std::path::Path::new("/nonexistent"))
                .unwrap();
        assert!(body.contains("Canonical example"));
        assert!(body.contains("delegate_to_worker"));
        assert!(body.contains("delegation_plan"));
    }

    #[test]
    fn per_agent_skill_exists_for_known_brains() {
        let fake = std::path::Path::new("/nonexistent");
        for agent in ["claude-code-acp", "kiro", "codex", "gemini"] {
            let name = format!("brain-delegation-{}", agent);
            assert!(
                crate::skills::load_skill(&name, fake).is_some(),
                "missing bundled skill for {agent}"
            );
        }
    }

    #[test]
    fn unknown_agent_skill_returns_none() {
        let fake = std::path::Path::new("/nonexistent");
        assert!(crate::skills::load_skill("brain-delegation-unknown-agent-xyz", fake).is_none());
    }

    // --- Workers-block rendering: build minimal fixtures from AgentConfig ---

    use spur_acp::config::{AgentConfig, Tier};

    fn cfg_with_good_for(name: &str, good_for: Vec<String>) -> AgentConfig {
        let mut cfg = AgentConfig::with_defaults(name);
        cfg.delegation.good_for = good_for;
        cfg.delegation.description = Some(format!("{} test descriptor", name));
        cfg.delegation.tier = Some(Tier::Generalist);
        cfg
    }

    /// Render the workers block over an explicit agent slice, bypassing
    /// orchestrator self. Mirrors the logic of `render_workers_block`.
    fn render_workers_block_over(agents: &[AgentConfig]) -> String {
        let mut out = String::from("## Available worker agents\n\n");
        let mut any = false;
        for agent in agents {
            if agent.delegation.good_for.is_empty() {
                continue;
            }
            any = true;
            let tier = agent
                .delegation
                .tier
                .map(|t| match t {
                    Tier::Specialist => "specialist",
                    Tier::Generalist => "generalist",
                })
                .unwrap_or("generalist");
            let desc = agent
                .delegation
                .description
                .as_deref()
                .unwrap_or("(no description)");
            out.push_str(&format!(
                "### {}  ({}, cost: medium)\n{}\n\n",
                agent.name, tier, desc,
            ));
        }
        if !any {
            out.push_str("(no worker-capable agents with descriptors configured)\n\n");
        }
        out
    }

    #[test]
    fn workers_block_lists_agents_with_non_empty_good_for() {
        let agents = vec![
            cfg_with_good_for("claude-x", vec!["refactors".into()]),
            cfg_with_good_for("kiro-x", vec!["specs".into()]),
        ];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("claude-x"));
        assert!(block.contains("kiro-x"));
    }

    #[test]
    fn workers_block_excludes_empty_good_for_agents() {
        let agents = vec![
            cfg_with_good_for("has-good-for", vec!["real".into()]),
            cfg_with_good_for("bare", vec![]), // will be excluded
        ];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("has-good-for"));
        assert!(!block.contains("bare"));
    }

    #[test]
    fn workers_block_says_none_when_all_excluded() {
        let agents = vec![cfg_with_good_for("bare", vec![])];
        let block = render_workers_block_over(&agents);
        assert!(block.contains("(no worker-capable agents with descriptors configured)"));
    }

    #[test]
    fn workers_block_is_deterministic_for_same_input() {
        let agents = vec![cfg_with_good_for("a", vec!["x".into()])];
        let a = render_workers_block_over(&agents);
        let b = render_workers_block_over(&agents);
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod interactive_input_tests {
    use super::InteractiveInput;
    use chrono::Utc;
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
    use spur_acp::types::SessionId;
    use std::time::Instant;

    #[test]
    fn system_continuation_variant_constructs() {
        let c = BrainContinuation {
            delegation_id: "abc".into(),
            attempt: 1,
            brain_session: SessionId("brain-session-1".into()),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None,
                diff_summary: None,
                worker_branch: None,
                artifact_ref: None,
                estimated_cost_micros: None,
                artifact_id: None,
                fetch_hint: None,
                base_hint: None,
            },
            created_at_wall: Utc::now(),
            created_at_mono: Instant::now(),
        };
        let input = InteractiveInput::SystemContinuation {
            session: SessionId::new(),
            continuation: c,
        };
        match input {
            InteractiveInput::SystemContinuation { .. } => (),
            _ => panic!("expected SystemContinuation variant"),
        }
    }

    #[test]
    fn warm_connect_variant_constructs() {
        let input = InteractiveInput::WarmConnect;
        match input {
            InteractiveInput::WarmConnect => (),
            _ => panic!("expected WarmConnect variant"),
        }
    }
}

#[cfg(test)]
mod phase5_orchestrator_finalization_tests {
    use super::{commit_rendered_batch, session::retire_brain_session, TurnGuard};
    use crate::continuation_bridge::{new_overflow_buf, ContinuationEventSink, RenderOutcome};
    use crate::event_funnel::spawn_funnel;
    use crate::scheduler::{BrainScheduler, ScheduledAction};
    use async_trait::async_trait;
    use chrono::Utc;
    use dashmap::DashMap;
    use futures::FutureExt;
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::{
        BrainContinuation, ContinuationPayload, ContinuationSource, DeferReason, DelegationKey,
        DropReason,
    };
    use spur_acp::types::SessionId;
    use spur_license::policy::PolicyResolver;
    use spur_license::FeatureGate;
    use spur_mcp::handlers::PlanResolver;
    use spur_mcp::plan::PlanState;
    use spur_mcp::worker_server::{WorkerMcpDeps, WorkerMcpServer};
    use spur_pm::test_workspace::TestBeadsWorkspace;
    use std::future::Future;
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;
    use tokio::net::TcpStream;
    use tokio::sync::{broadcast, Notify};

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<SpurEventBody>>,
    }

    impl RecordingSink {
        fn snapshot(&self) -> Vec<SpurEventBody> {
            self.events.lock().unwrap().clone()
        }
    }

    impl ContinuationEventSink for RecordingSink {
        fn emit(&self, body: SpurEventBody) {
            self.events.lock().unwrap().push(body);
        }
    }

    fn mk_scheduler(active_session: Option<SessionId>) -> (BrainScheduler, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let scheduler = BrainScheduler::new(
            active_session.map(spur_acp::types::BrainSessionId::from),
            sink.clone(),
        );
        (scheduler, sink)
    }

    fn mk_cont(id: &str, attempt: u32, brain_session: &SessionId) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            attempt,
            brain_session: brain_session.clone(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: Some(format!("summary-{id}")),
                diff_summary: None,
                worker_branch: None,
                artifact_ref: None,
                estimated_cost_micros: None,
                artifact_id: None,
                fetch_hint: None,
                base_hint: None,
            },
            created_at_wall: Utc::now(),
            created_at_mono: Instant::now(),
        }
    }

    fn continuation_batch(scheduler: &mut BrainScheduler) -> crate::scheduler::DrainedBatch {
        match scheduler.next(Instant::now()) {
            ScheduledAction::ContinuationPrompt(batch) => batch,
            other => panic!("expected ContinuationPrompt, got {other:?}"),
        }
    }

    fn test_funnel() -> (
        crate::event_funnel::FunnelHandle,
        broadcast::Receiver<spur_acp::domain::events::SpurEvent>,
    ) {
        let (tx, rx) = broadcast::channel(32);
        let seq = Arc::new(AtomicU64::new(0));
        (spawn_funnel(tx, seq), rx)
    }

    struct NullWorkerMcpEventSink;

    impl spur_mcp::events::McpEventSink for NullWorkerMcpEventSink {
        fn emit(&self, _event: SpurEventBody) {}
    }

    struct NullWorkerPlanResolver;

    #[async_trait]
    impl PlanResolver for NullWorkerPlanResolver {
        async fn load_or_project_plan(
            &self,
            plan_id: &str,
        ) -> Result<Arc<tokio::sync::Mutex<PlanState>>, String> {
            Err(format!("test resolver: unknown plan_id '{plan_id}'"))
        }
    }

    async fn test_worker_pm_service(repo: &Path) -> Arc<spur_pm::PmService> {
        let workspace = TestBeadsWorkspace::init();
        let beads_dir = repo.join(".beads");
        std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
        workspace.copy_db_to(&beads_dir);
        Arc::new(
            spur_pm::PmService::try_new(None, true, false, repo, None)
                .await
                .expect("PmService::try_new failed")
                .expect("expected beads pm"),
        )
    }

    fn test_worker_deps(pm: Arc<spur_pm::PmService>) -> WorkerMcpDeps {
        WorkerMcpDeps {
            pm_service: pm,
            feature_gate: Arc::new(FeatureGate::new(PolicyResolver::embedded())),
            funnel: Arc::new(NullWorkerMcpEventSink),
            plan_resolver: Arc::new(NullWorkerPlanResolver),
            reconciler_outcomes: Arc::new(tokio::sync::Mutex::new(
                spur_mcp::plan::outcomes::OutcomeStore::default(),
            )),
            outcome_store: Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            repo_root: None,
        }
    }

    enum ShutdownMode {
        Ready,
        Wait(Arc<Notify>),
    }

    struct MockRetiringServer {
        shutdown_mode: ShutdownMode,
        mark_calls: AtomicUsize,
        cancel_calls: AtomicUsize,
        force_calls: AtomicUsize,
        shutdown_calls: AtomicUsize,
    }

    impl MockRetiringServer {
        fn ready() -> Self {
            Self {
                shutdown_mode: ShutdownMode::Ready,
                mark_calls: AtomicUsize::new(0),
                cancel_calls: AtomicUsize::new(0),
                force_calls: AtomicUsize::new(0),
                shutdown_calls: AtomicUsize::new(0),
            }
        }

        fn blocked(notify: Arc<Notify>) -> Self {
            Self {
                shutdown_mode: ShutdownMode::Wait(notify),
                mark_calls: AtomicUsize::new(0),
                cancel_calls: AtomicUsize::new(0),
                force_calls: AtomicUsize::new(0),
                shutdown_calls: AtomicUsize::new(0),
            }
        }
    }

    impl super::session::RetirableMcpServer for MockRetiringServer {
        fn mark_retiring(&self) {
            self.mark_calls.fetch_add(1, Ordering::SeqCst);
        }

        fn cancel_in_flight_workers(&self) {
            self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        }

        fn force_abort(&self) {
            self.force_calls.fetch_add(1, Ordering::SeqCst);
        }

        fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            self.shutdown_calls.fetch_add(1, Ordering::SeqCst);
            match &self.shutdown_mode {
                ShutdownMode::Ready => Box::pin(async {}),
                ShutdownMode::Wait(notify) => {
                    let notify = Arc::clone(notify);
                    Box::pin(async move {
                        notify.notified().await;
                    })
                }
            }
        }
    }

    #[tokio::test]
    async fn test_retire_brain_session_clean_shutdown() {
        let old_session = SessionId("brain-old".into());
        let new_session = SessionId("brain-new".into());
        let (funnel, _rx) = test_funnel();
        let (mut scheduler, _sink) = mk_scheduler(Some(old_session.clone()));
        let overflow = new_overflow_buf();
        let server = Arc::new(MockRetiringServer::ready());
        let mut mcp_server = Some(server.clone());
        let worker_mcp_servers = DashMap::new();

        retire_brain_session(
            &funnel,
            &old_session,
            &mut mcp_server,
            None,
            &worker_mcp_servers,
            &mut scheduler,
            &overflow,
            Some(new_session.clone().into()),
        )
        .await;

        assert!(mcp_server.is_none());
        assert_eq!(server.mark_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.cancel_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.shutdown_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.force_calls.load(Ordering::SeqCst), 0);

        scheduler.push_continuation(mk_cont("post-retire", 1, &new_session));
        assert_eq!(scheduler.pending_continuation_len(), 1);
    }

    #[tokio::test]
    async fn test_retire_brain_session_shuts_down_worker_mcp_server() {
        let session = SessionId("brain-worker-mcp".into());
        let brain_session = spur_acp::types::BrainSessionId::from(session.clone());
        let (funnel, _rx) = test_funnel();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        let overflow = new_overflow_buf();
        let dir = TempDir::new().expect("tempdir");
        let pm = test_worker_pm_service(dir.path()).await;
        let worker_server = WorkerMcpServer::start(session.to_string(), test_worker_deps(pm))
            .await
            .expect("worker MCP server starts");
        let worker_addr = worker_server
            .url()
            .strip_prefix("http://")
            .and_then(|url| url.strip_suffix("/mcp"))
            .expect("worker MCP URL shape")
            .to_string();
        let worker_mcp_servers = DashMap::new();
        worker_mcp_servers.insert(brain_session.clone(), worker_server);
        let mut mcp_server: Option<Arc<MockRetiringServer>> = None;

        retire_brain_session(
            &funnel,
            &session,
            &mut mcp_server,
            None,
            &worker_mcp_servers,
            &mut scheduler,
            &overflow,
            None,
        )
        .await;

        assert!(
            !worker_mcp_servers.contains_key(&brain_session),
            "retire must remove the worker MCP server entry"
        );
        let probe = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&worker_addr))
            .await
            .expect("connect must complete within 2s after retire");
        let connect_err = probe.expect_err("listener must be closed after retire");
        assert!(
            matches!(
                connect_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
            ),
            "expected ConnectionRefused/Reset, got {connect_err:?}"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_retire_brain_session_timeout_force_aborts() {
        let session = SessionId("brain-timeout".into());
        let (funnel, _rx) = test_funnel();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        let overflow = new_overflow_buf();
        let server = Arc::new(MockRetiringServer::blocked(Arc::new(Notify::new())));
        let mut mcp_server = Some(server.clone());
        let worker_mcp_servers = DashMap::new();

        retire_brain_session(
            &funnel,
            &session,
            &mut mcp_server,
            None,
            &worker_mcp_servers,
            &mut scheduler,
            &overflow,
            None,
        )
        .await;

        assert_eq!(server.mark_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.cancel_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.shutdown_calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.force_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_retire_brain_session_emits_mcp_shutdown_timeout_event() {
        let session = SessionId("brain-timeout-event".into());
        let (funnel, mut rx) = test_funnel();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        let overflow = new_overflow_buf();
        let server = Arc::new(MockRetiringServer::blocked(Arc::new(Notify::new())));
        let mut mcp_server = Some(server);
        let worker_mcp_servers = DashMap::new();

        retire_brain_session(
            &funnel,
            &session,
            &mut mcp_server,
            None,
            &worker_mcp_servers,
            &mut scheduler,
            &overflow,
            None,
        )
        .await;

        let event = rx.recv().await.expect("timeout event");
        assert!(matches!(
            event.body,
            SpurEventBody::McpShutdownTimeout {
                session: ref event_session,
                timeout_ms: 5_000,
            } if event_session == &session
        ));
    }

    #[tokio::test]
    async fn test_retire_brain_session_note_session_swap_called_with_overflow() {
        let old_session = SessionId("brain-old".into());
        let new_session = SessionId("brain-new".into());
        let (funnel, _rx) = test_funnel();
        let (mut scheduler, sink) = mk_scheduler(Some(old_session.clone()));
        let overflow = new_overflow_buf();
        let server = Arc::new(MockRetiringServer::ready());
        let mut mcp_server = Some(server);
        let worker_mcp_servers = DashMap::new();

        scheduler.push_continuation(mk_cont("pending-1", 1, &old_session));
        {
            let mut guard = overflow.lock().await;
            guard.push_back((old_session.clone(), mk_cont("overflow-1", 1, &old_session)));
        }

        retire_brain_session(
            &funnel,
            &old_session,
            &mut mcp_server,
            None,
            &worker_mcp_servers,
            &mut scheduler,
            &overflow,
            Some(new_session.clone().into()),
        )
        .await;

        assert_eq!(scheduler.pending_continuation_len(), 0);
        assert!(overflow.lock().await.is_empty());

        let events = sink.snapshot();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| matches!(
            event,
            SpurEventBody::ContinuationDropped {
                reason: DropReason::SessionSwap,
                ..
            }
        )));

        scheduler.push_continuation(mk_cont("new-session-ok", 1, &new_session));
        assert_eq!(scheduler.pending_continuation_len(), 1);
    }

    #[test]
    fn test_dispatch_merged_commits_on_ok() {
        let session = SessionId("brain-merged".into());
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let delivered = mk_cont("deliver-me", 1, &session);
        let spilled = mk_cont("spill-me", 1, &session);
        let delivered_key = DelegationKey::from(&delivered);
        let spilled_key = DelegationKey::from(&spilled);
        scheduler.push_continuation(delivered.clone());
        scheduler.push_continuation(spilled.clone());
        let batch = continuation_batch(&mut scheduler);

        commit_rendered_batch(
            &mut scheduler,
            batch,
            RenderOutcome {
                blocks: vec![],
                delivered_keys: vec![delivered_key.clone()],
                deferred_spill: vec![(
                    spilled.clone(),
                    DeferReason::BudgetSpill {
                        budget_bytes: 512,
                        continuation_bytes: 900,
                    },
                )],
                dropped_oversized: vec![],
            },
        );

        scheduler.push_continuation(delivered);
        assert_eq!(scheduler.pending_continuation_len(), 1);
        let events = sink.snapshot();
        assert!(events.iter().any(|event| matches!(
            event,
            SpurEventBody::ContinuationDeferred {
                delegation_id,
                reason: DeferReason::BudgetSpill { .. },
                ..
            } if delegation_id == spilled_key.delegation_id.as_str()
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            SpurEventBody::ContinuationDropped {
                delegation_id,
                reason: DropReason::AlreadyDelivered,
                ..
            } if delegation_id == delivered_key.delegation_id.as_str()
        )));
    }

    #[test]
    fn test_dispatch_merged_rollbacks_on_err() {
        let session = SessionId("brain-rollback".into());
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        scheduler.push_continuation(mk_cont("rollback-me", 1, &session));
        let batch = continuation_batch(&mut scheduler);

        scheduler.rollback(batch, vec![]);

        assert_eq!(scheduler.pending_continuation_len(), 1);
        let events = sink.snapshot();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            SpurEventBody::ContinuationDeferred {
                reason: DeferReason::PromptDispatchFailure,
                requeue_count: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_dispatch_merged_turn_guard_clears_on_panic() {
        let (scheduler, _sink) = mk_scheduler(Some(SessionId("brain-guard".into())));
        let flag = scheduler.turn_flag();

        let result = std::panic::AssertUnwindSafe(async {
            let _guard = TurnGuard::arm(flag.clone());
            panic!("boom");
        })
        .catch_unwind()
        .await;

        assert!(result.is_err());
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_dispatch_merged_oversized_dropped_via_commit_partial() {
        let session = SessionId("brain-oversized".into());
        let (mut scheduler, sink) = mk_scheduler(Some(session.clone()));
        let oversized = mk_cont("too-large", 1, &session);
        let oversized_key = DelegationKey::from(&oversized);
        scheduler.push_continuation(oversized);
        let batch = continuation_batch(&mut scheduler);

        commit_rendered_batch(
            &mut scheduler,
            batch,
            RenderOutcome {
                blocks: vec![],
                delivered_keys: vec![],
                deferred_spill: vec![],
                dropped_oversized: vec![(oversized_key.clone(), 9_999)],
            },
        );

        assert_eq!(scheduler.pending_continuation_len(), 0);
        let events = sink.snapshot();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            SpurEventBody::ContinuationDropped {
                delegation_id,
                reason: DropReason::OversizedSingleItem {
                    continuation_bytes: 9_999,
                    ..
                },
                ..
            } if delegation_id == oversized_key.delegation_id.as_str()
        ));
    }
}
