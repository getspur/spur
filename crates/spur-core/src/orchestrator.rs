use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::task::AbortOnDropHandle;
use tracing::{debug, error, info, warn};

use spur_acp::config::{SpurConfig, WorktreeConfig};
use spur_acp::connection::AgentConnection;
use spur_acp::registry::AgentRegistry;
use spur_acp::session_lock::{AcquireOutcome, SessionAttachGuard};
use spur_acp::types::*;
use spur_acp::{
    CancellationControl, DelegationAbortHandle, DelegationAbortReason, DelegationDispatchError,
    DelegationResult, DelegationStatus, LifecycleState, ReviewKind, ReviewPayload, SpurEvent,
    SpurEventBody, TimeoutFallback,
};
use spur_pm::Issue;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, McpServer, McpServerHttp, PromptRequest, ProtocolVersion,
    SessionUpdate, SetSessionModeRequest, TextContent,
};

use spur_blob_store::{
    ContentType, MeasuredOutcomeStore, OutcomeKey, OutcomeMetadata, OutcomeStore,
};
use spur_cost::CostTracker;
use spur_license::SpurLicense;
use spur_mcp::tools::BaseSpec;
use spur_mcp::worker_server::WorkerMcpServer;
use spur_mcp::{
    build_worker_info, DelegationChannel, DelegationRequest, McpCallbackServer, WorkerInfo,
};

use dashmap::DashMap;
use spur_pm::PmService;
use spur_worktree::git_blob_store::GitBlobOutcomeStore;
use spur_worktree::{manager::WorktreeError, WorktreeManager};

use crate::lineage::ExecutorId;
use crate::review_sink::ReviewSink;
use crate::scheduler::TurnGuard;

mod codex_discovery;
pub mod connection;
mod delegation;
pub mod input;
mod plan_ops;
mod pm_bridge;
pub mod prompt;
mod review;
pub mod types;
mod util;
mod worker_mcp;

use codex_discovery::filter_sessions_for_repo;
pub use delegation::cleanup::{should_commit_worker_diff, should_preserve_worktree};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use delegation::execute::{render_retry_context, RetryAttempt};
use input::strip_bang_prefix;
pub use input::InteractiveInput;
use plan_ops::load_plan_summaries;
use pm_bridge::{handle_get_issue_graph, issue_to_detail_event, refresh_pm_state};
#[cfg(any(test, feature = "test-support"))]
use review::apply_decision_to_candidate;
pub use review::{cleanup_cancelled_review, review_dispatcher_loop};
pub use types::{
    ActiveConnection, BrainSession, FaultInjectionHooks, LoadBrainSessionError, ReconnectError,
    RunOpts, RunResult,
};
pub use util::normalize_agent_name;
use util::{
    arm_cancel_deadline, binary_on_path, cancel_mode_for, format_error_chain, is_connection_death,
    reconnect_failure_event, render_beads_startup_warning, shellexpand_tilde,
    startup_beads_warning,
};
use worker_mcp::{build_worker_mcp_servers_with, WorkerMcpFetcher};

type McpGuarded<T> = (T, AbortOnDropHandle<()>);
type BrainRunBootstrap = (
    Box<dyn spur_acp::AgentConnection>,
    JoinHandle<()>,
    bool,
    Option<String>,
    SessionId,
);
type NewBrainSessionBootstrap = (
    spur_acp::config::AgentConfig,
    Option<tokio::sync::broadcast::Receiver<spur_acp::SessionNotification>>,
    agent_client_protocol::schema::NewSessionResponse,
    spur_acp::BrainSessionId,
    SessionId,
);
type LoadedBrainSessionBootstrap = (
    spur_acp::config::AgentConfig,
    Option<tokio::sync::broadcast::Receiver<spur_acp::SessionNotification>>,
    String,
    Option<std::pin::Pin<Box<dyn futures::Stream<Item = spur_acp::SessionNotification> + Send>>>,
    bool,
    spur_acp::LoadOutcome,
    spur_acp::BrainSessionId,
    SessionId,
);

