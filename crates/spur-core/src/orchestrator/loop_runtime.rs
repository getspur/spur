use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::worker_mcp::WorkerMcpFetcher;
use super::Orchestrator;
use crate::plan::loops::leadership::{LoopRuntimeLeadership, LoopRuntimeLeadershipOutcome};

const LEADERSHIP_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const RUNTIME_START_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

type LeadershipAcquirer = dyn Fn(&Path) -> LoopRuntimeLeadershipOutcome + Send + Sync + 'static;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectLoopRuntimeState {
    Standby,
    LeaderRunning,
    UnsafeDisabled,
    Stopped,
}

#[async_trait]
pub(crate) trait ProjectLoopRuntimeInstance: Send {
    async fn shutdown(self: Box<Self>);

    async fn wait_for_exit(&mut self) {
        std::future::pending::<()>().await;
    }
}

#[async_trait]
pub(crate) trait ProjectLoopRuntimeFactory: Send + Sync {
    async fn start(&self) -> anyhow::Result<Box<dyn ProjectLoopRuntimeInstance>>;
}

pub(crate) struct ProjectLoopRuntimeSupervisor {
    #[cfg(test)]
    state_rx: watch::Receiver<ProjectLoopRuntimeState>,
    cancel: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

impl ProjectLoopRuntimeSupervisor {
    pub(crate) fn start_for_orchestrator(orchestrator: &Orchestrator) -> Option<Self> {
        let factory = ProjectLoopRuntimeDeps::from_orchestrator(orchestrator)?;
        Some(Self::start_with(
            orchestrator.repo_root.clone(),
            Arc::new(factory),
            Arc::new(LoopRuntimeLeadership::try_acquire),
            LEADERSHIP_RETRY_INTERVAL,
        ))
    }

    fn start_with(
        repo_root: PathBuf,
        factory: Arc<dyn ProjectLoopRuntimeFactory>,
        acquire: Arc<LeadershipAcquirer>,
        retry_interval: Duration,
    ) -> Self {
        let (state_tx, state_rx) = watch::channel(ProjectLoopRuntimeState::Standby);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            run_supervisor(
                repo_root,
                factory,
                acquire,
                retry_interval,
                task_cancel,
                state_tx,
            )
            .await;
        });
        #[cfg(not(test))]
        drop(state_rx);
        Self {
            #[cfg(test)]
            state_rx,
            cancel,
            handle: Some(handle),
        }
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> ProjectLoopRuntimeState {
        *self.state_rx.borrow()
    }

    #[cfg(test)]
    pub(crate) fn subscribe_state(&self) -> watch::Receiver<ProjectLoopRuntimeState> {
        self.state_rx.clone()
    }

