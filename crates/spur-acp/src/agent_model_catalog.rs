use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::future::Future;
use std::io::{Error, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::adapter::config_options::extract_choices;
use crate::capability_evidence::{CliIdentity, EvidenceClaim, EvidenceProvenance};
use crate::connection::AgentConnection;
use crate::spur_agent_caps::{
    thought_level_option_from, CapabilityEvidenceSnapshot, SpurAgentCaps,
};
use crate::types::AgentKind;
use crate::InitializeRequest;

const CACHE_VERSION: u32 = 1;
const CACHE_TTL_SECONDS: i64 = 24 * 3600;
pub const CAPABILITY_EVIDENCE_CACHE_VERSION: u32 = 1;
pub const CAPABILITY_EVIDENCE_SCHEMA_VERSION: u32 = 1;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

/// Exact non-secret CLI identity and evidence schema owning one cache epoch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapabilityEvidenceCacheKey {
    pub resolved_executable: PathBuf,
    pub upstream_version: Option<String>,
    pub argv_fingerprint: String,
    pub environment_fingerprint: String,
    pub evidence_schema_version: u32,
}

impl CapabilityEvidenceCacheKey {
    #[must_use]
    pub fn new(identity: &CliIdentity, evidence_schema_version: u32) -> Self {
        Self {
            resolved_executable: identity.resolved_executable.clone(),
            upstream_version: identity.upstream_version.clone(),
            argv_fingerprint: identity.argv_fingerprint.clone(),
            environment_fingerprint: identity.environment_fingerprint.clone(),
            evidence_schema_version,
        }
    }

    fn matches_identity(&self, identity: &CliIdentity) -> bool {
        self.resolved_executable == identity.resolved_executable
            && self.upstream_version == identity.upstream_version
            && self.argv_fingerprint == identity.argv_fingerprint
            && self.environment_fingerprint == identity.environment_fingerprint
    }

    fn bind_observed_identity(&self, identity: &CliIdentity) -> Option<Self> {
        let same_normalized_identity = self.resolved_executable == identity.resolved_executable
            && self.argv_fingerprint == identity.argv_fingerprint
            && self.environment_fingerprint == identity.environment_fingerprint;
        let compatible_version = match (&self.upstream_version, &identity.upstream_version) {
            (None, _) => true,
            (Some(expected), Some(observed)) => expected == observed,
            (Some(_), None) => false,
        };
        (same_normalized_identity && compatible_version)
            .then(|| Self::new(identity, self.evidence_schema_version))
    }
}

/// One complete immutable evidence epoch published by an isolated probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedCapabilityEvidenceEpoch {
    pub key: CapabilityEvidenceCacheKey,
    pub probed_at: DateTime<Utc>,
    pub snapshot: CapabilityEvidenceSnapshot,
}

impl CachedCapabilityEvidenceEpoch {
    fn is_stale(&self, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(self.probed_at) >= Duration::seconds(CACHE_TTL_SECONDS)
    }
}

/// Versioned on-disk cache. Entries are isolated by their full CLI/schema key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityEvidenceCacheV1 {
    pub version: u32,
    pub entries: Vec<CachedCapabilityEvidenceEpoch>,
}

/// Cloneable single-flight cache handle for one evidence schema.
#[derive(Debug, Clone)]
pub struct CapabilityEvidenceCache {
    path: PathBuf,
    evidence_schema_version: u32,
    gate: Arc<Mutex<HashMap<CapabilityEvidenceCacheKey, CapabilityEvidenceCacheKey>>>,
}

