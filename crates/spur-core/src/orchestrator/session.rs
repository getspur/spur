use super::*;

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
            include_str!("../../../spur-acp/tests/data/codex_acp_0_12_new_session_response.json");
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

pub(super) async fn abort_mcp_handle(handle: AbortOnDropHandle<()>) {
    handle.abort();
    let _ = handle.await;
}

pub(super) async fn cleanup_mcp_on_err<T, F>(
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

pub(super) const MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) trait RetirableMcpServer: Send + Sync {
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

pub(super) async fn shutdown_mcp_server<S: RetirableMcpServer + ?Sized>(
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
pub(super) async fn retire_brain_session<S: RetirableMcpServer + ?Sized>(
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

impl Orchestrator {
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
    pub(super) async fn retire_active_brain(
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

    pub(super) async fn shutdown_active_brain(
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
    pub(super) async fn create_brain_session(
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
            self.config.delegation.normalize.bypass_hooks,
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
    pub(super) async fn load_brain_session(
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
            self.config.delegation.normalize.bypass_hooks,
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
    pub(super) async fn try_reconnect_brain(
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
    pub(super) async fn reconnect_with_events(
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
}
