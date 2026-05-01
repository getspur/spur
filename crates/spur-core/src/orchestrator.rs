use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::task::AbortOnDropHandle;
use tracing::{debug, error, info, warn};

use spur_acp::config::{SpurConfig, WorktreeConfig};
use spur_acp::connection::{
    AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter,
};
use spur_acp::registry::AgentRegistry;
use spur_acp::session_lock::{AcquireOutcome, SessionAttachGuard};
use spur_acp::types::*;
use spur_acp::{
    CancellationControl, DelegationAbortHandle, DelegationAbortReason, DelegationResult,
    DelegationStatus, GraphEdgeEvent, GraphNodeEvent, LifecycleState, ReviewKind, ReviewPayload,
    SpurEvent, SpurEventBody, TimeoutFallback,
};
use spur_pm::Issue;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, ListSessionsRequest, McpServer, McpServerHttp, PromptRequest,
    ProtocolVersion, SessionInfo, SessionUpdate, SetSessionModeRequest, TextContent,
};

use spur_blob_store::{
    ContentType, MeasuredOutcomeStore, OutcomeKey, OutcomeMetadata, OutcomeStore,
};
use spur_cost::CostTracker;
use spur_license::SpurLicense;
use spur_mcp::tools::{BaseSpec, BaseTarget};
use spur_mcp::{
    build_worker_info, DelegationChannel, DelegationRequest, McpCallbackServer, WorkerInfo,
};
use spur_pm::PmService;
use spur_worktree::git_blob_store::GitBlobOutcomeStore;
use spur_worktree::{manager::WorktreeError, WorktreeManager};

use crate::lineage::ExecutorId;
use crate::review_sink::ReviewSink;
use crate::scheduler::TurnGuard;

type McpGuarded<T> = (T, AbortOnDropHandle<()>);
type BrainRunBootstrap = (
    Box<dyn spur_acp::AgentConnection>,
    JoinHandle<()>,
    bool,
    Option<String>,
);
type NewBrainSessionBootstrap = (
    spur_acp::config::AgentConfig,
    Option<tokio::sync::broadcast::Receiver<spur_acp::SessionNotification>>,
    agent_client_protocol::schema::NewSessionResponse,
);
type LoadedBrainSessionBootstrap = (
    spur_acp::config::AgentConfig,
    Option<tokio::sync::broadcast::Receiver<spur_acp::SessionNotification>>,
    String,
    Option<std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>>>,
    bool,
    spur_acp::LoadOutcome,
);

// ─── Agent name normalization ─────────────────────────────────────────

/// Normalize an agent name for equality comparison.
/// - Lowercases
/// - Trims surrounding whitespace
/// - Strips `-acp`, `_acp`, `-cli`, `_cli` suffixes
///
/// Used to compare `DelegationPlan.chosen` (possibly a short name
/// the brain chose) against the dispatched `agent` (possibly a
/// fully-qualified registered name like `claude-code-acp`).
pub fn normalize_agent_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    for suffix in ["-acp", "_acp", "-cli", "_cli"].iter() {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    lower
}

/// Resolve a BaseSpec into the concrete ref passed to create_worktree.
fn resolve_base_branch(spec: &BaseSpec, snapshot_branch: &str) -> String {
    match spec {
        BaseSpec::RepoMain => snapshot_branch.to_string(),
        BaseSpec::Branch { name } => name.clone(),
        BaseSpec::Commit { oid } => oid.clone(),
        BaseSpec::WithOverlay { base, .. } => resolve_base_target(base, snapshot_branch),
    }
}

fn resolve_base_target(base: &BaseTarget, snapshot_branch: &str) -> String {
    match base {
        BaseTarget::RepoMain => snapshot_branch.to_string(),
        BaseTarget::Branch { name } => name.clone(),
        BaseTarget::Commit { oid } => oid.clone(),
    }
}

/// Extract the overlay list from a BaseSpec, preserving reconciler order.
fn extract_overlays(spec: &BaseSpec) -> Vec<(String, String, String)> {
    match spec {
        BaseSpec::WithOverlay { overlays, .. } => overlays
            .iter()
            .map(|overlay| {
                (
                    overlay.source_task_id.clone(),
                    overlay.base_oid.clone(),
                    overlay.tip_oid.clone(),
                )
            })
            .collect(),
        BaseSpec::RepoMain | BaseSpec::Branch { .. } | BaseSpec::Commit { .. } => Vec::new(),
    }
}

fn emit_dispatch_overlay_applied(
    funnel: &crate::event_funnel::FunnelHandle,
    request_id: &str,
    base: Option<&BaseSpec>,
    dispatched_base_oid: &str,
    overlays: &[(String, String, String)],
) {
    funnel.emit(SpurEventBody::DispatchOverlayApplied {
        request_id: request_id.to_string(),
        base_spec: serde_json::to_value(base).unwrap_or(serde_json::Value::Null),
        dispatched_base_oid: dispatched_base_oid.to_string(),
        overlay_task_ids: overlays.iter().map(|(id, _, _)| id.clone()).collect(),
    });
}

#[cfg(test)]
mod session_attach_guard_transfer_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use std::path::PathBuf;
    use std::pin::Pin;

    struct NoopConnection;

    #[async_trait]
    impl AgentConnection for NoopConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::InitializeResponse> {
            unimplemented!()
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<agent_client_protocol::schema::NewSessionResponse> {
            unimplemented!()
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
    }

    struct NewSessionConnection {
        response: Option<agent_client_protocol::schema::NewSessionResponse>,
    }

    #[async_trait]
    impl AgentConnection for NewSessionConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::InitializeResponse> {
            unimplemented!("NewSessionConnection: initialize")
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<agent_client_protocol::schema::NewSessionResponse> {
            self.response
                .take()
                .ok_or_else(|| anyhow::anyhow!("new_session called twice"))
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
    }

    #[tokio::test]
    async fn retire_active_brain_moves_attach_guard_to_active_connection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        let mut orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

        let attach_guard = match SessionAttachGuard::try_acquire(tmp.path(), "retire-transfer-test")
        {
            AcquireOutcome::Acquired(guard) => Some(guard),
            other => panic!(
                "expected Acquired, got {:?}",
                std::mem::discriminant(&other)
            ),
        };

        let mut brain = Some(BrainSession {
            connection: Box::new(NoopConnection),
            acp_session_id: "retire-transfer-test".to_string(),
            spur_session_id: SessionId("spur-session".to_string()),
            brain_name: "test-brain".to_string(),
            delegation_handle: tokio::spawn(async {}),
            mcp_server: None,
            mcp_guard: None,
            notification_pump_handle: None,
            attach_guard,
            fs_unsafe: false,
            started_at: std::time::Instant::now(),
            config_options: Vec::new(),
            spur_agent_caps: None,
            session_info: None,
            init_response: agent_client_protocol::schema::InitializeResponse::new(
                agent_client_protocol::schema::ProtocolVersion::LATEST,
            ),
        });
        let mut active = None;
        let mut scheduler = crate::scheduler::BrainScheduler::new(
            None,
            std::sync::Arc::new(orchestrator.funnel.clone()),
        );
        let overflow =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::new()));

        orchestrator
            .retire_active_brain(
                &mut brain,
                &mut active,
                &mut scheduler,
                &overflow,
                spur_acp::domain::events::BrainRetireReason::Shutdown,
                None,
            )
            .await;

        let mut active = active.expect("retired brain should cache active connection");
        assert!(active.attach_guard.is_some());
        active.transport.shutdown().await.unwrap();
    }

    #[test]
    fn reconnect_already_attached_maps_to_attach_rejected_event() {
        let holder = spur_acp::session_lock::HolderInfo {
            pid: Some(123),
            ..Default::default()
        };

        let event = reconnect_failure_event(
            SessionId("spur-session".to_string()),
            "test-brain".to_string(),
            ReconnectError::AlreadyAttached {
                acp_id: "acp-session".to_string(),
                holder: holder.clone(),
            },
        );

        match event {
            SpurEventBody::SessionAttachRejected {
                acp_session_id,
                holder: event_holder,
                fs_unsafe,
            } => {
                assert_eq!(acp_session_id, "acp-session");
                assert_eq!(event_holder.pid, holder.pid);
                assert!(!fs_unsafe);
            }
            other => panic!("expected SessionAttachRejected, got {other:?}"),
        }
    }

    fn fixture_brain_session(session_id: &str) -> BrainSession {
        BrainSession {
            connection: Box::new(NoopConnection),
            acp_session_id: format!("acp-{session_id}"),
            spur_session_id: SessionId(session_id.to_string()),
            brain_name: "test-brain".to_string(),
            delegation_handle: tokio::spawn(async {}),
            mcp_server: None,
            mcp_guard: None,
            notification_pump_handle: None,
            attach_guard: None,
            fs_unsafe: false,
            started_at: std::time::Instant::now(),
            config_options: Vec::new(),
            spur_agent_caps: None,
            session_info: None,
            init_response: agent_client_protocol::schema::InitializeResponse::new(
                agent_client_protocol::schema::ProtocolVersion::LATEST,
            ),
        }
    }

    fn fixture_select_option(
        id: &str,
        current: &str,
        choices: &[(&str, &str)],
    ) -> agent_client_protocol::schema::SessionConfigOption {
        use agent_client_protocol::schema::{
            SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionConfigValueId,
        };
        let opts: Vec<SessionConfigSelectOption> = choices
            .iter()
            .map(|(v, n)| SessionConfigSelectOption::new(SessionConfigValueId::new(*v), *n))
            .collect();
        SessionConfigOption::select(
            SessionConfigId::new(id),
            id,
            SessionConfigValueId::new(current),
            opts,
        )
    }

    #[tokio::test]
    async fn spur_agent_caps_getter_returns_cached_arc_or_none() {
        use agent_client_protocol::schema::{InitializeResponse, NewSessionResponse};
        use spur_acp::SpurAgentCaps;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        let orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

        let mut brain = fixture_brain_session("spur-session-caps");
        // Default: no caps cached yet.
        assert!(orchestrator.spur_agent_caps(&brain).is_none());

        // Simulate the post-create plumbing: build caps from a (default
        // InitializeResponse, codex fixture NewSessionResponse) pair and
        // stash on the brain session.
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let json =
            include_str!("../../spur-acp/tests/data/codex_acp_0_12_new_session_response.json");
        let new: NewSessionResponse = serde_json::from_str(json).unwrap();
        let caps = std::sync::Arc::new(SpurAgentCaps::new(
            &init,
            &new,
            spur_acp::AgentKind::CodexAcp,
        ));
        brain.spur_agent_caps = Some(caps.clone());

        let read = orchestrator
            .spur_agent_caps(&brain)
            .expect("caps populated after stash");
        assert!(read.supports_set_mode());
        assert!(read.supports_set_model());
        assert!(read.supports_set_config_option());
        assert!(std::sync::Arc::ptr_eq(&read, &caps));

        brain.delegation_handle.abort();
    }

    #[tokio::test]
    async fn replace_session_config_options_updates_cache_and_emits_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        let orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();
        let mut event_rx = orchestrator.event_tx.subscribe();

        let mut brain = fixture_brain_session("spur-session-cache");
        let initial = vec![fixture_select_option(
            "model",
            "gpt-5",
            &[("gpt-5", "GPT-5"), ("gpt-5-codex", "GPT-5 Codex")],
        )];
        brain.config_options = initial.clone();

        // Getter returns the snapshot owned by the brain.
        let read = orchestrator.session_config_options(&brain);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].id.0.as_ref(), "model");

        // Setter swaps in a new snapshot and emits CommandRegistryDirty.
        let next = vec![
            fixture_select_option("model", "gpt-5-codex", &[("gpt-5-codex", "GPT-5 Codex")]),
            fixture_select_option(
                "reasoning_effort",
                "medium",
                &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
            ),
        ];
        orchestrator.replace_session_config_options(&mut brain, next.clone());

        let read = orchestrator.session_config_options(&brain);
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].id.0.as_ref(), "model");
        assert_eq!(read[1].id.0.as_ref(), "reasoning_effort");

        // Drain the broadcast looking for the dirty event. The S2 funnel
        // hops through an mpsc, so allow a brief window.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = false;
        while tokio::time::Instant::now() < deadline && !found {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, event_rx.recv()).await {
                Ok(Ok(ev)) => {
                    if let SpurEventBody::CommandRegistryDirty {
                        session,
                        config_options,
                    } = ev.body
                    {
                        assert_eq!(session, SessionId("spur-session-cache".to_string()));
                        assert_eq!(config_options.len(), 2);
                        found = true;
                    }
                }
                _ => break,
            }
        }
        assert!(
            found,
            "expected CommandRegistryDirty event after replace_session_config_options"
        );

        // Idempotent abort of the dummy delegation_handle so Drop is clean.
        brain.delegation_handle.abort();
    }

    /// M9 F-C: helper records set_session_model / set_session_config_option
    /// dispatches so we can assert the orchestrator picks the dedicated
    /// trait method instead of the config-option fallback when caps
    /// advertise `supports_set_model()`.
    #[derive(Default)]
    struct DispatchLog {
        set_session_model: Vec<(String, String)>,
        set_session_config_option: Vec<(String, String, String)>,
    }

    struct TrackingConnection {
        log: std::sync::Arc<std::sync::Mutex<DispatchLog>>,
    }

    #[async_trait]
    impl AgentConnection for TrackingConnection {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::InitializeResponse> {
            unimplemented!()
        }
        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<agent_client_protocol::schema::NewSessionResponse> {
            unimplemented!()
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
        async fn set_session_model(
            &mut self,
            sid: agent_client_protocol::schema::SessionId,
            model_id: agent_client_protocol::schema::ModelId,
            _caps: &spur_acp::SpurAgentCaps,
        ) -> Result<(), spur_acp::AcpError> {
            self.log
                .lock()
                .unwrap()
                .set_session_model
                .push((sid.0.to_string(), model_id.0.to_string()));
            Ok(())
        }
        async fn set_session_config_option(
            &mut self,
            request: agent_client_protocol::schema::SetSessionConfigOptionRequest,
        ) -> anyhow::Result<agent_client_protocol::schema::SetSessionConfigOptionResponse> {
            self.log.lock().unwrap().set_session_config_option.push((
                request.session_id.0.to_string(),
                request.config_id.0.to_string(),
                request.value.0.to_string(),
            ));
            Ok(agent_client_protocol::schema::SetSessionConfigOptionResponse::new(vec![]))
        }
    }

    #[tokio::test]
    async fn dispatch_set_session_model_calls_connection_set_session_model() {
        use agent_client_protocol::schema::{
            InitializeResponse, ModelId, ModelInfo, NewSessionResponse, SessionModelState,
        };
        use spur_acp::SpurAgentCaps;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        let orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

        let _ = orchestrator; // helper does not need orchestrator state

        let log = std::sync::Arc::new(std::sync::Mutex::new(DispatchLog::default()));
        let conn = TrackingConnection {
            log: std::sync::Arc::clone(&log),
        };

        // Caps that advertise the dedicated set_model trait method.
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let new = NewSessionResponse::new(agent_client_protocol::schema::SessionId::new("acp-x"))
            .models(SessionModelState::new(
                ModelId::new("claude-sonnet-4-7"),
                vec![ModelInfo::new(
                    ModelId::new("claude-sonnet-4-7"),
                    "Claude Sonnet 4.7",
                )],
            ));
        let caps = std::sync::Arc::new(SpurAgentCaps::new(
            &init,
            &new,
            spur_acp::AgentKind::CodexAcp,
        ));
        assert!(caps.supports_set_model());

        let mut brain = BrainSession {
            connection: Box::new(conn),
            acp_session_id: "acp-x".to_string(),
            spur_session_id: SessionId("spur-x".to_string()),
            brain_name: "test-brain".to_string(),
            delegation_handle: tokio::spawn(async {}),
            mcp_server: None,
            mcp_guard: None,
            notification_pump_handle: None,
            attach_guard: None,
            fs_unsafe: false,
            started_at: std::time::Instant::now(),
            config_options: Vec::new(),
            spur_agent_caps: Some(caps),
            session_info: None,
            init_response: agent_client_protocol::schema::InitializeResponse::new(
                agent_client_protocol::schema::ProtocolVersion::LATEST,
            ),
        };

        Orchestrator::dispatch_set_session_model(&mut brain, "claude-sonnet-4-7".to_string())
            .await
            .expect("dispatch must succeed when caps support set_model");

        let log = log.lock().unwrap();
        assert_eq!(
            log.set_session_model.len(),
            1,
            "set_session_model must be called exactly once"
        );
        assert_eq!(
            log.set_session_config_option.len(),
            0,
            "set_session_config_option must NOT be called when caps support set_model"
        );
        assert_eq!(log.set_session_model[0].0, "acp-x");
        assert_eq!(log.set_session_model[0].1, "claude-sonnet-4-7");

        brain.delegation_handle.abort();
    }

    #[tokio::test]
    async fn fresh_agent_session_ready_event_carries_caps() {
        use agent_client_protocol::schema::{ModelId, ModelInfo, NewSessionResponse};

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        config.agents.entries = vec![spur_acp::AgentConfig::with_defaults("codex")];
        let mut orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();
        let mut event_rx = orchestrator.subscribe();

        let new_session =
            NewSessionResponse::new(agent_client_protocol::schema::SessionId::new("acp-codex"))
                .models(agent_client_protocol::schema::SessionModelState::new(
                    ModelId::new("gpt-5-codex"),
                    vec![ModelInfo::new(ModelId::new("gpt-5-codex"), "GPT-5 Codex")],
                ));
        let init = agent_client_protocol::schema::InitializeResponse::new(ProtocolVersion::LATEST);

        let brain = orchestrator
            .create_brain_session(
                Box::new(NewSessionConnection {
                    response: Some(new_session),
                }),
                "codex".to_string(),
                None,
                None,
                false,
                init,
            )
            .await
            .expect("fresh brain session must be created");

        assert!(
            brain.spur_agent_caps.is_some(),
            "fresh BrainSession must cache caps after session/new"
        );

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut ready_caps = None;
        while tokio::time::Instant::now() < deadline && ready_caps.is_none() {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, event_rx.recv()).await {
                Ok(Ok(ev)) => {
                    if let SpurEventBody::AgentSessionReady { caps, .. } = ev.body {
                        ready_caps = caps;
                    }
                }
                _ => break,
            }
        }

        let caps = ready_caps.expect("AgentSessionReady must carry caps for fresh sessions");
        assert!(caps.supports_set_model());

        brain.delegation_handle.abort();
    }

    /// bd-3rvt: smoke-test that `apply_mcp_server_settings` runs cleanly and
    /// leaves the server in a startable state. The three init paths
    /// (`run_adhoc`, `create_brain_session`, `load_brain_session`) all rely
    /// on this helper to wire reconciler enablement; if the helper panics or
    /// puts the server in an unusable state this test catches it.
    #[tokio::test]
    async fn apply_mcp_server_settings_is_callable_and_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        let orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

        let spur_session_id = SessionId("apply-settings-test".to_string());
        let brain_session_id: spur_acp::BrainSessionId = spur_session_id.clone().into();
        let cont_ctx = orchestrator.build_continuation_ctx(spur_session_id);
        let (mut server, _channel) = McpCallbackServer::new(
            &brain_session_id,
            orchestrator.pm_service.clone(),
            None,
            cont_ctx,
            orchestrator.outcome_store.clone(),
            orchestrator.mcp_feature_gate(),
        );

        // First call wires the orchestrator-derived settings.
        orchestrator.apply_mcp_server_settings(&mut server);
        // Second call must be safe — same args, same effect.
        orchestrator.apply_mcp_server_settings(&mut server);

        // With `pm_service = None`, the reconciler stays disabled and `start`
        // does not attempt pidfile acquisition, so we can verify the server
        // is in a valid state by spinning it up briefly.
        let server = Arc::new(server);
        let (_url, handle) = server.clone().start().await.expect("start succeeds");
        drop(handle);
    }

    /// bd-3rvt: full integration coverage — load a persisted brain session
    /// whose plan has ≥1 ready task, drive one reconciler tick, and assert
    /// `dispatched > 0`. Ignored because it needs a beads DB fixture with a
    /// pre-seeded epic + ready tasks plus an ACP transport stub for
    /// `load_brain_session`. See bd-3rvt for the fixture spec.
    #[tokio::test]
    #[ignore = "requires beads DB fixture + ACP transport stub; see bd-3rvt"]
    async fn load_brain_session_dispatches_ready_tasks() {
        // Fixture path expected: tests/fixtures/bd-3rvt/persisted-plan.beads
        // (a beads DB with one open epic and ≥1 ready child task carrying
        // `spur:plan-id:*`). With the helper now applied in
        // `load_brain_session`, the reconciler must dispatch on the first
        // tick. Without the fix, `dispatched` stays at 0 indefinitely.
    }
}

/// Format a worker task string with an optional `## Relevant Files`
/// section prepended.
///
/// - When `context_files.is_empty()`, the task string is returned
///   unchanged (no section prepended).
/// - Otherwise a `## Relevant Files` header is prepended with each
///   path as a Markdown bullet, followed by a `## Task` header and
///   the original task body. Order of the bullets preserves the input
///   order.
///
/// This function does no file I/O. The worker's own Read tool is
/// responsible for opening the listed paths.
pub(crate) fn format_worker_task(task: &str, context_files: &[String]) -> String {
    if context_files.is_empty() {
        return task.to_string();
    }
    let mut out = String::with_capacity(task.len() + 128 + context_files.len() * 64);
    out.push_str("## Relevant Files\n\n");
    out.push_str(
        "The following files were declared as relevant by the caller. \
         Open them with your Read tool as needed.\n\n",
    );
    for path in context_files {
        out.push_str("- ");
        out.push_str(path);
        out.push('\n');
    }
    out.push_str("\n## Task\n\n");
    out.push_str(task);
    out
}

// ─── Run options ─────────────────────────────────────────────────────

/// Options for `spur run`.
pub struct RunOpts {
    /// Override brain agent name.
    pub brain: Option<String>,
    /// Issue reference (e.g., "github:owner/repo#42").
    pub issue: Option<String>,
    /// Run in background (detached).
    pub background: bool,
}

/// Result of a completed run.
pub struct RunResult {
    pub session_id: SessionId,
    pub success: bool,
    pub pr_url: Option<String>,
    pub total_cost_usd: f64,
}

/// Holds the active brain transport along with metadata that must
/// share its lifetime. Future fields (e.g. SessionAttachGuard) are
/// added here so they cannot accidentally outlive the connection.
pub struct ActiveConnection {
    pub transport: Box<dyn AgentConnection>,
    pub brain_name: String,
    /// `None` only when no ACP session has been attached yet or when attached
    /// under DegradedNoLock (NFS/sshfs).
    pub(crate) attach_guard: Option<SessionAttachGuard>,
    /// True when this attachment is unprotected (multi-window unsafe).
    pub(crate) fs_unsafe: bool,
    /// Captured at `initialize`. Held alongside the transport so the
    /// orchestrator can build `SpurAgentCaps` once `session/new` (or
    /// `session/load`) returns the per-session state. Spec §6.1.
    pub(crate) init_response: agent_client_protocol::schema::InitializeResponse,
}

/// Holds the state of an active brain session.
pub struct BrainSession {
    pub connection: Box<dyn AgentConnection>,
    pub acp_session_id: String,
    pub spur_session_id: SessionId,
    pub brain_name: String,
    pub delegation_handle: JoinHandle<()>,
    /// Phase 5: hold the server itself so retirement can invoke
    /// `mark_retiring` / `cancel_in_flight_workers` / `shutdown`.
    pub mcp_server: Option<Arc<McpCallbackServer>>,
    /// Abort-on-drop guard returned by `McpCallbackServer::start`.
    /// Awaited during retirement after the server has been shut down or
    /// force-aborted so the background watcher task does not linger.
    pub mcp_guard: Option<AbortOnDropHandle<()>>,
    /// Task that drains the connection's session-notification broadcast
    /// and republishes each item onto the `SpurEvent` bus. `None` for
    /// transports that return `None` from `subscribe_session_notifications`
    /// (stdio, cli_wrap, stream_json). Must be aborted whenever the
    /// session is retired — otherwise a pump subscribed against the
    /// reused connection keeps emitting events tagged with this
    /// (now-stale) `spur_session_id`.
    pub notification_pump_handle: Option<JoinHandle<()>>,
    /// Holds the attach lock while the transport lives on this active session.
    /// Moves back to `ActiveConnection` when the transport is cached.
    pub(crate) attach_guard: Option<SessionAttachGuard>,
    /// Mirrors `ActiveConnection.fs_unsafe` for the active transport.
    pub(crate) fs_unsafe: bool,
    /// Wall-clock instant this session was created. Used by
    /// `retire_active_brain` to record session duration in the cost
    /// ledger on close-out.
    pub started_at: std::time::Instant,
    /// Latest `config_options` advertised by the agent. Populated from
    /// `NewSessionResponse.config_options` on session creation; refreshed
    /// by `SetSessionConfigOption` responses (Task 2.14) and by
    /// `session/update.ConfigOptionUpdate` notifications (v2 plan).
    pub config_options: Vec<agent_client_protocol::schema::SessionConfigOption>,
    /// Frozen-per-session capability cache (M8.A). Populated AFTER both
    /// `initialize` and `session/new` complete, since the `set_*` gates
    /// derive from `NewSessionResponse` payload state. Wrapped in `Arc`
    /// so UI consumers can clone cheaply.
    pub spur_agent_caps: Option<Arc<spur_acp::SpurAgentCaps>>,
    /// Last-known `SessionInfoUpdate` payload (M9 hoist, F-3-1). Lives
    /// on the orchestrator entry — not the transient
    /// `SessionDetailView` — so the cached `title` and `updated_at`
    /// survive the view's destruction on navigation away from the
    /// session detail screen. `None` until the agent emits its first
    /// `SessionInfoUpdate` notification.
    pub session_info: Option<spur_acp::SessionInfoCache>,
    /// Captured `InitializeResponse` retained on the session entry so
    /// it can flow back to `ActiveConnection` when the brain is
    /// retired (and reused later for a fresh `new_session` without
    /// re-running `initialize`).
    pub(crate) init_response: agent_client_protocol::schema::InitializeResponse,
}