const MAX_SESSION_LIST_PAGES: usize = 1000;
const MAX_SESSION_LIST_SESSIONS: usize = 100_000;

#[cfg(test)]
mod session_attach_guard_transfer_tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

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

    struct RecordingCancelConnection {
        cancelled_sessions: Arc<Mutex<Vec<String>>>,
        fail_cancel: bool,
    }

    #[async_trait]
    impl AgentConnection for RecordingCancelConnection {
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

        async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
            self.cancelled_sessions
                .lock()
                .expect("cancel recorder poisoned")
                .push(session_id.to_string());
            if self.fail_cancel {
                return Err(anyhow::anyhow!("cancel not supported"));
            }
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

    #[tokio::test]
    async fn retire_active_brain_dispatches_one_best_effort_cancel_for_acp_transport() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        config.agents.entries = vec![spur_acp::AgentConfig::with_defaults("test-brain")];
        let mut orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

        let cancelled_sessions = Arc::new(Mutex::new(Vec::new()));
        let mut brain = Some(BrainSession {
            connection: Box::new(RecordingCancelConnection {
                cancelled_sessions: Arc::clone(&cancelled_sessions),
                fail_cancel: false,
            }),
            acp_session_id: "acp-retired-session".to_string(),
            spur_session_id: SessionId("spur-retired-session".to_string()),
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
                spur_acp::domain::events::BrainRetireReason::UserClear,
                None,
            )
            .await;

        assert_eq!(
            *cancelled_sessions.lock().expect("cancel recorder poisoned"),
            vec!["acp-retired-session".to_string()]
        );

        let mut active = active.expect("retired brain should cache active connection");
        active.transport.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retire_active_brain_skips_cancel_for_process_kill_transport() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        let mut agent = spur_acp::AgentConfig::with_defaults("test-brain");
        agent.transport = TransportKind::CliWrap;
        config.agents.entries = vec![agent];
        let mut orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

        let cancelled_sessions = Arc::new(Mutex::new(Vec::new()));
        let mut brain = Some(BrainSession {
            connection: Box::new(RecordingCancelConnection {
                cancelled_sessions: Arc::clone(&cancelled_sessions),
                fail_cancel: false,
            }),
            acp_session_id: "acp-retired-session".to_string(),
            spur_session_id: SessionId("spur-retired-session".to_string()),
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
                spur_acp::domain::events::BrainRetireReason::UserClear,
                None,
            )
            .await;

        assert!(
            cancelled_sessions
                .lock()
                .expect("cancel recorder poisoned")
                .is_empty(),
            "process-kill transports must not be cancelled during retire"
        );

        let mut active = active.expect("retired brain should cache active connection");
        active.transport.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retire_active_brain_swallows_best_effort_cancel_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        config.agents.entries = vec![spur_acp::AgentConfig::with_defaults("test-brain")];
        let mut orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();

        let cancelled_sessions = Arc::new(Mutex::new(Vec::new()));
        let mut brain = Some(BrainSession {
            connection: Box::new(RecordingCancelConnection {
                cancelled_sessions: Arc::clone(&cancelled_sessions),
                fail_cancel: true,
            }),
            acp_session_id: "acp-retired-session".to_string(),
            spur_session_id: SessionId("spur-retired-session".to_string()),
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
                spur_acp::domain::events::BrainRetireReason::UserClear,
                None,
            )
            .await;

        assert_eq!(
            *cancelled_sessions.lock().expect("cancel recorder poisoned"),
            vec!["acp-retired-session".to_string()]
        );

        let mut active = active.expect("retired brain should still cache active connection");
        active.transport.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_active_brain_emits_brain_retired_shutdown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = SpurConfig::default();
        config.cost.db_path = tmp.path().join("cost.db").display().to_string();
        let mut orchestrator = Orchestrator::new(tmp.path().to_path_buf(), config, None).unwrap();
        let mut event_rx = orchestrator.subscribe();

        let retired_session = SessionId("shutdown-session".to_string());
        let mut brain = Some(fixture_brain_session(retired_session.0.as_str()));
        let mut agent_connection = None;
        let mut scheduler = crate::scheduler::BrainScheduler::new(
            Some(retired_session.clone().into()),
            std::sync::Arc::new(orchestrator.funnel.clone()),
        );
        let overflow =
            std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::VecDeque::new()));

        orchestrator
            .shutdown_active_brain(&mut brain, &mut agent_connection, &mut scheduler, &overflow)
            .await;

        assert!(brain.is_none());
        assert!(agent_connection.is_none());

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut saw_shutdown_retire = false;
        while tokio::time::Instant::now() < deadline && !saw_shutdown_retire {
            let remaining = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(remaining, event_rx.recv()).await {
                Ok(Ok(event)) => {
                    saw_shutdown_retire = matches!(
                        event.body,
                        SpurEventBody::BrainRetired {
                            session,
                            reason: spur_acp::domain::events::BrainRetireReason::Shutdown,
                        } if session == retired_session
                    );
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                _ => break,
            }
        }

        assert!(
            saw_shutdown_retire,
            "live-brain shutdown must emit BrainRetired{{Shutdown}}"
        );
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
        let brain_session_id_cell = Arc::new(std::sync::OnceLock::new());
        brain_session_id_cell
            .set(spur_session_id)
            .expect("test brain session id set once");
        let cont_ctx = orchestrator.build_continuation_ctx(brain_session_id_cell);
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&brain_session_id),
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

#[allow(clippy::too_many_arguments)]
async fn retire_brain_session<S: RetirableMcpServer + ?Sized>(
    funnel: &crate::event_funnel::FunnelHandle,
    session: &SessionId,
    mcp_server: &mut Option<Arc<S>>,
    mcp_guard: Option<&mut Option<AbortOnDropHandle<()>>>,
    worker_mcp_servers: &DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>,
    scheduler: &mut crate::scheduler::BrainScheduler,
    overflow: &crate::continuation_bridge::OverflowBuf,
    new_active: Option<spur_acp::types::BrainSessionId>,
) {
    if let Some((_session, worker_server)) =
        worker_mcp_servers.remove(&spur_acp::BrainSessionId::from(session.clone()))
    {
        let outcome = worker_server.shutdown(MCP_SHUTDOWN_TIMEOUT).await;
        if !outcome.drained {
            warn!(
                session = %session,
                timeout_ms = MCP_SHUTDOWN_TIMEOUT.as_millis() as u64,
                active_at_deadline = outcome.active_at_deadline,
                "Worker MCP server drain timed out; forcing shutdown"
            );
        }
    }
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
    /// Per-`BrainSession` worker MCP servers, lazily started on first
    /// dispatch with `enable_worker_mcp = true`. Phase 5 / Task 25 —
    /// the field exists; population happens via
    /// [`Orchestrator::ensure_worker_mcp_server`]. Wiring into the
    /// dispatch path lands in a follow-up task.
    pub(crate) worker_mcp_servers: Arc<DashMap<spur_acp::BrainSessionId, Arc<WorkerMcpServer>>>,
    fault_injection_hooks: FaultInjectionHooks,
    /// Abort handle for the production peer-mailbox reconciler task spawned
    /// by `Orchestrator::new` when `peer_mailbox_enabled = true`. Stored
    /// directly so introspection does not depend on `background_tasks`
    /// insertion order. The task itself is still tracked in
    /// `background_tasks` for `Drop` to abort.
    pub(crate) peer_mailbox_reconciler_abort: Option<tokio::task::AbortHandle>,
}

impl Drop for Orchestrator {
    fn drop(&mut self) {
        for handle in self.background_tasks.drain(..) {
            handle.abort();
        }
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
            worker_mcp_servers: Arc::new(DashMap::new()),
            fault_injection_hooks: FaultInjectionHooks::default(),
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

    /// Clone the orchestrator's event funnel so adjacent frontend tasks can
    /// emit through the same sequencing path as the orchestrator.
    pub fn event_funnel_handle(&self) -> crate::event_funnel::FunnelHandle {
        self.funnel.clone()
    }

    pub fn with_fault_injection_hooks(mut self, hooks: FaultInjectionHooks) -> Self {
        self.fault_injection_hooks = hooks;
        self
    }

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
    fn build_continuation_ctx(
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
            let mcp_servers = vec![McpServer::Http(McpServerHttp::new("spur-mcp", &mcp_url))];

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

                        let sessions_result = match Self::list_sessions_from_rpc(&mut *conn).await {
                            Ok(sessions) => Ok(sessions),
                            Err(e) => {
                                warn!(error = %e, "list_sessions failed, trying filesystem fallback");
                                Self::list_sessions_from_disk(&brain_name)
                            }
                        };

                        match sessions_result {
                            Ok(sessions) => {
                                let sessions = filter_sessions_for_repo(sessions, &self.repo_root);
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

    // ─── Private helpers ─────────────────────────────────────────────

    /// Retire the currently-active brain session's ephemeral state
    /// (delegation handler task, MCP server) while preserving the
    /// initialized ACP connection in `agent_connection` for reuse by the
    /// next `load_brain_session` / `create_brain_session`.
    ///
    /// Called by any path that retires the current brain:
    /// `NewSessionWithMessage` (`/clear`), `ResumeSession`, and
    /// `shutdown_active_brain` during interactive shutdown. The first two
    /// paths preserve the initialized ACP connection for reuse by the next
    /// `load_brain_session` / `create_brain_session`, saving the cost of
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
    /// For ACP-soft transports, sends a best-effort `session/cancel` for the
    /// retired ACP session id. Other transports no-op here because their
    /// `cancel()` implementations terminate the subprocess instead of freeing
    /// one cooperative ACP session.
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

        if self
            .registry
            .get(&b.brain_name)
            .is_some_and(|cfg| cancel_mode_for(cfg.transport) == CancelMode::AcpSoft)
        {
            match tokio::time::timeout(
                Duration::from_millis(10),
                b.connection.cancel(&b.acp_session_id),
            )
            .await
            {
                Ok(Ok(())) => {
                    debug!(
                        session = %b.spur_session_id.0,
                        acp_session = %b.acp_session_id,
                        brain = %b.brain_name,
                        "sent best-effort ACP session cancel during brain retire"
                    );
                }
                Ok(Err(error)) => {
                    debug!(
                        session = %b.spur_session_id.0,
                        acp_session = %b.acp_session_id,
                        brain = %b.brain_name,
                        %error,
                        "best-effort ACP session cancel failed during brain retire"
                    );
                }
                Err(_) => {
                    debug!(
                        session = %b.spur_session_id.0,
                        acp_session = %b.acp_session_id,
                        brain = %b.brain_name,
                        "best-effort ACP session cancel timed out during brain retire"
                    );
                }
            }
        }

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
            &self.worker_mcp_servers,
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

    async fn shutdown_active_brain(
        &mut self,
        brain: &mut Option<BrainSession>,
        agent_connection: &mut Option<ActiveConnection>,
        scheduler: &mut crate::scheduler::BrainScheduler,
        overflow: &crate::continuation_bridge::OverflowBuf,
    ) {
        self.retire_active_brain(
            brain,
            agent_connection,
            scheduler,
            overflow,
            spur_acp::domain::events::BrainRetireReason::Shutdown,
            None,
        )
        .await;

        if let Some(ActiveConnection {
            transport: mut conn,
            ..
        }) = agent_connection.take()
        {
            let _ = conn.shutdown().await;
        }
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
        // Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id_cell = Arc::new(std::sync::OnceLock::new());
        let cont_ctx = self.build_continuation_ctx(Arc::clone(&brain_session_id_cell));
        let (mcp_server, delegation_channel) = McpCallbackServer::new(
            None,
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

        let (
            (brain_cfg, presub_notif_rx, session_response, brain_session_id, session_id),
            mcp_handle,
        ): McpGuarded<NewBrainSessionBootstrap> = cleanup_mcp_on_err(mcp_handle, async {
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

            let acp_session_id = spur_acp::SessionId(session_response.session_id.to_string());
            let brain_session_id = spur_mcp::plan::labels::derive_brain_session_id(&acp_session_id);
            mcp_server
                .set_brain_session_id(brain_session_id.clone())
                .expect("set once");
            brain_session_id_cell
                .set(brain_session_id.as_session_id().clone())
                .expect("set once");
            let session_id = brain_session_id.as_session_id().clone();
            Arc::clone(&mcp_server)
                .enable_reconciler()
                .await
                .context("Failed to enable MCP reconciler")?;

            Ok((
                brain_cfg,
                presub_notif_rx,
                session_response,
                brain_session_id,
                session_id,
            ))
        })
        .await?;

        info!(brain = %brain_name, session = %session_id, "Creating brain session");
        self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: brain_name.clone(),
            session: session_id.clone(),
        }));

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
        is_reconnect: bool,
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

        info!(brain = %brain_name, acp_session = %acp_session_id, "Loading brain session");

        // Start MCP callback server.
        let sink: Option<std::sync::Arc<dyn spur_mcp::McpEventSink>> =
            Some(std::sync::Arc::new(self.funnel.clone()));
        let brain_session_id_cell = Arc::new(std::sync::OnceLock::new());
        let cont_ctx = self.build_continuation_ctx(Arc::clone(&brain_session_id_cell));
        let (mcp_server, delegation_channel) = McpCallbackServer::new(
            None,
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

        let (
            (
                brain_cfg,
                presub_notif_rx,
                final_acp_session_id,
                history_stream,
                resumed,
                load_outcome,
                brain_session_id,
                session_id,
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

            let final_acp_session = spur_acp::SessionId(final_acp_session_id.clone());
            let brain_session_id =
                spur_mcp::plan::labels::derive_brain_session_id(&final_acp_session);
            mcp_server
                .set_brain_session_id(brain_session_id.clone())
                .expect("set once");
            brain_session_id_cell
                .set(brain_session_id.as_session_id().clone())
                .expect("set once");
            let session_id = brain_session_id.as_session_id().clone();
            Arc::clone(&mcp_server)
                .enable_reconciler()
                .await
                .context("Failed to enable MCP reconciler")?;

            Ok((
                brain_cfg,
                presub_notif_rx,
                final_acp_session_id,
                history_stream,
                resumed,
                load_outcome,
                brain_session_id,
                session_id,
            ))
        })
        .await?;

        if final_acp_session_id != requested_acp_session_id {
            drop(attach_guard.take());
            (attach_guard, fs_unsafe) = self.acquire_attach_guard_for_new(&final_acp_session_id)?;
        }

        info!(brain = %brain_name, session = %session_id, acp_session = %final_acp_session_id, "Loaded brain session");
        if !is_reconnect {
            self.emit(SpurEvent::now(SpurEventBody::BrainSpawned {
                agent: brain_name.clone(),
                session: session_id.clone(),
            }));
        }

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
                true,
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
mod list_sessions_tests {
    use super::*;
    use agent_client_protocol::schema::{ListSessionsRequest, SessionInfo};
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;

    struct NonProgressingCursorConnection {
        calls: usize,
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
            assert!(request.cwd.is_none());
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

    #[tokio::test]
    async fn pagination_breaks_on_non_progressing_cursor() {
        let mut conn = NonProgressingCursorConnection { calls: 0 };

        let sessions = Orchestrator::list_sessions_from_rpc(&mut conn)
            .await
            .expect("list sessions");

        assert_eq!(conn.calls, 2);
        assert!(conn.calls <= 3);
        assert_eq!(sessions.len(), 2);
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
