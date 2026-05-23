use super::*;

impl Orchestrator {
    /// Run an ad-hoc task through the brain agent.
    pub async fn run_adhoc(&mut self, task: &str, opts: RunOpts) -> Result<RunResult> {
        let start = Instant::now();

        // 1. Resolve brain agent.
        let brain_name = opts
            .brain
            .as_deref()
            .unwrap_or(&self.config.brain.default)
            .to_string();

        let brain_config = self
            .registry
            .get(&brain_name)
            .ok_or_else(|| anyhow!("Brain agent '{}' not found in registry", brain_name))?
            .clone();

        // 1b. Parallel-fetch issues + graph intelligence for TUI + brain prompt.
        let graph_summary = if let Some(pm) = &self.pm_service {
            refresh_pm_state(pm, &self.funnel, None, true).await
        } else {
            None
        };

        // 2. Optionally fetch issue context.
        let issue_context = if let Some(ref issue_ref) = opts.issue {
            match self.fetch_issue_context(issue_ref).await {
                Ok(issue) => {
                    self.emit(SpurEvent::now(SpurEventBody::IssueReceived {
                        source: format!("{:?}", issue.source),
                        id: issue.id.clone(),
                    }));
                    Some(issue)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to fetch issue context, proceeding without it");
                    None
                }
            }
        } else {
            None
        };

        // 3. Build brain prompt (enriched with graph intelligence).
        let enriched_task = match &graph_summary {
            Some(summary) => format!("{summary}\n\n{task}"),
            None => task.to_string(),
        };

        // 4. Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id_cell = Arc::new(std::sync::OnceLock::new());
        let adhoc_ctx = self.build_continuation_ctx(Arc::clone(&brain_session_id_cell));
        let (mcp_server, delegation_channel) = McpCallbackServer::new(
            None,
            self.pm_service.clone(),
            sink,
            adhoc_ctx,
            self.outcome_store.clone(),
            self.mcp_feature_gate(),
        );
        let mut mcp_server = mcp_server;

        // Populate available workers.
        let workers: Vec<WorkerInfo> = self
            .registry
            .worker_capable()
            .into_iter()
            .map(build_worker_info)
            .collect();
        mcp_server.set_workers(workers);
        // INV-6: wire the cancellation side-channel.
        mcp_server.set_cancellation_control(self.cancellation_control.clone());
        // Phase 1c: async-first dispatch window.
        mcp_server.set_inline_wait(std::time::Duration::from_millis(
            self.config.delegation.inline_wait_ms,
        ));
        self.apply_mcp_server_settings(&mut mcp_server);

        let mcp_server = Arc::new(mcp_server);
        let (mcp_url, mcp_handle) = mcp_server
            .clone()
            .start()
            .await
            .context("Failed to start MCP callback server")?;

        let ((mut connection, delegation_handle, success, pr_url, session_id), mcp_handle): McpGuarded<
            BrainRunBootstrap,
        > = cleanup_mcp_on_err(mcp_handle, async {
            // 6. Spawn brain agent via AgentConnection.
            let mut connection = self.create_connection(&brain_config, None);

            let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
            let _capabilities = connection
                .initialize(init_request)
                .await
                .context("Failed to initialize brain agent")?;

            debug!(
                brain = %brain_name,
                "Brain agent initialized"
            );

            // MCP callback server is now HTTP — pass URL directly.
            let socket_nonce = uuid::Uuid::new_v4().simple().to_string();
            let mcp_servers = crate::notebook::brain_mcp_servers(&mcp_url, &socket_nonce);

            let session_response = crate::skip_perm::new_session_with_bypass(
                &mut *connection,
                &brain_config,
                self.repo_root.clone(),
                mcp_servers,
            )
            .await
            .context("Failed to create brain session")?;

            let acp_session_id = spur_acp::SessionId(session_response.session_id.to_string());
            let brain_session_id =
                spur_mcp::plan::labels::derive_brain_session_id(&acp_session_id);
            mcp_server
                .set_brain_session_id(brain_session_id.clone())
                .expect("set once");
            self.register_notebook_socket(brain_session_id.clone(), socket_nonce);
            brain_session_id_cell
                .set(brain_session_id.as_session_id().clone())
                .expect("set once");
            let session_id = brain_session_id.as_session_id().clone();
            Arc::clone(&mcp_server)
                .enable_reconciler()
                .await
                .context("Failed to enable MCP reconciler")?;

            info!(brain = %brain_name, session = %session_id, "Starting ad-hoc run");
            self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
                agent: brain_name.clone(),
                session: session_id.clone(),
            }));

            // 5. Log session start.
            if let Some(ref ct) = self.cost_tracker {
                let _ = ct.start_session(
                    &session_id,
                    &brain_name,
                    "brain",
                    None,
                    task,
                    self.config.project.as_ref().map(|p| p.name.as_str()),
                    opts.issue.as_deref(),
                );
            }