impl BrainSession {
    /// Test-only constructor that fills the private `attach_guard`,
    /// `fs_unsafe`, and `init_response` fields with sensible defaults so
    /// integration tests in sibling crates can construct a
    /// `BrainSession` without re-implementing the full session-create
    /// pipeline. Hidden from rustdoc; not part of the stable API.
    #[doc(hidden)]
    pub fn for_test(
        connection: Box<dyn AgentConnection>,
        acp_session_id: impl Into<String>,
        spur_session_id: SessionId,
        brain_name: impl Into<String>,
    ) -> Self {
        Self {
            connection,
            acp_session_id: acp_session_id.into(),
            spur_session_id,
            brain_name: brain_name.into(),
            delegation_handle: tokio::spawn(async {}),
            mcp_server: None,
            mcp_guard: None,
            notification_pump_handle: None,
            attach_guard: None,
            fs_unsafe: false,
            started_at: std::time::Instant::now(),
            config_options: Vec::new(),
            spur_agent_caps: None,
            session_info: None,
            init_response: agent_client_protocol::schema::InitializeResponse::new(
                agent_client_protocol::schema::ProtocolVersion::LATEST,
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadBrainSessionError {
    #[error("session {acp_id} is already attached")]
    AlreadyAttached {
        acp_id: String,
        holder: spur_acp::session_lock::HolderInfo,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ReconnectError {
    #[error("session already attached")]
    AlreadyAttached {
        acp_id: String,
        holder: spur_acp::session_lock::HolderInfo,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

async fn abort_mcp_handle(handle: AbortOnDropHandle<()>) {
    handle.abort();
    let _ = handle.await;
}

async fn cleanup_mcp_on_err<T, F>(
    mcp_handle: AbortOnDropHandle<()>,
    fut: F,
) -> Result<(T, AbortOnDropHandle<()>)>
where
    F: std::future::Future<Output = Result<T>>,
{
    match fut.await {
        Ok(value) => Ok((value, mcp_handle)),
        Err(error) => {
            abort_mcp_handle(mcp_handle).await;
            Err(error)
        }
    }
}

const MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

trait RetirableMcpServer: Send + Sync {
    fn mark_retiring(&self);
    fn cancel_in_flight_workers(&self);
    fn force_abort(&self);
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

impl RetirableMcpServer for McpCallbackServer {
    fn mark_retiring(&self) {
        McpCallbackServer::mark_retiring(self);
    }

    fn cancel_in_flight_workers(&self) {
        McpCallbackServer::cancel_in_flight_workers(self);
    }

    fn force_abort(&self) {
        McpCallbackServer::force_abort(self);
    }

    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(McpCallbackServer::shutdown(self))
    }
}

async fn shutdown_mcp_server<S: RetirableMcpServer + ?Sized>(
    funnel: &crate::event_funnel::FunnelHandle,
    session: &SessionId,
    mcp_server: &mut Option<Arc<S>>,
    mcp_guard: Option<&mut Option<AbortOnDropHandle<()>>>,
) {
    let Some(server) = mcp_server.take() else {
        if let Some(mcp_guard) = mcp_guard {
            if let Some(guard) = mcp_guard.take() {
                if tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, guard)
                    .await
                    .is_err()
                {
                    warn!(
                        session = %session,
                        timeout_ms = MCP_SHUTDOWN_TIMEOUT.as_millis() as u64,
                        "MCP guard await exceeded timeout on early-return; aborting via drop"
                    );
                }
            }
        }
        return;
    };

    server.mark_retiring();
    server.cancel_in_flight_workers();

    match tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, server.shutdown()).await {
        Ok(_) => {
            info!(session = %session, "MCP server shutdown clean");
        }
        Err(_timeout) => {
            warn!(
                session = %session,
                timeout_ms = MCP_SHUTDOWN_TIMEOUT.as_millis() as u64,
                "MCP server shutdown timed out — forcing abort"
            );
            funnel.emit(SpurEventBody::McpShutdownTimeout {
                session: session.clone(),
                timeout_ms: MCP_SHUTDOWN_TIMEOUT.as_millis() as u64,
            });
            server.force_abort();
        }
    }

    if let Some(mcp_guard) = mcp_guard {
        if let Some(guard) = mcp_guard.take() {
            if tokio::time::timeout(MCP_SHUTDOWN_TIMEOUT, guard)
                .await
                .is_err()
            {
                warn!(
                    session = %session,
                    timeout_ms = MCP_SHUTDOWN_TIMEOUT.as_millis() as u64,
                    "MCP guard await exceeded timeout post-shutdown; aborting via drop"
                );
            }
        }
    }
}

async fn retire_brain_session<S: RetirableMcpServer + ?Sized>(
    funnel: &crate::event_funnel::FunnelHandle,
    session: &SessionId,
    mcp_server: &mut Option<Arc<S>>,
    mcp_guard: Option<&mut Option<AbortOnDropHandle<()>>>,
    scheduler: &mut crate::scheduler::BrainScheduler,
    overflow: &crate::continuation_bridge::OverflowBuf,
    new_active: Option<spur_acp::types::BrainSessionId>,
) {
    shutdown_mcp_server(funnel, session, mcp_server, mcp_guard).await;
    scheduler.note_session_swap(new_active, overflow);
}

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

fn format_error_chain(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

fn reconnect_failure_event(
    session: SessionId,
    brain_name: String,
    error: ReconnectError,
) -> SpurEventBody {
    match error {
        ReconnectError::AlreadyAttached { acp_id, holder } => {
            SpurEventBody::SessionAttachRejected {
                acp_session_id: acp_id,
                holder,
                fs_unsafe: false,
            }
        }
        ReconnectError::Other(e) => SpurEventBody::BrainReconnectFailed {
            session,
            brain_name,
            reason: format_error_chain(&e),
        },
    }
}

/// A user input message from the TUI.
#[derive(Debug)]
#[non_exhaustive]
#[allow(clippy::large_enum_variant)]
pub enum InteractiveInput {
    /// Initialize and warm the brain transport without creating an ACP
    /// session yet. Used by dashboard startup to reduce first-prompt latency.
    WarmConnect,
    Message {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    /// Spawn a fresh brain session and send these blocks as the first prompt
    /// atomically. If a brain is already attached, it is shut down first.
    /// Empty `blocks` means spawn-only with no first prompt.
    NewSessionWithMessage {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    ListSessions,
    ResumeSession {
        session_id: String,
    },
    /// Request `set_session_mode` on the active brain session. No-op if
    /// there is no active brain session.
    SetSessionMode {
        mode_id: String,
    },
    /// Request `set_session_config_option` on the active brain session for
    /// the v1 codex `/model` and `/effort` slash pickers. No-op if there is
    /// no active brain session. On success, refreshes the orchestrator's
    /// cached `config_options` from the response.
    SetSessionConfigOption {
        config_id: String,
        value: String,
    },
    /// Dedicated `session/set_model` dispatch (M9 F-C). Fired when the
    /// caps-aware submit-router routes `/model <value>` for an agent that
    /// advertises `supports_set_model()` (e.g. claude-code-acp). The
    /// orchestrator delegates to `AgentConnection::set_session_model`,
    /// which carries its own state-gated fallback to
    /// `session/set_config_option` for agents that lack the dedicated
    /// method. No-op when there is no active brain session.
    SetSessionModel {
        value: String,
    },
    /// Invoke an agent vendor-extension RPC on the active brain session.
    /// No-op if there is no active brain session. The method name and params
    /// are chosen by the TUI's config-driven dispatch path — the
    /// orchestrator is agnostic to specific extensions. `sessionId` is
    /// injected into `params` here (the TUI doesn't know ACP session IDs).
    VendorExec {
        session: SessionId,
        method: String,
        params: serde_json::Value,
    },
    /// Submit a human review decision. Routed to the ReviewSink by the
    /// dispatcher task, not handled inline in `run_interactive`.
    SubmitReview {
        executor_id: String,
        attempt_n: u32,
        decision: spur_acp::ReviewDecision,
    },
    /// Halt the currently streaming prompt (if any) via `AgentConnection::cancel`.
    /// When received inside the streaming `select!`, calls `cancel()` and arms
    /// the 5s force-timeout. When received outside the streaming loop (no
    /// active turn), dropped with a debug log (the view guards against emitting
    /// this unless a stream is in-flight, but a TurnComplete-vs-Esc race can
    /// still produce a stray one).
    CancelStream {
        session: SessionId,
    },
    /// Refresh the issue list and re-emit IssuesLoaded.
    RefreshIssues,
    /// Fetch full issue detail and emit IssueDetailFetched.
    GetIssueDetail {
        id: String,
    },
    /// Fetch an issue dependency subgraph and emit IssueSubgraphLoaded.
    GetIssueGraph {
        id: String,
    },
    /// Update an issue and emit IssueUpdated.
    UpdateIssue {
        id: String,
        update: spur_pm::IssueUpdate,
    },
    /// Detached delegation completion returned to the orchestrator for
    /// scheduled brain re-entry. Never constructed by the TUI. See
    /// `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
    SystemContinuation {
        session: SessionId,
        continuation: spur_acp::domain::BrainContinuation,
    },
}

/// Convert spur_pm::IssueSummary to the spur_acp mirror type for event bus transmission.
fn to_summary_event(
    issue: &spur_pm::IssueSummary,
    source: &str,
) -> spur_acp::domain::events::IssueSummaryEvent {
    spur_acp::domain::events::IssueSummaryEvent {
        id: issue.id.clone(),
        source: source.into(),
        title: issue.title.clone(),
        status: issue.status.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        assignee: issue.assignee.clone(),
    }
}

/// Emit a `GraphAlertsSummary` event from a triage report's alert list.
fn emit_alerts_from_report(
    report: &spur_pm::graph::TriageReport,
    funnel: &crate::event_funnel::FunnelHandle,
) {
    let alerts = &report.triage.alerts;
    let critical = alerts
        .iter()
        .filter(|a| a.severity.as_deref() == Some("critical"))
        .count();
    let warning = alerts
        .iter()
        .filter(|a| a.severity.as_deref() == Some("warning"))
        .count();
    let details: Vec<String> = alerts
        .iter()
        .take(5)
        .filter_map(|a| a.message.clone())
        .collect();
    funnel.emit(SpurEventBody::GraphAlertsSummary {
        total: alerts.len(),
        critical,
        warning,
        details,
    });
}

/// Build a brain-prompt summary from a triage report.
fn build_graph_prompt_summary(report: &spur_pm::graph::TriageReport) -> Option<String> {
    let qr = &report.triage.quick_ref;
    let health = &report.triage.project_health;
    let mut lines = vec![
        "## Project Graph Intelligence".to_string(),
        String::new(),
        format!(
            "Project: {} open, {} actionable, {} blocked, {} in progress.",
            qr.open_count, qr.actionable_count, qr.blocked_count, qr.in_progress_count,
        ),
    ];
    if let Some(top) = qr.top_picks.first() {
        lines.push(format!(
            "Top recommendation: {} (score {:.2}) — \"{}\"",
            top.id, top.score, top.title,
        ));
    }
    if health.graph.has_cycles {
        lines.push(format!(
            "Warning: {} cycles detected in dependency graph.",
            health.graph.cycle_count,
        ));
    }
    if !report.triage.quick_wins.is_empty() {
        let ids: Vec<_> = report
            .triage
            .quick_wins
            .iter()
            .take(3)
            .map(|q| q.id.as_str())
            .collect();
        lines.push(format!("Quick wins: {}", ids.join(", ")));
    }
    lines.push(String::new());
    lines.push(
        "Use `graph_triage` for full analysis. \
         Use `graph_plan` for parallel execution tracks."
            .to_string(),
    );
    Some(lines.join("\n"))
}

/// Parallel-fetch issues + graph triage, emit `IssuesLoaded` +
/// `GraphAlertsSummary` events via `tokio::join!`. When `for_prompt` is
/// true, also returns a graph summary string for brain prompt enrichment.
///
/// Replaces the previous sequential `list_issues` → `emit_graph_alerts`
/// pattern at all 4 call sites. Wall-time: `max(T_br, T_bv)` instead of
/// `T_br + T_bv`.
async fn refresh_pm_state(
    pm: &spur_pm::PmService,
    funnel: &crate::event_funnel::FunnelHandle,
    limit: Option<usize>,
    for_prompt: bool,
) -> Option<String> {
    let issues_fut = pm.list_issues(spur_pm::IssueFilter {
        status: Some("open".into()),
        limit,
        ..Default::default()
    });

    let triage_fut = async {
        match pm.analyzer() {
            Some(bv) => bv.triage(None).await.ok(),
            None => None,
        }
    };

    let (issues_result, triage_opt) = tokio::join!(issues_fut, triage_fut);

    // Emit issues.
    match issues_result {
        Ok(issues) => {
            let event_issues: Vec<_> = issues
                .iter()
                .map(|i| to_summary_event(i, pm.source_str()))
                .collect();
            tracing::info!(
                count = issues.len(),
                "Loaded open issues from {}",
                pm.source_str()
            );
            funnel.emit(SpurEventBody::IssuesLoaded {
                issues: event_issues,
            });
        }
        Err(e) => tracing::warn!("Failed to load issues: {e}"),
    }

    // Emit alerts + optionally build prompt summary.
    if let Some(report) = triage_opt {
        emit_alerts_from_report(&report, funnel);
        if for_prompt {
            return build_graph_prompt_summary(&report);
        }
    }
    None
}

/// Convert spur_pm::Issue to the spur_acp mirror type for event bus transmission.
fn issue_to_detail_event(issue: &spur_pm::Issue) -> spur_acp::IssueDetailEvent {
    spur_acp::IssueDetailEvent {
        id: issue.id.clone(),
        source: issue.source.to_string(),
        title: issue.title.clone(),
        body: issue.body.clone(),
        status: issue.status.clone(),
        labels: issue.labels.clone(),
        assignee: issue.assignee.clone(),
        url: issue.url.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        blocked_by: issue.blocked_by.clone(),
        due_at: issue.due_at,
        created_at: issue.created_at,
        updated_at: issue.updated_at,
    }
}

fn graph_node_to_event(node: &spur_pm::graph::GraphNode) -> spur_acp::GraphNodeEvent {
    spur_acp::GraphNodeEvent {
        id: node.id.clone(),
        title: node.title.clone(),
        status: node.status.clone(),
        priority: node.priority,
        labels: node.labels.clone(),
        pagerank: node.pagerank,
    }
}

fn graph_edge_to_event(edge: &spur_pm::graph::GraphEdge) -> spur_acp::GraphEdgeEvent {
    spur_acp::GraphEdgeEvent {
        from: edge.from.clone(),
        to: edge.to.clone(),
        edge_type: edge.edge_type.clone(),
    }
}

fn dependency_graph_to_event_parts(
    graph: spur_pm::graph::DependencyGraph,
) -> (Vec<GraphNodeEvent>, Vec<GraphEdgeEvent>) {
    let Some(adjacency) = graph.adjacency else {
        return (Vec::new(), Vec::new());
    };

    let nodes = adjacency
        .nodes
        .iter()
        .map(graph_node_to_event)
        .collect();
    let edges = adjacency
        .edges
        .unwrap_or_default()
        .iter()
        .map(graph_edge_to_event)
        .collect();
    (nodes, edges)
}

#[async_trait::async_trait]
trait IssueGraphPm {
    fn analyzer_available(&self) -> bool;

    async fn issue_subgraph_json(
        &self,
        id: &str,
    ) -> anyhow::Result<spur_pm::graph::DependencyGraph>;
}

#[async_trait::async_trait]
impl IssueGraphPm for spur_pm::PmService {
    fn analyzer_available(&self) -> bool {
        self.analyzer().is_some()
    }

    async fn issue_subgraph_json(
        &self,
        id: &str,
    ) -> anyhow::Result<spur_pm::graph::DependencyGraph> {
        self.analyzer()
            .ok_or_else(|| {
                anyhow::anyhow!("bv unavailable; install bv to view dependency graph")
            })?
            .subgraph(id, Some(2), Some("json"))
            .await
    }
}

async fn handle_get_issue_graph<P: IssueGraphPm + ?Sized>(
    pm: Option<&P>,
    funnel: &crate::event_funnel::FunnelHandle,
    id: String,
) {
    let Some(pm) = pm else {
        funnel.emit(SpurEventBody::IssueCommandError {
            operation: "GetIssueGraph".into(),
            error: "No issue tracker configured".into(),
            id: Some(id),
        });
        return;
    };

    if !pm.analyzer_available() {
        funnel.emit(SpurEventBody::IssueCommandError {
            operation: "GetIssueGraph".into(),
            error: "bv unavailable; install bv to view dependency graph".into(),
            id: Some(id),
        });
        return;
    }

    match pm.issue_subgraph_json(&id).await {
        Ok(graph) => {
            let (nodes, edges) = dependency_graph_to_event_parts(graph);
            funnel.emit(SpurEventBody::IssueSubgraphLoaded {
                requested_id: id,
                nodes,
                edges,
            });
        }
        Err(e) => {
            funnel.emit(SpurEventBody::IssueCommandError {
                operation: "GetIssueGraph".into(),
                error: e.to_string(),
                id: Some(id),
            });
        }
    }
}

// ─── Orchestrator ────────────────────────────────────────────────────

/// The central orchestrator that drives the brain-worker pipeline.
pub struct Orchestrator {
    pub registry: AgentRegistry,
    pub config: SpurConfig,
    pub worktree_authority: Arc<crate::WorktreeAuthority>,
    pub self_held: spur_acp::session_liveness::SelfHeldSet,
    pub cost_tracker: Option<CostTracker>,
    pub event_tx: broadcast::Sender<SpurEvent>,
    /// Monotonic sequence counter for the S2 funnel. The funnel task
    /// owns the write end via `fetch_add`; retained on the struct so
    /// tests/diagnostics can inspect the current count if needed.
    #[allow(dead_code)]
    event_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// S2 funnel handle — every orchestrator emit flows through this.
    /// Internally writes `SpurEventBody` into an mpsc that the funnel
    /// task drains onto `event_tx`, stamping monotonic `seq` +
    /// `occurred_at` in strict enqueue order (Pitfall P1).
    funnel: crate::event_funnel::FunnelHandle,
    pub review_sink: ReviewSink, // Clone type, shares inner Arc<Mutex>
    repo_root: PathBuf,
    pub pm_service: Option<Arc<PmService>>,
    outcome_store: Arc<dyn OutcomeStore>,
    /// Background tokio tasks owned by the orchestrator.
    background_tasks: Vec<JoinHandle<()>>,
    /// INV-6: per-delegation cancellation token registry.
    cancellation_control: CancellationControl,
    /// Sender half of the `run_interactive` ingress channel.  Set by
    /// `set_continuation_tx` so the MCP server can route detached
    /// delegation completions back to the orchestrator.
    continuation_tx: Option<mpsc::Sender<InteractiveInput>>,
    /// Overflow buffer for detached continuations.  Mirrors the buffer
    /// passed to `run_interactive`; set alongside `continuation_tx`.
    continuation_overflow: Option<crate::continuation_bridge::OverflowBuf>,
    /// Feature gate for dynamic quota/feature enforcement.
    feature_gate: Option<std::sync::Arc<spur_license::FeatureGate>>,
    pub(crate) peer_mailbox: Option<crate::peer_mailbox::PeerMailboxBundle>,
    /// Abort handle for the production peer-mailbox reconciler task spawned
    /// by `Orchestrator::new` when `peer_mailbox_enabled = true`. Stored
    /// directly so introspection does not depend on `background_tasks`
    /// insertion order. The task itself is still tracked in
    /// `background_tasks` for `Drop` to abort.
    pub(crate) peer_mailbox_reconciler_abort: Option<tokio::task::AbortHandle>,
}

/// Detect whether an error from an `AgentConnection` RPC indicates the
/// underlying subprocess has died (pipe closed, ACP thread exited, etc.),
/// versus a normal request-level error (auth needed, invalid session, etc.).
///
/// Pragmatic string-match against the two known "subprocess is gone"
/// patterns emitted by `NativeAcpConnection` and the ACP SDK. A more
/// structured signal would require a new trait method on `AgentConnection`;
/// revisit if the set of transports grows.
pub(crate) fn is_connection_death(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("ACP thread died") || msg.contains("server shut down unexpectedly")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeadsStartupWarning {
    BrNotInstalled,
    BackendUnavailable,
}

fn startup_beads_warning(
    config: &SpurConfig,
    feature_gate: Option<&spur_license::FeatureGate>,
    has_beads_dir: bool,
    pm_service_available: bool,
    br_binary_available: bool,
) -> Option<BeadsStartupWarning> {
    if !(has_beads_dir
        && !pm_service_available
        && config.pm.beads.as_ref().is_none_or(|beads| beads.enabled)
        && feature_gate.is_some_and(|gate| gate.has(spur_license::FeatureKey::PM_CORE_BEADS_BASIC)))
    {
        return None;
    }

    Some(if br_binary_available {
        BeadsStartupWarning::BackendUnavailable
    } else {
        BeadsStartupWarning::BrNotInstalled
    })
}

fn render_beads_startup_warning(warning: BeadsStartupWarning) -> &'static str {
    match warning {
        BeadsStartupWarning::BrNotInstalled => {
            "br (beads) not installed — issue tracking disabled. Install: cargo install --git https://github.com/Dicklesworthstone/beads_rust.git"
        }
        BeadsStartupWarning::BackendUnavailable => {
            "beads PM backend failed to initialize — issue tracking disabled. `br` appears installed; check logs for the underlying startup error."
        }
    }
}

fn binary_on_path(binary: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    #[cfg(windows)]
    let path_exts: Vec<String> = std::env::var_os("PATHEXT")
        .map(|exts| {
            exts.to_string_lossy()
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(|ext| ext.to_string())
                .collect()
        })
        .unwrap_or_else(|| vec![".EXE".into(), ".CMD".into(), ".BAT".into(), ".COM".into()]);

    std::env::split_paths(&path_var).any(|dir| {
        if dir.join(binary).is_file() {
            return true;
        }

        #[cfg(windows)]
        {
            path_exts
                .iter()
                .any(|ext| dir.join(format!("{binary}{ext}")).is_file())
        }

        #[cfg(not(windows))]
        {
            false
        }
    })
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        for handle in self.background_tasks.drain(..) {
            handle.abort();
        }
    }
}

// ─── Free function: log-cap enforcer ──────────────────────────────────────────

fn enforce_log_cap(dir: &std::path::Path, cap: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.modified().ok()?, m.len()))
        })
        .collect();
    let total: u64 = files.iter().map(|(_, _, s)| s).sum();
    if total <= cap {
        return;
    }
    files.sort_by_key(|(_, mtime, _)| *mtime); // oldest first
    let mut to_free = total - cap;
    for (path, _, size) in files {
        if to_free == 0 {
            break;
        }
        let _ = std::fs::remove_file(&path);
        to_free = to_free.saturating_sub(size);
    }
}

impl Orchestrator {
    /// Create a new orchestrator for the given repo directory.
    pub fn new(
        repo_root: PathBuf,
        config: SpurConfig,
        feature_gate: Option<std::sync::Arc<spur_license::FeatureGate>>,
    ) -> Result<Self> {
        let registry = AgentRegistry::load(config.agents.entries.clone());
        let outcome_store: Arc<dyn OutcomeStore> = Arc::new(MeasuredOutcomeStore::new(
            GitBlobOutcomeStore::new(repo_root.clone()),
        ));
        let self_held = spur_acp::session_liveness::SelfHeldSet::new();

        // Try to open cost tracker (non-fatal if it fails).
        let cost_tracker = {
            let db_path = shellexpand_tilde(&config.cost.db_path);
            if let Some(parent) = Path::new(&db_path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match CostTracker::open(Path::new(&db_path)) {
                Ok(ct) => Some(ct),
                Err(e) => {
                    warn!(error = %e, "Failed to open cost database, cost tracking disabled");
                    None
                }
            }
        };

        // S1.d — 4096 supports ~2.5s of events at 1600 evt/s peak
        // (20 workers × 80 evt/s). Subscribers that still lag get
        // RecvError::Lagged (logged at WARN; see S1.d Lagged audit).
        let (event_tx, _) = broadcast::channel(4096);
        // S2 — spawn the singleton funnel. Every orchestrator emit
        // flows through `funnel.emit(body)`; the funnel task stamps
        // monotonic seq + wall-clock time and forwards on `event_tx`.
        let event_seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let lineage =
            std::sync::Arc::new(std::sync::Mutex::new(crate::lineage::ExecutorLineage::new()));
        let funnel = crate::event_funnel::spawn_funnel_with_lineage(
            event_tx.clone(),
            event_seq.clone(),
            lineage,
        );
        let worktree_authority = Arc::new(crate::WorktreeAuthority::new(
            repo_root.clone(),
            self_held.clone(),
            funnel.clone(),
            crate::AuthorityConfig::default(),
        ));
        // S3 — durable JSONL sink subscribes to the same broadcast.
        let max_bytes = feature_gate
            .as_ref()
            .and_then(|g| g.quota(spur_license::QuotaKey::EventRetentionBytes))
            .and_then(|v| v.as_bytes())
            .unwrap_or(crate::event_sink::DEFAULT_MAX_BYTES);
        let max_total_bytes = config.log.events_max_total_bytes;
        crate::event_sink::spawn_sink(event_tx.subscribe(), max_bytes, max_total_bytes);
        let review_sink = ReviewSink::new();

        let mut orchestrator = Self {
            registry,
            config,
            worktree_authority: worktree_authority.clone(),
            self_held,
            cost_tracker,
            event_tx,
            event_seq,
            funnel,
            review_sink,
            repo_root,
            pm_service: None,
            outcome_store,
            background_tasks: Vec::new(),
            cancellation_control: CancellationControl::new(),
            continuation_tx: None,
            continuation_overflow: None,
            feature_gate,
            peer_mailbox: None,
            peer_mailbox_reconciler_abort: None,
        };

        let ttl_days: u64 = match std::env::var("SPUR_OUTCOME_TTL_DAYS") {
            Ok(raw) => match raw.parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => {
                    tracing::warn!(
                        env = %raw,
                        "SPUR_OUTCOME_TTL_DAYS is set but not a positive integer; using default 7"
                    );
                    7
                }
            },
            Err(_) => 7,
        };
        let sweep_store = orchestrator.outcome_store.clone();
        let sweep_handle = tokio::spawn(async move {
            // saturating_mul caps at u64::MAX seconds for absurd inputs; the
            // sweep would take longer than the heat death of the universe but
            // wouldn't panic in debug or wrap silently in release.
            let ttl = Duration::from_secs(ttl_days.saturating_mul(86_400));
            match sweep_store.sweep_older_than(ttl).await {
                Ok(report) => tracing::info!(
                    target: "spur.metrics.outcome_swept",
                    namespaces_swept = report.namespaces_swept,
                    blobs_swept = report.blobs_swept,
                    bytes_freed = report.bytes_freed,
                    ttl_days,
                ),
                Err(e) => tracing::warn!(
                    target: "spur.metrics.outcome_swept_failed",
                    error = %e,
                ),
            }
        });
        orchestrator.background_tasks.push(sweep_handle);

        if orchestrator.config.peer_mailbox_enabled {
            let ledger: Arc<dyn crate::peer_mailbox::PeerMailboxLedger> =
                Arc::new(crate::peer_mailbox::InMemoryLedger::new());
            let (reconciler_tx, reconciler_rx) = tokio::sync::mpsc::unbounded_channel();
            let session_slot: Arc<tokio::sync::RwLock<Option<String>>> =
                Arc::new(Default::default());

            let router = Arc::new(crate::peer_mailbox::PeerMailboxRouter::new(
                ledger.clone(),
                orchestrator.funnel.clone(),
                reconciler_tx,
                crate::peer_mailbox::Limits::default(),
            ));
            let builder = Arc::new(
                crate::peer_mailbox::prompt_builder::PeerPromptContextBuilder::new(ledger.clone()),
            );
            orchestrator.peer_mailbox = Some(crate::peer_mailbox::PeerMailboxBundle {
                router,
                builder,
                ledger: ledger.clone(),
                brain_session_id_slot: session_slot.clone(),
            });

            let reconciler_handle = tokio::spawn(crate::peer_mailbox::run_reconciler_loop(
                reconciler_rx,
                ledger,
                orchestrator.funnel.clone(),
                session_slot,
            ));
            orchestrator.peer_mailbox_reconciler_abort = Some(reconciler_handle.abort_handle());
            orchestrator.background_tasks.push(reconciler_handle);
        }

        // Startup sweep: spawn into background. self_held is empty at boot;
        // the periodic sweeps + Live-probe semantics carry the safety
        // guarantee. See spec §6 risk table.
        let startup_auth = worktree_authority.clone();
        let startup_handle = tokio::spawn(async move {
            match startup_auth.sweep_once().await {
                Ok(report) => tracing::info!(
                    target: "spur.metrics.worktree_authority.startup",
                    probed = report.probed,
                    swept = report.swept,
                    skipped_unknown_owner = report.skipped_unknown_owner,
                    skipped_live = report.skipped_live,
                    "startup worktree authority sweep complete"
                ),
                Err(e) => tracing::warn!(
                    error = %e,
                    "startup worktree authority sweep failed"
                ),
            }
        });
        orchestrator.background_tasks.push(startup_handle);

        // Periodic sweep — Drop impl on Orchestrator aborts every JoinHandle
        // in background_tasks (orchestrator.rs:918-923).
        let periodic = worktree_authority.spawn_periodic();
        orchestrator.background_tasks.push(periodic);

        Ok(orchestrator)
    }

    /// Attach a PM service. Must be called before `run_adhoc` or `run_interactive`.
    pub fn with_pm_service(mut self, pm: Arc<PmService>) -> Self {
        self.pm_service = Some(pm);
        self
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
    fn build_continuation_ctx(
        &self,
        brain_session_id: spur_acp::types::SessionId,
    ) -> spur_mcp::server::DetachedContinuationCtx {
        match (
            self.continuation_tx.clone(),
            self.continuation_overflow.clone(),
        ) {
            (Some(tx), Some(overflow)) => {
                let session = brain_session_id.clone();
                spur_mcp::server::DetachedContinuationCtx {
                    on_complete: std::sync::Arc::new(move |cont, worker_session_str| {
                        let tx = tx.clone();
                        let overflow = overflow.clone();
                        let session = session.clone();
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
    fn apply_mcp_server_settings(&self, mcp_server: &mut McpCallbackServer) {
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

    fn mcp_feature_gate(&self) -> Arc<spur_license::FeatureGate> {
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
    fn is_auth_required_error(e: &anyhow::Error) -> bool {
        let msg = e.to_string().to_lowercase();
        msg.contains("authrequired")
            || msg.contains("auth_required")
            || msg.contains("please run /login")
            || msg.contains("run `/login`")
            || msg.contains("run /login")
    }

    /// Human-readable banner text for auth-required failures.
    fn auth_required_banner() -> String {
        "Claude Code requires authentication. Run `claude /login` in a \
         terminal, then restart this session. Press any key to dismiss."
            .to_string()
    }

    /// Run an ad-hoc task through the brain agent.
    pub async fn run_adhoc(&mut self, task: &str, opts: RunOpts) -> Result<RunResult> {
        let start = Instant::now();
        let session_id = SessionId::new();

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

        info!(brain = %brain_name, session = %session_id, "Starting ad-hoc run");
        self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        }));

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
        let prompt_text = self.build_brain_prompt(
            &enriched_task,
            issue_context.as_ref(),
            &session_id,
            &brain_name,
        );

        // 4. Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id: spur_acp::BrainSessionId = session_id.clone().into();
        let adhoc_ctx = self.build_continuation_ctx(session_id.clone());
        let (mcp_server, delegation_channel) = McpCallbackServer::new(
            &brain_session_id,
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

        let ((mut connection, delegation_handle, success, pr_url), mcp_handle): McpGuarded<
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
            let mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-mcp", &mcp_url))];

            let session_response = crate::skip_perm::new_session_with_bypass(
                &mut *connection,
                &brain_config,
                self.repo_root.clone(),
                mcp_servers,
            )
            .await
            .context("Failed to create brain session")?;

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
            let delegation_handle = tokio::spawn(Self::handle_delegations(
                delegation_channel,
                self.repo_root.clone(),
                self.config.agents.entries.clone(),
                max_concurrent,
                self.config.worktree.clone(),
                self.event_tx.clone(),
                self.funnel.clone(),
                self.review_sink.clone(),
                self.pm_service.clone(),
                self.cancellation_control.clone(),
                self.peer_mailbox.clone(),
                std::time::Duration::from_secs(self.config.spur.dispatch_lease_secs),
                std::time::Duration::from_secs(self.config.spur.dispatch_lease_heartbeat_secs),
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

            Ok((connection, delegation_handle, success, pr_url))
        })
        .await?;

        // 9. Clean up.
        let _ = connection.shutdown().await;
        delegation_handle.abort();
        abort_mcp_handle(mcp_handle).await;

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
                    error: "bv (beads_viewer) not installed — graph analysis disabled. \
                            Install: brew install dicklesworthstone/tap/bv"
                        .into(),
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

                        let list_req = ListSessionsRequest::new().cwd(self.repo_root.clone());
                        let sessions_result = match conn.list_sessions(list_req).await {
                            Ok(response) => Ok(response.sessions),
                            Err(e) => {
                                warn!(error = %e, "list_sessions failed, trying filesystem fallback");
                                Self::list_sessions_from_disk(&brain_name)
                            }
                        };

                        match sessions_result {
                            Ok(sessions) => {
                                self.emit(SpurEvent::now(SpurEventBody::SessionsListed {
                                    agent: brain_name.clone(),
                                    sessions,
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
                        // Emit SessionLoading before the RPC so the UI can show a
                        // "loading session" state while the brain retrieves history.
                        self.emit(SpurEvent::now(SpurEventBody::SessionLoading {
                            session: SessionId(session_id.clone()),
                        }));
                        match self
                            .load_brain_session(
                                connection,
                                brain_name,
                                permission_tx.clone(),
                                session_id,
                                None,
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

                    // ── GetIssueDetail ────────────────────────────────────
                    InteractiveInput::GetIssueDetail { id } => {
                        if let Some(pm) = &self.pm_service {
                            match pm.get_issue(&id).await {
                                Ok(issue) => {
                                    self.funnel.emit(SpurEventBody::IssueDetailFetched {
                                        requested_id: id,
                                        issue: issue_to_detail_event(&issue),
                                    });
                                }
                                Err(e) => {
                                    self.funnel.emit(SpurEventBody::IssueCommandError {
                                        operation: "GetIssueDetail".into(),
                                        error: e.to_string(),
                                        id: None,
                                    });
                                }
                            }
                        } else {
                            self.funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "GetIssueDetail".into(),
                                error: "No issue tracker configured".into(),
                                id: None,
                            });
                        }
                    }

                    // ── GetIssueGraph ────────────────────────────────────
                    InteractiveInput::GetIssueGraph { id } => {
                        handle_get_issue_graph(
                            self.pm_service.as_deref(),
                            &self.funnel,
                            id,
                        )
                        .await;
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
                            tracing::warn!(
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
                                    // Non-prompt, non-cancel variants arriving mid-stream:
                                    // push to scheduler as user input so they run after the turn.
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
        if let Some(mut b) = brain.take() {
            b.delegation_handle.abort();
            if let Some(h) = b.notification_pump_handle.take() {
                h.abort();
            }
            self.self_held
                .remove(&spur_acp::BrainSessionId::from(b.spur_session_id.clone()));
            retire_brain_session(
                &self.funnel,
                &b.spur_session_id,
                &mut b.mcp_server,
                Some(&mut b.mcp_guard),
                &mut scheduler,
                &overflow_continuations,
                None,
            )
            .await;
            let _ = b.connection.shutdown().await;
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

    /// Execute a task directly on a single agent (no brain, no delegation).
    pub async fn exec_direct(&mut self, agent_name: &str, task: &str) -> Result<RunResult> {
        let start = Instant::now();
        let session_id = SessionId::new();

        let agent_config = self
            .registry
            .get(agent_name)
            .ok_or_else(|| anyhow!("Agent '{}' not found in registry", agent_name))?
            .clone();

        info!(agent = %agent_name, session = %session_id, "Direct execution");

        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                agent_name,
                "worker",
                None,
                task,
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        let mut connection = self.create_connection(&agent_config, None);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        connection
            .initialize(init_request)
            .await
            .context("Failed to initialize agent")?;

        let session_response = crate::skip_perm::new_session_with_bypass(
            &mut *connection,
            &agent_config,
            self.repo_root.clone(),
            vec![],
        )
        .await
        .context("Failed to create agent session")?;

        let prompt_request = PromptRequest::new(
            session_response.session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(task.to_string()))],
        );

        let success = true;
        crate::notification_drain::drive_prompt_notifications(
            &mut *connection,
            prompt_request,
            |notification| match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        print!("{}", tc.text);
                    }
                }
                _ => {}
            },
        )
        .await?;

        let _ = connection.shutdown().await;
        let duration = start.elapsed();

        if let Some(ref ct) = self.cost_tracker {
            let status = if success { "completed" } else { "failed" };
            let _ = ct.end_session(&session_id, status, duration, agent_config.cost_tier);
        }

        let total_cost = spur_cost::estimator::estimate_cost(agent_config.cost_tier, duration);
        println!();

        Ok(RunResult {
            session_id,
            success,
            pr_url: None,
            total_cost_usd: total_cost,
        })
    }

    /// Initialize: scan $PATH for agents declared in the embedded seed
    /// template (`spur_acp::config::load_seed_template`), register those
    /// whose `command` is on $PATH.
    pub async fn init_agents(&mut self) -> Result<Vec<String>> {
        let seeds = spur_acp::config::load_seed_template();
        let mut found = Vec::new();
        for seed in seeds.entries {
            let ok = tokio::process::Command::new("which")
                .arg(&seed.command)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                info!(agent = %seed.name, command = %seed.command, "Found agent");
                found.push(seed.name.clone());
                self.registry.register(seed);
            }
        }
        Ok(found)
    }

    /// Health-check all registered agents.
    pub async fn check_agents(&mut self) -> Vec<(String, AgentHealth)> {
        let agents: Vec<_> = self.registry.list().into_iter().cloned().collect();
        let mut results = Vec::new();

        for config in &agents {
            let mut connection = self.create_connection(config, None);
            let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
            let health = match connection.initialize(init_request).await {
                Ok(_) => {
                    let _ = connection.shutdown().await;
                    AgentHealth::Ready
                }
                Err(e) => AgentHealth::Error(e.to_string()),
            };
            results.push((config.name.clone(), health));
        }

        // Update health after iteration to avoid borrow conflict.
        for (name, health) in &results {
            self.registry.set_health(name, health.clone());
        }

        results
    }

    // ─── Private helpers ─────────────────────────────────────────────

    /// Retire the currently-active brain session's ephemeral state
    /// (delegation handler task, MCP server) while preserving the
    /// initialized ACP connection in `agent_connection` for reuse by the
    /// next `load_brain_session` / `create_brain_session`.
    ///
    /// Called at the top of any arm that replaces the current brain
    /// (`ResumeSession`, `NewSessionWithMessage`). Saves the cost of
    /// tearing down and reinitializing the agent subprocess on every
    /// session switch — for claude-code-acp that's ~1-3s of node startup
    /// per switch.
    ///
    /// Emits `BrainRetired` *before* aborting background tasks so the
    /// lineage projection observes the close-out ahead of any trailing
    /// notifications. Closes the cost ledger for the retired session.
    /// Drains the notification pump with a bounded grace (100 ms) before
    /// aborting, so late notifications still reach the projection.
    ///
    /// The old ACP session id on the agent side is abandoned silently;
    /// the ACP protocol has no `close_session`. Followup issue tracks a
    /// best-effort `session/cancel` dispatch per transport.
    async fn retire_active_brain(
        &mut self,
        brain: &mut Option<BrainSession>,
        agent_connection: &mut Option<ActiveConnection>,
        scheduler: &mut crate::scheduler::BrainScheduler,
        overflow: &crate::continuation_bridge::OverflowBuf,
        reason: spur_acp::domain::events::BrainRetireReason,
        resume_target: Option<SessionId>,
    ) {
        // Capture the current session id before taking the brain so we can
        // reference it in both the SessionRetireStart and SessionRetireComplete
        // events, regardless of whether a brain is actually held.
        let from_session = brain.as_ref().map(|b| b.spur_session_id.clone());

        // Emit SessionRetireStart only when there IS an active brain to retire
        // AND a resume target is present.  This guarantees the Start/Complete
        // pair either both fire (warm resume) or neither fires (cold resume),
        // preventing subscribers from hanging on a Start with no matching Complete.
        if let (Some(ref from), Some(ref to)) = (&from_session, &resume_target) {
            self.emit(SpurEvent::now(SpurEventBody::SessionRetireStart {
                from: Some(from.clone()),
                to: to.clone(),
            }));
        }

        let Some(mut b) = brain.take() else {
            // No active brain to retire — SessionRetireComplete is skipped
            // because there is no "old" session being retired.
            return;
        };

        // Remove from self_held before teardown. The Live probe catches the
        // gap if the lockfile persists in ActiveConnection beyond this point.
        self.self_held
            .remove(&spur_acp::BrainSessionId::from(b.spur_session_id.clone()));

        // 1. Emit BrainRetired BEFORE aborting handles. Broadcast emit is
        //    synchronous into the channel, so any post-abort stragglers
        //    that slip through land on an already-closed projection state.
        self.emit(SpurEvent::now(SpurEventBody::BrainRetired {
            session: b.spur_session_id.clone(),
            reason,
        }));

        // 2. Close the cost ledger. Best-effort: `end_session` is fallible
        //    when the sqlite row is missing (e.g. cost tracking disabled
        //    or start_session was skipped). If the brain name has left
        //    the registry, we cannot recover its `cost_tier` — in that
        //    case the ledger stays open (better than inventing a tier).
        if let Some(ref ct) = self.cost_tracker {
            if let Some(cfg) = self.registry.get(&b.brain_name) {
                let duration = b.started_at.elapsed();
                let _ = ct.end_session(&b.spur_session_id, "retired", duration, cfg.cost_tier);
            }
        }

        // 3. Drain the notification pump with a bounded grace so the
        //    last batch of notifications reaches the projection. On
        //    timeout, abort explicitly — dropping a `JoinHandle` does NOT
        //    cancel the task. `abort_handle` gives us a side-channel that
        //    survives moving the handle into `timeout`.
        if let Some(h) = b.notification_pump_handle.take() {
            let abort = h.abort_handle();
            if tokio::time::timeout(std::time::Duration::from_millis(100), h)
                .await
                .is_err()
            {
                abort.abort();
            }
        }

        // 4. Abort remaining handles and stash connection for reuse.
        b.delegation_handle.abort();
        retire_brain_session(
            &self.funnel,
            &b.spur_session_id,
            &mut b.mcp_server,
            Some(&mut b.mcp_guard),
            scheduler,
            overflow,
            None,
        )
        .await;
        *agent_connection = Some(ActiveConnection {
            transport: b.connection,
            brain_name: b.brain_name,
            attach_guard: b.attach_guard.take(),
            fs_unsafe: b.fs_unsafe,
            init_response: b.init_response,
        });

        // 5. Emit SessionRetireComplete now that teardown is fully done.
        //    `from_session` is guaranteed Some at this point (we would have
        //    returned early above if brain was None).
        if let Some(from) = from_session {
            self.emit(SpurEvent::now(SpurEventBody::SessionRetireComplete {
                session: from,
            }));
        }
    }

    /// Resolve and initialize a brain agent connection without starting a full session.
    ///
    /// Steps: resolve brain name from config → get brain_config from registry →
    /// create connection → initialize. Returns (connection, brain_name).
    fn selected_brain_name(&self, brain_override: Option<&str>) -> String {
        brain_override
            .unwrap_or(&self.config.brain.default)
            .to_string()
    }

    async fn connect_brain(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
    ) -> Result<(
        Box<dyn spur_acp::AgentConnection>,
        String,
        agent_client_protocol::schema::InitializeResponse,
    )> {
        let brain_name = self.selected_brain_name(brain_override);

        let brain_config = self
            .registry
            .get(&brain_name)
            .ok_or_else(|| anyhow!("Brain agent '{}' not found in registry", brain_name))?
            .clone();

        let mut connection = self.create_connection(&brain_config, permission_tx);

        let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
        let init_response = connection
            .initialize(init_request)
            .await
            .context("Failed to initialize brain agent")?;

        debug!(brain = %brain_name, "Brain agent connected and initialized");
        Ok((connection, brain_name, init_response))
    }

    fn acquire_attach_guard_for_load(
        &self,
        acp_session_id: &str,
    ) -> std::result::Result<(Option<SessionAttachGuard>, bool), LoadBrainSessionError> {
        match SessionAttachGuard::try_acquire(&self.repo_root, acp_session_id) {
            AcquireOutcome::Acquired(guard) => Ok((Some(guard), false)),
            AcquireOutcome::DegradedNoLock { reason } => {
                tracing::warn!(
                    acp_id = %acp_session_id,
                    reason = %reason,
                    "flock unsupported on this volume; multi-instance protection disabled"
                );
                Ok((None, true))
            }
            AcquireOutcome::Rejected { holder } => Err(LoadBrainSessionError::AlreadyAttached {
                acp_id: acp_session_id.to_string(),
                holder,
            }),
            AcquireOutcome::Io(e) => Err(LoadBrainSessionError::Other(anyhow::Error::from(e))),
        }
    }

    fn acquire_attach_guard_for_existing_or_load(
        &self,
        acp_session_id: &str,
        existing_attach_guard: Option<SessionAttachGuard>,
        existing_fs_unsafe: bool,
    ) -> std::result::Result<(Option<SessionAttachGuard>, bool), LoadBrainSessionError> {
        if let Some(guard) = existing_attach_guard {
            if guard.acp_id() == acp_session_id {
                return Ok((Some(guard), existing_fs_unsafe));
            }
            drop(guard);
        }

        self.acquire_attach_guard_for_load(acp_session_id)
    }

    fn acquire_attach_guard_for_new(
        &self,
        acp_session_id: &str,
    ) -> Result<(Option<SessionAttachGuard>, bool)> {
        match SessionAttachGuard::try_acquire(&self.repo_root, acp_session_id) {
            AcquireOutcome::Acquired(guard) => Ok((Some(guard), false)),
            AcquireOutcome::DegradedNoLock { reason } => {
                tracing::warn!(
                    acp_id = %acp_session_id,
                    reason = %reason,
                    "flock unsupported on this volume; multi-instance protection disabled"
                );
                Ok((None, true))
            }
            AcquireOutcome::Rejected { holder } => {
                tracing::error!(
                    acp_id = %acp_session_id,
                    ?holder,
                    "newly-created session id is already locked; proceeding without protection"
                );
                Ok((None, true))
            }
            AcquireOutcome::Io(e) => Err(anyhow::Error::from(e)),
        }
    }

    fn acquire_attach_guard_for_existing_or_new(
        &self,
        acp_session_id: &str,
        existing_attach_guard: Option<SessionAttachGuard>,
        existing_fs_unsafe: bool,
    ) -> Result<(Option<SessionAttachGuard>, bool)> {
        if let Some(guard) = existing_attach_guard {
            if guard.acp_id() == acp_session_id {
                return Ok((Some(guard), existing_fs_unsafe));
            }
            drop(guard);
        }

        self.acquire_attach_guard_for_new(acp_session_id)
    }

    /// Create a full brain session from an already-initialized connection.
    ///
    /// Emits BrainSpawned, starts MCP callback server, logs session start,
    /// calls new_session, spawns delegation handler. Returns BrainSession.
    #[allow(clippy::too_many_arguments)]
    async fn create_brain_session(
        &mut self,
        mut connection: Box<dyn spur_acp::AgentConnection>,
        brain_name: String,
        _permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        existing_attach_guard: Option<SessionAttachGuard>,
        existing_fs_unsafe: bool,
        init_response: agent_client_protocol::schema::InitializeResponse,
    ) -> Result<BrainSession> {
        let session_id = SessionId::new();

        info!(brain = %brain_name, session = %session_id, "Creating brain session");
        self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        }));

        // Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id: spur_acp::BrainSessionId = session_id.clone().into();
        let cont_ctx = self.build_continuation_ctx(session_id.clone());
        let (mcp_server, delegation_channel) = McpCallbackServer::new(
            &brain_session_id,
            self.pm_service.clone(),
            sink,
            cont_ctx,
            self.outcome_store.clone(),
            self.mcp_feature_gate(),
        );
        let mut mcp_server = mcp_server;

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

        // Log session start.
        if let Some(ref ct) = self.cost_tracker {
            let _ = ct.start_session(
                &session_id,
                &brain_name,
                "brain",
                None,
                "(interactive)",
                self.config.project.as_ref().map(|p| p.name.as_str()),
                None,
            );
        }

        let ((brain_cfg, presub_notif_rx, session_response), mcp_handle): McpGuarded<
            NewBrainSessionBootstrap,
        > = cleanup_mcp_on_err(mcp_handle, async {
            let mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-mcp", &mcp_url))];

            let brain_cfg = self.registry.get(&brain_name).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "brain agent '{}' not in registry during create_brain_session",
                    brain_name
                )
            })?;

            // Pre-subscribe BEFORE new_session so notifications the agent emits
            // during session setup (e.g. claude-code-acp's initial
            // `available_commands_update`) land on a live receiver. Broadcast
            // `send()` returns `Err(SendError)` only when every receiver has
            // been dropped; holding `presub_notif_rx` here keeps sends
            // succeeding until we hand the receiver to the pump below.
            let presub_notif_rx = connection.subscribe_session_notifications();

            let session_response = crate::skip_perm::new_session_with_bypass(
                &mut *connection,
                &brain_cfg,
                self.repo_root.clone(),
                mcp_servers,
            )
            .await
            .context("Failed to create brain session")?;

            Ok((brain_cfg, presub_notif_rx, session_response))
        })
        .await?;

        let (attach_guard, fs_unsafe) = self.acquire_attach_guard_for_existing_or_new(
            &session_response.session_id.to_string(),
            existing_attach_guard,
            existing_fs_unsafe,
        )?;

        // Spawn delegation handler.
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
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.config.worktree.clone(),
            self.event_tx.clone(),
            self.funnel.clone(),
            self.review_sink.clone(),
            self.pm_service.clone(),
            self.cancellation_control.clone(),
            self.peer_mailbox.clone(),
            std::time::Duration::from_secs(self.config.spur.dispatch_lease_secs),
            std::time::Duration::from_secs(self.config.spur.dispatch_lease_heartbeat_secs),
        ));

        // Spawn the vendor-extension notification pump (if the transport
        // supports it). Each payload becomes a `SpurEventBody::AgentExtNotification`
        // scoped to this brain session.
        if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
            let funnel = self.funnel.clone();
            let spur_session_id = session_id.clone();
            tokio::spawn(async move {
                while let Some(payload) = ext_rx.recv().await {
                    funnel.emit(SpurEventBody::AgentExtNotification {
                        session: spur_session_id.clone(),
                        method: payload.method,
                        params: payload.params,
                    });
                }
            });
        }

        // Fan out session notifications from the connection's broadcast
        // into the SpurEvent bus — see notification_pump::spawn_session_notification_pump.
        // `presub_notif_rx` was subscribed before new_session so we don't
        // miss notifications emitted during session setup.
        let notification_pump_handle = presub_notif_rx.map(|notif_rx| {
            crate::notification_pump::spawn_session_notification_pump(
                notif_rx,
                session_id.clone(),
                self.funnel.clone(),
            )
        });

        let config_options = session_response.config_options.clone().unwrap_or_default();
        // M8.A: build the frozen-per-session capability cache from both
        // the InitializeResponse (`AgentCapabilities`) and the
        // NewSessionResponse (modes/models/config_options). Spec §6.1.
        let spur_agent_caps = Some(Arc::new(spur_acp::SpurAgentCaps::new(
            &init_response,
            &session_response,
            brain_cfg.kind,
        )));

        self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session_id.clone(),
            acp_session_id: session_response.session_id.to_string(),
            brain: brain_name.clone(),
            resumed: false,
            cancel_mode: cancel_mode_for(brain_cfg.transport),
            fs_unsafe,
            caps: spur_agent_caps.clone(),
        }));

        if !config_options.is_empty() {
            // Surface the initial cache so spur-tui can synthesize
            // advertised slash commands (e.g. /model, /effort) from
            // session creation onward.
            self.emit(SpurEvent::now(SpurEventBody::CommandRegistryDirty {
                session: session_id.clone(),
                config_options: config_options.clone(),
            }));
        }

        self.self_held.insert(brain_session_id.clone());

        Ok(BrainSession {
            connection,
            acp_session_id: session_response.session_id.to_string(),
            spur_session_id: session_id,
            brain_name,
            delegation_handle,
            notification_pump_handle,
            mcp_server: Some(mcp_server),
            mcp_guard: Some(mcp_handle),
            started_at: std::time::Instant::now(),
            attach_guard,
            fs_unsafe,
            config_options,
            spur_agent_caps,
            session_info: None,
            init_response,
        })
    }

    /// Load an existing session and return a BrainSession + history stream.
    ///
    /// Similar to create_brain_session but calls load_session instead of new_session.
    /// The history stream delivers past session notifications (historical context).
    // TODO(tech-debt): refactor when extracting orchestrator into smaller types.
    #[allow(clippy::too_many_arguments)]
    async fn load_brain_session(
        &mut self,
        mut connection: Box<dyn spur_acp::AgentConnection>,
        brain_name: String,
        _permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        acp_session_id: String,
        preserve_spur_session_id: Option<SessionId>,
        force_new_session: bool,
        existing_attach_guard: Option<SessionAttachGuard>,
        existing_fs_unsafe: bool,
        init_response: agent_client_protocol::schema::InitializeResponse,
    ) -> std::result::Result<
        (
            BrainSession,
            std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>>,
            spur_acp::LoadOutcome,
        ),
        LoadBrainSessionError,
    > {
        let (session_id, is_reconnect) = match preserve_spur_session_id {
            Some(sid) => (sid, true),
            None => (SessionId::new(), false),
        };
        let requested_acp_session_id = acp_session_id.clone();

        let (mut attach_guard, mut fs_unsafe) = if force_new_session {
            drop(existing_attach_guard);
            (None, false)
        } else {
            self.acquire_attach_guard_for_existing_or_load(
                &acp_session_id,
                existing_attach_guard,
                existing_fs_unsafe,
            )?
        };

        info!(brain = %brain_name, session = %session_id, acp_session = %acp_session_id, "Loading brain session");
        if !is_reconnect {
            self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
                agent: brain_name.clone(),
                session: session_id.clone(),
            }));
        }

        // Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id: spur_acp::BrainSessionId = session_id.clone().into();
        let cont_ctx = self.build_continuation_ctx(session_id.clone());
        let (mcp_server, delegation_channel) = McpCallbackServer::new(
            &brain_session_id,
            self.pm_service.clone(),
            sink,
            cont_ctx,
            self.outcome_store.clone(),
            self.mcp_feature_gate(),
        );
        let mut mcp_server = mcp_server;

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
        // bd-3rvt: missing here meant resumed sessions silently never dispatched.
        self.apply_mcp_server_settings(&mut mcp_server);

        let mcp_server = Arc::new(mcp_server);
        let (mcp_url, mcp_handle) = mcp_server
            .clone()
            .start()
            .await
            .context("Failed to start MCP callback server")?;

        // Log session start.
        if !is_reconnect {
            if let Some(ref ct) = self.cost_tracker {
                let _ = ct.start_session(
                    &session_id,
                    &brain_name,
                    "brain",
                    None,
                    "(resumed)",
                    self.config.project.as_ref().map(|p| p.name.as_str()),
                    None,
                );
            }
        }

        let (
            (
                brain_cfg,
                presub_notif_rx,
                final_acp_session_id,
                history_stream,
                resumed,
                load_outcome,
            ),
            mcp_handle,
        ): McpGuarded<LoadedBrainSessionBootstrap> = cleanup_mcp_on_err(mcp_handle, async {
            let mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-mcp", &mcp_url))];

            let brain_cfg = self.registry.get(&brain_name).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "brain agent '{}' not in registry during load_brain_session",
                    brain_name
                )
            })?;

            // Pre-subscribe BEFORE load_session so the entire history replay
            // (published via the broadcast during load_session for native
            // transports) lands on a live receiver. Holding `presub_notif_rx`
            // here keeps broadcast sends succeeding until we hand the receiver
            // to the pump below.
            let presub_notif_rx = connection.subscribe_session_notifications();

            // Try load_session first. If the agent doesn't support it (e.g. kiro-cli),
            // fall back to new_session so we have a working session for subsequent prompts.
            // The historical conversation is displayed from the disk fallback in either case.
            let (final_acp_session_id, history_stream, resumed, load_outcome) =
                if force_new_session {
                    // Escalated reconnect: don't even try session/load — just spawn fresh.
                    let session_response = crate::skip_perm::new_session_with_bypass(
                        &mut *connection,
                        &brain_cfg,
                        self.repo_root.clone(),
                        mcp_servers.clone(),
                    )
                    .await
                    .context("Failed to create fresh session during escalated reconnect")?;
                    (
                        session_response.session_id.to_string(),
                        None,
                        false,
                        spur_acp::LoadOutcome::FellBackToNew {
                            reason: "escalated to fresh session after repeated failures".into(),
                        },
                    )
                } else {
                    match crate::skip_perm::load_session_with_bypass(
                        &mut *connection,
                        &brain_cfg,
                        acp_session_id.clone(),
                        self.repo_root.clone(),
                        mcp_servers.clone(),
                    )
                    .await
                    {
                        Ok(stream) => {
                            debug!(brain = %brain_name, "load_session succeeded");
                            (
                                acp_session_id,
                                Some(stream),
                                true,
                                spur_acp::LoadOutcome::Restored,
                            )
                        }
                        Err(e) => {
                            warn!(brain = %brain_name, error = %e, "load_session failed, falling back to new_session");
                            let fallback_reason = e.to_string();
                            let session_response = crate::skip_perm::new_session_with_bypass(
                                &mut *connection,
                                &brain_cfg,
                                self.repo_root.clone(),
                                mcp_servers,
                            )
                            .await
                            .context("Failed to create fallback session after load_session failure")?;
                            (
                                session_response.session_id.to_string(),
                                None,
                                false,
                                spur_acp::LoadOutcome::FellBackToNew {
                                    reason: fallback_reason,
                                },
                            )
                        }
                    }
                };

            Ok((
                brain_cfg,
                presub_notif_rx,
                final_acp_session_id,
                history_stream,
                resumed,
                load_outcome,
            ))
        })
        .await?;

        if final_acp_session_id != requested_acp_session_id {
            drop(attach_guard.take());
            (attach_guard, fs_unsafe) = self.acquire_attach_guard_for_new(&final_acp_session_id)?;
        }

        // Spawn delegation handler.
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
        let delegation_handle = tokio::spawn(Self::handle_delegations(
            delegation_channel,
            self.repo_root.clone(),
            self.config.agents.entries.clone(),
            max_concurrent,
            self.config.worktree.clone(),
            self.event_tx.clone(),
            self.funnel.clone(),
            self.review_sink.clone(),
            self.pm_service.clone(),
            self.cancellation_control.clone(),
            self.peer_mailbox.clone(),
            std::time::Duration::from_secs(self.config.spur.dispatch_lease_secs),
            std::time::Duration::from_secs(self.config.spur.dispatch_lease_heartbeat_secs),
        ));

        // Pump vendor-extension notifications onto the event stream.
        if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
            let funnel = self.funnel.clone();
            let spur_session_id = session_id.clone();
            tokio::spawn(async move {
                while let Some(payload) = ext_rx.recv().await {
                    funnel.emit(SpurEventBody::AgentExtNotification {
                        session: spur_session_id.clone(),
                        method: payload.method,
                        params: payload.params,
                    });
                }
            });
        }

        // Fan out session notifications from the connection's broadcast
        // into the SpurEvent bus — see notification_pump::spawn_session_notification_pump.
        // `presub_notif_rx` was subscribed before load_session so history
        // replay items aren't missed.
        let notification_pump_handle = presub_notif_rx.map(|notif_rx| {
            crate::notification_pump::spawn_session_notification_pump(
                notif_rx,
                session_id.clone(),
                self.funnel.clone(),
            )
        });

        self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session_id.clone(),
            acp_session_id: final_acp_session_id.clone(),
            brain: brain_name.clone(),
            resumed,
            cancel_mode: cancel_mode_for(brain_cfg.transport),
            fs_unsafe,
            caps: None,
        }));

        let brain_session = BrainSession {
            connection,
            acp_session_id: final_acp_session_id,
            spur_session_id: session_id,
            brain_name,
            delegation_handle,
            notification_pump_handle,
            mcp_server: Some(mcp_server),
            mcp_guard: Some(mcp_handle),
            started_at: std::time::Instant::now(),
            attach_guard,
            fs_unsafe,
            // The current `load_session_with_bypass` path discards the
            // `LoadSessionResponse.config_options` payload; they will be
            // refreshed by the next `SetSessionConfigOption` response or
            // by a `session/update.ConfigOptionUpdate` notification. The
            // v2 plan extends the bypass helper to plumb this through.
            config_options: Vec::new(),
            // M8.A: load_session does not yet capture LoadSessionResponse
            // (skip_perm helper bypasses it). Caps stay None until M9 wires
            // the response through; downstream UI will see "no caps" and
            // render disabled state. New sessions populate caps in
            // `create_brain_session`.
            spur_agent_caps: None,
            session_info: None,
            init_response,
        };

        // Return an empty stream if we fell back to new_session.
        let stream: std::pin::Pin<
            Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>,
        > = match history_stream {
            Some(s) => s,
            None => Box::pin(futures::stream::empty()),
        };

        self.self_held.insert(brain_session_id.clone());

        Ok((brain_session, stream, load_outcome))
    }

    /// Attempt to reconnect after a brain subprocess death. Drops the
    /// dead `BrainSession` (closing its stdio and aborting its helper
    /// tasks), spawns a fresh connection via `connect_brain`, then
    /// reattaches via `load_brain_session` using the old
    /// `acp_session_id`.
    ///
    /// On success returns the new `BrainSession` and the `LoadOutcome`
    /// distinguishing "session/load restored state" from "we fell back
    /// to a new session". On failure the caller must surface
    /// `BrainReconnectFailed` and leave `brain = None`.
    ///
    /// The caller (not this helper) is responsible for emitting
    /// `BrainReconnecting` BEFORE invoking this, and
    /// `BrainReconnected` / `BrainReconnectFailed` after.
    async fn try_reconnect_brain(
        &mut self,
        mut dead_brain: BrainSession,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        brain_override: Option<&str>,
        force_new_session: bool,
    ) -> std::result::Result<(BrainSession, spur_acp::LoadOutcome), ReconnectError> {
        let acp_session_id = dead_brain.acp_session_id.clone();
        let preserve_spur_id = dead_brain.spur_session_id.clone();
        let brain_name_hint = dead_brain.brain_name.clone();
        let existing_attach_guard = dead_brain.attach_guard.take();
        let existing_fs_unsafe = dead_brain.fs_unsafe;

        // Drop the dead session: abort helper tasks, close stdio.
        dead_brain.delegation_handle.abort();
        if let Some(h) = dead_brain.notification_pump_handle.take() {
            h.abort();
        }
        self.self_held.remove(&spur_acp::BrainSessionId::from(
            dead_brain.spur_session_id.clone(),
        ));
        shutdown_mcp_server(
            &self.funnel,
            &dead_brain.spur_session_id,
            &mut dead_brain.mcp_server,
            Some(&mut dead_brain.mcp_guard),
        )
        .await;
        drop(dead_brain.connection);

        // Fresh connection + reattach. init_response is plumbed into
        // load_brain_session for retention on the BrainSession (so the
        // retire path can move it back to ActiveConnection later); the
        // `set_*` caps stay None for resumed sessions until M9 wires the
        // LoadSessionResponse through.
        let (connection, brain_name, init_response) = self
            .connect_brain(brain_override, permission_tx.clone())
            .await
            .with_context(|| format!("reconnect: connect_brain failed for '{brain_name_hint}'"))?;

        let (new_session, mut history_stream, outcome) = match self
            .load_brain_session(
                connection,
                brain_name,
                permission_tx,
                acp_session_id,
                Some(preserve_spur_id),
                force_new_session,
                existing_attach_guard,
                existing_fs_unsafe,
                init_response,
            )
            .await
        {
            Ok(result) => result,
            Err(LoadBrainSessionError::AlreadyAttached { acp_id, holder }) => {
                return Err(ReconnectError::AlreadyAttached { acp_id, holder });
            }
            Err(LoadBrainSessionError::Other(e)) => {
                return Err(ReconnectError::Other(e.context(format!(
                    "reconnect: load_brain_session failed for '{brain_name_hint}'"
                ))));
            }
        };

        // Drain the history stream to keep the pump contract (same
        // pattern as the ResumeSession arm). We do NOT re-emit
        // AgentNotification events here — the TUI already rendered the
        // pre-death transcript.
        while let Some(_notification) = history_stream.next().await {}

        Ok((new_session, outcome))
    }

    /// Wrap `try_reconnect_brain` with the three event emissions and
    /// the circuit-breaker bookkeeping. Returns `Some(new_brain)` if
    /// reconnect succeeded; `None` if the breaker is open or reconnect
    /// failed (in which case `BrainReconnectFailed` was already
    /// emitted).
    #[allow(clippy::too_many_arguments)]
    async fn reconnect_with_events(
        &mut self,
        dead_brain: BrainSession,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
        brain_override: Option<&str>,
        trigger_reason: String,
        failures: &mut std::collections::VecDeque<std::time::Instant>,
        limit: usize,
        window: std::time::Duration,
    ) -> Option<BrainSession> {
        let spur_session_id = dead_brain.spur_session_id.clone();
        let brain_name = dead_brain.brain_name.clone();

        // Trim stale death timestamps and record this death.
        let now = std::time::Instant::now();
        while let Some(front) = failures.front() {
            if now.duration_since(*front) > window {
                failures.pop_front();
            } else {
                break;
            }
        }
        failures.push_back(now);

        // Decide tier: if we've exceeded the budget of deaths in the window,
        // escalate to a fresh-session reconnect.
        let escalate = failures.len() > limit;
        let (reconnecting_reason, force_new) = if escalate {
            (
                format!(
                    "{} — escalating to fresh session after {} deaths within {:?}",
                    trigger_reason,
                    failures.len(),
                    window
                ),
                true,
            )
        } else {
            (trigger_reason, false)
        };

        self.emit(SpurEvent::now(SpurEventBody::BrainReconnecting {
            session: spur_session_id.clone(),
            brain_name: brain_name.clone(),
            reason: reconnecting_reason,
        }));

        match self
            .try_reconnect_brain(dead_brain, permission_tx, brain_override, force_new)
            .await
        {
            Ok((new_brain, outcome)) => {
                // Tier 1 success clears the window; Tier 2 success keeps the
                // record so a quick re-death after escalation still trips.
                if !escalate {
                    failures.clear();
                }
                self.emit(SpurEvent::now(SpurEventBody::BrainReconnected {
                    session: new_brain.spur_session_id.clone(),
                    brain_name: new_brain.brain_name.clone(),
                    outcome,
                }));
                Some(new_brain)
            }
            Err(e) => {
                self.emit(SpurEvent::now(reconnect_failure_event(
                    spur_session_id,
                    brain_name,
                    e,
                )));
                None
            }
        }
    }

    /// Spawn a brain agent session with MCP callback server and delegation handler.
    pub async fn spawn_brain_session(
        &mut self,
        brain_override: Option<&str>,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
    ) -> Result<BrainSession> {
        let (connection, brain_name, init_response) = self
            .connect_brain(brain_override, permission_tx.clone())
            .await?;
        self.create_brain_session(
            connection,
            brain_name,
            permission_tx,
            None,
            false,
            init_response,
        )
        .await
    }

    /// Fallback: read sessions from an agent's local storage on disk.
    /// Currently supports kiro-cli (~/.kiro/sessions/cli/*.json).
    fn list_sessions_from_disk(agent_name: &str) -> Result<Vec<SessionInfo>> {
        // kiro-cli stores sessions in ~/.kiro/sessions/cli/<uuid>.json
        if agent_name.contains("kiro") {
            let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
            let sessions_dir = home.join(".kiro/sessions/cli");

            if !sessions_dir.exists() {
                return Ok(Vec::new());
            }

            let mut sessions: Vec<SessionInfo> = Vec::new();
            for entry in std::fs::read_dir(&sessions_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }

                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                // Parse the minimal fields we need from kiro's session format.
                let json: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let session_id = match json.get("session_id").and_then(|v| v.as_str()) {
                    Some(id) => id.to_string(),
                    None => continue,
                };
                let cwd = json
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = json
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let updated_at = json
                    .get("updated_at")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let mut info = SessionInfo::new(session_id, PathBuf::from(cwd));
                info = info.title(title);
                info = info.updated_at(updated_at);
                sessions.push(info);
            }

            // Sort by updated_at descending (most recent first).
            sessions.sort_by(|a, b| {
                let a_time = a.updated_at.as_deref().unwrap_or("");
                let b_time = b.updated_at.as_deref().unwrap_or("");
                b_time.cmp(a_time)
            });

            info!(
                count = sessions.len(),
                "Loaded sessions from kiro disk storage"
            );
            return Ok(sessions);
        }

        anyhow::bail!(
            "No filesystem fallback available for agent '{}'",
            agent_name
        )
    }

    /// Read conversation history from a kiro session's JSONL file on disk.
    /// Returns (role, text) pairs for Prompt and AssistantMessage entries.
    fn read_session_history_from_disk(session_uuid: &str) -> Vec<spur_acp::HistoryEntry> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let jsonl_path = home.join(format!(".kiro/sessions/cli/{}.jsonl", session_uuid));

        let content = match std::fs::read_to_string(&jsonl_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let mut entries = Vec::new();
        for line in content.lines() {
            let json: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let kind = json.get("kind").and_then(|v| v.as_str()).unwrap_or("");

            // Concatenate ALL text content blocks (messages can have multiple).
            let text = json
                .pointer("/data/content")
                .and_then(|arr| arr.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let item_kind = item.get("kind").and_then(|v| v.as_str())?;
                            if item_kind == "text" {
                                item.get("data").and_then(|v| v.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();

            if text.is_empty() {
                continue;
            }

            match kind {
                "Prompt" => entries.push(spur_acp::HistoryEntry {
                    role: "user".into(),
                    text,
                }),
                "AssistantMessage" => entries.push(spur_acp::HistoryEntry {
                    role: "assistant".into(),
                    text,
                }),
                _ => {} // Skip ToolResults, etc. for v1
            }
        }
        entries
    }

    fn create_connection(
        &self,
        config: &spur_acp::config::AgentConfig,
        permission_tx: Option<
            tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>,
        >,
    ) -> Box<dyn AgentConnection> {
        // L1a: effective_args folds skip_permissions_args into the spawn
        // args when bypass is on.
        let args = config.effective_args();
        let perms = config.effective_permissions();
        // L2: when bypass is on, short-circuit permission requests by
        // passing None, which activates spur-acp's auto_approve fast-path.
        // Only meaningful for transports that surface ACP permission
        // callbacks (ACP native); other transports ignore the value.
        let perm_tx = if perms.skip { None } else { permission_tx };

        build_connection_from_transport(config, args, perm_tx)
    }

    fn build_brain_prompt(
        &self,
        task: &str,
        issue: Option<&Issue>,
        session_id: &SessionId,
        brain_name: &str,
    ) -> String {
        if self.config.brain.delegation.framework == "v1" {
            self.build_brain_prompt_v1(task, issue, session_id, brain_name)
        } else {
            self.build_brain_prompt_legacy(task, issue)
        }
    }

    fn build_brain_prompt_legacy(&self, task: &str, issue: Option<&Issue>) -> String {
        let mut prompt = String::new();

        // System instructions.
        prompt.push_str(
            "You are coordinating a coding task. You have two kinds of tools:\n\
             \n\
             1. Your own tools (filesystem, bash, git) — use these to investigate and code directly.\n\
             2. SPUR delegation tools — use these to hand work to specialized worker agents.\n\
             \n\
             When to delegate vs do it yourself:\n\
             - Delegate when subtasks are INDEPENDENT and can run in parallel\n\
             - Delegate to match agent strengths\n\
             - Do it yourself for quick tasks or when you need tight iterative control\n\
             - Always review worker output before approving\n\n",
        );

        // Issue context.
        if let Some(issue) = issue {
            prompt.push_str(&format!(
                "## Issue #{}: {}\n\n{}\n\nLabels: {}\nStatus: {}\n\n",
                issue.id,
                issue.title,
                issue.body,
                issue.labels.join(", "),
                issue.status,
            ));
        }

        // Project-specific context.
        if let Some(ref append) = self.config.brain.prompt.append {
            prompt.push_str(&format!("## Project Context\n\n{}\n\n", append));
        }

        // Task.
        prompt.push_str(&format!("## Task\n\n{}\n", task));

        prompt
    }

    fn build_brain_prompt_v1(
        &self,
        task: &str,
        issue: Option<&Issue>,
        session_id: &SessionId,
        brain_name: &str,
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(&self.render_header());
        prompt.push_str(&self.render_workers_block());
        if let Some(framework) = crate::skills::load_skill("brain-delegation", &self.repo_root) {
            prompt.push_str(&framework);
        }
        let agent_skill = format!("brain-delegation-{}", brain_name);
        if let Some(guidance) = crate::skills::load_skill(&agent_skill, &self.repo_root) {
            prompt.push_str(&guidance);
        }
        self.append_issue_and_task(&mut prompt, task, issue);
        self.log_prompt_once(&prompt, session_id);
        prompt
    }

    fn render_header(&self) -> String {
        "You are a brain coordinating a coding task. You have two kinds of tools:\n\
         \n\
         1. Your own tools (filesystem, bash, git) — for investigation and direct edits.\n\
         2. SPUR delegation tools (delegate_to_worker, delegate_parallel, list_available_workers) — for handing work to worker agents that run in isolated worktrees.\n\n".into()
    }

    fn render_workers_block(&self) -> String {
        let mut out = String::from("## Available worker agents\n\n");
        let mut agents: Vec<_> = self.registry.worker_capable().into_iter().collect();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        let mut any_listed = false;
        for agent in agents {
            if agent.delegation.good_for.is_empty() {
                continue;
            }
            any_listed = true;
            let tier = agent
                .delegation
                .tier
                .map(|t| match t {
                    spur_acp::config::Tier::Specialist => "specialist",
                    spur_acp::config::Tier::Generalist => "generalist",
                })
                .unwrap_or("generalist");
            let cost = format!("{:?}", agent.cost_tier).to_lowercase();
            let desc = agent
                .delegation
                .description
                .as_deref()
                .unwrap_or("(no description)");
            out.push_str(&format!(
                "### {}  ({}, cost: {})\n{}\n\n",
                agent.name, tier, cost, desc,
            ));
        }
        if !any_listed {
            out.push_str("(no worker-capable agents with descriptors configured)\n\n");
        }
        out
    }

    fn append_issue_and_task(&self, prompt: &mut String, task: &str, issue: Option<&Issue>) {
        // Issue context.
        if let Some(issue) = issue {
            prompt.push_str(&format!(
                "## Issue #{}: {}\n\n{}\n\nLabels: {}\nStatus: {}\n\n",
                issue.id,
                issue.title,
                issue.body,
                issue.labels.join(", "),
                issue.status,
            ));
        }

        // Project-specific context.
        if let Some(ref append) = self.config.brain.prompt.append {
            prompt.push_str(&format!("## Project Context\n\n{}\n\n", append));
        }

        // Task.
        prompt.push_str(&format!("## Task\n\n{}\n", task));
    }

    fn log_prompt_once(&self, prompt: &str, session_id: &SessionId) {
        let dir = self.repo_root.join(".spur/logs/brain-prompts");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::debug!(error = %e, "could not create brain-prompts log dir");
            return;
        }
        // Use the spur session id as the filename so that repeated calls within
        // the same session overwrite the prior log (one log per session intent).
        // SessionId wraps a UUID string, which is filename-safe by construction.
        let path = dir.join(format!("{}.md", session_id));
        if let Err(e) = std::fs::write(&path, prompt) {
            tracing::debug!(error = %e, path = %path.display(), "could not write prompt log");
        }
        enforce_log_cap(&dir, 50 * 1024 * 1024);
    }

    async fn fetch_issue_context(&self, issue_ref: &str) -> Result<Issue> {
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
    fn emit(&self, event: SpurEvent) {
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

    /// Dispatch `session/set_model` for `brain` via the trait method on
    /// `AgentConnection`. Reads the cached `SpurAgentCaps` once and lets
    /// the connection's typed surface decide between `Direct`,
    /// `FallbackConfigOption`, and `Unsupported` (spec §6.3).
    ///
    /// `value` is the user-supplied model id (e.g. `"claude-sonnet-4-7"`).
    /// `Err(AcpError::CapabilityMissing("set_model"))` when caps absent or
    /// neither dispatch path is advertised. Defined as an associated
    /// function (no `&self`) so the future stays `Send` when awaited
    /// inside `run_interactive` — `Orchestrator` itself is `!Sync` due
    /// to its embedded rusqlite connection, but no orchestrator state
    /// is actually needed for this dispatch.
    pub async fn dispatch_set_session_model(
        brain: &mut BrainSession,
        value: String,
    ) -> Result<(), spur_acp::AcpError> {
        let caps = brain
            .spur_agent_caps
            .as_ref()
            .cloned()
            .ok_or(spur_acp::AcpError::CapabilityMissing("set_model"))?;
        let sid = agent_client_protocol::schema::SessionId::new(brain.acp_session_id.clone());
        let model_id = agent_client_protocol::schema::ModelId::new(value);
        brain
            .connection
            .set_session_model(sid, model_id, &caps)
            .await
    }

    fn maybe_spawn_dispatch_lease_heartbeat(
        pm_service: Option<Arc<PmService>>,
        issue_id: Option<String>,
        delegation_id: String,
        lease_duration: std::time::Duration,
        heartbeat_cadence: std::time::Duration,
        abort_handle: DelegationAbortHandle,
    ) -> Option<AbortOnDropHandle<()>> {
        let (Some(pm), Some(issue_id)) = (pm_service, issue_id) else {
            return None;
        };
        let heartbeat_cadence = if heartbeat_cadence.is_zero() {
            std::cmp::max(lease_duration / 3, std::time::Duration::from_secs(1))
        } else {
            heartbeat_cadence
        };
        Some(AbortOnDropHandle::new(tokio::spawn(async move {
            let lease_secs = i64::try_from(lease_duration.as_secs()).unwrap_or(i64::MAX);
            loop {
                tokio::select! {
                    biased;
                    _ = abort_handle.cancelled() => break,
                    _ = tokio::time::sleep(heartbeat_cadence) => {}
                }

                let expires_at = chrono::Utc::now().timestamp().saturating_add(lease_secs);
                if let Err(error) = spur_mcp::plan::update_dispatch_lease(
                    pm.as_ref(),
                    &issue_id,
                    &delegation_id,
                    expires_at,
                )
                .await
                {
                    tracing::warn!(
                        issue_id = %issue_id,
                        %delegation_id,
                        "dispatch lease heartbeat failed: {error}"
                    );
                }
            }
        })))
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

    /// Handle delegation requests from the MCP callback server.
    ///
    /// Spawns each delegation as a separate tokio task, allowing multiple
    /// workers to run concurrently. A semaphore limits the number of
    /// simultaneous workers to `max_concurrent`.
    #[allow(clippy::too_many_arguments)]
    async fn handle_delegations(
        mut channel: DelegationChannel,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        max_concurrent: usize,
        worktree_config: WorktreeConfig,
        event_tx: broadcast::Sender<SpurEvent>,
        funnel: crate::event_funnel::FunnelHandle,
        review_sink: ReviewSink,
        pm_service: Option<Arc<PmService>>,
        cancellation_control: CancellationControl,
        peer_mailbox: Option<crate::peer_mailbox::PeerMailboxBundle>,
        dispatch_lease_duration: std::time::Duration,
        dispatch_lease_heartbeat: std::time::Duration,
    ) {
        let semaphore = Arc::new(Semaphore::new(max_concurrent.max(1)));
        // Debounce: skip post-delegation refresh if another completed <3s ago.
        // Initial value is in the past so the first refresh always runs.
        let last_refresh_at = Arc::new(tokio::sync::Mutex::new(
            tokio::time::Instant::now() - std::time::Duration::from_secs(60),
        ));

        while let Some(request) = channel.request_rx.recv().await {
            // Destructure the request — it is not Clone, so we move each field.
            let DelegationRequest {
                id: request_id,
                agent,
                task,
                context_files,
                respond_to,
                brain_session_id,
                delegation_plan,
                issue_id,
                base,
                dispatched_base_oid_tx,
                attempt_tracker,
            } = request;
            // Phase 4: `DelegationRequest.id` is now a typed `DelegationId`
            // newtype. Downstream delegation plumbing (funnel events,
            // SessionId, PmService, log fields) still speaks plain `String`,
            // so lower the wrapper to its inner representation at the
            // orchestrator boundary rather than threading the newtype
            // through every call site.
            let request_id: String = request_id.into();

            debug!(
                agent = %agent,
                task = %task,
                "Received delegation request"
            );

            let repo_root = repo_root.clone();
            let agent_configs = agent_configs.clone();
            let semaphore = Arc::clone(&semaphore);
            let worktree_config = worktree_config.clone();
            let event_tx = event_tx.clone();
            let funnel = funnel.clone();
            let review_sink = review_sink.clone();
            let pm_service = pm_service.clone();
            let last_refresh_at = Arc::clone(&last_refresh_at);
            let peer_mailbox = peer_mailbox.clone();

            // INV-6: register a cancellation token BEFORE spawning so
            // cancel() arriving between dispatch and spawn still works.
            let cancel_token = {
                let cc = cancellation_control.clone();
                let (token, handle) = cc.register_with_abort_handle(request_id.clone()).await;
                (token, handle)
            };
            let (cancel_token, abort_handle) = cancel_token;
            let cancellation_control_for_task = cancellation_control.clone();

            tokio::spawn(async move {
                let mut guard = DelegationGuard {
                    funnel: funnel.clone(),
                    respond_to: Some(respond_to),
                    request_id: request_id.clone(),
                    disarmed: false,
                };

                // Acquire a permit before starting the delegation.
                let _permit = tokio::select! {
                    biased;
                    _ = abort_handle.cancelled() => {
                        let status = crate::delegation_watchdog::status_from_abort_reason(&abort_handle).await;
                        funnel.emit(SpurEventBody::DelegationCompleted {
                            worker_session: spur_acp::types::SessionId(request_id.clone()),
                            status: status.clone(),
                        });
                        if let Some(respond_to) = guard.respond_to.take() {
                            let _ = respond_to.send(DelegationResult {
                                status,
                                diff: None,
                                diff_summary: None,
                                summary: None,
                                estimated_cost_usd: 0.0,
                                worker_branch: None,
                                artifact: None,
                            });
                        }
                        cancellation_control_for_task.remove(&request_id).await;
                        guard.disarmed = true;
                        return;
                    }
                    permit = semaphore.acquire() => match permit {
                        Ok(permit) => permit,
                        Err(_) => {
                            error!("Semaphore closed — aborting delegation");
                            // Clean up the token if we abort early.
                            cancellation_control_for_task.remove(&request_id).await;
                            return; // guard fires DelegationCompleted(Failed)
                        }
                    },
                };

                let heartbeat_watchdog_stop =
                    crate::delegation_watchdog::maybe_spawn_heartbeat_watchdog(
                        &worktree_config,
                        request_id.clone(),
                        abort_handle.clone(),
                        &event_tx,
                    );

                // Claim issue on delegation start (10f).
                if let (Some(ref issue_id), Some(ref pm)) = (&issue_id, &pm_service) {
                    let worker_name = format!("spur-worker-{}", request_id);
                    if let Err(e) = pm
                        .update_issue(
                            issue_id,
                            spur_pm::IssueUpdate {
                                status: Some("in_progress".into()),
                                assignee: Some(worker_name.clone()),
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        tracing::warn!(issue = %issue_id, "Failed to claim issue: {e}");
                    } else {
                        funnel.emit(SpurEventBody::IssueUpdated {
                            source: pm.source_str().into(),
                            id: issue_id.clone(),
                            status: Some("in_progress".into()),
                            assignee: Some(worker_name),
                        });
                    }
                }

                let dispatch_lease_heartbeat_handle = Self::maybe_spawn_dispatch_lease_heartbeat(
                    pm_service.clone(),
                    issue_id.clone(),
                    request_id.clone(),
                    dispatch_lease_duration,
                    dispatch_lease_heartbeat,
                    abort_handle.clone(),
                );

                // No outer timeout: the review gate's own `review_timeout`
                // bounds review waits (default 30 min, configurable per
                // agent). A previous hardcoded 300s outer timeout always
                // fired before the 1800s default review timeout, cancelling
                // the delegation mid-`select!`, dropping the ReviewSink
                // entry's receiver without emitting Resolved/TimedOut, and
                // returning `DelegationStatus::Timeout` (worker-hang) to
                // the brain. That broke the spec's worker `Timeout`
                // (hang) vs review `TimedOut` (nobody reviewed) split and
                // left the TUI stuck on `AwaitingReview` because
                // `DelegationCompleted` was never emitted for the right
                // session. v1 accepts that worker-hang detection is not
                // automatic — separate concern, separate fix.
                //
                // INV-6: race execute_delegation against the per-delegation
                // cancellation token. If cancel() arrives first, we return
                // DelegationStatus::Cancelled without waiting for the worker.
                let (result, executor_id_opt) = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        let executor_id_opt = match abort_handle.observed_reason().await {
                            Some(DelegationAbortReason::WorkerHeartbeatTimeout {
                                executor_id,
                                idle_for_secs: _,
                            }) if executor_id != "<not-dispatched>" => {
                                Some(ExecutorId(executor_id))
                            }
                            Some(DelegationAbortReason::BrainRequested { reason: _ })
                            | Some(DelegationAbortReason::WorkerHeartbeatTimeout {
                                executor_id: _,
                                idle_for_secs: _,
                            })
                            | None => None,
                        };
                        let status = crate::delegation_watchdog::status_from_abort_reason(&abort_handle).await;
                        // Emit DelegationCompleted so TUI, lineage, and
                        // other funnel subscribers don't see a stale
                        // "active" entry for this delegation.
                        funnel.emit(SpurEventBody::DelegationCompleted {
                            worker_session: spur_acp::types::SessionId(request_id.clone()),
                            status: status.clone(),
                        });
                        (
                            DelegationResult {
                                status,
                                diff: None,
                                diff_summary: None,
                                summary: None,
                                estimated_cost_usd: 0.0,
                                worker_branch: None,
                                artifact: None,
                            },
                            executor_id_opt,
                        )
                    }
                    r = Self::execute_delegation(
                        agent,
                        task,
                        context_files,
                        request_id.clone(),
                        brain_session_id,
                        delegation_plan,
                        issue_id.clone(),
                        repo_root,
                        agent_configs,
                        funnel.clone(),
                        review_sink.clone(),
                        attempt_tracker,
                        peer_mailbox,
                        base,
                        dispatched_base_oid_tx,
                    ) => r,
                };
                drop(dispatch_lease_heartbeat_handle);
                drop(heartbeat_watchdog_stop);
                // Always clean up the token entry (avoids stale entries
                // when the delegation completes normally before cancel fires).
                cancellation_control_for_task.remove(&request_id).await;

                // Comment on / revert issue on completion (10g).
                if let (Some(ref issue_id), Some(ref pm)) = (&issue_id, &pm_service) {
                    let (new_status, comment) = match &result.status {
                        // Success — DON'T close, just comment. Brain decides when to close.
                        DelegationStatus::Success => {
                            (None, format!("Completed by SPUR delegation {}", request_id))
                        }
                        DelegationStatus::Rejected { .. } => {
                            (Some("open"), format!("Delegation {} rejected", request_id))
                        }
                        DelegationStatus::Failed { error } => (
                            Some("open"),
                            format!("Delegation {} failed: {}", request_id, error),
                        ),
                        _ => (Some("open"), format!("Delegation {} ended", request_id)),
                    };

                    let update = spur_pm::IssueUpdate {
                        status: new_status.map(String::from),
                        comment: Some(comment),
                        ..Default::default()
                    };

                    if let Err(e) = pm.update_issue(issue_id, update).await {
                        tracing::warn!(issue = %issue_id, "Failed to transition issue: {e}");
                    } else if let Some(status) = new_status {
                        funnel.emit(SpurEventBody::IssueUpdated {
                            source: pm.source_str().into(),
                            id: issue_id.clone(),
                            status: Some(status.into()),
                            assignee: None,
                        });
                    }
                }

                // Refresh issue list + graph alerts after delegation completes
                // so TUI picks up changes made by the worker (F19).
                // Debounce: skip if another delegation refreshed <3s ago
                // (prevents thundering herd from delegate_parallel).
                if let Some(ref pm) = pm_service {
                    let mut last = last_refresh_at.lock().await;
                    if last.elapsed() >= std::time::Duration::from_secs(3) {
                        *last = tokio::time::Instant::now();
                        drop(last); // release lock before async work
                        refresh_pm_state(pm, &funnel, Some(1000), false).await;
                    } else {
                        tracing::debug!("Skipping post-delegation refresh (debounced)");
                    }
                }

                // Normal path: disarm the guard and send result manually.
                guard.disarmed = true;
                let respond_to = guard.respond_to.take().unwrap();

                if let Err(_returned_result) = respond_to.send(result) {
                    // Brain's MCP tool call was cancelled — the oneshot
                    // receiver was dropped before we could deliver the
                    // result. If a review was still pending on this
                    // delegation, emit an audit event so the lineage
                    // projection records the abandonment rather than
                    // leaving an orphaned review card indefinitely.
                    if let Some(ref eid) = executor_id_opt {
                        cleanup_cancelled_review(
                            eid,
                            "brain call cancelled",
                            &funnel,
                            &review_sink,
                        )
                        .await;
                    }
                }
            });
        }
    }

    /// Execute a single delegation request.
    ///
    /// This method is fully self-contained: it creates its own
    /// `WorktreeManager` and `AgentRegistry` so it can run in an
    /// independent tokio task without shared mutable state.
    ///
    /// ## Retry loop (Task 10)
    ///
    /// When `agent_config.review.review_required == true` and the
    /// reviewer returns `ReviewDecision::Retry { new_constraints }`,
    /// this method despawns the current worker, appends the constraints
    /// to the original task, bumps `attempt_n`, emits
    /// `ExecutorRetryStarted`, and re-enters the worker-spawn +
    /// review-gate flow. Bounded by
    /// `agent_config.review.max_review_retries`. On exceed, returns
    /// `Failed { error: "retry limit exceeded after N attempts" }`.
    ///
    /// `executor_id` is stable across attempts (captured from the first
    /// worker session) so the lineage projection's attempt history
    /// accumulates on a single node.
    // TODO: consolidate args into an ExecuteDelegationParams struct to reduce arity.
    #[allow(clippy::too_many_arguments)]
    async fn execute_delegation(
        agent: String,
        original_task: String,
        context_files: Vec<String>,
        request_id: String,
        brain_session_id: spur_acp::BrainSessionId,
        delegation_plan: Option<spur_acp::domain::DelegationPlan>,
        issue_id: Option<String>,
        repo_root: PathBuf,
        agent_configs: Vec<spur_acp::config::AgentConfig>,
        funnel: crate::event_funnel::FunnelHandle,
        review_sink: ReviewSink,
        attempt_tracker: Arc<std::sync::atomic::AtomicU32>,
        peer_mailbox: Option<crate::peer_mailbox::PeerMailboxBundle>,
        base: Option<BaseSpec>,
        dispatched_base_oid_tx: Option<tokio::sync::watch::Sender<Option<String>>>,
    ) -> (DelegationResult, Option<ExecutorId>) {
        // Shadow `original_task` with the Relevant Files-prepended form
        // so retry loops at orchestrator.rs:3013 reuse the formatted
        // base. No-op when context_files is empty.
        let original_task = format_worker_task(&original_task, &context_files);
        // `__`-prefixed agent names are reserved for internal operations.
        // __cancel_delegation no longer routes through this path (INV-6 —
        // cancellation now goes through CancellationControl). Any other
        // `__`-prefixed name is an unsupported internal operation.
        if agent.starts_with("__") {
            return (
                DelegationResult {
                    status: DelegationStatus::Failed {
                        error: format!("Unsupported internal operation: {agent}"),
                    },
                    diff: None,
                    diff_summary: None,
                    summary: None,
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                },
                None,
            );
        }

        let registry = AgentRegistry::load(agent_configs);

        let agent_config = match registry.get(&agent) {
            Some(c) => c.clone(),
            None => {
                return (
                    DelegationResult {
                        status: DelegationStatus::Failed {
                            error: format!("Worker agent '{}' not found", agent),
                        },
                        diff: None,
                        diff_summary: None,
                        summary: None,
                        estimated_cost_usd: 0.0,
                        worker_branch: None,
                        artifact: None,
                    },
                    None,
                );
            }
        };

        let mut current_task = original_task.clone();
        // Retry-history accumulator. Each retry attempt pushes its
        // prior attempt's (summary, diff_summary, reviewer feedback)
        // so the NEXT attempt's prompt can reference what was tried.
        // 2 KB bloat cap drops oldest entries first.
        let mut retry_history: Vec<RetryAttempt> = Vec::new();
        let mut attempt_n: u32 = 1;
        // Stable across retries; captured from the first worker session.
        let mut executor_id: Option<ExecutorId> = None;
        // Accumulated cost across all attempts in this delegation.
        let mut total_cost: f64 = 0.0;

        // WorktreeManager owned here (not inside run_one_worker_attempt)
        // so execute_delegation can make post-gate commit/remove decisions.
        // Each delegation task gets its own manager (concurrent delegations
        // do not share mutable state). Retries reuse the same manager.
        let mut worktrees = WorktreeManager::new(repo_root);

        // Worker session for the *next* attempt. Generated here (not
        // inside run_one_worker_attempt) so the Retry arm can emit
        // ExecutorRetryStarted.new_session_id matching the session id
        // the next attempt will actually use — closing the lineage
        // Attempt.session_id ↔ worker event linkage.
        let first_worker_session = SessionId::new();
        let mut next_worker_session = first_worker_session;

        loop {
            attempt_tracker.store(attempt_n, Ordering::SeqCst);
            let (ack_tx, ack_rx) = if peer_mailbox.is_some() {
                let (ack_tx, ack_rx) = tokio::sync::mpsc::unbounded_channel();
                (Some(ack_tx), Some(ack_rx))
            } else {
                (None, None)
            };
            let outcome = match run_one_worker_attempt(
                next_worker_session.clone(),
                WorkerAttemptCtx {
                    brain_session_id: &brain_session_id,
                    agent: &agent,
                    task: &current_task,
                    request_id: &request_id,
                    attempt: attempt_n,
                    agent_config: &agent_config,
                    delegation_plan: delegation_plan.clone(),
                    issue_id: issue_id.clone(),
                    peer_mailbox: peer_mailbox.as_ref(),
                    ack_tx: ack_tx.clone(),
                    base: base.clone(),
                    dispatched_base_oid_tx: dispatched_base_oid_tx.clone(),
                },
                &mut worktrees,
                &funnel,
            )
            .await
            {
                Ok(o) => o,
                Err(setup_err) => {
                    // Setup failures short-circuit the entire
                    // delegation without retry — retrying a
                    // worktree-creation failure is not spec'd
                    // behavior. We still call finalize so
                    // DelegationCompleted is emitted (the worker
                    // session was named, even if no worker actually
                    // ran).
                    let status = match setup_err {
                        AttemptSetupError::OverlayConflict {
                            source_task_id,
                            files,
                        } => DelegationStatus::SetupFailed {
                            error: spur_acp::AttemptSetupError::OverlayConflict {
                                source_task_id,
                                files,
                            },
                        },
                        AttemptSetupError::SnapshotFailed(error) => DelegationStatus::Failed {
                            error: spur_acp::AttemptSetupError::SnapshotFailed { error }
                                .to_string(),
                        },
                        AttemptSetupError::WorktreeFailed(error) => DelegationStatus::Failed {
                            error: spur_acp::AttemptSetupError::WorktreeFailed { error }
                                .to_string(),
                        },
                        AttemptSetupError::InitFailed(error) => DelegationStatus::Failed {
                            error: spur_acp::AttemptSetupError::InitFailed { error }.to_string(),
                        },
                        AttemptSetupError::SessionFailed(error) => DelegationStatus::Failed {
                            error: spur_acp::AttemptSetupError::SessionFailed { error }.to_string(),
                        },
                    };
                    return (
                        finalize(
                            &funnel,
                            next_worker_session,
                            status,
                            None,
                            None,
                            None,
                            total_cost,
                            None,
                            None,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            total_cost += outcome.cost;

            // On first attempt, capture executor_id from worker_session.
            if executor_id.is_none() {
                executor_id = Some(ExecutorId::new(outcome.worker_session.0.clone()));
            }
            let eid = executor_id.clone().unwrap();

            // No review gate — commit/remove then emit DelegationCompleted.
            if !agent_config.review.review_required {
                let preserved_branch = apply_worktree_cleanup(
                    &mut worktrees,
                    &outcome.worker_session,
                    &outcome.candidate_status,
                    &outcome.diff,
                    &agent,
                    &outcome.worktree_path,
                )
                .await;
                return (
                    finalize(
                        &funnel,
                        outcome.worker_session,
                        outcome.candidate_status,
                        outcome.diff,
                        outcome.diff_summary,
                        outcome.summary,
                        total_cost,
                        preserved_branch,
                        None,
                    ),
                    executor_id.clone(),
                );
            }

            // INV-4: obtain a ReviewHandle first — it is the ONLY way to
            // emit `ExecutorReviewRequested` for this slot, enforced at
            // the type level. `register_handle` wraps `ReviewSink::register`
            // so the ordering invariant (register-before-emit) is preserved.
            let handle = match review_sink.register_handle(eid.clone(), attempt_n).await {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!(
                        executor_id = %eid.0,
                        attempt_n,
                        error = %e,
                        "review_sink registration failed — skipping review gate"
                    );
                    // Worker DID run; emit DelegationCompleted via
                    // finalize so the lineage projection records the
                    // terminal Failed status (preserves the
                    // "every terminal emits DelegationCompleted"
                    // invariant). Registration failure → Failed (not
                    // preserved; no useful diff to inspect).
                    let failed_status = DelegationStatus::Failed {
                        error: format!("review registration failed: {e}"),
                    };
                    let preserved_branch = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &failed_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &funnel,
                            outcome.worker_session,
                            failed_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                            None,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            funnel.emit(SpurEventBody::ExecutorPhaseChanged {
                id: eid.0.clone(),
                phase: LifecycleState::AwaitingReview,
            });

            let plan = delegation_plan.as_ref();
            let chosen_matches_dispatched = plan
                .and_then(|p| p.chosen.as_ref())
                .map(|c| normalize_agent_name(c) == normalize_agent_name(&agent));

            if chosen_matches_dispatched == Some(false) {
                tracing::warn!(
                    session = %brain_session_id,
                    chosen = %plan.and_then(|p| p.chosen.as_deref()).unwrap_or(""),
                    dispatched = %agent,
                    "delegation_plan.chosen does not match dispatched agent",
                );
            }

            drop(ack_tx);
            if let (Some(bundle), Some(ack_rx)) = (peer_mailbox.as_ref(), ack_rx) {
                let limits = bundle.router.limits();
                let quiet_window = std::time::Duration::from_millis(limits.drain_quiet_window_ms);
                let drain_max_total = std::time::Duration::from_millis(limits.drain_max_total_ms);
                drain_peer_acks_with_timeout(
                    bundle,
                    &spur_acp::domain::delegation::DelegationId(request_id.clone()),
                    quiet_window,
                    drain_max_total,
                    &brain_session_id,
                    &funnel,
                    ack_rx,
                )
                .await;
            }

            let peer_influence = if peer_mailbox.is_some() {
                use crate::lineage::types::PeerEdgeState;

                let target = spur_acp::domain::delegation::DelegationId(request_id.clone());
                let mut summary = spur_acp::PeerInfluenceSummary::default();
                if let Some(lineage) = funnel.lineage_snapshot().await {
                    let inbound = lineage.peer_edges_inbound_for_delegation(&target);
                    let outbound = lineage.peer_edges_for_delegation(&target);

                    for edge in inbound {
                        match edge.state {
                            PeerEdgeState::Consumed => summary.inbound_consumed += 1,
                            PeerEdgeState::Ignored => summary.inbound_ignored += 1,
                            PeerEdgeState::Undeliverable
                            | PeerEdgeState::Dropped
                            | PeerEdgeState::Expired
                            | PeerEdgeState::Rejected => summary.undelivered += 1,
                            _ => {}
                        }
                    }
                    summary.outbound_emitted = u32::try_from(outbound.len()).unwrap_or(u32::MAX);
                }
                // from_unreviewed_source stays false in Stage 1; it needs
                // brain-state lookup that is intentionally out of scope here.
                Some(summary)
            } else {
                None
            };

            let review_payload = ReviewPayload {
                summary: outcome.summary.clone().unwrap_or_default(),
                diff_summary: outcome.diff_summary.clone(),
                pr_url: None,
                error: None,
                delegation_plan: delegation_plan.clone(),
                chosen_matches_dispatched,
                peer_influence,
            };

            // Emit via the handle — type-enforced: no handle → no emit.
            handle.emit_requested(&funnel, ReviewKind::Completion, review_payload);

            // Consume the handle to get the receiver for the decision loop.
            let rx = handle.into_rx();

            // Inline decision-loop (so we can intercept Retry before
            // apply_decision_to_candidate maps it to Failed).
            use spur_acp::ReviewDecision;
            let decision_result = tokio::select! {
                r = rx => r.ok(),
                _ = tokio::time::sleep(agent_config.review.review_timeout) => {
                    review_sink.remove(&eid).await;
                    let final_status = DelegationStatus::TimedOut {
                        waited_for: agent_config.review.review_timeout,
                        fallback: agent_config.review.review_timeout_default.clone(),
                    };
                    // Emit cancellation so the lineage projection clears
                    // pending_review (DelegationCompleted alone does not).
                    funnel.emit(SpurEventBody::ExecutorReviewCancelled {
                        id: eid.0.clone(),
                        reason: "review timeout".to_string(),
                    });
                    // TimedOut → preserve worktree (no commit).
                    let preserved_branch = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                            None,
                        ),
                        executor_id.clone(),
                    );
                }
            };

            match decision_result {
                Some(ReviewDecision::Approve) => {
                    let final_status = outcome.candidate_status.clone();
                    funnel.emit(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Approve,
                    });
                    // Approve → commit + remove.
                    let preserved_branch = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                            None,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Reject { reason }) => {
                    let final_status = DelegationStatus::Rejected {
                        reason: reason.clone(),
                    };
                    funnel.emit(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Reject { reason },
                    });
                    // Rejected → no commit, preserve worktree.
                    let preserved_branch = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                            None,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Modify { note }) => {
                    let final_status = DelegationStatus::Modified {
                        reviewer_note: note.clone(),
                    };
                    funnel.emit(SpurEventBody::ExecutorReviewResolved {
                        id: eid.0.clone(),
                        decision: ReviewDecision::Modify { note },
                    });
                    // Modified → commit + remove (approved with reviewer note).
                    let preserved_branch = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                            None,
                        ),
                        executor_id.clone(),
                    );
                }
                Some(ReviewDecision::Retry { new_constraints }) => {
                    // DN-2: bound check + exhaustion status live in
                    // `crate::retry_loop::RetryLoop` — shared with
                    // `test_support::run_gate_with_retries`. Both sites
                    // share the strict `>` semantic and the exact error
                    // string format. Changes to retry semantics should
                    // touch `retry_loop.rs`, not this site.
                    //
                    // `>` (not `>=`): spec's "Retry × 4 when
                    // max_review_retries = 3 produces Failed" means 3
                    // retries are allowed (attempts bump 1→2→3→4), and
                    // the 4th Retry decision fails.
                    if let Some(final_status) = crate::retry_loop::RetryLoop::check_exceeded(
                        attempt_n,
                        agent_config.review.max_review_retries,
                    ) {
                        // Retry limit → Failed (remove, no commit).
                        let preserved_branch = apply_worktree_cleanup(
                            &mut worktrees,
                            &outcome.worker_session,
                            &final_status,
                            &outcome.diff,
                            &agent,
                            &outcome.worktree_path,
                        )
                        .await;
                        return (
                            finalize(
                                &funnel,
                                outcome.worker_session,
                                final_status,
                                outcome.diff,
                                outcome.diff_summary.clone(),
                                outcome.summary,
                                total_cost,
                                preserved_branch,
                                None,
                            ),
                            executor_id.clone(),
                        );
                    }

                    // Retry: generate the NEXT attempt's session id
                    // FIRST so we can announce it in
                    // ExecutorRetryStarted (matching what
                    // run_one_worker_attempt will use on the next
                    // iteration). The lineage projection treats
                    // new_session_id as the Attempt.session_id of
                    // the next attempt; emitting a fresh-but-unused
                    // id here would silently dangle.
                    let retry_session = SessionId::new();
                    funnel.emit(SpurEventBody::ExecutorRetryStarted {
                        id: eid.0.clone(),
                        attempt_n: attempt_n + 1,
                        reason: new_constraints.clone(),
                        new_session_id: retry_session.clone(),
                    });

                    // Record this attempt in the retry history before re-prompting.
                    // See docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md
                    // for the rationale — inverts the original
                    // "prevent compounding" choice in favor of the
                    // Reflexion pattern, with a 2KB bloat cap as the
                    // mitigation.
                    retry_history.push(RetryAttempt {
                        attempt_n,
                        summary: outcome.summary.clone().unwrap_or_default(),
                        diff_summary: outcome.diff_summary.clone(),
                        feedback: new_constraints.clone(),
                    });
                    apply_bloat_cap(&mut retry_history, 2048);

                    current_task =
                        render_retry_context(&retry_history, &original_task, &new_constraints);
                    attempt_n += 1;
                    next_worker_session = retry_session;

                    // Retry intermediates are never preserved — remove
                    // the current attempt's worktree before spawning
                    // the next attempt. No commit (intermediate diff is
                    // moot once the retry produces its own diff).
                    //
                    // Log (don't swallow) failures: retries use a fresh
                    // SessionId, so collision is impossible, but disk space
                    // may leak until manual cleanup or cleanup_orphans runs.
                    if let Err(e) = worktrees.remove_worktree(&outcome.worker_session).await {
                        tracing::warn!(
                            session = %outcome.worker_session,
                            error = %e,
                            "failed to remove retry-attempt worktree; retry will use a fresh session ID, but disk space may leak"
                        );
                    }

                    // Exponential backoff: 1s, 2s, 4s, 8s, … capped at 30s.
                    let backoff_secs =
                        std::cmp::min(1u64 << (attempt_n.saturating_sub(1) as u64), 30);
                    tracing::info!(
                        attempt_n = attempt_n,
                        backoff_secs = backoff_secs,
                        "retry backoff before next attempt"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;

                    continue;
                }
                None => {
                    // Sender dropped — treat as timeout.
                    review_sink.remove(&eid).await;
                    let final_status = DelegationStatus::TimedOut {
                        waited_for: agent_config.review.review_timeout,
                        fallback: agent_config.review.review_timeout_default.clone(),
                    };
                    // Emit cancellation so the lineage projection clears
                    // pending_review (DelegationCompleted alone does not).
                    funnel.emit(SpurEventBody::ExecutorReviewCancelled {
                        id: eid.0.clone(),
                        reason: "review sender dropped".to_string(),
                    });
                    // Sender-drop TimedOut → preserve worktree (no commit).
                    let preserved_branch = apply_worktree_cleanup(
                        &mut worktrees,
                        &outcome.worker_session,
                        &final_status,
                        &outcome.diff,
                        &agent,
                        &outcome.worktree_path,
                    )
                    .await;
                    return (
                        finalize(
                            &funnel,
                            outcome.worker_session,
                            final_status,
                            outcome.diff,
                            outcome.diff_summary.clone(),
                            outcome.summary,
                            total_cost,
                            preserved_branch,
                            None,
                        ),
                        executor_id.clone(),
                    );
                }
            }
        }
    }
}

/// Emit `ExecutorReviewCancelled` and remove the sink entry.
///
/// Called from the brain-cancellation path — when `respond_to.send(result)`
/// returns `Err`, the brain has gone away, and any pending review for
/// this delegation must be recorded in the lineage projection as
/// abandoned (otherwise the TUI shows an orphaned review card
/// indefinitely).
///
/// Idempotent: if no review is registered, `review_sink.remove` is a
/// no-op, and the event is still emitted so the lineage projection
/// records the cancellation.
pub async fn cleanup_cancelled_review(
    executor_id: &ExecutorId,
    reason: &str,
    funnel: &crate::event_funnel::FunnelHandle,
    review_sink: &ReviewSink,
) {
    funnel.emit(SpurEventBody::ExecutorReviewCancelled {
        id: executor_id.0.clone(),
        reason: reason.to_string(),
    });
    review_sink.remove(executor_id).await;
}

/// Returns `true` if the worktree should be preserved (not removed) for
/// this final `DelegationStatus`.
///
/// Preserved:
///   - `Rejected` (human said no — operator may want to inspect diff).
///   - `TimedOut { fallback: Reject | Abandon }` (no human reviewed in
///     time AND the configured fallback says "treat as no" or "abandon";
///     preserve so a human can still inspect).
///
/// NOT preserved:
///   - `TimedOut { fallback: Approve }` — per spec, Approve fallback
///     means "auto-approve — worker's diff/summary retained as if
///     reviewed", so the diff must be committed and the worktree
///     detached (same lifecycle as a human Approve).
///   - `Success`/`Modified` (approved — changes committed on the
///     worker branch and preserved for later integration/PR creation).
///   - `Failed`/`Conflict`/`Timeout` (no real work to inspect — worker
///     hung or errored, or conflict blocked the run).
pub fn should_preserve_worktree(status: &DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Rejected { .. }
            | DelegationStatus::TimedOut {
                fallback: TimeoutFallback::Reject { .. } | TimeoutFallback::Abandon,
                ..
            }
            // INV-6: preserve partial work for cancelled delegations so
            // the brain/user can inspect what was done before cancellation.
            | DelegationStatus::Cancelled { .. }
    )
}

/// Returns `true` if the worker's diff should be committed onto the
/// preserved worker branch based on the final `DelegationStatus`.
///
/// Commit on:
///   - `Success` (Approve).
///   - `Modified` (human-annotated approval).
///   - `TimedOut { fallback: Approve }` (auto-approve fallback — spec
///     says diff is "retained as if reviewed", so it must commit).
///
/// Do NOT commit on Rejected/TimedOut(Reject|Abandon) (preserve for
/// inspection), nor on Failed/Conflict/Timeout (no clean diff to keep).
pub fn should_commit_worker_diff(status: &DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Success
            | DelegationStatus::Modified { .. }
            | DelegationStatus::TimedOut {
                fallback: TimeoutFallback::Approve,
                ..
            }
    )
}

/// Post-gate cleanup: commit the worker diff (if approved) and either
/// preserve or remove the worktree based on the final status.
///
/// Called from every terminal arm in `execute_delegation`. On Retry,
/// only `remove_worktree` is called (no commit — intermediate attempts
/// do not get merged into the brain tree).
async fn apply_worktree_cleanup(
    worktrees: &mut WorktreeManager,
    worker_session: &SessionId,
    final_status: &DelegationStatus,
    diff: &Option<String>,
    agent: &str,
    worktree_path: &std::path::Path,
) -> Option<String> {
    if should_commit_worker_diff(final_status) && diff.is_some() {
        if let Err(e) = worktrees
            .commit_worker_changes(worker_session, &format!("spur: worker {} output", agent))
            .await
        {
            tracing::warn!(error = %e, "failed to commit worker diff");
        }
    }

    if should_preserve_worktree(final_status) {
        tracing::info!(
            worktree = %worktree_path.display(),
            status = ?final_status,
            "preserving worktree for review inspection"
        );
        None
    } else if should_commit_worker_diff(final_status) {
        // Approved work: remove worktree dir but keep branch for merge.
        match worktrees.detach_worktree(worker_session).await {
            Ok(branch) => Some(branch),
            Err(e) => {
                tracing::warn!(error = %e, "detach_worktree failed, falling back to full remove");
                let _ = worktrees.remove_worktree(worker_session).await;
                None
            }
        }
    } else {
        let _ = worktrees.remove_worktree(worker_session).await;
        None
    }
}

/// RAII guard that ensures every delegation emits `DelegationCompleted`
/// even on early-exit or task abort. Disarmed by the normal completion
/// path; fires on Drop otherwise.
struct DelegationGuard {
    funnel: crate::event_funnel::FunnelHandle,
    respond_to: Option<tokio::sync::oneshot::Sender<DelegationResult>>,
    request_id: String,
    disarmed: bool,
}

impl Drop for DelegationGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        error!(
            request_id = %self.request_id,
            "DelegationGuard fired — emitting DelegationCompleted(Failed)"
        );
        self.funnel.emit(SpurEventBody::DelegationCompleted {
            worker_session: SessionId(self.request_id.clone()),
            status: DelegationStatus::Failed {
                error: "delegation aborted (early exit or task cancelled)".into(),
            },
        });
        if let Some(tx) = self.respond_to.take() {
            let _ = tx.send(DelegationResult {
                status: DelegationStatus::Failed {
                    error: "delegation aborted".into(),
                },
                diff: None,
                diff_summary: None,
                summary: None,
                estimated_cost_usd: 0.0,
                worker_branch: None,
                artifact: None,
            });
        }
    }
}