    pub(crate) async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for ProjectLoopRuntimeSupervisor {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

async fn run_supervisor(
    repo_root: PathBuf,
    factory: Arc<dyn ProjectLoopRuntimeFactory>,
    acquire: Arc<LeadershipAcquirer>,
    retry_interval: Duration,
    cancel: CancellationToken,
    state_tx: watch::Sender<ProjectLoopRuntimeState>,
) {
    loop {
        if cancel.is_cancelled() {
            state_tx.send_replace(ProjectLoopRuntimeState::Stopped);
            return;
        }
        match acquire(&repo_root) {
            LoopRuntimeLeadershipOutcome::Acquired(leadership) => {
                tracing::info!(
                    repo = %repo_root.display(),
                    "project L3 runtime acquired repository leadership"
                );
                loop {
                    let runtime = loop {
                        let start = factory.start();
                        tokio::pin!(start);
                        match tokio::select! {
                            biased;
                            _ = cancel.cancelled() => None,
                            result = &mut start => Some(result),
                        } {
                            None => break None,
                            Some(Ok(runtime)) => break Some(runtime),
                            Some(Err(error)) => {
                                tracing::warn!(
                                    %error,
                                    "project L3 runtime failed to start; retaining leadership and retrying"
                                );
                                tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => break None,
                                    _ = tokio::time::sleep(RUNTIME_START_RETRY_INTERVAL) => {}
                                }
                            }
                        }
                    };
                    let Some(mut runtime) = runtime else {
                        break;
                    };
                    state_tx.send_replace(ProjectLoopRuntimeState::LeaderRunning);
                    let unexpected_exit = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => false,
                        _ = runtime.wait_for_exit() => true,
                    };
                    runtime.shutdown().await;
                    if !unexpected_exit || cancel.is_cancelled() {
                        break;
                    }
                    tracing::warn!(
                        "project L3 runtime child exited unexpectedly; retaining leadership and restarting"
                    );
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(retry_interval) => {}
                    }
                }
                drop(leadership);
                state_tx.send_replace(ProjectLoopRuntimeState::Stopped);
                return;
            }
            LoopRuntimeLeadershipOutcome::Standby { holder } => {
                state_tx.send_replace(ProjectLoopRuntimeState::Standby);
                tracing::debug!(?holder, "project L3 runtime standing by for leadership");
            }
            LoopRuntimeLeadershipOutcome::Unsafe { reason } => {
                tracing::warn!(
                    %reason,
                    "project L3 runtime disabled because advisory locking is unsafe"
                );
                state_tx.send_replace(ProjectLoopRuntimeState::UnsafeDisabled);
                cancel.cancelled().await;
                state_tx.send_replace(ProjectLoopRuntimeState::Stopped);
                return;
            }
            LoopRuntimeLeadershipOutcome::Io(error) => {
                tracing::warn!(
                    %error,
                    "project L3 runtime leadership check failed; remaining in standby"
                );
                state_tx.send_replace(ProjectLoopRuntimeState::Standby);
            }
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                state_tx.send_replace(ProjectLoopRuntimeState::Stopped);
                return;
            }
            _ = tokio::time::sleep(retry_interval) => {}
        }
    }
}

struct ProjectLoopRuntimeDeps {
    repo_root: PathBuf,
    pm_service: Arc<spur_pm::PmService>,
    agent_configs: Arc<parking_lot::RwLock<Vec<spur_acp::config::AgentConfig>>>,
    workers: Vec<crate::server::WorkerInfo>,
    max_concurrent: usize,
    worktree_config: spur_acp::config::WorktreeConfig,
    event_tx: tokio::sync::broadcast::Sender<spur_acp::SpurEvent>,
    funnel: crate::event_funnel::FunnelHandle,
    review_sink: crate::review_sink::ReviewSink,
    feature_gate: Arc<spur_license::FeatureGate>,
    cancellation_control: spur_acp::CancellationControl,
    fault_injection_hooks: super::FaultInjectionHooks,
    dispatch_lease_duration: Duration,
    dispatch_lease_heartbeat: Duration,
    worker_mcp_servers:
        Arc<dashmap::DashMap<spur_acp::BrainSessionId, Arc<crate::worker_server::WorkerMcpServer>>>,
    outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    inline_wait: Duration,
    loops_enabled: bool,
    pause_all_loops: bool,
    auto_merge_approved_plans: bool,
    plan_pending_grace: Duration,
    versioned_cache_serve: bool,
    nonadvisory_review_writes: bool,
    normalize_bypass_hooks: bool,
}

