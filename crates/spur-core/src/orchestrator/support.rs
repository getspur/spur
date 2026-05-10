use super::*;

impl Orchestrator {
    /// Phase 5 / Task 25/26 — return the existing per-`BrainSession`
    /// [`WorkerMcpServer`], booting one on first call. Concurrent callers
    /// for the same `brain` collapse to a single server: at most one boot
    /// wins the `DashMap` insert and any others drop the loser server.
    ///
    /// `mcp_server` is the per-`BrainSession` [`McpCallbackServer`] that
    /// supplies the `PlanResolver` + reconciler outcome buffer the worker
    /// MCP dispatcher needs. The orchestrator captures the same instance
    /// when building [`WorkerMcpFetcher`] for the dispatch path so a
    /// direct call here observes the same cache.
    pub async fn ensure_worker_mcp_server(
        &self,
        brain: &spur_acp::BrainSessionId,
        mcp_server: Arc<McpCallbackServer>,
    ) -> Result<Arc<WorkerMcpServer>, DelegationDispatchError> {
        self.worker_mcp_fetcher_for(mcp_server).ensure(brain).await
    }

    /// Construct a clonable [`WorkerMcpFetcher`] capturing all deps the
    /// dispatch path needs to lazily ensure (and mint a token against)
    /// the per-`BrainSession` `WorkerMcpServer` from a static context.
    pub(crate) fn worker_mcp_fetcher_for(
        &self,
        mcp_server: Arc<McpCallbackServer>,
    ) -> WorkerMcpFetcher {
        WorkerMcpFetcher {
            cache: Arc::clone(&self.worker_mcp_servers),
            pm_service: self.pm_service.clone(),
            feature_gate: self.feature_gate.clone(),
            funnel: self.funnel.clone(),
            mcp_server,
            outcome_store: self.outcome_store.clone(),
            repo_root: Some(self.repo_root.clone()),
        }
    }

    /// Wire in the sender half of the `run_interactive` ingress channel so
    /// the MCP server can route detached delegation completions back to the
    /// orchestrator. Call this before `run_interactive`.
    pub fn set_continuation_tx(
        &mut self,
        tx: mpsc::Sender<InteractiveInput>,
        overflow: crate::continuation_bridge::OverflowBuf,
    ) {
        self.continuation_tx = Some(tx);
        self.continuation_overflow = Some(overflow);
    }

    /// Stage-1 peer mailbox bundle attachment for tests and custom embedding.
    /// Production opt-in construction happens in `Orchestrator::new`.
    pub fn attach_peer_mailbox(&mut self, bundle: crate::peer_mailbox::PeerMailboxBundle) {
        self.peer_mailbox = Some(bundle);
    }

    /// Expose the production peer-mailbox bundle.
    ///
    /// For integration tests and diagnostic introspection (e.g. health checks,
    /// admin RPCs, metrics exporters). Returns `None` when
    /// `peer_mailbox_enabled = false`. Not currently used by `spur-tui` /
    /// `spur-cli`; kept `pub` so future production callers do not need to
    /// reach into private orchestrator state.
    pub fn peer_mailbox_bundle(&self) -> Option<&crate::peer_mailbox::PeerMailboxBundle> {
        self.peer_mailbox.as_ref()
    }

    /// Return the reconciler task abort handle when the production peer mailbox
    /// is attached.
    ///
    /// For integration tests and graceful-shutdown callers. The handle is a
    /// clone of the one stored in `background_tasks`; aborting via either path
    /// is equivalent. Returns `None` when `peer_mailbox_enabled = false`.
    pub fn peer_mailbox_reconciler_abort_handle(&self) -> Option<tokio::task::AbortHandle> {
        self.peer_mailbox_reconciler_abort.clone()
    }