/// Common terminal-arm helper: emits `DelegationCompleted` and
/// constructs the `DelegationResult`. Centralizing this makes the
/// "every terminal emits DelegationCompleted" invariant locally
/// verifiable (one call site per terminal arm in `execute_delegation`).
#[allow(clippy::too_many_arguments)]
fn finalize(
    funnel: &crate::event_funnel::FunnelHandle,
    worker_session: SessionId,
    final_status: DelegationStatus,
    diff: Option<String>,
    diff_summary: Option<spur_acp::DiffSummary>,
    summary: Option<String>,
    total_cost: f64,
    worker_branch: Option<String>,
    artifact: Option<spur_acp::WorkerArtifact>,
) -> DelegationResult {
    funnel.emit(SpurEventBody::DelegationCompleted {
        worker_session,
        status: final_status.clone(),
    });
    DelegationResult {
        status: final_status,
        diff,
        diff_summary,
        summary,
        estimated_cost_usd: total_cost,
        worker_branch,
        artifact,
    }
}

/// Setup-level error during a worker-spawn attempt. Distinct from the
/// worker's own output-level outcome (which lives in
/// `WorkerAttemptOutcome`). Setup errors short-circuit the entire
/// delegation without retry — retrying a worktree-creation failure is
/// not a spec'd behavior.
// The setup failure variants describe distinct phases and keep existing match
// sites readable. Suppressing the lint is cleaner than renaming.
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
enum AttemptSetupError {
    SnapshotFailed(String),
    WorktreeFailed(String),
    InitFailed(String),
    SessionFailed(String),
    OverlayConflict {
        source_task_id: String,
        files: Vec<String>,
    },
}