impl CapabilityEvidenceCache {
    #[must_use]
    pub fn new(path: PathBuf, evidence_schema_version: u32) -> Self {
        Self {
            path,
            evidence_schema_version,
            gate: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Return a fresh exact-key epoch or run one coalesced isolated probe.
    pub async fn get_or_probe<F, Fut>(
        &self,
        identity: &CliIdentity,
        now: DateTime<Utc>,
        probe: F,
    ) -> anyhow::Result<CapabilityEvidenceSnapshot>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<AgentModelCatalogProbe>>,
    {
        let requested_key = CapabilityEvidenceCacheKey::new(identity, self.evidence_schema_version);
        if let Some(snapshot) = fresh_evidence_snapshot(&self.path, &requested_key, now) {
            return Ok(snapshot);
        }

        let mut observed_keys = self.gate.lock().await;
        let key = observed_keys
            .get(&requested_key)
            .cloned()
            .unwrap_or_else(|| requested_key.clone());
        if let Some(snapshot) = fresh_evidence_snapshot(&self.path, &key, now) {
            return Ok(snapshot);
        }

        let probed = probe().await?;
        let snapshot = probed.evidence.ok_or_else(|| {
            anyhow::anyhow!("isolated probe returned no complete capability evidence epoch")
        })?;
        let published_key = validate_publishable_snapshot(&key, &snapshot)?;

        let mut cache = read_evidence_cache(&self.path).unwrap_or(CapabilityEvidenceCacheV1 {
            version: CAPABILITY_EVIDENCE_CACHE_VERSION,
            entries: Vec::new(),
        });
        cache.entries.retain(|entry| entry.key != published_key);
        cache.entries.push(CachedCapabilityEvidenceEpoch {
            key: published_key.clone(),
            probed_at: now,
            snapshot: snapshot.clone(),
        });
        write_evidence_cache(&self.path, &cache)?;
        observed_keys.insert(requested_key, published_key.clone());
        if published_key.upstream_version.is_some() {
            let mut unversioned_key = published_key.clone();
            unversioned_key.upstream_version = None;
            observed_keys.insert(unversioned_key, published_key);
        }
        Ok(snapshot)
    }
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
    pub evidence: Option<CapabilityEvidenceSnapshot>,
}

/// Overrides the derived cache location. Lets tests pin the cache to a
/// tempdir without mutating process-global HOME, which concurrent tests
/// (e.g. DuckDB extension autoload) also read.
pub const CACHE_PATH_ENV: &str = "SPUR_AGENT_MODEL_CATALOG_PATH";

pub fn cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CACHE_PATH_ENV) {
        return Some(PathBuf::from(path));
    }
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

pub fn read_evidence_cache(path: &Path) -> Option<CapabilityEvidenceCacheV1> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return None,
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to read capability evidence cache file"
            );
            return None;
        }
    };
    let cache = match serde_json::from_str::<CapabilityEvidenceCacheV1>(&contents) {
        Ok(cache) => cache,
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to parse capability evidence cache file"
            );
            return None;
        }
    };
    if cache.version != CAPABILITY_EVIDENCE_CACHE_VERSION {
        return None;
    }

    let mut keys = HashSet::with_capacity(cache.entries.len());
    if cache.entries.iter().any(|entry| {
        !keys.insert(entry.key.clone())
            || !entry
                .key
                .matches_identity(entry.snapshot.epoch().identity())
            || !snapshot_is_complete_and_conclusive(&entry.snapshot)
    }) {
        return None;
    }
    Some(cache)
}

pub fn write(path: &Path, cache: &AgentModelCatalogV1) -> std::io::Result<()> {
    write_json_atomically(path, cache)
}

fn write_evidence_cache(path: &Path, cache: &CapabilityEvidenceCacheV1) -> std::io::Result<()> {
    write_json_atomically(path, cache)
}

fn write_json_atomically<T: Serialize + ?Sized>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        match std::fs::create_dir_all(parent) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }

    let contents = serde_json::to_vec(value).map_err(Error::other)?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
    let result = (|| {
        let mut tmp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        tmp.write_all(&contents)?;
        tmp.sync_all()?;
        std::fs::rename(&tmp_path, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn fresh_evidence_snapshot(
    path: &Path,
    key: &CapabilityEvidenceCacheKey,
    now: DateTime<Utc>,
) -> Option<CapabilityEvidenceSnapshot> {
    read_evidence_cache(path)?
        .entries
        .into_iter()
        .find(|entry| entry.key == *key && !entry.is_stale(now))
        .map(|entry| entry.snapshot)
}

fn validate_publishable_snapshot(
    key: &CapabilityEvidenceCacheKey,
    snapshot: &CapabilityEvidenceSnapshot,
) -> anyhow::Result<CapabilityEvidenceCacheKey> {
    let published_key = key
        .bind_observed_identity(snapshot.epoch().identity())
        .ok_or_else(|| {
            anyhow::anyhow!("isolated probe evidence identity does not match cache identity")
        })?;
    if !snapshot_is_complete_and_conclusive(snapshot) {
        anyhow::bail!("isolated probe evidence epoch is partial or inconclusive");
    }
    Ok(published_key)
}

fn snapshot_is_complete_and_conclusive(snapshot: &CapabilityEvidenceSnapshot) -> bool {
    let records = snapshot.epoch().records();
    snapshot.is_complete()
        && !records.is_empty()
        && records.iter().all(|record| {
            !matches!(
                record.claim,
                EvidenceClaim::Inconclusive | EvidenceClaim::Unknown
            ) && record.provenance != EvidenceProvenance::InconclusiveFailure
        })
}

pub async fn probe_agent_model_catalog(
    connection: &mut dyn AgentConnection,
    agent_kind: AgentKind,
    cwd: PathBuf,
) -> anyhow::Result<AgentModelCatalogProbe> {
    let initialize = match connection
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
    {
        Ok(initialize) => initialize,
        Err(error) => {
            let _ = connection.shutdown().await;
            return Err(error);
        }
    };
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
    let evidence = caps.capability_evidence.clone();

    connection.shutdown().await?;

    Ok(AgentModelCatalogProbe {
        models,
        efforts,
        evidence,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_prefers_env_override() {
        let previous = std::env::var_os(CACHE_PATH_ENV);
        std::env::set_var(
            CACHE_PATH_ENV,
            "/tmp/spur-test-catalog/agent-model-catalog.json",
        );
        let path = cache_path();
        match previous {
            Some(value) => std::env::set_var(CACHE_PATH_ENV, value),
            None => std::env::remove_var(CACHE_PATH_ENV),
        }
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/tmp/spur-test-catalog/agent-model-catalog.json"
            ))
        );
    }
}