impl ProjectLoopRuntimeDeps {
    fn from_orchestrator(orchestrator: &Orchestrator) -> Option<Self> {
        let pm_service = orchestrator.pm_service.as_ref()?.clone();
        if pm_service.source_str() != "beads" || pm_service.advanced().is_none() {
            return None;
        }
        let feature_gate = orchestrator.mcp_feature_gate();
        if crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .is_err()
        {
            return None;
        }
        let max_concurrent = orchestrator
            .feature_gate
            .as_ref()
            .and_then(|gate| gate.quota(spur_license::QuotaKey::MaxConcurrentWorkers))
            .and_then(|value| value.as_count())
            .map(|value| value as usize)
            .unwrap_or(orchestrator.config.worktree.max_concurrent);
        let workers = orchestrator
            .registry
            .worker_capable()
            .into_iter()
            .map(crate::server::build_worker_info)
            .collect();
        Some(Self {
            repo_root: orchestrator.repo_root.clone(),
            pm_service,
            agent_configs: Arc::clone(&orchestrator.agent_configs),
            workers,
            max_concurrent,
            worktree_config: orchestrator.config.worktree.clone(),
            event_tx: orchestrator.event_tx.clone(),
            funnel: orchestrator.funnel.clone(),
            review_sink: orchestrator.review_sink.clone(),
            feature_gate,
            cancellation_control: orchestrator.cancellation_control.clone(),
            fault_injection_hooks: orchestrator.fault_injection_hooks.clone(),
            dispatch_lease_duration: Duration::from_secs(
                orchestrator.config.spur.dispatch_lease_secs,
            ),
            dispatch_lease_heartbeat: Duration::from_secs(
                orchestrator.config.spur.dispatch_lease_heartbeat_secs,
            ),
            worker_mcp_servers: Arc::clone(&orchestrator.worker_mcp_servers),
            outcome_store: Arc::clone(&orchestrator.outcome_store),
            inline_wait: Duration::from_millis(orchestrator.config.delegation.inline_wait_ms),
            loops_enabled: orchestrator.config.spur.loops_enabled,
            pause_all_loops: orchestrator.config.spur.pause_all_loops,
            auto_merge_approved_plans: orchestrator.config.spur.auto_merge_approved_plans,
            plan_pending_grace: Duration::from_secs(
                orchestrator.config.spur.plan_pending_grace_secs,
            ),
            versioned_cache_serve: orchestrator
                .config
                .plan
                .substrate_migration
                .versioned_cache_serve,
            nonadvisory_review_writes: orchestrator
                .config
                .plan
                .substrate_migration
                .nonadvisory_review_writes,
            normalize_bypass_hooks: orchestrator.config.delegation.normalize.bypass_hooks,
        })
    }
}

struct RunningProjectLoopRuntime {
    system_id: spur_acp::BrainSessionId,
    server: Arc<crate::server::McpCallbackServer>,
    delegation_handle: Option<JoinHandle<()>>,
    delegation_shutdown: CancellationToken,
    worker_mcp_servers:
        Arc<dashmap::DashMap<spur_acp::BrainSessionId, Arc<crate::worker_server::WorkerMcpServer>>>,
}

#[async_trait]
impl ProjectLoopRuntimeInstance for RunningProjectLoopRuntime {
    async fn shutdown(mut self: Box<Self>) {
        let deadline = tokio::time::Instant::now() + RUNTIME_SHUTDOWN_TIMEOUT;
        self.server.mark_retiring();
        self.server.cancel_in_flight_workers();
        self.delegation_shutdown.cancel();
        if let Some(delegation_handle) = self.delegation_handle.take() {
            let mut delegation_handle = delegation_handle;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if tokio::time::timeout(remaining, &mut delegation_handle)
                .await
                .is_err()
            {
                tracing::warn!(
                    timeout_ms = RUNTIME_SHUTDOWN_TIMEOUT.as_millis() as u64,
                    "project L3 delegation shutdown exceeded grace; force-aborting child"
                );
                delegation_handle.abort();
                tokio::task::yield_now().await;
                if delegation_handle.is_finished() {
                    let _ = delegation_handle.await;
                } else {
                    tracing::warn!(
                        "project L3 delegation child did not acknowledge abort immediately"
                    );
                }
            }
        }

        if let Some((_system_id, worker_server)) = self.worker_mcp_servers.remove(&self.system_id) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let outcome = worker_server.shutdown(remaining).await;
            if !outcome.drained {
                tracing::warn!(
                    timeout_ms = RUNTIME_SHUTDOWN_TIMEOUT.as_millis() as u64,
                    active_at_deadline = outcome.active_at_deadline,
                    "project L3 worker MCP server exceeded drain grace"
                );
            }
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if tokio::time::timeout(remaining, self.server.shutdown())
            .await
            .is_err()
        {
            tracing::warn!(
                timeout_ms = RUNTIME_SHUTDOWN_TIMEOUT.as_millis() as u64,
                "project L3 server shutdown exceeded grace; force-aborting tasks"
            );
            self.server.force_abort();
        }
    }