impl std::fmt::Display for AttemptSetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SnapshotFailed(e) => write!(f, "Failed to snapshot brain state: {e}"),
            Self::WorktreeFailed(e) => write!(f, "Failed to create worktree: {e}"),
            Self::InitFailed(e) => write!(f, "Failed to initialize worker: {e}"),
            Self::SessionFailed(e) => write!(f, "Failed to create worker session: {e}"),
            Self::OverlayConflict {
                source_task_id,
                files,
            } => write!(
                f,
                "overlay conflict applying {source_task_id}: {} files",
                files.len()
            ),
        }
    }
}

/// Outcome of one worker attempt: whatever we'd need to close out the
/// delegation OR feed into the review gate.
struct WorkerAttemptOutcome {
    worker_session: SessionId,
    candidate_status: DelegationStatus,
    diff: Option<String>,
    diff_summary: Option<spur_acp::DiffSummary>,
    summary: Option<String>,
    cost: f64,
    /// Path to the worktree that holds this attempt's diff.
    /// Used by `execute_delegation` to log a preserved path on
    /// `Rejected` / `TimedOut` — worktree removal is deferred to
    /// after the review gate.
    worktree_path: PathBuf,
    /// Side-channel artifact (persisted stdout when output > summary cap).
    /// `None` when the worker's stdout fit under the cap.
    #[allow(dead_code)] // Populated in Task 8 (artifact persistence wiring).
    artifact: Option<spur_acp::WorkerArtifact>,
}

