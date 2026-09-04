use async_trait::async_trait;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
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
    LeaderDraining,
    UnsafeDisabled,
    Stopped,
}

pub(crate) struct ProjectLoopRuntimeDrain {
    completion: Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
}

impl ProjectLoopRuntimeDrain {
    fn new(future: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            completion: Box::pin(future),
        }
    }

    #[cfg(test)]
    fn completed() -> Self {
        Self::new(std::future::ready(()))
    }
}

#[async_trait]
pub(crate) trait ProjectLoopRuntimeInstance: Send {
    async fn shutdown(self: Box<Self>) -> ProjectLoopRuntimeDrain;

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
        // Dropping a Tokio JoinHandle detaches the task. Keep the supervisor
        // task alive so it can transfer the leadership guard into its drain;
        // aborting it here would drop the guard before runtime children
        // acknowledge cancellation.
        let _ = self.handle.take();
    }
}

fn detach_leader_drain(
    leadership: LoopRuntimeLeadership,
    drain: ProjectLoopRuntimeDrain,
    state_tx: watch::Sender<ProjectLoopRuntimeState>,
) {
    state_tx.send_replace(ProjectLoopRuntimeState::LeaderDraining);
    tokio::spawn(async move {
        drain.completion.await;
        drop(leadership);
        state_tx.send_replace(ProjectLoopRuntimeState::Stopped);
    });
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
                    let mut drain = runtime.shutdown().await;
                    if !unexpected_exit || cancel.is_cancelled() {
                        detach_leader_drain(leadership, drain, state_tx.clone());
                        return;
                    }
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            detach_leader_drain(leadership, drain, state_tx.clone());
                            return;
                        }
                        _ = &mut drain.completion => {}
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
    worker_mcp_default: bool,
    worker_mcp_servers:
        Arc<dashmap::DashMap<spur_acp::BrainSessionId, Arc<crate::worker_server::WorkerMcpServer>>>,
    outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    context_service_config: spur_acp::config::ContextServiceConfig,
    inline_wait: Duration,
    loops_enabled: bool,
    pause_all_loops: bool,
    auto_merge_approved_plans: bool,
    plan_pending_grace: Duration,
    versioned_cache_serve: bool,
    nonadvisory_review_writes: bool,
    normalize_bypass_hooks: bool,
    #[cfg(feature = "test-support")]
    delegation_capture: Option<tokio::sync::mpsc::Sender<crate::DelegationRequest>>,
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
            worker_mcp_default: orchestrator
                .config
                .mcp_servers
                .builtin_overrides
                .worker_mcp_enabled,
            worker_mcp_servers: Arc::clone(&orchestrator.worker_mcp_servers),
            outcome_store: Arc::clone(&orchestrator.outcome_store),
            context_service_config: orchestrator.config.context_service.clone(),
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
            #[cfg(feature = "test-support")]
            delegation_capture: orchestrator.project_loop_runtime_delegation_capture.clone(),
        })
    }

    fn spawn_delegation_handler(
        &self,
        delegation_channel: crate::DelegationChannel,
        worker_mcp_fetcher: WorkerMcpFetcher,
        delegation_shutdown: CancellationToken,
    ) -> JoinHandle<()> {
        #[cfg(feature = "test-support")]
        if let Some(capture) = self.delegation_capture.clone() {
            let mut request_rx = delegation_channel.request_rx;
            return tokio::spawn(async move {
                loop {
                    let request = tokio::select! {
                        _ = delegation_shutdown.cancelled() => break,
                        request = request_rx.recv() => match request {
                            Some(request) => request,
                            None => break,
                        },
                    };
                    tokio::select! {
                        _ = delegation_shutdown.cancelled() => break,
                        result = capture.send(request) => {
                            if result.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }

        tokio::spawn(super::delegation::handle_delegations(
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
            self.worker_mcp_default,
            worker_mcp_fetcher,
            self.normalize_bypass_hooks,
            delegation_shutdown,
        ))
    }
}

struct RunningProjectLoopRuntime {
    system_id: spur_acp::BrainSessionId,
    server: Arc<crate::server::McpCallbackServer>,
    delegation_handle: Option<JoinHandle<()>>,
    delegation_shutdown: CancellationToken,
    worker_mcp_servers:
        Arc<dashmap::DashMap<spur_acp::BrainSessionId, Arc<crate::worker_server::WorkerMcpServer>>>,
    shutdown_transferred_to_drain: bool,
}

#[async_trait]
impl ProjectLoopRuntimeInstance for RunningProjectLoopRuntime {
    async fn shutdown(mut self: Box<Self>) -> ProjectLoopRuntimeDrain {
        let deadline = tokio::time::Instant::now() + RUNTIME_SHUTDOWN_TIMEOUT;
        self.server.mark_retiring();
        self.server.cancel_in_flight_workers();
        self.delegation_shutdown.cancel();
        let mut pending_delegation = None;
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
                pending_delegation = Some(delegation_handle);
            }
        }

        let pending_worker_server = self
            .worker_mcp_servers
            .remove(&self.system_id)
            .map(|(_system_id, worker_server)| worker_server);
        let server = Arc::clone(&self.server);
        self.shutdown_transferred_to_drain = true;

        ProjectLoopRuntimeDrain::new(async move {
            let delegation_drain = async move {
                if let Some(handle) = pending_delegation {
                    let _ = handle.await;
                }
            };
            let worker_server_drain = async move {
                if let Some(worker_server) = pending_worker_server {
                    let outcome = Arc::clone(&worker_server)
                        .shutdown(RUNTIME_SHUTDOWN_TIMEOUT)
                        .await;
                    if !outcome.drained {
                        tracing::warn!(
                            timeout_ms = RUNTIME_SHUTDOWN_TIMEOUT.as_millis() as u64,
                            active_at_deadline = outcome.active_at_deadline,
                            "project L3 worker MCP server exceeded drain grace"
                        );
                    }
                    worker_server.force_abort_handlers_and_wait().await;
                }
            };
            let server_drain = async move {
                server.force_abort_and_wait().await;
            };
            tokio::join!(delegation_drain, worker_server_drain, server_drain);
        })
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
        if !self.shutdown_transferred_to_drain {
            self.server.force_abort();
        }
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
            context_service_config: self.context_service_config.clone(),
        };
        let delegation_shutdown = CancellationToken::new();
        let delegation_handle = self.spawn_delegation_handler(
            delegation_channel,
            worker_mcp_fetcher,
            delegation_shutdown.clone(),
        );
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
            shutdown_transferred_to_drain: false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rmcp::{
        model::CallToolRequestParams,
        transport::{
            streamable_http_client::StreamableHttpClientTransportConfig,
            StreamableHttpClientTransport,
        },
        ServiceExt,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::Notify;

    struct ImmediateRuntime;

    #[async_trait]
    impl ProjectLoopRuntimeInstance for ImmediateRuntime {
        async fn shutdown(self: Box<Self>) -> ProjectLoopRuntimeDrain {
            ProjectLoopRuntimeDrain::completed()
        }
    }

    struct ExitImmediatelyRuntime;

    #[async_trait]
    impl ProjectLoopRuntimeInstance for ExitImmediatelyRuntime {
        async fn shutdown(self: Box<Self>) -> ProjectLoopRuntimeDrain {
            ProjectLoopRuntimeDrain::completed()
        }

        async fn wait_for_exit(&mut self) {}
    }

    struct BlockingRuntime {
        shutdown_started: Arc<Notify>,
        shutdown_release: Arc<Notify>,
    }

    #[async_trait]
    impl ProjectLoopRuntimeInstance for BlockingRuntime {
        async fn shutdown(self: Box<Self>) -> ProjectLoopRuntimeDrain {
            self.shutdown_started.notify_waiters();
            self.shutdown_release.notified().await;
            ProjectLoopRuntimeDrain::completed()
        }
    }

    struct ModeSelectingRuntime {
        graceful_calls: Arc<AtomicUsize>,
        immediate_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProjectLoopRuntimeInstance for ModeSelectingRuntime {
        async fn shutdown(self: Box<Self>) -> ProjectLoopRuntimeDrain {
            self.graceful_calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        }

        async fn shutdown_immediately(self: Box<Self>) -> ProjectLoopRuntimeDrain {
            self.immediate_calls.fetch_add(1, Ordering::SeqCst);
            ProjectLoopRuntimeDrain::completed()
        }
    }

    struct ModeSelectingFactory {
        graceful_calls: Arc<AtomicUsize>,
        immediate_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProjectLoopRuntimeFactory for ModeSelectingFactory {
        async fn start(&self) -> anyhow::Result<Box<dyn ProjectLoopRuntimeInstance>> {
            Ok(Box::new(ModeSelectingRuntime {
                graceful_calls: Arc::clone(&self.graceful_calls),
                immediate_calls: Arc::clone(&self.immediate_calls),
            }))
        }
    }

    struct ControlledDrainRuntime {
        drain_started: Arc<Notify>,
        drain_release: Arc<Notify>,
    }

    #[async_trait]
    impl ProjectLoopRuntimeInstance for ControlledDrainRuntime {
        async fn shutdown(self: Box<Self>) -> ProjectLoopRuntimeDrain {
            self.drain_started.notify_one();
            let drain_release = Arc::clone(&self.drain_release);
            ProjectLoopRuntimeDrain::new(async move {
                drain_release.notified().await;
            })
        }
    }

    struct ControlledDrainFactory {
        starts: Arc<AtomicUsize>,
        drain_started: Arc<Notify>,
        drain_release: Arc<Notify>,
    }

    struct SingleRuntimeFactory {
        runtime: std::sync::Mutex<Option<RunningProjectLoopRuntime>>,
        starts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ProjectLoopRuntimeFactory for SingleRuntimeFactory {
        async fn start(&self) -> anyhow::Result<Box<dyn ProjectLoopRuntimeInstance>> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(
                self.runtime
                    .lock()
                    .expect("single runtime lock")
                    .take()
                    .expect("single runtime starts once"),
            ))
        }
    }

    #[derive(Default)]
    struct PermanentlyHangingSignalSink {
        entered: Notify,
    }

    #[async_trait]
    impl crate::worker_server::WorkerSignalSink for PermanentlyHangingSignalSink {
        async fn report_signal(
            &self,
            _ctx: &crate::handlers::WorkerCallContext,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value, crate::handlers::McpHandlerError> {
            std::future::pending().await
        }

        async fn report_progress(
            &self,
            _ctx: &crate::handlers::WorkerCallContext,
            _args: serde_json::Value,
        ) -> Result<serde_json::Value, crate::handlers::McpHandlerError> {
            self.entered.notify_one();
            std::future::pending().await
        }
    }

    #[async_trait]
    impl ProjectLoopRuntimeFactory for ControlledDrainFactory {
        async fn start(&self) -> anyhow::Result<Box<dyn ProjectLoopRuntimeInstance>> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(ControlledDrainRuntime {
                drain_started: Arc::clone(&self.drain_started),
                drain_release: Arc::clone(&self.drain_release),
            }))
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

    #[tokio::test]
    async fn immediate_supervisor_shutdown_skips_graceful_runtime_drain() {
        let dir = TempDir::new().expect("tempdir");
        let graceful_calls = Arc::new(AtomicUsize::new(0));
        let immediate_calls = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(ModeSelectingFactory {
            graceful_calls: Arc::clone(&graceful_calls),
            immediate_calls: Arc::clone(&immediate_calls),
        });
        let mut supervisor = ProjectLoopRuntimeSupervisor::start_with(
            dir.path().to_path_buf(),
            factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&supervisor, ProjectLoopRuntimeState::LeaderRunning).await;

        tokio::time::timeout(Duration::from_secs(1), supervisor.shutdown_immediately())
            .await
            .expect("immediate supervisor shutdown selected the graceful drain");

        assert_eq!(immediate_calls.load(Ordering::SeqCst), 1);
        assert_eq!(graceful_calls.load(Ordering::SeqCst), 0);
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
                shutdown_transferred_to_drain: false,
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
            context_service_config: deps.context_service_config.clone(),
        }
    }

    async fn call_hanging_worker_progress(
        server: &crate::worker_server::WorkerMcpServer,
        token: &str,
    ) {
        let config = StreamableHttpClientTransportConfig::with_uri(server.url()).auth_header(token);
        let client =
            ().serve(StreamableHttpClientTransport::from_config(config))
                .await
                .expect("rmcp client initialize");
        let mut request = CallToolRequestParams::new("report_progress");
        request.arguments = serde_json::json!({ "message": "hang forever" })
            .as_object()
            .cloned();
        let _ = client.call_tool(request).await;
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
    async fn runtime_shutdown_awaits_aborted_child() {
        let (_repo, deps) = runtime_deps_fixture().await;
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let delegation = tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("signal blocking child start");
            release_rx.recv().expect("release blocking child");
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocking child did not start");
        let (runtime, _system_id) = bare_runtime(&deps, Some(delegation));
        let shutdown = tokio::spawn(async move {
            let drain = Box::new(runtime).shutdown().await;
            drain.completion.await;
        });

        tokio::task::yield_now().await;
        tokio::time::advance(RUNTIME_SHUTDOWN_TIMEOUT + Duration::from_millis(100)).await;
        tokio::task::yield_now().await;
        assert!(
            !shutdown.is_finished(),
            "drain must retain and await a child that has not acknowledged abort"
        );
        release_tx.send(()).expect("release blocking child");
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("drain did not finish after child acknowledgement")
            .expect("shutdown task panicked");
    }

    #[tokio::test]
    async fn runtime_drain_force_aborts_and_acknowledges_callback_task() {
        struct DropAck(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for DropAck {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }

        let (_repo, deps) = runtime_deps_fixture().await;
        let (runtime, _) = bare_runtime(&deps, None);
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let callback = tokio::spawn(async move {
            let _ack = DropAck(Some(ack_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("callback task started");
        *runtime.server.root_handle.lock().unwrap() = Some(callback);

        let drain = Box::new(runtime).shutdown().await;
        drain.completion.await;

        tokio::time::timeout(Duration::from_secs(1), ack_rx)
            .await
            .expect("callback abort acknowledgement timed out")
            .expect("callback acknowledgement sender dropped");
    }

    #[tokio::test]
    async fn retirement_keeps_leadership_until_worker_server_acknowledges() {
        let (repo, deps) = runtime_deps_fixture().await;
        let (runtime, system_id) = bare_runtime(&deps, None);
        let funnel: Arc<dyn spur_mcp::McpEventSink> = Arc::new(deps.funnel.clone());
        let signal_sink = Arc::new(PermanentlyHangingSignalSink::default());
        let plan_resolver: Arc<dyn crate::handlers::PlanResolver> =
            Arc::clone(&runtime.server) as Arc<dyn crate::handlers::PlanResolver>;
        let worker_read_sink = Arc::new(crate::mcp::worker::WorkerReadMcpModule::new(
            crate::mcp::worker::WorkerReadMcpDeps {
                pm_service: Some(Arc::clone(&deps.pm_service)),
                feature_gate: Arc::clone(&deps.feature_gate),
                plan_resolver,
                reconciler_outcomes: runtime.server.reconciler_outcomes_handle(),
                outcome_store: Arc::clone(&deps.outcome_store),
                repo_root: Some(deps.repo_root.clone()),
            },
        ));
        let worker_server = crate::worker_server::WorkerMcpServer::start(
            system_id.to_string(),
            crate::worker_server::WorkerMcpDeps {
                pm_service: Arc::clone(&deps.pm_service),
                feature_gate: Arc::clone(&deps.feature_gate),
                funnel,
                worker_signal_sink: Arc::clone(&signal_sink)
                    as Arc<dyn crate::worker_server::WorkerSignalSink>,
                worker_read_sink,
                repo_root: Some(deps.repo_root.clone()),
            },
        )
        .await
        .expect("start hanging worker server");
        worker_server.register_delegation(
            "hung-reviewer".into(),
            crate::worker_server::DelegationContext {
                enable_worker_progress: true,
            },
        );
        let token = worker_server.issue_token("hung-reviewer", Duration::from_secs(60));
        deps.worker_mcp_servers
            .insert(system_id, Arc::clone(&worker_server));
        let call_server = Arc::clone(&worker_server);
        let call = tokio::spawn(async move {
            call_hanging_worker_progress(call_server.as_ref(), &token).await;
        });
        signal_sink.entered.notified().await;

        let (accept_started_tx, accept_started_rx) = std::sync::mpsc::sync_channel(1);
        let (accept_release_tx, accept_release_rx) = std::sync::mpsc::sync_channel(1);
        let accept_loop = tokio::task::spawn_blocking(move || {
            accept_started_tx.send(()).expect("signal accept start");
            let _ = accept_release_rx.recv();
        });
        let (flusher_started_tx, flusher_started_rx) = std::sync::mpsc::sync_channel(1);
        let (flusher_release_tx, flusher_release_rx) = std::sync::mpsc::sync_channel(1);
        let flusher = tokio::task::spawn_blocking(move || {
            flusher_started_tx.send(()).expect("signal flusher start");
            let _ = flusher_release_rx.recv();
        });
        accept_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("accept blocker did not start");
        flusher_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("flusher blocker did not start");
        let (old_accept, old_flusher) =
            worker_server.replace_background_handles_for_test(accept_loop, flusher);
        for handle in [old_accept, old_flusher].into_iter().flatten() {
            handle.abort();
            let _ = handle.await;
        }

        let leader_starts = Arc::new(AtomicUsize::new(0));
        let leader_factory = Arc::new(SingleRuntimeFactory {
            runtime: std::sync::Mutex::new(Some(runtime)),
            starts: Arc::clone(&leader_starts),
        });
        let standby_factory = Arc::new(CountingFactory::immediate());
        let mut leader = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            leader_factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&leader, ProjectLoopRuntimeState::LeaderRunning).await;
        let mut standby = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&standby_factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&standby, ProjectLoopRuntimeState::Standby).await;

        leader.shutdown().await;
        assert_eq!(leader.state(), ProjectLoopRuntimeState::LeaderDraining);
        tokio::time::sleep(RUNTIME_SHUTDOWN_TIMEOUT + Duration::from_millis(250)).await;
        let state_before_accept_ack = standby.state();
        accept_release_tx.send(()).expect("release accept blocker");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let state_before_flusher_ack = standby.state();
        flusher_release_tx
            .send(())
            .expect("release flusher blocker");

        tokio::time::timeout(Duration::from_secs(7), async {
            loop {
                if standby.state() == ProjectLoopRuntimeState::LeaderRunning {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("standby did not promote after hung handler force-abort");
        assert_eq!(state_before_accept_ack, ProjectLoopRuntimeState::Standby);
        assert_eq!(state_before_flusher_ack, ProjectLoopRuntimeState::Standby);
        assert_eq!(worker_server.active_count(), 0);
        assert_eq!(leader_starts.load(Ordering::SeqCst), 1);
        assert_eq!(standby_factory.starts.load(Ordering::SeqCst), 1);

        call.abort();
        let _ = call.await;
        standby.shutdown().await;
    }

    #[tokio::test]
    async fn zero_active_retirement_rejects_late_handler_and_holds_leadership_for_ack() {
        let (repo, deps) = runtime_deps_fixture().await;
        let (runtime, system_id) = bare_runtime(&deps, None);
        worker_fetcher(&deps, &runtime)
            .fetch_url_token(&system_id, "late-handler")
            .await
            .expect("start worker MCP server");
        let worker_server = Arc::clone(
            deps.worker_mcp_servers
                .get(&system_id)
                .expect("worker MCP server cached")
                .value(),
        );

        let (accept_started_tx, accept_started_rx) = std::sync::mpsc::sync_channel(1);
        let (accept_release_tx, accept_release_rx) = std::sync::mpsc::sync_channel(1);
        let accept_loop = tokio::task::spawn_blocking(move || {
            accept_started_tx.send(()).expect("signal accept start");
            let _ = accept_release_rx.recv();
        });
        let flusher = tokio::spawn(async {});
        accept_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("accept blocker did not start");
        let (old_accept, old_flusher) =
            worker_server.replace_background_handles_for_test(accept_loop, flusher);
        for handle in [old_accept, old_flusher].into_iter().flatten() {
            handle.abort();
            let _ = handle.await;
        }

        let leader_factory = Arc::new(SingleRuntimeFactory {
            runtime: std::sync::Mutex::new(Some(runtime)),
            starts: Arc::new(AtomicUsize::new(0)),
        });
        let standby_factory = Arc::new(CountingFactory::immediate());
        let mut leader = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            leader_factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&leader, ProjectLoopRuntimeState::LeaderRunning).await;
        let mut standby = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&standby_factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&standby, ProjectLoopRuntimeState::Standby).await;

        leader.shutdown().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !worker_server.accept_loop_handle_taken_for_test() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown did not pass the zero-active snapshot");
        assert_eq!(worker_server.active_count(), 0);

        // Model an already-accepted request entering the lifecycle wrapper
        // after shutdown observed zero but before the accept task acknowledged.
        let mut late_handler = worker_server.register_handler_lifecycle_for_test();
        let late_handler_aborted = late_handler
            .aborted_within(Duration::from_millis(100))
            .await;
        accept_release_tx.send(()).expect("release accept blocker");
        tokio::time::sleep(Duration::from_millis(250)).await;
        let leadership_held_for_ack = standby.state() == ProjectLoopRuntimeState::Standby;

        drop(late_handler);
        wait_for_state(&standby, ProjectLoopRuntimeState::LeaderRunning).await;
        standby.shutdown().await;

        assert!(
            late_handler_aborted && leadership_held_for_ack,
            "late_handler_aborted={late_handler_aborted}, leadership_held_for_ack={leadership_held_for_ack}"
        );
    }

    #[tokio::test]
    async fn standby_waits_for_leader_drain() {
        let repo = TempDir::new().unwrap();
        let drain_started = Arc::new(Notify::new());
        let drain_release = Arc::new(Notify::new());
        let first_starts = Arc::new(AtomicUsize::new(0));
        let first_factory = Arc::new(ControlledDrainFactory {
            starts: Arc::clone(&first_starts),
            drain_started: Arc::clone(&drain_started),
            drain_release: Arc::clone(&drain_release),
        });
        let second_factory = Arc::new(CountingFactory::immediate());
        let mut first = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            first_factory,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&first, ProjectLoopRuntimeState::LeaderRunning).await;
        let mut first_states = first.subscribe_state();
        let mut second = ProjectLoopRuntimeSupervisor::start_with(
            repo.path().to_path_buf(),
            Arc::clone(&second_factory) as Arc<dyn ProjectLoopRuntimeFactory>,
            filesystem_acquirer(),
            Duration::from_millis(10),
        );
        wait_for_state(&second, ProjectLoopRuntimeState::Standby).await;

        let first_shutdown = tokio::spawn(async move { first.shutdown().await });
        tokio::time::timeout(Duration::from_secs(1), drain_started.notified())
            .await
            .expect("leader did not enter drain");
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if *first_states.borrow() == ProjectLoopRuntimeState::LeaderDraining {
                    break;
                }
                first_states.changed().await.expect("first state channel");
            }
        })
        .await
        .expect("leader did not publish LeaderDraining");
        tokio::time::timeout(Duration::from_secs(1), first_shutdown)
            .await
            .expect("public shutdown must return while drain remains")
            .expect("leader shutdown task panicked");

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(second.state(), ProjectLoopRuntimeState::Standby);
        assert_eq!(second_factory.starts.load(Ordering::SeqCst), 0);

        drain_release.notify_one();
        wait_for_state(&second, ProjectLoopRuntimeState::LeaderRunning).await;
        assert_eq!(second_factory.starts.load(Ordering::SeqCst), 1);
        assert_eq!(first_starts.load(Ordering::SeqCst), 1);
        second.shutdown().await;
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