    /// Build a `DetachedContinuationCtx` for `McpCallbackServer::new`.
    ///
    /// Wires the `on_complete` async callback to `report_detached_completion`.
    /// `DelegationCompleted` is emitted by `execute_delegation` before the
    /// oneshot fires, so INV-C3 is preserved without emitting here.
    ///
    /// If no `continuation_tx` has been wired (e.g. `run_adhoc`), the
    /// callback is a no-op — continuations are silently dropped, which is
    /// correct for the one-shot batch path.
    pub(in crate::orchestrator) fn build_continuation_ctx(
        &self,
        brain_session_id: Arc<std::sync::OnceLock<spur_acp::types::SessionId>>,
    ) -> spur_mcp::server::DetachedContinuationCtx {
        match (
            self.continuation_tx.clone(),
            self.continuation_overflow.clone(),
        ) {
            (Some(tx), Some(overflow)) => {
                let session_cell = Arc::clone(&brain_session_id);
                spur_mcp::server::DetachedContinuationCtx {
                    on_complete: std::sync::Arc::new(move |cont, worker_session_str| {
                        let tx = tx.clone();
                        let overflow = overflow.clone();
                        let session = session_cell
                            .get()
                            .expect("brain_session_id must be set before detached completion")
                            .clone();
                        let worker_session = spur_acp::types::SessionId(worker_session_str);
                        Box::pin(async move {
                            crate::continuation_bridge::report_detached_completion(
                                &tx,
                                &overflow,
                                session,
                                worker_session,
                                cont,
                            )
                            .await;
                        })
                    }),
                }
            }
            _ => {
                // No ingress channel wired — produce a no-op ctx so the
                // constructor signature is satisfied (run_adhoc path).
                spur_mcp::server::DetachedContinuationCtx {
                    on_complete: std::sync::Arc::new(|_cont, _worker| Box::pin(async {})),
                }
            }
        }
    }

    /// Apply orchestrator-derived MCP callback-server settings.
    ///
    /// Shared by all three brain-session init paths (`run_adhoc`,
    /// `create_brain_session`, `load_brain_session`). Omitting any setter —
    /// notably `set_reconciler_enabled` — leaves the reconciler in
    /// observe-only mode so persisted plans silently never dispatch (bd-3rvt).
    pub(in crate::orchestrator) fn apply_mcp_server_settings(
        &self,
        mcp_server: &mut McpCallbackServer,
    ) {
        // v0a.3: enable reconciler for beads backends only (not github).
        // Reconciler is observation-only in v0a; dispatch lands in v0b.
        let reconciler_enabled = self
            .pm_service
            .as_ref()
            .map(|pm| pm.source_str() == "beads")
            .unwrap_or(false);
        if reconciler_enabled {
            info!("reconciler enabled (beads backend)");
        }
        mcp_server.set_reconciler_enabled(reconciler_enabled, None);
        mcp_server.set_repo_root(self.repo_root.clone());
        mcp_server.set_auto_merge_approved_plans(self.config.spur.auto_merge_approved_plans);
        mcp_server.set_plan_pending_grace(std::time::Duration::from_secs(
            self.config.spur.plan_pending_grace_secs,
        ));
        mcp_server
            .set_versioned_cache_serve(self.config.plan.substrate_migration.versioned_cache_serve);
        mcp_server.set_nonadvisory_review_writes(
            self.config
                .plan
                .substrate_migration
                .nonadvisory_review_writes,
        );
        mcp_server.set_dispatch_lease_duration(std::time::Duration::from_secs(
            self.config.spur.dispatch_lease_secs,
        ));
    }

    /// Subscribe to orchestrator events (for TUI, logging, etc.).
    pub fn subscribe(&self) -> broadcast::Receiver<SpurEvent> {
        self.event_tx.subscribe()
    }

    /// INV-6: Return a clonable handle to the cancellation token registry.
    /// Pass a clone to `McpCallbackServer` so `handle_cancel_delegation` can
    /// signal running delegations without routing through the delegation channel.
    pub fn cancellation_control(&self) -> CancellationControl {
        self.cancellation_control.clone()
    }

    /// Spawn the licensing runtime helper against this orchestrator's event funnel.
    pub fn spawn_license_runtime(&self, license: SpurLicense) -> JoinHandle<()> {
        crate::license_runtime::spawn_license_runtime(license, self.funnel.clone())
    }

    pub(in crate::orchestrator) fn mcp_feature_gate(&self) -> Arc<spur_license::FeatureGate> {
        self.feature_gate
            .clone()
            .unwrap_or_else(|| {
                tracing::warn!(
                    "MCP server constructed without explicit FeatureGate; falling back to community-tier permissions"
                );
                spur_mcp::server::community_feature_gate()
            })
    }

    /// Classify an error as an auth-required failure.
    ///
    /// The ACP spec reserves error code `-32000` with `authRequired`-shaped
    /// data payloads for this, but in practice the agent_client_protocol
    /// crate surfaces it as a stringly-typed error. Claude Code's wrapper
    /// also prints human-readable prompts. Match on substrings.
    pub(in crate::orchestrator) fn is_auth_required_error(e: &anyhow::Error) -> bool {
        let msg = e.to_string().to_lowercase();
        msg.contains("authrequired")
            || msg.contains("auth_required")
            || msg.contains("please run /login")
            || msg.contains("run `/login`")
            || msg.contains("run /login")
    }