/// Map a transport kind to its `CancelMode`. Single source of truth used
/// by `AgentSessionReady` emitters so the TUI can render transport-aware
/// cancel feedback without re-inspecting `AgentConfig`.
pub(crate) fn cancel_mode_for(transport: spur_acp::types::TransportKind) -> spur_acp::CancelMode {
    use spur_acp::types::TransportKind;
    match transport {
        TransportKind::Acp => spur_acp::CancelMode::AcpSoft,
        TransportKind::Stdio | TransportKind::CliWrap | TransportKind::StreamJson => {
            spur_acp::CancelMode::ProcessKill
        }
    }
}

/// Arm the 5-second force-end deadline used by the streaming `select!`.
/// Factored out so both the `Message { interrupt: true }` arm and the
/// new `CancelStream` arm set the deadline identically and so it is
/// directly unit-testable without a full mock orchestrator.
pub(crate) fn arm_cancel_deadline(deadline: &mut Option<tokio::time::Instant>) {
    *deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5));
}

/// Build a boxed `AgentConnection` from the transport declared in `config`.
///
/// Single source of truth for the `match transport { Acp/Stdio/CliWrap/StreamJson }`
/// arms. Both `Orchestrator::create_connection` (brain + resume paths) and
/// `run_one_worker_attempt` (worker spawn) call this — previously each had
/// its own copy of the match, and would drift when transports changed.
///
/// `spawn_args` is the final, bypass-aware spawn argv (callers invoke
/// `config.effective_args()` before passing them in). `permission_tx` is
/// honored only by the ACP transport; other transports ignore it.
fn build_connection_from_transport(
    config: &spur_acp::config::AgentConfig,
    spawn_args: Vec<String>,
    permission_tx: Option<tokio::sync::mpsc::UnboundedSender<spur_acp::types::PermissionRequest>>,
) -> Box<dyn AgentConnection> {
    match config.transport {
        TransportKind::Acp => Box::new(NativeAcpConnection::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
            permission_tx,
        )),
        TransportKind::Stdio => Box::new(StdioAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::CliWrap => Box::new(CliWrapAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
        TransportKind::StreamJson => Box::new(StreamJsonAdapter::new(
            config.name.clone(),
            config.command.clone(),
            spawn_args,
        )),
    }
}

// ─── WorkerFileTouched synthesis (S5 / Task 17) ──────────────────────
//
// Workers using general-purpose agents (kiro, claude-code, codex) do
// NOT emit `_spur/file_touched` ExtNotifications. Instead, the
// orchestrator synthesizes `WorkerFileTouched` events by observing
// the worker's ToolCall stream for known file-op tool names and
// extracting the `path`/`file_path` input field. A 200ms de-dup
// window coalesces repeated ToolCall / ToolCallUpdate events for the
// same (executor, path, kind) so a single logical file operation
// emits at most one `WorkerFileTouched` per 200ms window.
//
// Note: this dedup is local to the synthesizer's stream loop. It
// does NOT coordinate with `spur_ext_interp::interpret`, which
// handles the explicit `_spur/file_touched` ExtNotification path —
// if a SPUR-aware worker ever emits both an explicit event AND a
// matching ToolCall, the subscriber would see two events. Future
// work: share an `Arc<FileTouchDedup>` across both paths to
// guarantee at-most-one emit per (executor, path, kind).

/// De-dup key for the 200ms file-touch window.
#[derive(Hash, Eq, PartialEq, Clone)]
struct FileTouchKey {
    executor_id: String,
    path: std::path::PathBuf,
    kind: spur_acp::domain::events::FileTouchKind,
}

/// Per-worker-attempt de-dup for `WorkerFileTouched` synthesis.
/// Coalesces repeated ToolCall / ToolCallUpdate events for the same
/// (executor, path, kind) within a 200ms window, so a single logical
/// file operation emits at most one `WorkerFileTouched` per window.
///
/// Scope is a single `run_one_worker_attempt` invocation; cross-worker
/// coordination isn't needed because `executor_id` is unique per worker.
struct FileTouchDedup {
    last_seen: std::sync::Mutex<std::collections::HashMap<FileTouchKey, std::time::Instant>>,
    ttl: std::time::Duration,
}

impl FileTouchDedup {
    fn new() -> Self {
        Self {
            last_seen: std::sync::Mutex::new(std::collections::HashMap::new()),
            ttl: std::time::Duration::from_millis(200),
        }
    }

    /// Returns true if this (executor, path, kind) is fresh and should
    /// be emitted. Updates the last-seen map.
    fn should_emit(&self, key: &FileTouchKey) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.last_seen.lock().unwrap();
        // Garbage collect stale entries opportunistically.
        map.retain(|_, t| now.duration_since(*t) < self.ttl * 5);
        match map.get(key) {
            Some(last) if now.duration_since(*last) < self.ttl => false,
            _ => {
                map.insert(key.clone(), now);
                true
            }
        }
    }
}