    async fn wait_for_exit(&mut self) {
        loop {
            let delegation_finished = self
                .delegation_handle
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished);
            if delegation_finished || self.server.reconciler_task_finished() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for RunningProjectLoopRuntime {
    fn drop(&mut self) {
        self.delegation_shutdown.cancel();
        self.server.force_abort();
        if let Some(delegation_handle) = self.delegation_handle.take() {
            delegation_handle.abort();
        }
    }
}

#[async_trait]
impl ProjectLoopRuntimeFactory for ProjectLoopRuntimeDeps {
    async fn start(&self) -> anyhow::Result<Box<dyn ProjectLoopRuntimeInstance>> {
        let system_id = spur_acp::BrainSessionId::new(spur_acp::SessionId(
            crate::plan::loops::LOOP_RUNTIME_OWNER_ID.into(),
        ));
        let event_sink: Option<Arc<dyn spur_mcp::events::McpEventSink>> =
            Some(Arc::new(self.funnel.clone()));
        let continuation_ctx = crate::server::DetachedContinuationCtx {
            on_complete: Arc::new(|_continuation, _worker| Box::pin(async {})),
        };
        let (mut server, delegation_channel) =
            crate::server::McpCallbackServer::new_headless_project_runtime(
                &system_id,
                Arc::clone(&self.pm_service),
                event_sink,
                continuation_ctx,
                Arc::clone(&self.outcome_store),
                Arc::clone(&self.feature_gate),
            );
        server.set_workers(self.workers.clone());
        server.set_cancellation_control(self.cancellation_control.clone());
        server.set_inline_wait(self.inline_wait);
        server.set_reconciler_enabled(true, None);
        server.set_reconciler_scopes(
            crate::plan::loops::LoopSweepScope::L3Only,
            crate::plan::reconciler::PlanScope::SystemL3Only,
        );
        server.set_repo_root(self.repo_root.clone());
        server.set_auto_merge_approved_plans(self.auto_merge_approved_plans);
        server.set_loop_runtime(self.loops_enabled, self.pause_all_loops);
        server.set_plan_pending_grace(self.plan_pending_grace);
        server.set_versioned_cache_serve(self.versioned_cache_serve);
        server.set_nonadvisory_review_writes(self.nonadvisory_review_writes);
        server.set_dispatch_lease_duration(self.dispatch_lease_duration);

        let server = Arc::new(server);
        let worker_mcp_fetcher = WorkerMcpFetcher {
            cache: Arc::clone(&self.worker_mcp_servers),
            pm_service: Some(Arc::clone(&self.pm_service)),
            feature_gate: Some(Arc::clone(&self.feature_gate)),
            funnel: self.funnel.clone(),
            mcp_server: Arc::clone(&server),
            outcome_store: Arc::clone(&self.outcome_store),
            repo_root: Some(self.repo_root.clone()),
        };
        let delegation_shutdown = CancellationToken::new();
        let delegation_handle = tokio::spawn(super::delegation::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            Arc::clone(&self.agent_configs),
            self.max_concurrent,
            self.worktree_config.clone(),
            self.event_tx.clone(),
            self.funnel.clone(),
            self.review_sink.clone(),
            Some(Arc::clone(&self.pm_service)),
            Arc::clone(&self.feature_gate),
            self.cancellation_control.clone(),
            None,
            self.fault_injection_hooks.clone(),
            self.dispatch_lease_duration,
            self.dispatch_lease_heartbeat,
            worker_mcp_fetcher,
            self.normalize_bypass_hooks,
            delegation_shutdown.clone(),
        ));
        if let Err(error) = Arc::clone(&server).enable_reconciler().await {
            delegation_shutdown.cancel();
            let _ = delegation_handle.await;
            return Err(error);
        }
        server.fast_forward_reconciler();
        Ok(Box::new(RunningProjectLoopRuntime {
            system_id,
            server,
            delegation_handle: Some(delegation_handle),
            delegation_shutdown,
            worker_mcp_servers: Arc::clone(&self.worker_mcp_servers),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::Notify;

    struct ImmediateRuntime;

    #[async_trait]
    impl ProjectLoopRuntimeInstance for ImmediateRuntime {
        async fn shutdown(self: Box<Self>) {}
    }

    struct ExitImmediatelyRuntime;

    #[async_trait]
    impl ProjectLoopRuntimeInstance for ExitImmediatelyRuntime {
        async fn shutdown(self: Box<Self>) {}

        async fn wait_for_exit(&mut self) {}
    }

    struct BlockingRuntime {
        shutdown_started: Arc<Notify>,
        shutdown_release: Arc<Notify>,
    }

    #[async_trait]
    impl ProjectLoopRuntimeInstance for BlockingRuntime {
        async fn shutdown(self: Box<Self>) {
            self.shutdown_started.notify_waiters();
            self.shutdown_release.notified().await;
        }
    }

    #[derive(Clone)]
    struct CountingFactory {
        starts: Arc<AtomicUsize>,
        shutdown_started: Option<Arc<Notify>>,
        shutdown_release: Option<Arc<Notify>>,
    }

    struct RestartingFactory {
        starts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProjectLoopRuntimeFactory for RestartingFactory {
        async fn start(&self) -> anyhow::Result<Box<dyn ProjectLoopRuntimeInstance>> {
            let attempt = self.starts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                Ok(Box::new(ExitImmediatelyRuntime))
            } else {
                Ok(Box::new(ImmediateRuntime))
            }
        }
    }

    impl CountingFactory {
        fn immediate() -> Self {
            Self {
                starts: Arc::new(AtomicUsize::new(0)),
                shutdown_started: None,
                shutdown_release: None,
            }
        }

        fn blocking(shutdown_started: Arc<Notify>, shutdown_release: Arc<Notify>) -> Self {
            Self {
                starts: Arc::new(AtomicUsize::new(0)),
                shutdown_started: Some(shutdown_started),
                shutdown_release: Some(shutdown_release),
            }
        }
    }

    #[async_trait]
    impl ProjectLoopRuntimeFactory for CountingFactory {
        async fn start(&self) -> anyhow::Result<Box<dyn ProjectLoopRuntimeInstance>> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            match (&self.shutdown_started, &self.shutdown_release) {
                (Some(started), Some(release)) => Ok(Box::new(BlockingRuntime {
                    shutdown_started: Arc::clone(started),
                    shutdown_release: Arc::clone(release),
                })),
                _ => Ok(Box::new(ImmediateRuntime)),
            }
        }
    }

    async fn wait_for_state(
        supervisor: &ProjectLoopRuntimeSupervisor,
        expected: ProjectLoopRuntimeState,
    ) {
        let mut states = supervisor.subscribe_state();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if *states.borrow() == expected {
                    return;
                }
                states.changed().await.expect("supervisor state channel");
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {expected:?}"));
    }

    fn filesystem_acquirer() -> Arc<
        dyn Fn(&Path) -> crate::plan::loops::leadership::LoopRuntimeLeadershipOutcome + Send + Sync,
    > {
        Arc::new(crate::plan::loops::leadership::LoopRuntimeLeadership::try_acquire)
    }

    async fn runtime_deps_fixture() -> (TempDir, ProjectLoopRuntimeDeps) {
        let repo = TempDir::new().unwrap();
        let workspace = spur_pm::test_workspace::TestBeadsWorkspace::init();
        let beads_dir = repo.path().join(".beads");
        std::fs::create_dir_all(&beads_dir).unwrap();
        workspace.copy_db_to(&beads_dir);
        let pm = Arc::new(
            spur_pm::PmService::try_new(None, true, false, repo.path(), None)
                .await
                .unwrap()
                .unwrap(),
        );
        let orchestrator = Orchestrator::new(
            repo.path().to_path_buf(),
            spur_acp::config::SpurConfig::default(),
            Some(crate::server::pro_feature_gate()),
        )
        .unwrap()
        .with_pm_service(pm);
        let deps = ProjectLoopRuntimeDeps::from_orchestrator(&orchestrator)
            .expect("licensed beads runtime dependencies");
        (repo, deps)
    }

    fn bare_runtime(
        deps: &ProjectLoopRuntimeDeps,
        delegation_handle: Option<JoinHandle<()>>,
    ) -> (RunningProjectLoopRuntime, spur_acp::BrainSessionId) {
        let system_id = spur_acp::BrainSessionId::new(spur_acp::SessionId(
            crate::plan::loops::LOOP_RUNTIME_OWNER_ID.into(),
        ));
        let continuation_ctx = crate::server::DetachedContinuationCtx {
            on_complete: Arc::new(|_continuation, _worker| Box::pin(async {})),
        };
        let (server, _delegation_channel) =
            crate::server::McpCallbackServer::new_headless_project_runtime(
                &system_id,
                Arc::clone(&deps.pm_service),
                Some(Arc::new(deps.funnel.clone())),
                continuation_ctx,
                Arc::clone(&deps.outcome_store),
                Arc::clone(&deps.feature_gate),
            );
        (
            RunningProjectLoopRuntime {
                system_id: system_id.clone(),
                server: Arc::new(server),
                delegation_handle,
                delegation_shutdown: CancellationToken::new(),
                worker_mcp_servers: Arc::clone(&deps.worker_mcp_servers),
            },
            system_id,
        )
    }

    fn worker_fetcher(
        deps: &ProjectLoopRuntimeDeps,
        runtime: &RunningProjectLoopRuntime,
    ) -> WorkerMcpFetcher {
        WorkerMcpFetcher {
            cache: Arc::clone(&deps.worker_mcp_servers),
            pm_service: Some(Arc::clone(&deps.pm_service)),
            feature_gate: Some(Arc::clone(&deps.feature_gate)),
            funnel: deps.funnel.clone(),
            mcp_server: Arc::clone(&runtime.server),
            outcome_store: Arc::clone(&deps.outcome_store),
            repo_root: Some(deps.repo_root.clone()),
        }
    }

    #[tokio::test]
    async fn runtime_shutdown_evicts_worker_server_and_restart_uses_fresh_dependencies() {
        let (_repo, deps) = runtime_deps_fixture().await;
        let (first_runtime, system_id) = bare_runtime(&deps, None);
        let first_worker = worker_fetcher(&deps, &first_runtime)
            .ensure(&system_id)
            .await
            .expect("first runtime worker MCP server");

        Box::new(first_runtime).shutdown().await;
        let first_cache_retired = !deps.worker_mcp_servers.contains_key(&system_id);

        let (second_runtime, second_system_id) = bare_runtime(&deps, None);
        let second_worker = worker_fetcher(&deps, &second_runtime)
            .ensure(&second_system_id)
            .await
            .expect("second runtime worker MCP server");
        let fresh_server = !Arc::ptr_eq(&first_worker, &second_worker);
        Box::new(second_runtime).shutdown().await;
        let second_cache_retired = !deps.worker_mcp_servers.contains_key(&system_id);

        if let Some((_id, lingering)) = deps.worker_mcp_servers.remove(&system_id) {
            let _ = lingering.shutdown(Duration::ZERO).await;
        }
        assert!(
            first_cache_retired,
            "retired leadership must remove its stable system cache entry"
        );
        assert!(
            fresh_server,
            "same-process leadership restart must not reuse the prior runtime's worker server"
        );
        assert!(
            second_cache_retired,
            "successor shutdown must leave no stable system cache entry"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn runtime_shutdown_aborts_a_delegation_child_after_the_grace_bound() {
        let (_repo, deps) = runtime_deps_fixture().await;
        let delegation = tokio::spawn(std::future::pending::<()>());
        let (runtime, _system_id) = bare_runtime(&deps, Some(delegation));
        let mut shutdown = tokio::spawn(async move { Box::new(runtime).shutdown().await });

        tokio::task::yield_now().await;
        tokio::time::advance(RUNTIME_SHUTDOWN_TIMEOUT + Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        let finished = shutdown.is_finished();
        if !finished {
            shutdown.abort();
            let _ = (&mut shutdown).await;
        }
        assert!(
            finished,
            "shutdown must force-abort a child that outlives the grace period"
        );
    }

    #[tokio::test]
    async fn two_supervisors_start_exactly_one_leader() {
        let repo = TempDir::new().unwrap();
        let first_factory = Arc::new(CountingFactory::immediate());
        let second_factory = Arc::new(CountingFactory::immediate());
        let mut first = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&first_factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&first, ProjectLoopRuntimeState::LeaderRunning).await;
        let mut second = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&second_factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&second, ProjectLoopRuntimeState::Standby).await;

        assert_eq!(first_factory.starts.load(Ordering::SeqCst), 1);
        assert_eq!(second_factory.starts.load(Ordering::SeqCst), 0);
        first.shutdown().await;
        second.shutdown().await;
    }

    #[tokio::test]
    async fn standby_promotes_after_leader_shutdown() {
        let repo = TempDir::new().unwrap();
        let first_factory = Arc::new(CountingFactory::immediate());
        let second_factory = Arc::new(CountingFactory::immediate());
        let mut first = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            first_factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&first, ProjectLoopRuntimeState::LeaderRunning).await;
        let mut second = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&second_factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&second, ProjectLoopRuntimeState::Standby).await;

        first.shutdown().await;
        wait_for_state(&second, ProjectLoopRuntimeState::LeaderRunning).await;
        assert_eq!(second_factory.starts.load(Ordering::SeqCst), 1);
        second.shutdown().await;
    }

    #[tokio::test]
    async fn leadership_is_retained_until_runtime_shutdown_finishes() {
        let repo = TempDir::new().unwrap();
        let shutdown_started = Arc::new(Notify::new());
        let shutdown_release = Arc::new(Notify::new());
        let first_factory = Arc::new(CountingFactory::blocking(
            Arc::clone(&shutdown_started),
            Arc::clone(&shutdown_release),
        ));
        let second_factory = Arc::new(CountingFactory::immediate());
        let mut first = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            first_factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&first, ProjectLoopRuntimeState::LeaderRunning).await;
        let mut second = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            second_factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&second, ProjectLoopRuntimeState::Standby).await;

        let started = shutdown_started.notified();
        let first_shutdown = tokio::spawn(async move { first.shutdown().await });
        started.await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(second.state(), ProjectLoopRuntimeState::Standby);

        shutdown_release.notify_waiters();
        first_shutdown.await.unwrap();
        wait_for_state(&second, ProjectLoopRuntimeState::LeaderRunning).await;
        second.shutdown().await;
    }

    #[tokio::test]
    async fn unsafe_locking_disables_runtime_without_starting_factory() {
        let repo = TempDir::new().unwrap();
        let factory = Arc::new(CountingFactory::immediate());
        let unsafe_acquirer = Arc::new(|_repo: &Path| {
            crate::plan::loops::leadership::LoopRuntimeLeadershipOutcome::Unsafe {
                reason: "injected unsupported advisory lock".into(),
            }
        });
        let mut supervisor = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            unsafe_acquirer,
            Duration::from_millis(10),
        );

        wait_for_state(&supervisor, ProjectLoopRuntimeState::UnsafeDisabled).await;
        assert_eq!(factory.starts.load(Ordering::SeqCst), 0);
        supervisor.shutdown().await;
    }

