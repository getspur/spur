use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::path::{Path, PathBuf};

use agent_client_protocol::schema::ProtocolVersion;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::adapter::config_options::extract_choices;
use crate::connection::AgentConnection;
use crate::spur_agent_caps::{thought_level_option_from, SpurAgentCaps};
use crate::types::AgentKind;
use crate::InitializeRequest;

const CACHE_VERSION: u32 = 1;
const CACHE_TTL_SECONDS: i64 = 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModelCatalogV1 {
    pub version: u32,
    pub entries: HashMap<String, WorkerCatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCatalogEntry {
    pub probed_at: DateTime<Utc>,
    pub cli_identity: String,
    pub models: Vec<ConfigOptionChoice>,
    pub efforts: Vec<ConfigOptionChoice>,
}

impl WorkerCatalogEntry {
    #[must_use]
    pub fn is_stale(&self, now: DateTime<Utc>, cli_identity: &str) -> bool {
        self.cli_identity != cli_identity
            || now.signed_duration_since(self.probed_at) >= Duration::seconds(CACHE_TTL_SECONDS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigOptionChoice {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentModelCatalogProbe {
    pub models: Vec<ConfigOptionChoice>,
    pub efforts: Vec<ConfigOptionChoice>,
}

pub fn cache_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| {
        dirs.home_dir()
            .join(".spur")
            .join("cache")
            .join("agent-model-catalog.json")
    })
}

#[must_use]
pub fn cli_identity(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn read(path: &Path) -> Option<AgentModelCatalogV1> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            debug!(path = %path.display(), "agent model catalog cache file missing");
            return None;
        }
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to read agent model catalog cache file"
            );
            return None;
        }
    };

    let cache = match serde_json::from_str::<AgentModelCatalogV1>(&contents) {
        Ok(cache) => cache,
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to parse agent model catalog cache file"
            );
            return None;
        }
    };

    if cache.version != CACHE_VERSION {
        return None;
    }

    Some(cache)
}

pub fn write(path: &Path, cache: &AgentModelCatalogV1) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        match std::fs::create_dir_all(parent) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }

    let tmp_path = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let contents = serde_json::to_vec(cache).map_err(Error::other)?;

    std::fs::write(&tmp_path, contents)?;
    std::fs::rename(&tmp_path, path)
}

pub async fn probe_agent_model_catalog(
    connection: &mut dyn AgentConnection,
    agent_kind: AgentKind,
    cwd: PathBuf,
) -> anyhow::Result<AgentModelCatalogProbe> {
    let initialize = connection
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await?;
    let new_session = match connection.new_session(cwd, Vec::new()).await {
        Ok(new_session) => new_session,
        Err(error) => {
            let _ = connection.shutdown().await;
            return Err(error);
        }
    };
    let caps = SpurAgentCaps::new(&initialize, &new_session, agent_kind);
    let models = caps
        .model_option()
        .map(choices_from_option)
        .unwrap_or_default();
    let efforts = thought_level_option_from(&caps.config_options)
        .map(choices_from_option)
        .unwrap_or_default();

    connection.shutdown().await?;

    Ok(AgentModelCatalogProbe { models, efforts })
}

fn choices_from_option(option: &crate::SessionConfigOption) -> Vec<ConfigOptionChoice> {
    extract_choices(option)
        .into_iter()
        .map(|choice| ConfigOptionChoice {
            value: choice.value,
            name: choice.label,
            description: choice.description,
        })
        .collect()
}