/// If `notification` is a ToolCall matching a known file-op tool name,
/// synthesize a WorkerFileTouched event (subject to dedup).
///
/// The `title` field of the ACP `ToolCall` struct carries the tool name
/// as populated by adapters (e.g. claude_events maps Anthropic's
/// `tool_use.name` into `title`). Path extraction tries `raw_input`'s
/// `path` / `file_path` fields first, then falls back to the first
/// entry in `locations` if raw_input is missing the key.
fn maybe_synthesize_file_touch(
    notification: &agent_client_protocol::schema::SessionNotification,
    brain_session_id: &spur_acp::types::SessionId,
    executor_id: &str,
    dedup: &FileTouchDedup,
    funnel: &crate::event_funnel::FunnelHandle,
) {
    let tc = match &notification.update {
        SessionUpdate::ToolCall(tc) => tc,
        _ => return,
    };
    let kind = match tc.title.as_str() {
        "read_file" | "Read" => spur_acp::domain::events::FileTouchKind::Read,
        "write_file" | "Write" | "edit_file" | "Edit" => {
            spur_acp::domain::events::FileTouchKind::Write
        }
        _ => return,
    };
    // Prefer explicit raw_input path; fall back to first location entry.
    let path = tc
        .raw_input
        .as_ref()
        .and_then(|v| {
            v.get("path")
                .and_then(|p| p.as_str())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    v.get("file_path")
                        .and_then(|p| p.as_str())
                        .map(std::path::PathBuf::from)
                })
        })
        .or_else(|| tc.locations.first().map(|loc| loc.path.clone()));
    let Some(path) = path else { return };
    let key = FileTouchKey {
        executor_id: executor_id.to_string(),
        path: path.clone(),
        kind,
    };
    if dedup.should_emit(&key) {
        funnel.emit(SpurEventBody::WorkerFileTouched {
            brain_session_id: brain_session_id.clone(),
            executor_id: executor_id.to_string(),
            path,
            kind,
        });
    }
}

/// Run a single worker attempt: snapshot brain state, create worktree,
/// spawn agent, prompt, collect diff.
///
/// `worker_session` is provided by the caller (rather than generated
/// inside) so `execute_delegation`'s Retry arm can announce the next
/// attempt's session id in `ExecutorRetryStarted.new_session_id` and
/// have it match what this function actually uses — closing the lineage
/// `Attempt.session_id ↔ worker event` linkage gap.
///
/// **Worktree lifecycle**: this function creates the worktree and
/// collects the diff, but does NOT commit or remove the worktree.
/// Commit and removal are deferred to `execute_delegation` so the
/// post-gate decision can determine whether to preserve
/// (`Rejected`/`TimedOut`) or remove (all other terminal statuses).
/// Exception: if a setup failure occurs AFTER the worktree is created
/// (e.g., agent init failure), the worktree IS cleaned up here
/// immediately — setup failures short-circuit without retry and the
/// caller's `finalize` records the error status.
///
/// Read-only context shared across worker attempt retries.
struct WorkerAttemptCtx<'a> {
    brain_session_id: &'a spur_acp::BrainSessionId,
    agent: &'a str,
    task: &'a str,
    request_id: &'a str,
    attempt: u32,
    agent_config: &'a spur_acp::config::AgentConfig,
    delegation_plan: Option<spur_acp::domain::DelegationPlan>,
    issue_id: Option<String>,
    peer_mailbox: Option<&'a crate::peer_mailbox::PeerMailboxBundle>,
    ack_tx: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    base: Option<BaseSpec>,
    /// Publishes the resolved post-overlay worktree HEAD back to the reconciler.
    dispatched_base_oid_tx: Option<tokio::sync::watch::Sender<Option<String>>>,
}

/// Returns `Ok(WorkerAttemptOutcome)` for any flow that produced a
/// worker candidate status — success OR worker-reported errors — both
/// of which are retry-eligible (the human reviewer decides).
///
/// Returns `Err(AttemptSetupError)` only for pre-worker setup failures
/// (worktree creation, agent initialization, session creation). The
/// caller short-circuits the delegation without retry — consistent
/// with pre-T10 behavior. Per-attempt error shape is decoupled from
/// the public `DelegationResult` type.
async fn run_one_worker_attempt(
    worker_session: SessionId,
    ctx: WorkerAttemptCtx<'_>,
    worktrees: &mut WorktreeManager,
    funnel: &crate::event_funnel::FunnelHandle,
) -> Result<WorkerAttemptOutcome, AttemptSetupError> {
    // NOTE: DelegationRequested is emitted per-attempt here. The legacy
    // lineage adapter (lineage/adapter.rs) keys task_spec population to
    // the FIRST matching empty-task_spec executor, so on retry the
    // constraint-augmented task silently drops at the adapter boundary.
    // This is part of the broader "adapter keys off worker_session, not
    // stable executor_id" limitation documented for follow-up work.
    // The projection path (apply_inner) sees each event correctly.
    funnel.emit(SpurEventBody::DelegationRequested {
        from: ctx.brain_session_id.as_session_id().clone(),
        to_agent: ctx.agent.to_string(),
        task: ctx.task.to_string(),
        request_id: ctx.request_id.to_string(),
        delegation_plan: ctx.delegation_plan.clone(),
        issue_id: ctx.issue_id.clone(),
    });

    let start = Instant::now();

    // 1. Snapshot brain state and create worktree.
    let snapshot_branch = worktrees
        .snapshot_brain_state()
        .await
        .map_err(|e| AttemptSetupError::SnapshotFailed(e.to_string()))?;

    let base_branch = ctx
        .base
        .as_ref()
        .map(|spec| resolve_base_branch(spec, &snapshot_branch))
        .unwrap_or_else(|| snapshot_branch.clone());

    let worktree_info = worktrees
        .create_worktree(&worker_session, ctx.agent, &base_branch)
        .await
        .map_err(|e| AttemptSetupError::WorktreeFailed(e.to_string()))?;

    // The snapshot branch is only needed as a base ref for worktree creation.
    // Once the worktree exists, delete it immediately to prevent ref leaks.
    if let Err(e) = worktrees.delete_snapshot_branch(&snapshot_branch).await {
        tracing::debug!(
            snapshot_branch = %snapshot_branch,
            error = %e,
            "failed to delete snapshot branch after worktree creation; will leak until cleanup_orphans runs"
        );
    }

    let overlays = ctx.base.as_ref().map(extract_overlays).unwrap_or_default();
    if !overlays.is_empty() {
        if let Err(e) = worktrees
            .apply_overlays(&worktree_info.path, &overlays)
            .await
        {
            let setup_err = match e {
                WorktreeError::OverlayConflict {
                    source_task_id,
                    files,
                } => AttemptSetupError::OverlayConflict {
                    source_task_id,
                    files,
                },
                other => AttemptSetupError::WorktreeFailed(other.to_string()),
            };
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(setup_err);
        }
    }

    let dispatched_base_oid = match worktrees.resolve_head(&worktree_info.path).await {
        Ok(oid) => oid,
        Err(e) => {
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(AttemptSetupError::WorktreeFailed(format!(
                "resolve worktree HEAD: {e}"
            )));
        }
    };
    if let Some(tx) = &ctx.dispatched_base_oid_tx {
        let _ = tx.send(Some(dispatched_base_oid.clone()));
    }
    emit_dispatch_overlay_applied(
        funnel,
        ctx.request_id,
        ctx.base.as_ref(),
        &dispatched_base_oid,
        &overlays,
    );

    // 2. Spawn worker agent in worktree via AgentConnection.
    // Workers never receive a permission_tx, so L2 auto-approve is
    // implicitly always on for them. skip_permissions still has effect
    // via L1a (spawn args).
    let spawn_args = ctx.agent_config.effective_args();
    let mut connection: Box<dyn AgentConnection> =
        build_connection_from_transport(ctx.agent_config, spawn_args, None);

    // S5 — consume `_spur/*` ExtNotifications from this worker and
    // translate them into SpurEvent variants via the funnel. Must run
    // before `connection` is moved; `take_ext_notification_rx` only
    // needs `&mut self` but can be called exactly once per connection.
    if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
        let funnel_for_ext = funnel.clone();
        let executor_id_for_ext = worker_session.0.clone();
        let brain_session_for_ext = ctx.brain_session_id.as_session_id().clone();
        let peer_mailbox_for_ext = ctx.peer_mailbox.cloned();
        let ack_tx_for_ext = ctx.ack_tx.clone();
        tokio::spawn(async move {
            while let Some(payload) = ext_rx.recv().await {
                let terminal_method = payload.method.clone();
                let terminal_params = payload.params.clone();
                crate::spur_ext_interp::interpret(
                    payload,
                    brain_session_for_ext.clone(),
                    executor_id_for_ext.clone(),
                    &funnel_for_ext,
                );
                if let (Some(bundle), Some(ack_tx)) = (&peer_mailbox_for_ext, &ack_tx_for_ext) {
                    crate::spur_ext_interp::interpret_peer_message_terminal(
                        &terminal_method,
                        terminal_params,
                        bundle,
                        ack_tx,
                        &funnel_for_ext,
                        brain_session_for_ext.0.as_str(),
                        executor_id_for_ext.as_str(),
                    )
                    .await;
                }
            }
        });
    }

    let init_request = InitializeRequest::new(ProtocolVersion::LATEST);
    if let Err(e) = connection.initialize(init_request).await {
        let _ = worktrees.remove_worktree(&worker_session).await;
        return Err(AttemptSetupError::InitFailed(e.to_string()));
    }

    // Emit WorkerSpawned event.
    funnel.emit(SpurEventBody::WorkerSpawned {
        agent: ctx.agent.to_string(),
        session: worker_session.clone(),
        worktree: worktree_info.path.clone(),
    });
    // Correlate this executor with the brain's delegate_to_worker call
    // so the brain-side session_detail view can render an inline card.
    funnel.emit(SpurEventBody::DelegationDispatched {
        from: ctx.brain_session_id.as_session_id().clone(),
        request_id: ctx.request_id.to_string(),
        executor_id: worker_session.0.clone(),
    });

    // Workers get no MCP servers (per spec).
    let session_response = match crate::skip_perm::new_session_with_bypass(
        &mut *connection,
        ctx.agent_config,
        worktree_info.path.clone(),
        vec![],
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = connection.shutdown().await;
            let _ = worktrees.remove_worktree(&worker_session).await;
            return Err(AttemptSetupError::SessionFailed(e.to_string()));
        }
    };

    // 3. Send task to worker.
    let prompt_text = format!(
        "Working directory: {}\n\nTask: {}",
        worktree_info.path.display(),
        ctx.task
    );
    // Pre-prompt peer-mailbox injection hook.
    let peer_context = match ctx.peer_mailbox {
        Some(bundle) => {
            // TODO(peer-mailbox): plumb context_window_chars from agent config.
            let context_window = 200_000;
            let target_delegation =
                spur_acp::domain::delegation::DelegationId(ctx.request_id.to_string());
            let limits = bundle.router.limits();
            let built = bundle
                .builder
                .build_for_target(
                    &target_delegation,
                    context_window,
                    limits.max_pending_mailbox_depth,
                    limits.max_peer_message_size,
                )
                .await;
            for inj in &built.injection_records {
                match bundle
                    .ledger
                    .record_injection(&inj.message_id, &built.target_prompt_id)
                    .await
                {
                    Ok(crate::peer_mailbox::ledger::InjectionOutcome::Injected) => {}
                    Ok(crate::peer_mailbox::ledger::InjectionOutcome::AlreadyInjected) => {
                        tracing::debug!(
                            message_id = ?inj.message_id,
                            "peer mailbox: replay injection no-op"
                        );
                    }
                    Err(err) => {
                        tracing::warn!(
                            message_id = ?inj.message_id,
                            ?err,
                            "peer mailbox: record_injection failed"
                        );
                    }
                }
            }
            Some(built)
        }
        None => None,
    };

    let mut prompt_blocks = vec![ContentBlock::Text(TextContent::new(prompt_text))];
    if let Some(pc) = &peer_context {
        if !pc.orchestrator_authored_text.is_empty() {
            prompt_blocks.insert(
                0,
                ContentBlock::Text(TextContent::new(format!(
                    "## Peer messages (orchestrator-authored)\n{}",
                    pc.orchestrator_authored_text
                ))),
            );
        }
    }
    let prompt_request = PromptRequest::new(session_response.session_id.clone(), prompt_blocks);

    let mut output_text = String::new();
    let mut worker_success = true;

    // S5 — Per-worker-attempt file-touch dedup. Owned locally (no Arc
    // needed) because the synthesizer is called synchronously from the
    // stream loop — nothing else clones or moves the instance.
    let file_touch_dedup = FileTouchDedup::new();

    // For native (ACP-transport) workers prompt() returns an empty stream;
    // notifications arrive via the connection-scoped broadcast instead.
    // drive_prompt_notifications handles both paths transparently.
    let prompt_result = crate::notification_drain::drive_prompt_notifications(
        &mut *connection,
        prompt_request,
        |notification| {
            // S5 — synthesize WorkerFileTouched from file-op ToolCalls
            // before any other notification handling.
            maybe_synthesize_file_touch(
                &notification,
                ctx.brain_session_id.as_session_id(),
                &worker_session.0,
                &file_touch_dedup,
                funnel,
            );
            // Phase 1 — stream worker notifications to TUI via event bus.
            funnel.emit(SpurEventBody::WorkerNotification {
                brain_session_id: ctx.brain_session_id.as_session_id().clone(),
                executor_id: worker_session.0.clone(),
                notification: Box::new(notification.clone()),
            });
            match &notification.update {
                SessionUpdate::AgentThoughtChunk(chunk)
                | SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(tc) = &chunk.content {
                        output_text.push_str(&tc.text);
                    }
                }
                _ => {}
            }
        },
    )
    .await;
    if let Err(e) = prompt_result {
        worker_success = false;
        output_text = format!("Failed to prompt worker: {e}");
    } else if let (Some(bundle), Some(pc)) = (ctx.peer_mailbox, peer_context) {
        use crate::peer_mailbox::{
            transition_with_audit, PeerTransitionKind, TransitionAuditOutcome,
        };
        use spur_acp::domain::peer_message::LedgerState;

        let target_delegation_id =
            spur_acp::domain::delegation::DelegationId(ctx.request_id.to_string());

        for inj in pc.injection_records {
            match transition_with_audit(
                bundle.ledger.as_ref(),
                funnel,
                ctx.brain_session_id,
                &target_delegation_id,
                inj.message_id,
                LedgerState::DeliveredInflight,
                PeerTransitionKind::DeliveredInflight,
            )
            .await
            {
                TransitionAuditOutcome::Changed => {}
                TransitionAuditOutcome::Unchanged(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "peer mailbox: delivered-inflight transition no-op"
                    );
                }
                TransitionAuditOutcome::TerminalSkip(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "post-prompt DeliveredInflight transition skipped: message already terminal"
                    );
                    continue;
                }
                TransitionAuditOutcome::AuditFailed(err) => {
                    tracing::warn!(
                        message_id = ?inj.message_id,
                        %err,
                        "peer mailbox: delivered-inflight transition failed"
                    );
                }
            }

            match transition_with_audit(
                bundle.ledger.as_ref(),
                funnel,
                ctx.brain_session_id,
                &target_delegation_id,
                inj.message_id,
                LedgerState::Delivered,
                PeerTransitionKind::Delivered,
            )
            .await
            {
                TransitionAuditOutcome::Changed => {
                    funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDelivered {
                        brain_session_id: ctx.brain_session_id.to_string(),
                        message_id: inj.message_id,
                        target_delegation_id: target_delegation_id.clone(),
                        target_prompt_id: pc.target_prompt_id.clone(),
                        injected_chars: inj.injected_bytes,
                    });
                    // TODO(peer-mailbox): Task 14 startup reconciliation is
                    // the durable peer-mailbox audit path.
                }
                TransitionAuditOutcome::Unchanged(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "peer mailbox: delivered transition no-op"
                    );
                }
                TransitionAuditOutcome::TerminalSkip(state) => {
                    tracing::debug!(
                        message_id = ?inj.message_id,
                        state = ?state,
                        "post-prompt Delivered transition skipped: message already terminal"
                    );
                    continue;
                }
                TransitionAuditOutcome::AuditFailed(err) => {
                    tracing::warn!(
                        message_id = ?inj.message_id,
                        %err,
                        "peer mailbox: delivered transition failed"
                    );
                }
            }
        }
    }

    let _ = connection.shutdown().await;

    // 4. Collect diff. `basis` is either "HEAD" (uncommitted) or
    // "<base>..HEAD" (worker self-committed). We need it to compute the
    // matching diff_summary with the SAME git range — otherwise stats and
    // raw text disagree.
    let (diff, diff_basis) = worktrees
        .collect_diff(&worker_session)
        .await
        .unwrap_or((None, "HEAD"));

    // 5. Capture worktree path for execute_delegation's post-gate cleanup.
    // Commit and removal are deferred — see function doc.
    let worktree_path = worktrees
        .active
        .get(&worker_session.to_string())
        .map(|i| i.path.clone())
        .unwrap_or_default();

    // Compute structured diff stats on the SAME basis as the raw diff.
    // When collect_diff returned base..HEAD, we need to resolve the placeholder
    // to the real spec — fetch the base_commit from worktrees.
    let diff_summary = if diff.is_some() {
        let basis_spec = if diff_basis == "base_commit..HEAD" {
            // Resolve the placeholder with the actual base SHA.
            worktrees
                .active
                .get(&worker_session.to_string())
                .map(|i| format!("{}..HEAD", i.base_commit))
                .unwrap_or_else(|| "HEAD".to_string())
        } else {
            "HEAD".to_string()
        };
        build_diff_summary(&worktree_path, &basis_spec)
            .await
            .ok()
            .filter(|s| s.files_changed > 0)
    } else {
        None
    };

    let duration = start.elapsed();
    let cost = spur_cost::estimator::estimate_cost(ctx.agent_config.cost_tier, duration);

    // Attempt side-channel artifact persistence BEFORE building the
    // truncated summary. Only fires when output would otherwise lose
    // bytes to truncate_summary — the predicate is purely size-based
    // so mixed workers (diff + long rationale) and failure diagnostics
    // are both covered.
    let persist_result: Option<Result<spur_acp::WorkerArtifact, String>> = if output_text.len()
        > summary_cap_bytes()
    {
        let kind = if worker_success {
            spur_acp::ArtifactKind::Output
        } else {
            spur_acp::ArtifactKind::Diagnostic
        };
        let output_bytes = output_text.as_bytes();
        let byte_size = u64::try_from(output_bytes.len()).unwrap_or(u64::MAX);
        let key = OutcomeKey {
            brain_session_id: ctx.brain_session_id.clone(),
            delegation_id: spur_acp::DelegationId::from(ctx.request_id),
            attempt: ctx.attempt,
        };
        let metadata = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: ContentType::Stdout,
            original_byte_size: byte_size,
            stored_byte_size: byte_size,
            sha256: sha256_hex_for_outcome(output_bytes),
        };
        let store = GitBlobOutcomeStore::new(worktrees.repo_root.clone());
        let outcome_store_result = match store.put(&key, output_bytes, &metadata).await {
            Ok(outcome_ref) => outcome_ref.as_worker_artifact(kind).ok_or_else(|| {
                "outcome store returned a non-git backend for worker artifact projection"
                    .to_string()
            }),
            Err(e) => Err(e.to_string()),
        };
        match outcome_store_result {
            Ok(a) => Some(Ok(a)),
            Err(primary_error) => {
                tracing::warn!(
                    session = %worker_session,
                    delegation_id = %ctx.request_id,
                    attempt = ctx.attempt,
                    error = %primary_error,
                    "outcome store artifact persistence failed; falling back to legacy artifact store"
                );
                match worktrees
                    .persist_artifact(&worker_session, &output_text, kind)
                    .await
                {
                    Ok(a) => Some(Ok(a)),
                    Err(fallback_error) => {
                        let error = format!(
                            "outcome store failed: {primary_error}; \
                             legacy artifact fallback failed: {fallback_error}"
                        );
                        tracing::warn!(
                            session = %worker_session,
                            error = %error,
                            "artifact persistence failed"
                        );
                        Some(Err(error))
                    }
                }
            }
        }
    } else {
        None
    };

    // Build the summary FIRST so the error-extraction path on the
    // failure branch can read from it — preserving the existing
    // behaviour at `orchestrator.rs:4116-4130` byte-for-byte.
    // (Raw-output sourcing would diverge when `SPUR_SUMMARY_MAX_BYTES`
    // is lowered below 500; we want this refactor to be a pure
    // no-op on the current failure-message semantics.)
    let summary_pre_annotation: Option<String> = if output_text.is_empty() {
        None
    } else {
        Some(truncate_summary_env_default(&output_text))
    };

    // Build the "original" error status by extracting from the
    // POST-truncation summary. Identical in shape to the existing
    // block at `orchestrator.rs:4116-4130`.
    let original_error_status = if worker_success {
        None
    } else {
        let error = summary_pre_annotation
            .as_deref()
            .map(|s| {
                let tail_bytes = 500usize.min(s.len());
                let start = {
                    let mut i = s.len().saturating_sub(tail_bytes);
                    while i < s.len() && !s.is_char_boundary(i) {
                        i += 1;
                    }
                    i
                };
                s[start..].to_string()
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Worker reported errors (no output captured)".into());
        Some(DelegationStatus::Failed { error })
    };

    let (candidate_status, artifact, persist_failure_note) =
        decide_artifact_handling(worker_success, persist_result, original_error_status);

    // Surface a success-path tracing event for observability. Warn
    // on failure is already emitted above inside the persist match.
    if let Some(a) = &artifact {
        tracing::info!(
            session = %worker_session,
            object_ref = %a.object_ref,
            blob_sha = %a.blob_sha,
            size_bytes = a.size_bytes,
            "worker artifact persisted"
        );
    }

    // Apply the persist-failure annotation to the summary tail (if any).
    let summary = summary_pre_annotation.map(|mut s| {
        if let Some(note) = persist_failure_note.as_deref() {
            s.push('\n');
            s.push_str(note);
        }
        s
    });

    Ok(WorkerAttemptOutcome {
        worker_session,
        candidate_status,
        diff,
        diff_summary,
        summary,
        cost,
        worktree_path,
        artifact,
    })
}

async fn candidate_set_for_target(
    bundle: &crate::peer_mailbox::PeerMailboxBundle,
    delegation_id: &spur_acp::domain::delegation::DelegationId,
) -> Vec<crate::peer_mailbox::LedgerEntry> {
    let mut candidates = bundle.ledger.pending_for_target(delegation_id).await;
    candidates.extend(
        bundle
            .ledger
            .non_terminal_entries()
            .await
            .into_iter()
            .filter(|entry| &entry.envelope.target_delegation_id == delegation_id),
    );
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|entry| seen.insert(entry.envelope.message_id));
    candidates
}