    /// Human-readable banner text for auth-required failures.
    pub(in crate::orchestrator) fn auth_required_banner() -> String {
        "Claude Code requires authentication. Run `claude /login` in a \
         terminal, then restart this session. Press any key to dismiss."
            .to_string()
    }

    // ─── Private helpers ─────────────────────────────────────────────

    pub(in crate::orchestrator) async fn fetch_issue_context(
        &self,
        issue_ref: &str,
    ) -> Result<Issue> {
        let pm = self
            .pm_service
            .as_ref()
            .ok_or_else(|| anyhow!("No issue tracker configured"))?;

        // Strip prefix if present (e.g., "github:owner/repo#42" → "42")
        let id = if let Some(rest) = issue_ref.strip_prefix("github:") {
            rest.rsplit_once('#').map(|(_, id)| id).unwrap_or(rest)
        } else if let Some(rest) = issue_ref.strip_prefix("beads:") {
            rest
        } else {
            issue_ref
        };

        pm.get_issue(id).await
    }

    /// Emit an event through the S2 funnel. The funnel stamps `seq` +
    /// `occurred_at`, so the caller's `event.occurred_at` is discarded —
    /// the funnel's value is more accurate (wall-clock at send-to-broadcast
    /// moment). Signature unchanged so the ~22 method-scope
    /// `self.emit(SpurEvent::now(body))` callers compile transparently.
    pub(in crate::orchestrator) fn emit(&self, event: SpurEvent) {
        self.funnel.emit(event.body);
    }

    /// Read the cached `config_options` for the active brain session.
    ///
    /// `BrainSession` lives as a stack-local in `run_interactive`, so the
    /// caller threads it in. Returns the snapshot owned by the session;
    /// callers that hold only a `SessionId` can compare against
    /// `brain.spur_session_id` first.
    pub fn session_config_options(
        &self,
        brain: &BrainSession,
    ) -> Vec<agent_client_protocol::schema::SessionConfigOption> {
        brain.config_options.clone()
    }

    /// Read the cached `SpurAgentCaps` for the active brain session
    /// (M8.A). Mirrors `session_config_options`'s shape — `BrainSession`
    /// is the per-session entry, so the caller threads it in.
    ///
    /// Returns `None` when caps haven't been populated yet (e.g.
    /// resumed-via-load_session sessions on the M8.A code path), in
    /// which case downstream UI should render disabled state.
    pub fn spur_agent_caps(&self, brain: &BrainSession) -> Option<Arc<spur_acp::SpurAgentCaps>> {
        brain.spur_agent_caps.clone()
    }

    /// Read the cached `SessionInfoCache` for the active brain session
    /// (M9 hoist, F-3-1). Mirrors `spur_agent_caps`'s shape — `BrainSession`
    /// is the per-session entry, so the caller threads it in.
    ///
    /// Returns `None` when the agent has not yet emitted a
    /// `SessionInfoUpdate` notification. Once emitted, the cache survives
    /// view rebuilds (the cache lives on the orchestrator entry, not on
    /// the transient `SessionDetailView`).
    pub fn session_info(&self, brain: &BrainSession) -> Option<spur_acp::SessionInfoCache> {
        brain.session_info.clone()
    }

    /// Merge a `SessionInfoUpdate` notification into the brain session's
    /// cached `SessionInfoCache`, applying ACP `MaybeUndefined`
    /// semantics (Undefined preserves, Null clears, Value sets). Creates
    /// the cache lazily on the first emission.
    pub fn apply_session_info_update(
        &self,
        brain: &mut BrainSession,
        info: &agent_client_protocol::schema::SessionInfoUpdate,
    ) {
        let cache = brain
            .session_info
            .get_or_insert_with(spur_acp::SessionInfoCache::default);
        cache.merge(info);
        tracing::trace!(
            brain = %brain.brain_name,
            session_id = %brain.spur_session_id,
            title = ?cache.title,
            updated_at = ?cache.updated_at,
            "session_info_update merged into orchestrator cache",
        );
    }

    /// Replace the cached `config_options` on the active brain session and
    /// emit `CommandRegistryDirty` so spur-tui rebuilds the registry on
    /// the next ensure_cache.
    ///
    /// Used by the `SetSessionConfigOption` handler (Task 2.14) and by
    /// the `session/update.ConfigOptionUpdate` notification handler
    /// (v2 plan).
    pub fn replace_session_config_options(
        &self,
        brain: &mut BrainSession,
        opts: Vec<agent_client_protocol::schema::SessionConfigOption>,
    ) {
        brain.config_options = opts.clone();
        self.emit(SpurEvent::now(SpurEventBody::CommandRegistryDirty {
            session: brain.spur_session_id.clone(),
            config_options: opts,
        }));
    }
}
