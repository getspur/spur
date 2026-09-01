use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use agent_client_protocol::schema::v1::Meta;
use agent_client_protocol::schema::ProtocolVersion;
use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use futures::Stream;
use spur_acp::agent_model_catalog::{
    cache_path, cli_identity, probe_agent_model_catalog, read, read_evidence_cache, write,
    AgentModelCatalogProbe, AgentModelCatalogV1, CapabilityEvidenceCache, ConfigOptionChoice,
    WorkerCatalogEntry, CAPABILITY_EVIDENCE_CACHE_VERSION,
};
use spur_acp::capability_evidence::{
    CapabilityChoice, CapabilityKey, CapabilityKind, CliIdentity, DispatchRoute, EvidenceClaim,
    EvidenceEpoch, EvidenceEpochId, EvidenceProvenance, EvidenceRecord, EvidenceSessionScope,
    ObservationTime, RawEvidenceDigest,
};
use spur_acp::spur_agent_caps::CapabilityEvidenceSnapshot;
use spur_acp::{
    AgentConnection, AgentHealth, AgentKind, InitializeRequest, InitializeResponse, McpServer,
    NewSessionResponse, PromptRequest, SessionConfigId, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionNotification,
};
use tempfile::tempdir;
use tokio::sync::Notify;

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

fn evidence_identity(version: &str) -> CliIdentity {
    CliIdentity {
        resolved_executable: PathBuf::from("/opt/spur/bin/agent"),
        upstream_version: Some(version.to_string()),
        argv_fingerprint: "argv-sha256".to_string(),
        environment_fingerprint: "env-sha256".to_string(),
    }
}

fn unversioned_evidence_identity() -> CliIdentity {
    CliIdentity {
        upstream_version: None,
        ..evidence_identity("ignored")
    }
}

fn evidence_snapshot(
    epoch_id: u64,
    identity: &CliIdentity,
    claim: EvidenceClaim,
    provenance: EvidenceProvenance,
) -> CapabilityEvidenceSnapshot {
    let snapshot = incomplete_evidence_snapshot(epoch_id, identity, claim, provenance);
    let mut encoded = serde_json::to_value(snapshot).expect("serialize evidence snapshot");
    encoded["completeness"] = serde_json::json!("complete");
    serde_json::from_value(encoded).expect("complete evidence snapshot")
}

fn incomplete_evidence_snapshot(
    epoch_id: u64,
    identity: &CliIdentity,
    claim: EvidenceClaim,
    provenance: EvidenceProvenance,
) -> CapabilityEvidenceSnapshot {
    let record = EvidenceRecord {
        key: CapabilityKey {
            kind: CapabilityKind::Model,
            upstream_id: "model".to_string(),
        },
        claim,
        provenance,
        identity: identity.clone(),
        observed_at: ObservationTime(1_700_000_000_000),
        raw_digest: RawEvidenceDigest(format!("digest-{epoch_id}")),
        session_scope: EvidenceSessionScope::IsolatedProbe,
        choices: vec![CapabilityChoice {
            id: "dynamic-model".to_string(),
            label: "Dynamic model".to_string(),
            description: None,
        }],
    };
    let epoch = EvidenceEpoch::new(EvidenceEpochId(epoch_id), identity.clone(), vec![record])
        .expect("identity-bound epoch");
    CapabilityEvidenceSnapshot::from_epoch(epoch, identity)
}