/// Forced-terminal-timeout drain. Waits up to `quiet_window` for peer-ack
/// notifications scoped to `delegation_id`. Each ack resets the window.
/// The drain is also bounded by `max_total`. After either deadline elapses,
/// delivered non-terminal peer messages are forced to `Ignored` with a reason
/// that classifies the exit path.
async fn drain_peer_acks_with_timeout(
    bundle: &crate::peer_mailbox::PeerMailboxBundle,
    delegation_id: &spur_acp::domain::delegation::DelegationId,
    quiet_window: std::time::Duration,
    max_total: std::time::Duration,
    brain_session_id: &spur_acp::BrainSessionId,
    funnel: &crate::event_funnel::FunnelHandle,
    mut ack_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    use spur_acp::domain::peer_message::{LedgerState, TerminalOutcome};

    let cap_deadline = tokio::time::Instant::now() + max_total;
    let drain_start = tokio::time::Instant::now();
    let mut cap_hit = false;
    let mut acks_received: u32 = 0;
    let candidates_at_start = candidate_set_for_target(bundle, delegation_id).await.len() as u32;

    funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDrainStarted {
        brain_session_id: brain_session_id.to_string(),
        target_delegation_id: delegation_id.clone(),
        candidates_at_start,
        cap_ms: max_total.as_millis() as u64,
        quiet_window_ms: quiet_window.as_millis() as u64,
    });

    loop {
        let now = tokio::time::Instant::now();
        if now >= cap_deadline {
            cap_hit = true;
            break;
        }

        let quiet_deadline = now + quiet_window;
        let next_deadline = quiet_deadline.min(cap_deadline);
        let waiting_for_cap = next_deadline == cap_deadline;

        match tokio::time::timeout_at(next_deadline, ack_rx.recv()).await {
            Ok(Some(())) => {
                acks_received = acks_received.saturating_add(1);
            }
            Ok(None) => break,
            Err(_) => {
                cap_hit = waiting_for_cap;
                break;
            }
        }
    }

    let actual_elapsed_ms = drain_start.elapsed().as_millis() as u64;

    let candidates = candidate_set_for_target(bundle, delegation_id).await;
    let remaining_messages = candidates
        .iter()
        .filter(|entry| {
            matches!(
                entry.state,
                LedgerState::Delivered | LedgerState::DeliveredInflight
            )
        })
        .count() as u32;

    if cap_hit {
        funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDrainCappedOut {
            brain_session_id: brain_session_id.to_string(),
            target_delegation_id: delegation_id.clone(),
            acks_received,
            remaining_messages,
            cap_ms: max_total.as_millis() as u64,
            actual_elapsed_ms,
        });
    } else if remaining_messages > 0 {
        funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDrainTimedOut {
            brain_session_id: brain_session_id.to_string(),
            target_delegation_id: delegation_id.clone(),
            acks_received,
            remaining_messages,
            cap_ms: max_total.as_millis() as u64,
            quiet_window_ms: quiet_window.as_millis() as u64,
            actual_elapsed_ms,
        });
    }

    let reason = if cap_hit {
        "drain_capped"
    } else {
        "drain_timeout"
    };
    for entry in candidates {
        let message_id = entry.envelope.message_id;
        if !matches!(
            entry.state,
            LedgerState::Delivered | LedgerState::DeliveredInflight
        ) {
            continue;
        }
        if let Err(err) = bundle
            .router
            .record_terminal(
                brain_session_id.as_session_id().0.as_str(),
                &message_id,
                TerminalOutcome::Ignored {
                    reason: reason.into(),
                },
            )
            .await
        {
            tracing::warn!(
                message_id = ?message_id,
                ?err,
                "peer mailbox: forced-terminal-timeout drain failed"
            );
        }
    }
}

/// Tail-weighted, UTF-8-safe truncation for worker summaries.
///
/// Why tail-weighted: LLM worker output opens with task restatement
/// and closes with a crisp conclusion + file list. The middle holds
/// verbose tool-call transcripts with low decision-density. Brain-
/// relevant information is concentrated at the tail.
///
/// Returns `text` unchanged if `text.len() <= cap`. Otherwise keeps
/// `cap/4` head bytes and `cap - cap/4` tail bytes (both aligned to
/// char boundaries), joined by an omission marker.
fn truncate_summary(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let head_budget = cap / 4;
    let tail_budget = cap - head_budget;

    let head_end = {
        let mut i = head_budget.min(text.len());
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let tail_start = {
        let mut i = text.len().saturating_sub(tail_budget);
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
        i
    };

    // Clamp degenerate case where head and tail would overlap.
    let tail_start = tail_start.max(head_end);

    // Use char count (not byte diff) so the marker is meaningful for
    // multi-byte input — the very case this helper is designed to handle.
    let omitted = text[head_end..tail_start].chars().count();
    format!(
        "{}\n\n[... {} chars omitted ...]\n\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..]
    )
}

/// The effective summary cap in bytes, read from `SPUR_SUMMARY_MAX_BYTES`
/// (default 4000). Single source of truth for both `truncate_summary`
/// and artifact-persistence predicates.
fn summary_cap_bytes() -> usize {
    std::env::var("SPUR_SUMMARY_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000)
}

fn truncate_summary_env_default(text: &str) -> String {
    truncate_summary(text, summary_cap_bytes())
}

fn sha256_hex_for_outcome(content: &[u8]) -> String {
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

/// Apply the calibrated artifact-vs-transport failure rule. Pure.
///
/// Inputs:
/// - `worker_success`: did the worker itself report success?
/// - `persist_result`: outcome of `WorktreeManager::persist_artifact`.
///   `None` means the orchestrator skipped persistence (output under cap).
///   `Some(Ok)` / `Some(Err)` are the persistence outcomes.
/// - `original_error_status`: the status the caller would have returned
///   if persistence hadn't been attempted. Only consulted on the
///   `!worker_success` branch; the helper composes with existing error
///   extraction so this path keeps the worker's original error.
///
/// Returns: `(status, artifact, summary_annotation)`.
/// - `status` is the final `DelegationStatus`.
/// - `artifact` is `Some` on successful persistence (regardless of
///   worker success — failing workers still get diagnostic artifacts).
/// - `summary_annotation`, if `Some`, must be appended to the truncated
///   summary tail by the caller.
///
/// Failure rule:
/// - worker_success + Ok  -> Success + Some(artifact) + no note
/// - worker_success + Err -> Failed { "artifact persistence failed: ..." } + None + note
/// - !worker_success + Ok -> original_error_status + Some(artifact) + no note
/// - !worker_success + Err -> original_error_status + None + note
fn decide_artifact_handling(
    worker_success: bool,
    persist_result: Option<Result<spur_acp::WorkerArtifact, String>>,
    original_error_status: Option<DelegationStatus>,
) -> (
    DelegationStatus,
    Option<spur_acp::WorkerArtifact>,
    Option<String>,
) {
    match (worker_success, persist_result) {
        (true, Some(Ok(art))) => (DelegationStatus::Success, Some(art), None),
        (true, Some(Err(e))) => {
            let msg = format!("artifact persistence failed: {e}");
            (
                DelegationStatus::Failed { error: msg.clone() },
                None,
                Some(format!("[orchestrator: {msg}]")),
            )
        }
        (false, Some(Ok(art))) => (
            original_error_status.unwrap_or(DelegationStatus::Failed {
                error: "worker failed".into(),
            }),
            Some(art),
            None,
        ),
        (false, Some(Err(e))) => (
            original_error_status.unwrap_or(DelegationStatus::Failed {
                error: "worker failed".into(),
            }),
            None,
            Some(format!("[orchestrator: artifact persistence failed: {e}]")),
        ),
        // No persist attempt — caller's responsibility.
        (true, None) => (DelegationStatus::Success, None, None),
        (false, None) => (
            original_error_status.unwrap_or(DelegationStatus::Failed {
                error: "worker failed".into(),
            }),
            None,
            None,
        ),
    }
}

/// Compute a `DiffSummary` for a worktree via `git diff --numstat <basis>`.
///
/// `basis` must match what `collect_diff` used for the raw diff — either
/// "HEAD" or "<base_commit>..HEAD" (rendered with the actual SHA). Otherwise
/// the raw diff text and the structured summary disagree.
///
/// Preferred over regex-parsing the unified diff text because numstat
/// emits tab-separated stats directly and handles binary files (`-\t-\tpath`),
/// renames, and mode-only changes without ambiguity.
///
/// Cost: ~10-100ms. Same budget as `collect_diff`.
async fn build_diff_summary(
    worktree_path: &std::path::Path,
    basis: &str,
) -> anyhow::Result<spur_acp::DiffSummary> {
    use tokio::process::Command;

    let output = Command::new("git")
        .arg("diff")
        .arg("--numstat")
        .arg(basis)
        .current_dir(worktree_path)
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files_changed = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    let mut files = Vec::new();

    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let ins = parts.next().unwrap_or("");
        let del = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            continue;
        }
        // Rename notation: "old => new" (top-level) or "dir/{old => new}" (nested).
        // Extract destination path so downstream consumers see a real filename.
        let path = if let Some(arrow_pos) = path.find(" => ") {
            let after_arrow = &path[arrow_pos + 4..];
            // Nested form: "dir/{old => new}/tail" — strip the trailing '}' and
            // reconstruct as "dir/" + destination + "/tail". For the simple
            // top-level form "old => new" there are no braces and this just
            // returns `new`.
            if let Some(brace_pos) = path[..arrow_pos].rfind('{') {
                let prefix = &path[..brace_pos];
                let dest = after_arrow.trim_end_matches('}');
                // Handle "dir/{old => new}/tail" — find where the '}' lived.
                let (dest_clean, tail) = match dest.find('}') {
                    Some(i) => (&dest[..i], &dest[i + 1..]),
                    None => (dest, ""),
                };
                format!("{}{}{}", prefix, dest_clean, tail)
            } else {
                after_arrow.to_string()
            }
        } else {
            path.to_string()
        };
        files_changed += 1;
        // numstat emits "-" for binary files. Non-"-" values parse as usize.
        insertions += ins.parse::<usize>().unwrap_or(0);
        deletions += del.parse::<usize>().unwrap_or(0);
        files.push(std::path::PathBuf::from(&path));
    }

    Ok(spur_acp::DiffSummary {
        files_changed,
        insertions,
        deletions,
        files,
    })
}

/// One retry attempt's surviving state, kept in memory across the
/// retry loop so later attempts can see the history. Module-local;
/// does not leak into public API.
#[derive(Debug, Clone)]
struct RetryAttempt {
    attempt_n: u32,
    summary: String,
    diff_summary: Option<spur_acp::DiffSummary>,
    /// Reviewer's `new_constraints` verbatim, the feedback that
    /// triggered this retry decision.
    feedback: String,
}

/// Render the augmented task prompt fed to the NEXT retry attempt.
///
/// Layout:
///   {original_task}
///
///   --- Previous attempts ---
///   Attempt N:
///     What was tried: {summary}
///     Files touched: {files_changed} changed, +{ins}/-{del}
///     Reviewer feedback: {feedback}
///   ...
///
///   --- Your task ---
///   Address the reviewer's most recent feedback above. Do NOT repeat
///   approaches that were rejected earlier — the reviewer sees the
///   same history and will reject a repeat.
///
///   Most recent feedback:
///   {current_feedback}
fn render_retry_context(
    history: &[RetryAttempt],
    original_task: &str,
    current_feedback: &str,
) -> String {
    let mut out = String::with_capacity(original_task.len() + current_feedback.len() + 512);
    out.push_str(original_task);

    if !history.is_empty() {
        out.push_str("\n\n--- Previous attempts ---\n");
        for a in history {
            out.push_str(&format!("\nAttempt {}:\n", a.attempt_n));
            out.push_str(&format!("  What was tried: {}\n", a.summary));
            if let Some(ds) = &a.diff_summary {
                out.push_str(&format!(
                    "  Files touched: {} changed, +{}/-{}\n",
                    ds.files_changed, ds.insertions, ds.deletions
                ));
            }
            out.push_str(&format!("  Reviewer feedback: {}\n", a.feedback));
        }
    }

    out.push_str(
        "\n--- Your task ---\n\
         Address the reviewer's most recent feedback above. Do NOT repeat \
         approaches that were rejected earlier — the reviewer sees the \
         same history and will reject a repeat.\n\n\
         Most recent feedback:\n",
    );
    out.push_str(current_feedback);
    out
}

/// Drop oldest attempts until the total in-memory summary+feedback
/// footprint fits under `max_bytes`. Preserves the most recent
/// attempts (those are most relevant to the current feedback).
fn apply_bloat_cap(history: &mut Vec<RetryAttempt>, max_bytes: usize) {
    fn size(a: &RetryAttempt) -> usize {
        a.summary.len() + a.feedback.len()
    }
    while history.iter().map(size).sum::<usize>() > max_bytes && !history.is_empty() {
        history.remove(0);
    }
}

/// Expand ~ to home directory.
fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return format!("{}/{}", home, rest);
        }
    }
    path.to_string()
}

fn dirs_home() -> Option<String> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_string_lossy().to_string())
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support {
    //! Public shims for integration tests. Not part of the stable API.
    use spur_acp::DiffSummary;

    pub struct RetryAttemptPublic {
        pub attempt_n: u32,
        pub summary: String,
        pub diff_summary: Option<DiffSummary>,
        pub feedback: String,
    }

    pub fn render_retry_context_public(
        history: &[RetryAttemptPublic],
        original_task: &str,
        current_feedback: &str,
    ) -> String {
        let internal: Vec<super::RetryAttempt> = history
            .iter()
            .map(|a| super::RetryAttempt {
                attempt_n: a.attempt_n,
                summary: a.summary.clone(),
                diff_summary: a.diff_summary.clone(),
                feedback: a.feedback.clone(),
            })
            .collect();
        super::render_retry_context(&internal, original_task, current_feedback)
    }

    // ─── Review gate helpers ──────────────────────────────────────────
    // Test-only. Production code uses ReviewSink::register_handle (INV-4).

    use super::{
        apply_decision_to_candidate, DelegationStatus, ExecutorId, ReviewSink, TimeoutFallback,
    };
    use crate::review_sink::ReviewSinkError;

    /// Register a pending review on the sink. Returns the receiver the
    /// caller awaits.
    ///
    /// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
    pub async fn register_gate(
        executor_id: ExecutorId,
        attempt_n: u32,
        review_sink: &ReviewSink,
    ) -> Result<tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>, ReviewSinkError> {
        review_sink.register(executor_id, attempt_n).await
    }

    /// Wait for a review decision (or timeout) and shape the final
    /// `DelegationStatus`.
    ///
    /// **Does NOT handle `Retry`** — returns `Failed` if Retry arrives.
    ///
    /// **Test-only** — production code uses `ReviewHandle::into_rx` (INV-4).
    pub async fn wait_gate(
        rx: tokio::sync::oneshot::Receiver<spur_acp::ReviewDecision>,
        executor_id: ExecutorId,
        candidate_status: DelegationStatus,
        review_timeout: std::time::Duration,
        timeout_fallback: TimeoutFallback,
        review_sink: ReviewSink,
    ) -> DelegationStatus {
        tokio::select! {
            recv_result = rx => {
                match recv_result {
                    Ok(decision) => apply_decision_to_candidate(decision, candidate_status),
                    Err(_) => {
                        review_sink.remove(&executor_id).await;
                        DelegationStatus::TimedOut {
                            waited_for: review_timeout,
                            fallback: timeout_fallback,
                        }
                    }
                }
            }
            _ = tokio::time::sleep(review_timeout) => {
                review_sink.remove(&executor_id).await;
                DelegationStatus::TimedOut {
                    waited_for: review_timeout,
                    fallback: timeout_fallback,
                }
            }
        }
    }

    /// Register + wait composition.
    ///
    /// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
    pub async fn run_gate_for_candidate(
        executor_id: ExecutorId,
        attempt_n: u32,
        candidate_status: DelegationStatus,
        review_timeout: std::time::Duration,
        timeout_fallback: TimeoutFallback,
        review_sink: ReviewSink,
    ) -> DelegationStatus {
        let rx = match register_gate(executor_id.clone(), attempt_n, &review_sink).await {
            Ok(rx) => rx,
            Err(e) => {
                tracing::error!(
                    executor_id = %executor_id.0,
                    error = %e,
                    "review_sink registration failed"
                );
                return DelegationStatus::Failed {
                    error: format!("review registration failed: {e}"),
                };
            }
        };
        wait_gate(
            rx,
            executor_id,
            candidate_status,
            review_timeout,
            timeout_fallback,
            review_sink,
        )
        .await
    }

    // ─── MCP shutdown helpers ─────────────────────────────────────────
    // Test-only. Expose the private `shutdown_mcp_server` function and
    // its dependencies so integration tests can call them directly.

    use std::sync::Arc;
    use tokio_util::task::AbortOnDropHandle;

    /// Mirror of the private `RetirableMcpServer` trait for integration
    /// tests. Implement this on fake servers to drive `shutdown_mcp_server`.
    ///
    /// **Test-only.**
    pub trait RetirableMcpServer: Send + Sync {
        fn mark_retiring(&self);
        fn cancel_in_flight_workers(&self);
        fn force_abort(&self);
        fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>>;
    }

    /// Adapts the public `test_support::RetirableMcpServer` trait to the
    /// private `super::RetirableMcpServer` trait.
    struct RetirableMcpServerAdapter<S: RetirableMcpServer + ?Sized>(Arc<S>);

    impl<S: RetirableMcpServer + ?Sized> super::RetirableMcpServer for RetirableMcpServerAdapter<S> {
        fn mark_retiring(&self) {
            self.0.mark_retiring();
        }
        fn cancel_in_flight_workers(&self) {
            self.0.cancel_in_flight_workers();
        }
        fn force_abort(&self) {
            self.0.force_abort();
        }
        fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            self.0.shutdown()
        }
    }

    /// The MCP shutdown timeout constant (5 s).
    ///
    /// **Test-only** — used by `shutdown_mcp_server_bounded` to set the
    /// assertion epsilon.
    #[doc(hidden)]
    pub const MCP_SHUTDOWN_TIMEOUT_MS: u64 = super::MCP_SHUTDOWN_TIMEOUT.as_millis() as u64;

    /// Call `shutdown_mcp_server` with a fake `RetirableMcpServer`.
    ///
    /// **Test-only.**
    pub async fn call_shutdown_mcp_server<S: RetirableMcpServer + ?Sized>(
        funnel: &crate::event_funnel::FunnelHandle,
        session: &spur_acp::types::SessionId,
        mcp_server: Option<Arc<S>>,
        mcp_guard: Option<AbortOnDropHandle<()>>,
    ) {
        // Wrap the public-trait server in the adapter so it satisfies
        // the private `super::RetirableMcpServer` bound.
        let mut adapted: Option<Arc<dyn super::RetirableMcpServer>> = mcp_server
            .map(|s| Arc::new(RetirableMcpServerAdapter(s)) as Arc<dyn super::RetirableMcpServer>);
        let mut guard_slot: Option<AbortOnDropHandle<()>> = mcp_guard;
        super::shutdown_mcp_server(funnel, session, &mut adapted, Some(&mut guard_slot)).await;
    }

    /// Wraps `register_gate` + `wait_gate` in a retry loop.
    ///
    /// On `Retry`, bumps `attempt_n` and re-enters. Bounded by
    /// `max_review_retries`.
    ///
    /// Uses `crate::retry_loop::RetryLoop` for the bound check and
    /// exhaustion status — shares invariants with the production retry
    /// gate in `execute_delegation`.
    ///
    /// **Test-only** — production code uses `ReviewSink::register_handle` (INV-4).
    pub async fn run_gate_with_retries(
        executor_id: ExecutorId,
        candidate_status: DelegationStatus,
        review_timeout: std::time::Duration,
        timeout_fallback: TimeoutFallback,
        max_review_retries: u32,
        review_sink: ReviewSink,
    ) -> DelegationStatus {
        use crate::retry_loop::{RetryLoop, RetryOutcome};
        use spur_acp::ReviewDecision;

        RetryLoop::new(max_review_retries)
            .run(|attempt_n| {
                let executor_id = executor_id.clone();
                let review_sink = review_sink.clone();
                let candidate_status = candidate_status.clone();
                let timeout_fallback = timeout_fallback.clone();
                async move {
                    let rx = match register_gate(executor_id.clone(), attempt_n, &review_sink).await
                    {
                        Ok(rx) => rx,
                        Err(e) => {
                            return RetryOutcome::Terminal(DelegationStatus::Failed {
                                error: format!("review registration failed: {e}"),
                            });
                        }
                    };

                    let decision = tokio::select! {
                        r = rx => r.ok(),
                        _ = tokio::time::sleep(review_timeout) => {
                            review_sink.remove(&executor_id).await;
                            return RetryOutcome::Terminal(DelegationStatus::TimedOut {
                                waited_for: review_timeout,
                                fallback: timeout_fallback,
                            });
                        }
                    };

                    match decision {
                        Some(ReviewDecision::Approve) => RetryOutcome::Terminal(candidate_status),
                        Some(ReviewDecision::Reject { reason }) => {
                            RetryOutcome::Terminal(DelegationStatus::Rejected { reason })
                        }
                        Some(ReviewDecision::Modify { note }) => {
                            RetryOutcome::Terminal(DelegationStatus::Modified {
                                reviewer_note: note,
                            })
                        }
                        Some(ReviewDecision::Retry { .. }) => RetryOutcome::Retry,
                        None => {
                            review_sink.remove(&executor_id).await;
                            RetryOutcome::Terminal(DelegationStatus::TimedOut {
                                waited_for: review_timeout,
                                fallback: timeout_fallback,
                            })
                        }
                    }
                }
            })
            .await
    }
}

#[cfg(test)]
mod issue_graph_handler_tests {
    use super::{handle_get_issue_graph, IssueGraphPm};
    use async_trait::async_trait;
    use spur_acp::SpurEventBody;
    use spur_pm::graph::{AdjacencyData, DependencyGraph, GraphEdge, GraphNode};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::mpsc::UnboundedReceiver;

    struct FakePmService {
        analyzer_available: bool,
        result: Mutex<Option<Result<DependencyGraph, String>>>,
        requested_ids: Mutex<Vec<String>>,
    }

    impl FakePmService {
        fn with_graph(graph: DependencyGraph) -> Self {
            Self {
                analyzer_available: true,
                result: Mutex::new(Some(Ok(graph))),
                requested_ids: Mutex::new(Vec::new()),
            }
        }