    #[tokio::test]
    async fn unexpected_runtime_exit_restarts_without_releasing_leadership() {
        let repo = TempDir::new().unwrap();
        let starts = Arc::new(AtomicUsize::new(0));
        let restarting_factory = Arc::new(RestartingFactory {
            starts: Arc::clone(&starts),
        });
        let standby_factory = Arc::new(CountingFactory::immediate());
        let mut leader = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            restarting_factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while starts.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("runtime was not restarted after its unexpected exit");

        let mut standby = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&standby_factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&standby, ProjectLoopRuntimeState::Standby).await;
        assert_eq!(
            standby_factory.starts.load(Ordering::SeqCst),
            0,
            "restart must retain leadership instead of promoting a standby"
        );

        leader.shutdown().await;
        standby.shutdown().await;
    }

    #[tokio::test]
    async fn unlicensed_process_does_not_compete_for_runtime_leadership() {
        let repo = TempDir::new().unwrap();
        let workspace = spur_pm::test_workspace::TestBeadsWorkspace::init();
        let beads_dir = repo.path().join(".beads");
        std::fs::create_dir_all(&beads_dir).unwrap();
        workspace.copy_db_to(&beads_dir);
        let pm = Arc::new(
            spur_pm::PmService::try_new(None, true, false, repo.path(), None)
                .await
                .unwrap()
                .unwrap(),
        );
        let orchestrator = Orchestrator::new(
            repo.path().to_path_buf(),
            spur_acp::config::SpurConfig::default(),
            Some(crate::server::unlicensed_feature_gate()),
        )
        .unwrap()
        .with_pm_service(pm);

        assert!(
            ProjectLoopRuntimeDeps::from_orchestrator(&orchestrator).is_none(),
            "a process that cannot run the advanced reconciler must not hold L3 leadership"
        );
    }
}