fn completed_probe(snapshot: CapabilityEvidenceSnapshot) -> AgentModelCatalogProbe {
    AgentModelCatalogProbe {
        models: Vec::new(),
        efforts: Vec::new(),
        evidence: Some(snapshot),
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
async fn evidence_cache_preserves_ttl_and_isolates_identity_and_schema_versions() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("capability-evidence.json");
    let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    let identity_v1 = evidence_identity("1.0.0");
    let identity_v2 = evidence_identity("2.0.0");
    let probes = Arc::new(AtomicUsize::new(0));
    let cache_v1 = CapabilityEvidenceCache::new(path.clone(), 1);

    let first = cache_v1
        .get_or_probe(&identity_v1, now, {
            let probes = Arc::clone(&probes);
            let snapshot = evidence_snapshot(
                1,
                &identity_v1,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move {
                probes.fetch_add(1, Ordering::SeqCst);
                Ok(completed_probe(snapshot))
            }
        })
        .await
        .expect("first probe");
    assert_eq!(first.epoch().id(), EvidenceEpochId(1));

    let cached = cache_v1
        .get_or_probe(&identity_v1, now + Duration::hours(23), {
            let probes = Arc::clone(&probes);
            let snapshot = evidence_snapshot(
                99,
                &identity_v1,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move {
                probes.fetch_add(1, Ordering::SeqCst);
                Ok(completed_probe(snapshot))
            }
        })
        .await
        .expect("fresh cache hit");
    assert_eq!(cached.epoch().id(), EvidenceEpochId(1));

    let cache_schema_v2 = CapabilityEvidenceCache::new(path.clone(), 2);
    let schema_reprobe = cache_schema_v2
        .get_or_probe(&identity_v1, now + Duration::hours(23), {
            let probes = Arc::clone(&probes);
            let snapshot = evidence_snapshot(
                2,
                &identity_v1,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move {
                probes.fetch_add(1, Ordering::SeqCst);
                Ok(completed_probe(snapshot))
            }
        })
        .await
        .expect("schema drift reprobes");
    assert_eq!(schema_reprobe.epoch().id(), EvidenceEpochId(2));

    let identity_reprobe = cache_v1
        .get_or_probe(&identity_v2, now + Duration::hours(23), {
            let probes = Arc::clone(&probes);
            let snapshot = evidence_snapshot(
                3,
                &identity_v2,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move {
                probes.fetch_add(1, Ordering::SeqCst);
                Ok(completed_probe(snapshot))
            }
        })
        .await
        .expect("identity drift reprobes");
    assert_eq!(identity_reprobe.epoch().id(), EvidenceEpochId(3));

    let ttl_reprobe = cache_v1
        .get_or_probe(&identity_v1, now + Duration::hours(24), {
            let probes = Arc::clone(&probes);
            let snapshot = evidence_snapshot(
                4,
                &identity_v1,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move {
                probes.fetch_add(1, Ordering::SeqCst);
                Ok(completed_probe(snapshot))
            }
        })
        .await
        .expect("TTL boundary reprobes");
    assert_eq!(ttl_reprobe.epoch().id(), EvidenceEpochId(4));
    assert_eq!(probes.load(Ordering::SeqCst), 4);

    let persisted = read_evidence_cache(&path).expect("versioned cache");
    assert_eq!(persisted.version, CAPABILITY_EVIDENCE_CACHE_VERSION);
    assert_eq!(persisted.entries.len(), 3);

    let mut incompatible = serde_json::to_value(&persisted).expect("serialize cache");
    incompatible["version"] = serde_json::json!(CAPABILITY_EVIDENCE_CACHE_VERSION + 1);
    std::fs::write(
        &path,
        serde_json::to_vec(&incompatible).expect("serialize incompatible cache"),
    )
    .expect("write incompatible cache");
    assert!(read_evidence_cache(&path).is_none());
}

#[tokio::test]
async fn evidence_cache_enriches_an_unversioned_probe_identity_once() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("capability-evidence.json");
    let cache = CapabilityEvidenceCache::new(path.clone(), 1);
    let requested_identity = unversioned_evidence_identity();
    let observed_identity = evidence_identity("1.0.0");
    let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    let probes = Arc::new(AtomicUsize::new(0));

    let first = cache
        .get_or_probe(&requested_identity, now, {
            let probes = Arc::clone(&probes);
            let snapshot = evidence_snapshot(
                5,
                &observed_identity,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move {
                probes.fetch_add(1, Ordering::SeqCst);
                Ok(completed_probe(snapshot))
            }
        })
        .await
        .expect("initialize may enrich a missing upstream version");
    assert_eq!(first.epoch().identity(), &observed_identity);

    let cached = cache
        .get_or_probe(&requested_identity, now + Duration::hours(1), {
            let probes = Arc::clone(&probes);
            let snapshot = evidence_snapshot(
                6,
                &observed_identity,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move {
                probes.fetch_add(1, Ordering::SeqCst);
                Ok(completed_probe(snapshot))
            }
        })
        .await
        .expect("enriched identity is coalesced for this cache handle");
    assert_eq!(cached.epoch().id(), EvidenceEpochId(5));
    assert_eq!(probes.load(Ordering::SeqCst), 1);

    let persisted = read_evidence_cache(&path).expect("enriched cache entry");
    assert_eq!(persisted.entries.len(), 1);
    assert_eq!(
        persisted.entries[0].key.upstream_version.as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        persisted.entries[0]
            .snapshot
            .epoch()
            .identity()
            .upstream_version
            .as_deref(),
        Some("1.0.0")
    );
}

#[tokio::test]
async fn evidence_cache_rejects_version_drift_after_identity_enrichment() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("capability-evidence.json");
    let cache = CapabilityEvidenceCache::new(path.clone(), 1);
    let requested_identity = unversioned_evidence_identity();
    let old_identity = evidence_identity("1.0.0");
    let new_identity = evidence_identity("2.0.0");
    let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();

    cache
        .get_or_probe(&requested_identity, now, {
            let snapshot = evidence_snapshot(
                7,
                &old_identity,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move { Ok(completed_probe(snapshot)) }
        })
        .await
        .expect("first version observation enriches identity");

    let error = cache
        .get_or_probe(&requested_identity, now + Duration::hours(24), {
            let snapshot = evidence_snapshot(
                8,
                &new_identity,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move { Ok(completed_probe(snapshot)) }
        })
        .await
        .expect_err("later observed version drift must remain inconclusive");
    assert!(error.to_string().contains("identity does not match"));

    let persisted = read_evidence_cache(&path).expect("original epoch remains atomic");
    assert_eq!(persisted.entries.len(), 1);
    assert_eq!(
        persisted.entries[0].snapshot.epoch().id(),
        EvidenceEpochId(7)
    );
    assert_eq!(
        persisted.entries[0].key.upstream_version.as_deref(),
        Some("1.0.0")
    );
}

#[tokio::test]
async fn evidence_cache_coalesces_concurrent_misses_per_identity() {
    let dir = tempdir().expect("tempdir");
    let cache = Arc::new(CapabilityEvidenceCache::new(
        dir.path().join("capability-evidence.json"),
        1,
    ));
    let identity = evidence_identity("1.0.0");
    let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();
    let probes = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let first = tokio::spawn({
        let cache = Arc::clone(&cache);
        let identity = identity.clone();
        let probes = Arc::clone(&probes);
        let started = Arc::clone(&started);
        let release = Arc::clone(&release);
        async move {
            let probe_identity = identity.clone();
            cache
                .get_or_probe(&identity, now, move || async move {
                    probes.fetch_add(1, Ordering::SeqCst);
                    started.notify_one();
                    release.notified().await;
                    Ok(completed_probe(evidence_snapshot(
                        7,
                        &probe_identity,
                        EvidenceClaim::NativeVerified,
                        EvidenceProvenance::AcceptedActiveProbe,
                    )))
                })
                .await
        }
    });

    started.notified().await;
    let second = tokio::spawn({
        let cache = Arc::clone(&cache);
        let identity = identity.clone();
        let probes = Arc::clone(&probes);
        async move {
            let probe_identity = identity.clone();
            cache
                .get_or_probe(&identity, now, move || async move {
                    probes.fetch_add(1, Ordering::SeqCst);
                    Ok(completed_probe(evidence_snapshot(
                        8,
                        &probe_identity,
                        EvidenceClaim::NativeVerified,
                        EvidenceProvenance::AcceptedActiveProbe,
                    )))
                })
                .await
        }
    });
    tokio::task::yield_now().await;
    release.notify_one();

    let first = first.await.expect("first task").expect("first result");
    let second = second.await.expect("second task").expect("second result");
    assert_eq!(first.epoch().id(), EvidenceEpochId(7));
    assert_eq!(second.epoch().id(), EvidenceEpochId(7));
    assert_eq!(probes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn evidence_cache_atomically_publishes_only_complete_conclusive_epochs() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("capability-evidence.json");
    let cache = CapabilityEvidenceCache::new(path.clone(), 1);
    let identity = evidence_identity("1.0.0");
    let now = Utc.timestamp_opt(1_700_000_000, 0).single().unwrap();

    cache
        .get_or_probe(&identity, now, {
            let snapshot = evidence_snapshot(
                1,
                &identity,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
            );
            move || async move { Ok(completed_probe(snapshot)) }
        })
        .await
        .expect("seed complete epoch");

    let expired = now + Duration::hours(24);
    let auth_error = cache
        .get_or_probe(&identity, expired, || async {
            Err(anyhow::anyhow!("authentication required"))
        })
        .await
        .expect_err("auth failure is inconclusive");
    assert!(auth_error.to_string().contains("authentication required"));
    assert_eq!(
        read_evidence_cache(&path).unwrap().entries[0]
            .snapshot
            .epoch()
            .id(),
        EvidenceEpochId(1)
    );

    cache
        .get_or_probe(&identity, expired, || async {
            Ok(AgentModelCatalogProbe {
                models: Vec::new(),
                efforts: Vec::new(),
                evidence: None,
            })
        })
        .await
        .expect_err("partial probe must not publish");
    assert_eq!(
        read_evidence_cache(&path).unwrap().entries[0]
            .snapshot
            .epoch()
            .id(),
        EvidenceEpochId(1)
    );

    let inconclusive = evidence_snapshot(
        2,
        &identity,
        EvidenceClaim::Inconclusive,
        EvidenceProvenance::InconclusiveFailure,
    );
    assert_ne!(
        inconclusive.reduced_capabilities()[0].route,
        DispatchRoute::NativePreferred
    );
    cache
        .get_or_probe(&identity, expired, move || async move {
            Ok(completed_probe(inconclusive))
        })
        .await
        .expect_err("inconclusive epoch must not publish");
    assert_eq!(
        read_evidence_cache(&path).unwrap().entries[0]
            .snapshot
            .epoch()
            .id(),
        EvidenceEpochId(1)
    );

    let incomplete = incomplete_evidence_snapshot(
        3,
        &identity,
        EvidenceClaim::NativeVerified,
        EvidenceProvenance::AcceptedActiveProbe,
    );
    assert!(!incomplete.is_complete());
    assert_eq!(
        incomplete.reduced_capabilities()[0].route,
        DispatchRoute::NativePreferred
    );
    cache
        .get_or_probe(&identity, expired, move || async move {
            Ok(completed_probe(incomplete))
        })
        .await
        .expect_err("conclusive-looking incomplete epoch must not publish");
    assert_eq!(
        read_evidence_cache(&path).unwrap().entries[0]
            .snapshot
            .epoch()
            .id(),
        EvidenceEpochId(1)
    );

    let replacement = evidence_snapshot(
        4,
        &identity,
        EvidenceClaim::NativeVerified,
        EvidenceProvenance::AcceptedActiveProbe,
    );
    cache
        .get_or_probe(&identity, expired, move || async move {
            Ok(completed_probe(replacement))
        })
        .await
        .expect("complete replacement publishes");
    let persisted = read_evidence_cache(&path).expect("atomic complete cache");
    assert_eq!(persisted.entries.len(), 1);
    assert_eq!(
        persisted.entries[0].snapshot.epoch().id(),
        EvidenceEpochId(4)
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
        probed.evidence.expect("raw evidence epoch").epoch().id(),
        EvidenceEpochId(11)
    );
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
    identity: CliIdentity,
}

impl ProbeConnection {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            identity: evidence_identity("1.0.0"),
        }
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
        let snapshot = evidence_snapshot(
            11,
            &self.identity,
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::StandardAdvertisement,
        );
        let encoded = serde_json::to_value(snapshot).expect("serialize evidence snapshot");
        response.meta.get_or_insert_with(Meta::new).insert(
            "spur.capabilityEvidenceV1".to_string(),
            encoded["epoch"].clone(),
        );
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