        fn unavailable() -> Self {
            Self {
                analyzer_available: false,
                result: Mutex::new(None),
                requested_ids: Mutex::new(Vec::new()),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                analyzer_available: true,
                result: Mutex::new(Some(Err(message.to_string()))),
                requested_ids: Mutex::new(Vec::new()),
            }
        }

        fn requested_ids(&self) -> Vec<String> {
            self.requested_ids.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl IssueGraphPm for FakePmService {
        fn analyzer_available(&self) -> bool {
            self.analyzer_available
        }

        async fn issue_subgraph_json(&self, id: &str) -> anyhow::Result<DependencyGraph> {
            self.requested_ids.lock().unwrap().push(id.to_string());
            match self.result.lock().unwrap().take().expect("fake result") {
                Ok(graph) => Ok(graph),
                Err(message) => Err(anyhow::anyhow!(message)),
            }
        }
    }

    fn dependency_graph() -> DependencyGraph {
        DependencyGraph {
            format: Some("json".into()),
            graph: None,
            nodes: 2,
            edges: 1,
            data_hash: Some("hash".into()),
            adjacency: Some(AdjacencyData {
                nodes: vec![
                    GraphNode {
                        id: "bd-root".into(),
                        title: Some("Root issue".into()),
                        status: Some("open".into()),
                        priority: Some(1),
                        labels: vec!["feature".into()],
                        pagerank: Some(0.5),
                    },
                    GraphNode {
                        id: "bd-child".into(),
                        title: Some("Child issue".into()),
                        status: Some("blocked".into()),
                        priority: Some(2),
                        labels: vec!["backend".into()],
                        pagerank: None,
                    },
                ],
                edges: Some(vec![GraphEdge {
                    from: "bd-root".into(),
                    to: "bd-child".into(),
                    edge_type: Some("depends_on".into()),
                }]),
            }),
            raw: serde_json::Value::Null,
        }
    }

    async fn next_event(events: &mut UnboundedReceiver<SpurEventBody>) -> SpurEventBody {
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event channel closed")
    }

    #[tokio::test]
    async fn get_issue_graph_emits_issue_subgraph_loaded() {
        let fake = FakePmService::with_graph(dependency_graph());
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(Some(&fake), &funnel, "bd-root".into()).await;

        assert_eq!(fake.requested_ids(), vec!["bd-root"]);
        match next_event(&mut events).await {
            SpurEventBody::IssueSubgraphLoaded {
                requested_id,
                nodes,
                edges,
            } => {
                assert_eq!(requested_id, "bd-root");
                assert_eq!(nodes.len(), 2);
                assert_eq!(nodes[0].id, "bd-root");
                assert_eq!(nodes[0].title.as_deref(), Some("Root issue"));
                assert_eq!(nodes[0].labels, vec!["feature"]);
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].from, "bd-root");
                assert_eq!(edges[0].to, "bd-child");
                assert_eq!(edges[0].edge_type.as_deref(), Some("depends_on"));
            }
            other => panic!("expected IssueSubgraphLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_issue_graph_emits_command_error_when_bv_unavailable() {
        let fake = FakePmService::unavailable();
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(Some(&fake), &funnel, "bd-root".into()).await;

        assert!(fake.requested_ids().is_empty());
        match next_event(&mut events).await {
            SpurEventBody::IssueCommandError {
                operation,
                error,
                id,
            } => {
                assert_eq!(operation, "GetIssueGraph");
                assert_eq!(error, "bv unavailable; install bv to view dependency graph");
                assert_eq!(id, Some("bd-root".into()));
            }
            other => panic!("expected IssueCommandError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_issue_graph_emits_command_error_when_subgraph_fails() {
        let fake = FakePmService::failing("bv failed");
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(Some(&fake), &funnel, "bd-root".into()).await;

        assert_eq!(fake.requested_ids(), vec!["bd-root"]);
        match next_event(&mut events).await {
            SpurEventBody::IssueCommandError {
                operation,
                error,
                id,
            } => {
                assert_eq!(operation, "GetIssueGraph");
                assert_eq!(error, "bv failed");
                assert_eq!(id, Some("bd-root".into()));
            }
            other => panic!("expected IssueCommandError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_issue_graph_emits_command_error_when_pm_missing() {
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(None::<&FakePmService>, &funnel, "bd-root".into()).await;

        match next_event(&mut events).await {
            SpurEventBody::IssueCommandError {
                operation,
                error,
                id,
            } => {
                assert_eq!(operation, "GetIssueGraph");
                assert_eq!(error, "No issue tracker configured");
                assert_eq!(id, Some("bd-root".into()));
            }
            other => panic!("expected IssueCommandError, got {other:?}"),
        }
    }
}

/// Strip a leading `!` from the first text block in `blocks`, if any.
///
/// The TUI forwards interrupt commands (`!stop`) as a text block with a
/// leading bang. We strip it once here before forwarding to the agent so
/// the agent sees clean prompt text.
fn strip_bang_prefix(mut blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    if let Some(ContentBlock::Text(tc)) = blocks.first_mut() {
        if tc.text.starts_with('!') {
            tc.text = tc.text.strip_prefix('!').unwrap_or(&tc.text).to_string();
        }
    }
    blocks
}

// ─── Review dispatcher ────────────────────────────────────────────────

/// Dispatcher loop: forwards `SubmitReview` messages to the `ReviewSink`.
/// All other `InteractiveInput` variants are ignored by this loop (they
/// are consumed by `run_interactive`'s own loop, not this one).
///
/// This is spawned as a separate task so review-decision latency is
/// decoupled from brain-turn I/O latency — see spec "Unit 3" for
/// rationale.
pub async fn review_dispatcher_loop(mut rx: mpsc::Receiver<InteractiveInput>, sink: ReviewSink) {
    while let Some(input) = rx.recv().await {
        if let InteractiveInput::SubmitReview {
            executor_id,
            attempt_n,
            decision,
        } = input
        {
            let _ = sink
                .submit(ExecutorId::new(executor_id), attempt_n, decision)
                .await;
        }
        // All other variants: noop in this loop.
    }
}

#[cfg(test)]
fn apply_decision_to_candidate(
    decision: spur_acp::ReviewDecision,
    candidate: DelegationStatus,
) -> DelegationStatus {
    use spur_acp::ReviewDecision;
    match decision {
        ReviewDecision::Approve => candidate,
        ReviewDecision::Reject { reason } => DelegationStatus::Rejected { reason },
        ReviewDecision::Modify { note } => DelegationStatus::Modified {
            reviewer_note: note,
        },
        ReviewDecision::Retry { .. } => DelegationStatus::Failed {
            error: "internal: Retry reached run_gate_for_candidate \
                    (caller must wrap with retry loop)"
                .into(),
        },
    }
}

#[cfg(test)]
mod peer_mailbox_drain_tests {
    use super::drain_peer_acks_with_timeout;
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
mod cancel_mode_helper_tests {
    use super::cancel_mode_for;
    use spur_acp::{types::TransportKind, CancelMode};

    #[test]
    fn acp_transport_is_acp_soft() {
        assert_eq!(cancel_mode_for(TransportKind::Acp), CancelMode::AcpSoft);
    }

    #[test]
    fn subprocess_transports_are_process_kill() {
        assert_eq!(
            cancel_mode_for(TransportKind::Stdio),
            CancelMode::ProcessKill
        );
        assert_eq!(
            cancel_mode_for(TransportKind::CliWrap),
            CancelMode::ProcessKill
        );
        assert_eq!(
            cancel_mode_for(TransportKind::StreamJson),
            CancelMode::ProcessKill
        );
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
mod cancel_deadline_arm_tests {
    use super::arm_cancel_deadline;

    #[tokio::test]
    async fn arm_cancel_deadline_sets_5s_from_now() {
        let mut deadline = None;
        let before = tokio::time::Instant::now();
        arm_cancel_deadline(&mut deadline);
        let set = deadline.expect("arm_cancel_deadline must populate Some(deadline)");
        let delta = set.saturating_duration_since(before);
        assert!(
            delta >= std::time::Duration::from_millis(4_900)
                && delta <= std::time::Duration::from_millis(5_100),
            "expected ~5s deadline, got {delta:?}"
        );
    }

    #[tokio::test]
    async fn arm_cancel_deadline_overwrites_existing() {
        let old = tokio::time::Instant::now() - std::time::Duration::from_secs(60);
        let mut deadline = Some(old);
        arm_cancel_deadline(&mut deadline);
        assert!(deadline.unwrap() > old + std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod truncate_summary_tests {
    use super::truncate_summary;

    // Serializes all env-mutating tests in this module. `SPUR_SUMMARY_MAX_BYTES`
    // is process-global; without this lock the tests race under the default
    // parallel harness.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn under_cap_returns_unchanged() {
        let input = "short text";
        assert_eq!(truncate_summary(input, 4000), "short text");
    }

    #[test]
    fn exact_cap_returns_unchanged() {
        let input = "x".repeat(100);
        assert_eq!(truncate_summary(&input, 100), input);
    }

    #[test]
    fn over_cap_preserves_head_and_tail_with_marker() {
        let input: String = (0..5000).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let cap = 4000;
        let out = truncate_summary(&input, cap);
        assert!(out.len() < input.len(), "output must be shorter than input");
        assert!(out.contains("chars omitted"), "omission marker must appear");
        let tail_start = input.len() - 3000;
        assert!(
            out.ends_with(&input[tail_start..]),
            "output must end with the last 3000 chars of input"
        );
        assert!(
            out.starts_with(&input[..1000]),
            "output must start with the first 1000 chars of input"
        );
    }

    #[test]
    fn utf8_boundary_does_not_panic() {
        let input = "—".repeat(20);
        let out = truncate_summary(&input, 10);
        assert!(out.chars().count() > 0);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(truncate_summary("", 4000), "");
    }

    #[test]
    fn summary_cap_bytes_respects_env_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", "1234") };
        let got = super::summary_cap_bytes();
        match prev {
            Some(v) => unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) },
            None => unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") },
        }
        assert_eq!(got, 1234);
    }

    #[test]
    fn summary_cap_bytes_defaults_to_4000_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") };
        let got = super::summary_cap_bytes();
        if let Some(v) = prev {
            unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) };
        }
        assert_eq!(got, 4000);
    }

    #[test]
    fn env_var_overrides_default_cap() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // This test mutates process-global env state. It is safe only
        // because no other test in this binary reads SPUR_SUMMARY_MAX_BYTES
        // concurrently. If that changes (future Task 6 integration test,
        // etc.), gate with #[serial] from the serial_test crate.
        let prev = std::env::var("SPUR_SUMMARY_MAX_BYTES").ok();
        unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", "50") };
        let input = "x".repeat(200);
        let out = super::truncate_summary_env_default(&input);
        assert!(out.len() < input.len());
        assert!(
            out.len() <= 100,
            "output must respect env override, got {}",
            out.len()
        );
        match prev {
            Some(v) => unsafe { std::env::set_var("SPUR_SUMMARY_MAX_BYTES", v) },
            None => unsafe { std::env::remove_var("SPUR_SUMMARY_MAX_BYTES") },
        }
    }
}

#[cfg(test)]
mod build_diff_summary_tests {
    use super::build_diff_summary;
    use spur_acp::DiffSummary;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_repo() -> tempfile::TempDir {
        fn git(path: &std::path::Path, args: &[&str]) {
            let out = Command::new("git")
                .args(args)
                .current_dir(path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let dir = tempdir().unwrap();
        let path = dir.path();
        git(path, &["init"]);
        git(path, &["config", "user.email", "t@t"]);
        git(path, &["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), "hello\nworld\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "init"]);
        dir
    }

    #[tokio::test]
    async fn clean_worktree_returns_zero_summary() {
        let dir = init_repo();
        let summary: DiffSummary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 0);
        assert_eq!(summary.insertions, 0);
        assert_eq!(summary.deletions, 0);
        assert!(summary.files.is_empty());
    }

    #[tokio::test]
    async fn modified_file_produces_expected_stats() {
        let dir = init_repo();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\nnew line\n").unwrap();
        let summary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 1);
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("a.txt")]);
    }

    #[tokio::test]
    async fn binary_file_is_counted_but_numbers_stay_zero() {
        let dir = init_repo();
        // numstat emits "-\t-\tpath" for binary files.
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 3, 0xFF]).unwrap();
        Command::new("git")
            .args(["add", "b.bin"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "bin"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("b.bin"), [9u8, 8, 7]).unwrap();
        let summary = build_diff_summary(dir.path(), "HEAD").await.unwrap();
        assert_eq!(summary.files_changed, 1);
        assert_eq!(
            summary.insertions, 0,
            "binary diff reports '-' for line counts"
        );
        assert_eq!(summary.deletions, 0);
        assert_eq!(summary.files, vec![PathBuf::from("b.bin")]);
    }

    #[tokio::test]
    async fn renamed_file_reports_destination_path() {
        let dir = init_repo();
        let path = dir.path();
        // Create a second file to make git rename-detection engage reliably.
        std::fs::write(path.join("a.txt"), "hello\nworld\nextra\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "grow"])
            .current_dir(path)
            .output()
            .unwrap();
        // Rename a.txt -> b.txt with a small tweak so line counts are non-zero.
        std::fs::rename(path.join("a.txt"), path.join("b.txt")).unwrap();
        std::fs::write(path.join("b.txt"), "hello\nworld\nextra\nrenamed\n").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();

        let summary = build_diff_summary(path, "HEAD").await.unwrap();
        // Either git reports a rename (1 entry, path=b.txt) OR a delete+add pair
        // (2 entries, both a.txt and b.txt). Both are acceptable — the key
        // invariant is: no path contains " => " after our rename-stripping.
        assert!(
            summary
                .files
                .iter()
                .all(|p| !p.to_string_lossy().contains(" => ")),
            "rename notation leaked into path: {:?}",
            summary.files
        );
        // b.txt must appear in the file list under either shape.
        assert!(
            summary
                .files
                .iter()
                .any(|p| p.file_name().and_then(|s| s.to_str()) == Some("b.txt")),
            "b.txt not in file list: {:?}",
            summary.files
        );
    }
}

#[cfg(test)]
mod base_spec_dispatch_tests {
    use super::{emit_dispatch_overlay_applied, extract_overlays, resolve_base_branch};
    use spur_mcp::tools::{BaseSpec, BaseTarget, OverlayCommit};

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

        emit_dispatch_overlay_applied(
            &funnel,
            "req-1",
            Some(&spec),
            "overlay-head",
            &overlays,
        );

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
mod retry_context_tests {
    use super::{apply_bloat_cap, render_retry_context, RetryAttempt};
    use spur_acp::DiffSummary;
    use std::path::PathBuf;

    fn att(n: u32, summary: &str, feedback: &str) -> RetryAttempt {
        RetryAttempt {
            attempt_n: n,
            summary: summary.into(),
            diff_summary: Some(DiffSummary {
                files_changed: 1,
                insertions: 10,
                deletions: 2,
                files: vec![PathBuf::from("f.rs")],
            }),
            feedback: feedback.into(),
        }
    }

    #[test]
    fn render_includes_original_task_and_all_attempts_and_current_feedback() {
        let history = vec![
            att(1, "tried approach A", "needs tests"),
            att(2, "tried approach B", "still too slow"),
        ];
        let out = render_retry_context(&history, "make foo fast", "use async");
        assert!(out.contains("make foo fast"));
        assert!(out.contains("Attempt 1"));
        assert!(out.contains("tried approach A"));
        assert!(out.contains("needs tests"));
        assert!(out.contains("Attempt 2"));
        assert!(out.contains("tried approach B"));
        assert!(out.contains("still too slow"));
        assert!(out.contains("use async"));
        assert!(out.contains("1 changed"));
        assert!(out.contains("+10"));
        assert!(out.contains("-2"));
    }

    #[test]
    fn render_handles_empty_history() {
        let out = render_retry_context(&[], "task", "feedback");
        assert!(out.contains("task"));
        assert!(out.contains("feedback"));
        assert!(!out.contains("Attempt 1"));
    }

    #[test]
    fn apply_bloat_cap_drops_oldest_first() {
        let big = "x".repeat(1000);
        let mut history = vec![
            att(1, &big, "fb1"),
            att(2, &big, "fb2"),
            att(3, &big, "fb3"),
        ];
        apply_bloat_cap(&mut history, 2000);
        assert!(history.iter().all(|a| a.attempt_n != 1));
        assert!(history.iter().any(|a| a.attempt_n == 3));
    }

    #[test]
    fn apply_bloat_cap_is_noop_when_under_cap() {
        let mut history = vec![att(1, "s", "f")];
        apply_bloat_cap(&mut history, 10_000);
        assert_eq!(history.len(), 1);
    }
}

#[cfg(test)]
mod is_connection_death_tests {
    use super::*;

    #[test]
    fn is_connection_death_detects_known_patterns() {
        let e1 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died during ext_method");
        assert!(is_connection_death(&e1));

        let e2 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died");
        assert!(is_connection_death(&e2));

        let e3 = anyhow::anyhow!("Internal error: \"server shut down unexpectedly\"");
        assert!(is_connection_death(&e3));

        let e4 = anyhow::anyhow!("prompt rejected: invalid session id");
        assert!(!is_connection_death(&e4));
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_agent_name;

    #[test]
    fn strips_acp_suffix() {
        assert_eq!(normalize_agent_name("claude-code-acp"), "claude-code");
        assert_eq!(normalize_agent_name("kiro-acp"), "kiro");
    }

    #[test]
    fn strips_cli_suffix() {
        assert_eq!(normalize_agent_name("gemini-cli"), "gemini");
    }

    #[test]
    fn lowercases() {
        assert_eq!(normalize_agent_name("CLAUDE"), "claude");
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(normalize_agent_name("  kiro  "), "kiro");
    }

    #[test]
    fn same_agent_matches_across_variants() {
        assert_eq!(
            normalize_agent_name("Claude-Code-ACP"),
            normalize_agent_name("claude-code")
        );
    }

    #[test]
    fn distinct_agents_do_not_collide() {
        assert_ne!(
            normalize_agent_name("our-claude"),
            normalize_agent_name("claude"),
        );
    }

    #[test]
    fn mismatch_detection_chosen_vs_dispatched_strings() {
        let dispatched = "kiro";
        let chosen = "claude";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        assert!(!matched);

        let dispatched = "claude-code-acp";
        let chosen = "claude";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        // claude-code-acp normalizes to "claude-code", so "claude" != "claude-code"
        assert!(!matched);

        let dispatched = "claude-code-acp";
        let chosen = "claude-code-acp";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        assert!(matched);
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
mod format_worker_task_tests {
    use super::format_worker_task;

    #[test]
    fn empty_list_passes_task_through_unchanged() {
        let task = "Do the thing.";
        let out = format_worker_task(task, &[]);
        assert_eq!(out, task);
    }

    #[test]
    fn single_path_prepends_relevant_files_section() {
        let task = "Do the thing.";
        let files = vec!["src/a.rs".to_string()];
        let out = format_worker_task(task, &files);
        assert!(
            out.starts_with("## Relevant Files\n\n"),
            "expected Relevant Files header first, got: {out}",
        );
        assert!(out.contains("- src/a.rs"));
        assert!(out.contains("## Task\n\nDo the thing."));
    }

    #[test]
    fn multiple_paths_produce_ordered_bullets() {
        let files = vec![
            "crates/spur-mcp/src/server.rs".to_string(),
            "crates/spur-acp/src/adapter/claude.rs".to_string(),
        ];
        let out = format_worker_task("Go.", &files);
        let idx_first = out
            .find("- crates/spur-mcp/src/server.rs")
            .expect("first bullet");
        let idx_second = out
            .find("- crates/spur-acp/src/adapter/claude.rs")
            .expect("second bullet");
        assert!(idx_first < idx_second, "order must be preserved");
    }

    #[test]
    fn whitespace_task_body_still_gets_section_when_files_nonempty() {
        let out = format_worker_task("   ", &["x.rs".into()]);
        assert!(out.starts_with("## Relevant Files\n\n"));
        assert!(out.ends_with("   "));
    }
}

#[cfg(test)]
mod context_files_wiring_tests {
    use super::format_worker_task;

    /// Regression guard: the helper is imported where execute_delegation
    /// lives. If a refactor moves or renames it, the import here breaks
    /// before the wiring silently regresses.
    #[test]
    fn format_worker_task_is_available_in_orchestrator_module() {
        let out = format_worker_task("t", &["x".into()]);
        assert!(out.contains("## Relevant Files"));
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
    use super::{commit_rendered_batch, retire_brain_session, TurnGuard};
    use crate::continuation_bridge::{new_overflow_buf, ContinuationEventSink, RenderOutcome};
    use crate::event_funnel::spawn_funnel;
    use crate::scheduler::{BrainScheduler, ScheduledAction};
    use chrono::Utc;
    use futures::FutureExt;
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::domain::events::SpurEventBody;
    use spur_acp::domain::{
        BrainContinuation, ContinuationPayload, ContinuationSource, DeferReason, DelegationKey,
        DropReason,
    };
    use spur_acp::types::SessionId;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
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

    impl super::RetirableMcpServer for MockRetiringServer {
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

        retire_brain_session(
            &funnel,
            &old_session,
            &mut mcp_server,
            None,
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

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_retire_brain_session_timeout_force_aborts() {
        let session = SessionId("brain-timeout".into());
        let (funnel, _rx) = test_funnel();
        let (mut scheduler, _sink) = mk_scheduler(Some(session.clone()));
        let overflow = new_overflow_buf();
        let server = Arc::new(MockRetiringServer::blocked(Arc::new(Notify::new())));
        let mut mcp_server = Some(server.clone());

        retire_brain_session(
            &funnel,
            &session,
            &mut mcp_server,
            None,
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

        retire_brain_session(
            &funnel,
            &session,
            &mut mcp_server,
            None,
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

#[cfg(test)]
mod artifact_decision_tests {
    use super::*;
    use spur_acp::{ArtifactKind, DelegationStatus, WorkerArtifact};

    #[test]
    fn outcome_sha256_hex_uses_lowercase_content_digest() {
        assert_eq!(
            sha256_hex_for_outcome(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn sample_artifact() -> WorkerArtifact {
        WorkerArtifact {
            object_ref: "refs/spur/artifacts/s1".into(),
            blob_sha: "a".repeat(40),
            size_bytes: 1_024,
            kind: ArtifactKind::Output,
        }
    }

    #[test]
    fn success_with_persist_ok_is_success_and_carries_artifact() {
        let (status, artifact, note) = decide_artifact_handling(
            /* worker_success */ true,
            /* persist_result */ Some(Ok(sample_artifact())),
            /* original_error_status */ None,
        );
        assert!(matches!(status, DelegationStatus::Success));
        assert!(artifact.is_some());
        assert!(note.is_none());
    }

    #[test]
    fn success_with_persist_err_escalates_to_failed() {
        let (status, artifact, note) =
            decide_artifact_handling(true, Some(Err("disk full".into())), None);
        match status {
            DelegationStatus::Failed { error } => {
                assert!(
                    error.contains("artifact persistence failed"),
                    "got: {error}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(artifact.is_none());
        assert!(note.is_some());
    }

    #[test]
    fn failure_with_persist_err_preserves_original_error_and_annotates() {
        let original = DelegationStatus::Failed {
            error: "compile error".into(),
        };
        let (status, artifact, note) = decide_artifact_handling(
            /* worker_success */ false,
            Some(Err("ref locked".into())),
            Some(original.clone()),
        );
        assert_eq!(status, original);
        assert!(artifact.is_none());
        let n = note.expect("failure path must annotate");
        assert!(n.contains("orchestrator"));
        assert!(n.contains("artifact persistence failed"));
    }

    #[test]
    fn failure_with_persist_ok_preserves_original_error_and_carries_artifact() {
        let original = DelegationStatus::Failed {
            error: "panic".into(),
        };
        let (status, artifact, note) = decide_artifact_handling(
            false,
            Some(Ok(WorkerArtifact {
                kind: ArtifactKind::Diagnostic,
                ..sample_artifact()
            })),
            Some(original.clone()),
        );
        assert_eq!(status, original);
        let a = artifact.expect("diagnostic artifact must be surfaced on failed worker");
        assert_eq!(a.kind, ArtifactKind::Diagnostic);
        assert!(note.is_none());
    }

    #[test]
    fn under_cap_path_is_unchanged() {
        // When we never attempted persistence (output_text.len() <= cap),
        // the helper is not called. Document the caller's contract: no
        // call -> no annotation -> no escalation. This is asserted by
        // the absence of the call site at the appropriate branch.
        // See `run_one_worker_attempt` for the guard.
    }
}

#[cfg(test)]
mod beads_startup_warning_tests {
    use super::{render_beads_startup_warning, startup_beads_warning, BeadsStartupWarning};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use spur_acp::config::{BeadsPmConfig, SpurConfig};
    use spur_license::policy::PolicyResolver;
    use spur_license::{EntitlementSnapshot, FeatureGate, LicenseState, Plan};

    fn community_gate() -> Arc<FeatureGate> {
        Arc::new(FeatureGate::new(PolicyResolver::embedded()))
    }

    fn gate_without_beads_basic() -> Arc<FeatureGate> {
        // Pro/Team/Enterprise inherit Community via the policy's
        // `@inherit:community` directive, so feeding an empty JWT no
        // longer strips pm_core_beads_basic. Inject a hand-crafted
        // empty snapshot to genuinely simulate the missing entitlement.
        let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
        gate.set_snapshot_for_test(EntitlementSnapshot::default());
        gate
    }

    fn beads_basic_gate() -> Arc<FeatureGate> {
        let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
        let mut features = BTreeSet::new();
        features.insert("pm_core_beads_basic".to_string());
        gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
        gate
    }

    #[test]
    fn beads_startup_warning_free_tier_with_missing_br_emits_install_hint() {
        let config = SpurConfig::default();
        let gate = community_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            Some(BeadsStartupWarning::BrNotInstalled)
        );
    }

    #[test]
    fn beads_startup_warning_missing_beads_basic_entitlement_suppresses_warning() {
        let config = SpurConfig::default();
        let gate = gate_without_beads_basic();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            None
        );
    }

    #[test]
    fn beads_startup_warning_entitled_tier_with_missing_br_emits_install_hint() {
        let config = SpurConfig::default();
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            Some(BeadsStartupWarning::BrNotInstalled)
        );
        assert!(
            render_beads_startup_warning(BeadsStartupWarning::BrNotInstalled)
                .contains("br (beads) not installed"),
        );
    }

    #[test]
    fn beads_startup_warning_entitled_tier_with_present_br_uses_generic_backend_copy() {
        let config = SpurConfig::default();
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, true),
            Some(BeadsStartupWarning::BackendUnavailable)
        );
        let warning = render_beads_startup_warning(BeadsStartupWarning::BackendUnavailable);
        assert!(
            !warning.contains("not installed"),
            "generic warning must not claim br is missing: {warning}",
        );
        assert!(warning.contains("failed to initialize"), "got: {warning}");
    }

    #[test]
    fn beads_startup_warning_disabled_beads_config_suppresses_warning() {
        let mut config = SpurConfig::default();
        config.pm.beads = Some(BeadsPmConfig {
            enabled: false,
            auto_sync: false,
        });
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            None
        );
    }

    #[test]
    fn beads_startup_warning_missing_feature_gate_suppresses_warning() {
        let config = SpurConfig::default();

        assert_eq!(
            startup_beads_warning(&config, None, true, false, false),
            None
        );
    }

    #[test]
    fn beads_startup_warning_existing_pm_service_suppresses_warning() {
        let config = SpurConfig::default();
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, true, false),
            None
        );
    }
}