            let prompt_text = self.build_brain_prompt(
                &enriched_task,
                issue_context.as_ref(),
                &session_id,
                &brain_name,
            );

            // 7. Send prompt and stream events.
            let prompt_request = PromptRequest::new(
                session_response.session_id.clone(),
                vec![ContentBlock::Text(TextContent::new(prompt_text.clone()))],
            );

            // 8. Process brain output + delegation callbacks concurrently.
            let pr_url: Option<String> = None;
            let success = true;

            // Spawn delegation handler BEFORE prompt so delegation requests
            // that arrive during the prompt turn are not queued indefinitely.
            let max_concurrent = self
                .feature_gate
                .as_ref()
                .and_then(|g| g.quota(spur_license::QuotaKey::MaxConcurrentWorkers))
                .and_then(|v| v.as_count())
                .map(|n| n as usize)
                .unwrap_or(self.config.worktree.max_concurrent);
            if let Some(bundle) = self.peer_mailbox.clone() {
                *bundle.brain_session_id_slot.write().await = Some(brain_session_id.to_string());
                let drain_quiet_window =
                    std::time::Duration::from_millis(bundle.router.limits().drain_quiet_window_ms);
                // Idempotent: safe to call across multiple session boundaries because
                // run_startup_reconcile only emits WorkerPeerMailboxReconciled on Changed
                // (bd-cpf.5b). Stage-2 may consolidate these into a single helper.
                let _ = crate::peer_mailbox::reconciler::run_startup_reconcile(
                    bundle.ledger.clone(),
                    self.funnel.clone(),
                    brain_session_id.to_string(),
                    drain_quiet_window,
                )
                .await;
            }
            let delegation_handle = tokio::spawn(delegation::handle_delegations(
                delegation_channel,
                self.repo_root.clone(),
                self.config.agents.entries.clone(),
                max_concurrent,
                self.config.worktree.clone(),
                self.event_tx.clone(),
                self.funnel.clone(),
                self.review_sink.clone(),
                self.pm_service.clone(),
                self.mcp_feature_gate(),
                self.cancellation_control.clone(),
                self.peer_mailbox.clone(),
                self.fault_injection_hooks.clone(),
                std::time::Duration::from_secs(self.config.spur.dispatch_lease_secs),
                std::time::Duration::from_secs(self.config.spur.dispatch_lease_heartbeat_secs),
                self.worker_mcp_fetcher_for(Arc::clone(&mcp_server)),
                self.config.delegation.normalize.bypass_hooks,
            ));

            // Stream brain output. For native (ACP-transport) agents prompt()
            // returns an empty stream; notifications arrive via the
            // connection-scoped broadcast instead. drive_prompt_notifications
            // handles both paths transparently.
            let funnel_for_notif = self.funnel.clone();
            let session_id_for_notif = session_id.clone();
            crate::notification_drain::drive_prompt_notifications(
                &mut *connection,
                prompt_request,
                |notification| {
                    match &notification.update {
                        SessionUpdate::AgentThoughtChunk(chunk)
                        | SessionUpdate::AgentMessageChunk(chunk) => {
                            if let ContentBlock::Text(tc) = &chunk.content {
                                print!("{}", tc.text);
                            }
                        }
                        SessionUpdate::ToolCall(tc) => {
                            debug!(tool = %tc.title, "Brain calling tool");
                        }
                        _ => {}
                    }
                    funnel_for_notif.emit(SpurEventBody::AgentNotification {
                        session: session_id_for_notif.clone(),
                        notification: Box::new(notification),
                    });
                },
            )
            .await
            .context("Failed to send prompt to brain")?;

            Ok((connection, delegation_handle, success, pr_url, session_id))
        })
        .await?;

        // 9. Clean up.
        let _ = connection.shutdown().await;
        delegation_handle.abort();
        abort_mcp_handle(mcp_handle).await;
        self.remove_notebook_socket(&spur_acp::BrainSessionId::from(session_id.clone()));

        let duration = start.elapsed();

        // 10. Log session end.
        if let Some(ref ct) = self.cost_tracker {
            let status = if success { "completed" } else { "failed" };
            let _ = ct.end_session(&session_id, status, duration, brain_config.cost_tier);
        }

        let total_cost = spur_cost::estimator::estimate_cost(brain_config.cost_tier, duration);

        self.emit(SpurEvent::now(SpurEventBody::SessionCompleted {
            session: session_id.clone(),
            success,
        }));

        println!();
        info!(
            session = %session_id,
            duration_secs = duration.as_secs(),
            cost_usd = format!("{:.2}", total_cost),
            "Run complete"
        );

        Ok(RunResult {
            session_id,
            success,
            pr_url,
            total_cost_usd: total_cost,
        })
    }
}
