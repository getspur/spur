use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;

use agent_client_protocol::schema::ProtocolVersion;
use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use futures::Stream;
use spur_acp::agent_model_catalog::{
    cache_path, cli_identity, probe_agent_model_catalog, read, write, AgentModelCatalogV1,
    ConfigOptionChoice, WorkerCatalogEntry,
};
use spur_acp::{
    AgentConnection, AgentHealth, AgentKind, InitializeRequest, InitializeResponse, McpServer,
    NewSessionResponse, PromptRequest, SessionConfigId, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionNotification,
};
use tempfile::tempdir;

fn choice(value: &str, name: &str, description: Option<&str>) -> ConfigOptionChoice {
    ConfigOptionChoice {
        value: value.to_string(),
        name: name.to_string(),
        description: description.map(str::to_string),
    }
}

fn sample_entry(probed_at: chrono::DateTime<Utc>, identity: &str) -> WorkerCatalogEntry {
    WorkerCatalogEntry {
        probed_at,
        cli_identity: identity.to_string(),
        models: vec![choice("gpt-5", "GPT-5", Some("frontier"))],
        efforts: vec![choice("high", "High", Some("deeper"))],
    }
}

#[test]
fn catalog_roundtrips_and_staleness_uses_ttl_and_cli_identity() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("agent-model-catalog.json");
    let probed_at = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    let now = probed_at + Duration::hours(23);
    let stale_now = probed_at + Duration::hours(24) + Duration::seconds(1);

    let mut entries = HashMap::new();
    entries.insert(
        "codex-prod".to_string(),
        sample_entry(probed_at, "codex --acp"),
    );
    let catalog = AgentModelCatalogV1 {
        version: 1,
        entries,
    };

    write(&path, &catalog).expect("write catalog");

    let read_back = read(&path).expect("catalog should roundtrip");
    let entry = read_back.entries.get("codex-prod").expect("entry");
    assert_eq!(entry.models[0], choice("gpt-5", "GPT-5", Some("frontier")));
    assert!(!entry.is_stale(now, "codex --acp"));
    assert!(entry.is_stale(now, "codex --experimental-acp"));
    assert!(entry.is_stale(stale_now, "codex --acp"));
}

#[test]
fn catalog_uses_home_spur_cache_path_and_cli_identity_join() {
    let path = cache_path().expect("home directory should be available");

    assert!(path.ends_with(".spur/cache/agent-model-catalog.json"));
    assert_eq!(
        cli_identity("codex", &["--acp".to_string(), "--profile".to_string()]),
        "codex --acp --profile"
    );
}

#[tokio::test]
async fn probe_splits_model_and_effort_choices_from_config_options() {
    let cwd = tempdir().expect("tempdir");
    let mut conn = ProbeConnection::new();

    let probed =
        probe_agent_model_catalog(&mut conn, AgentKind::CodexAcp, cwd.path().to_path_buf())
            .await
            .expect("probe should succeed");

    assert_eq!(conn.calls, ["initialize", "new_session", "shutdown"]);
    assert_eq!(
        probed.models,
        vec![
            choice("gpt-5", "GPT-5", Some("frontier")),
            choice("gpt-4.1", "GPT-4.1", None),
        ]
    );
    assert_eq!(
        probed.efforts,
        vec![
            choice("low", "Low", Some("fast")),
            choice("high", "High", Some("deep")),
        ]
    );
}

struct ProbeConnection {
    calls: Vec<&'static str>,
}

impl ProbeConnection {
    fn new() -> Self {
        Self { calls: Vec::new() }
    }
}

#[async_trait]
impl AgentConnection for ProbeConnection {
    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        self.calls.push("initialize");
        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    async fn new_session(
        &mut self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        self.calls.push("new_session");
        assert!(cwd.exists());
        assert!(mcp_servers.is_empty());

        let mut response = NewSessionResponse::new("probe-session");
        response.config_options = Some(vec![
            SessionConfigOption::select(
                SessionConfigId::new("mode"),
                "Mode",
                "agent",
                vec![SessionConfigSelectOption::new("agent", "Agent")],
            ),
            SessionConfigOption::select(
                SessionConfigId::new("vendor_model"),
                "Model",
                "gpt-5",
                vec![
                    SessionConfigSelectOption::new("gpt-5", "GPT-5").description("frontier"),
                    SessionConfigSelectOption::new("gpt-4.1", "GPT-4.1"),
                ],
            )
            .category(SessionConfigOptionCategory::Model),
            SessionConfigOption::select(
                SessionConfigId::new("thinking_level"),
                "Thinking level",
                "low",
                vec![
                    SessionConfigSelectOption::new("low", "Low").description("fast"),
                    SessionConfigSelectOption::new("high", "High").description("deep"),
                ],
            )
            .category(SessionConfigOptionCategory::ThoughtLevel),
        ]);
        Ok(response)
    }

    async fn prompt(
        &mut self,
        _request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        panic!("probe must not prompt")
    }

    async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
        panic!("probe must not cancel")
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.calls.push("shutdown");
        Ok(())
    }

    fn health(&self) -> AgentHealth {
        AgentHealth::Ready
    }
}
