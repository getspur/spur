//! `SpurAgentCaps` — per-session capability snapshot.
//!
//! See `docs/superpowers/specs/2026-04-27-acp-capability-aware-spur-design.md` §6.1.
//!
//! Wraps two wire facts: the agent's `AgentCapabilities` (from
//! `InitializeResponse`) and the per-session response payload's
//! `modes` / `config_options`. Spur derives `set_*` support
//! from session state because ACP 0.12 does not gate these
//! protocol-stable methods on `AgentCapabilities` flags.
//!
//! Named `SpurAgentCaps` (not `SessionCapabilities`) to avoid collision
//! with the SDK's `SessionCapabilities` struct that lives on
//! `AgentCapabilities`.

use agent_client_protocol::schema::v1::{
    AgentCapabilities, InitializeResponse, LoadSessionResponse, Meta, NewSessionResponse,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionModeState,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::adapter::grok_session_display::{extract_grok_session_display, GrokSessionDisplay};
use crate::adapter::kiro_session_display::{extract_kiro_session_display, KiroSessionDisplay};
use crate::capability_evidence::{
    reduce_capability, CapabilityChoice, CapabilityKey, CapabilityKind, CliIdentity, DispatchRoute,
    EvidenceClaim, EvidenceEpoch, EvidenceEpochId, EvidenceProvenance, EvidenceRecord,
    EvidenceSessionScope, ObservationTime, RawEvidenceDigest, ReducedCapability,
};
use crate::types::AgentKind;

pub(crate) const CAPABILITY_EVIDENCE_META_KEY: &str = "spur.capabilityEvidenceV1";

/// Whether one evidence snapshot contains a fully captured ACP capability epoch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEvidenceCompleteness {
    /// The capture cannot prove a successful, bounded initialize plus session lifecycle.
    #[default]
    Incomplete,
    /// The capture contains a successful initialize plus new/load session lifecycle.
    Complete,
}

/// Immutable evidence epoch plus its provider-neutral reduced compatibility view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEvidenceSnapshot {
    epoch: EvidenceEpoch,
    completeness: CapabilityEvidenceCompleteness,
    reduced: Vec<ReducedCapability>,
    shadow_diffs: Vec<CapabilityShadowDiff>,
}

impl CapabilityEvidenceSnapshot {
    #[must_use]
    pub fn from_epoch(epoch: EvidenceEpoch, current_identity: &CliIdentity) -> Self {
        Self::from_epoch_with_completeness(
            epoch,
            current_identity,
            CapabilityEvidenceCompleteness::Incomplete,
        )
    }

    #[must_use]
    pub(crate) fn from_epoch_with_completeness(
        epoch: EvidenceEpoch,
        current_identity: &CliIdentity,
        completeness: CapabilityEvidenceCompleteness,
    ) -> Self {
        let keys = epoch
            .records()
            .iter()
            .map(|record| record.key.clone())
            .collect::<BTreeSet<_>>();
        let reduced = keys
            .iter()
            .map(|key| reduce_capability(&epoch, current_identity, key))
            .collect();
        Self {
            epoch,
            completeness,
            reduced,
            shadow_diffs: Vec::new(),
        }
    }

    #[must_use]
    pub fn epoch(&self) -> &EvidenceEpoch {
        &self.epoch
    }

    #[must_use]
    pub fn completeness(&self) -> CapabilityEvidenceCompleteness {
        self.completeness
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.completeness == CapabilityEvidenceCompleteness::Complete
    }

    #[must_use]
    pub fn reduced_capabilities(&self) -> &[ReducedCapability] {
        &self.reduced
    }

    #[must_use]
    pub fn shadow_diffs(&self) -> &[CapabilityShadowDiff] {
        &self.shadow_diffs
    }

    pub fn unexplained_shadow_diffs(&self) -> impl Iterator<Item = &CapabilityShadowDiff> {
        self.shadow_diffs.iter().filter(|diff| diff.unexplained)
    }
}

/// One bounded comparison between the existing facade route and the reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityShadowDiff {
    pub key: CapabilityKey,
    pub legacy_route: DispatchRoute,
    pub reduced_route: DispatchRoute,
    pub reason: String,
    pub unexplained: bool,
}

#[derive(Serialize, Deserialize)]
struct CapabilityEvidenceSnapshotWire {
    epoch: EvidenceEpochWire,
    #[serde(default)]
    completeness: CapabilityEvidenceCompleteness,
    #[serde(default)]
    reduced: Vec<ReducedCapabilityWire>,
    #[serde(default)]
    shadow_diffs: Vec<CapabilityShadowDiffWire>,
}

#[derive(Serialize, Deserialize)]
struct EvidenceEpochWire {
    id: u64,
    identity: CliIdentityWire,
    records: Vec<EvidenceRecordWire>,
}

#[derive(Serialize, Deserialize)]
struct EmbeddedCapabilityEvidenceWire {
    #[serde(flatten)]
    epoch: EvidenceEpochWire,
    #[serde(default)]
    completeness: CapabilityEvidenceCompleteness,
}

#[derive(Serialize, Deserialize)]
struct CliIdentityWire {
    resolved_executable: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upstream_version: Option<String>,
    argv_fingerprint: String,
    environment_fingerprint: String,
}

#[derive(Serialize, Deserialize)]
struct EvidenceRecordWire {
    key: CapabilityKeyWire,
    claim: String,
    provenance: String,
    observed_at: u64,
    raw_digest: String,
    session_scope: EvidenceSessionScopeWire,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    choices: Vec<CapabilityChoiceWire>,
}

#[derive(Serialize, Deserialize)]
struct CapabilityKeyWire {
    kind: String,
    upstream_id: String,
}

#[derive(Serialize, Deserialize)]
struct CapabilityChoiceWire {
    id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EvidenceSessionScopeWire {
    Global,
    Session { id: String },
    IsolatedProbe,
}

#[derive(Serialize, Deserialize)]
struct ReducedCapabilityWire {
    key: CapabilityKeyWire,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    choices: Vec<CapabilityChoiceWire>,
    confidence: String,
    route: String,
    sources: EvidenceSourceSummaryWire,
    evidence_epoch: u64,
}

#[derive(Serialize, Deserialize)]
struct EvidenceSourceSummaryWire {
    record_count: usize,
    provenances: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct CapabilityShadowDiffWire {
    key: CapabilityKeyWire,
    legacy_route: String,
    reduced_route: String,
    reason: String,
    unexplained: bool,
}

impl From<&CliIdentity> for CliIdentityWire {
    fn from(identity: &CliIdentity) -> Self {
        Self {
            resolved_executable: identity.resolved_executable.clone(),
            upstream_version: identity.upstream_version.clone(),
            argv_fingerprint: identity.argv_fingerprint.clone(),
            environment_fingerprint: identity.environment_fingerprint.clone(),
        }
    }
}

impl From<CliIdentityWire> for CliIdentity {
    fn from(identity: CliIdentityWire) -> Self {
        Self {
            resolved_executable: identity.resolved_executable,
            upstream_version: identity.upstream_version,
            argv_fingerprint: identity.argv_fingerprint,
            environment_fingerprint: identity.environment_fingerprint,
        }
    }
}

fn capability_kind_name(kind: &CapabilityKind) -> String {
    match kind {
        CapabilityKind::Model => "model".to_owned(),
        CapabilityKind::Effort => "effort".to_owned(),
        CapabilityKind::Mode => "mode".to_owned(),
        CapabilityKind::Command => "command".to_owned(),
        CapabilityKind::Custom(kind) => format!("custom:{kind}"),
    }
}

fn capability_kind_from_name(name: String) -> CapabilityKind {
    match name.as_str() {
        "model" => CapabilityKind::Model,
        "effort" => CapabilityKind::Effort,
        "mode" => CapabilityKind::Mode,
        "command" => CapabilityKind::Command,
        _ => CapabilityKind::Custom(
            name.strip_prefix("custom:")
                .unwrap_or(name.as_str())
                .to_owned(),
        ),
    }
}

impl From<&CapabilityKey> for CapabilityKeyWire {
    fn from(key: &CapabilityKey) -> Self {
        Self {
            kind: capability_kind_name(&key.kind),
            upstream_id: key.upstream_id.clone(),
        }
    }
}

impl From<CapabilityKeyWire> for CapabilityKey {
    fn from(key: CapabilityKeyWire) -> Self {
        Self {
            kind: capability_kind_from_name(key.kind),
            upstream_id: key.upstream_id,
        }
    }
}

impl From<&CapabilityChoice> for CapabilityChoiceWire {
    fn from(choice: &CapabilityChoice) -> Self {
        Self {
            id: choice.id.clone(),
            label: choice.label.clone(),
            description: choice.description.clone(),
        }
    }
}

impl From<CapabilityChoiceWire> for CapabilityChoice {
    fn from(choice: CapabilityChoiceWire) -> Self {
        Self {
            id: choice.id,
            label: choice.label,
            description: choice.description,
        }
    }
}

fn claim_name(claim: EvidenceClaim) -> &'static str {
    match claim {
        EvidenceClaim::CandidateObserved => "candidate_observed",
        EvidenceClaim::NativeVerified => "native_verified",
        EvidenceClaim::Rejected => "rejected",
        EvidenceClaim::Inconclusive => "inconclusive",
        EvidenceClaim::Unknown => "unknown",
        EvidenceClaim::NativeFailed => "native_failed",
    }
}

fn claim_from_name(name: &str) -> Option<EvidenceClaim> {
    match name {
        "candidate_observed" => Some(EvidenceClaim::CandidateObserved),
        "native_verified" => Some(EvidenceClaim::NativeVerified),
        "rejected" => Some(EvidenceClaim::Rejected),
        "inconclusive" => Some(EvidenceClaim::Inconclusive),
        "unknown" => Some(EvidenceClaim::Unknown),
        "native_failed" => Some(EvidenceClaim::NativeFailed),
        _ => None,
    }
}

fn provenance_name(provenance: EvidenceProvenance) -> &'static str {
    match provenance {
        EvidenceProvenance::StandardAdvertisement => "standard_advertisement",
        EvidenceProvenance::VendorAdvertisement => "vendor_advertisement",
        EvidenceProvenance::AcceptedActiveProbe => "accepted_active_probe",
        EvidenceProvenance::RejectedActiveProbe => "rejected_active_probe",
        EvidenceProvenance::ObservedNotification => "observed_notification",
        EvidenceProvenance::PromptFallback => "prompt_fallback",
        EvidenceProvenance::InconclusiveFailure => "inconclusive_failure",
        EvidenceProvenance::NativeDispatch => "native_dispatch",
    }
}

fn provenance_from_name(name: &str) -> Option<EvidenceProvenance> {
    match name {
        "standard_advertisement" => Some(EvidenceProvenance::StandardAdvertisement),
        "vendor_advertisement" => Some(EvidenceProvenance::VendorAdvertisement),
        "accepted_active_probe" => Some(EvidenceProvenance::AcceptedActiveProbe),
        "rejected_active_probe" => Some(EvidenceProvenance::RejectedActiveProbe),
        "observed_notification" => Some(EvidenceProvenance::ObservedNotification),
        "prompt_fallback" => Some(EvidenceProvenance::PromptFallback),
        "inconclusive_failure" => Some(EvidenceProvenance::InconclusiveFailure),
        "native_dispatch" => Some(EvidenceProvenance::NativeDispatch),
        _ => None,
    }
}

fn route_name(route: DispatchRoute) -> &'static str {
    match route {
        DispatchRoute::Hidden => "hidden",
        DispatchRoute::PromptOnly => "prompt_only",
        DispatchRoute::NativePreferred => "native_preferred",
    }
}

fn route_from_name(name: &str) -> Option<DispatchRoute> {
    match name {
        "hidden" => Some(DispatchRoute::Hidden),
        "prompt_only" => Some(DispatchRoute::PromptOnly),
        "native_preferred" => Some(DispatchRoute::NativePreferred),
        _ => None,
    }
}

impl From<&EvidenceSessionScope> for EvidenceSessionScopeWire {
    fn from(scope: &EvidenceSessionScope) -> Self {
        match scope {
            EvidenceSessionScope::Global => Self::Global,
            EvidenceSessionScope::Session(id) => Self::Session { id: id.clone() },
            EvidenceSessionScope::IsolatedProbe => Self::IsolatedProbe,
        }
    }
}

impl From<EvidenceSessionScopeWire> for EvidenceSessionScope {
    fn from(scope: EvidenceSessionScopeWire) -> Self {
        match scope {
            EvidenceSessionScopeWire::Global => Self::Global,
            EvidenceSessionScopeWire::Session { id } => Self::Session(id),
            EvidenceSessionScopeWire::IsolatedProbe => Self::IsolatedProbe,
        }
    }
}

impl From<&ReducedCapability> for ReducedCapabilityWire {
    fn from(reduced: &ReducedCapability) -> Self {
        Self {
            key: (&reduced.key).into(),
            choices: reduced.choices.iter().map(Into::into).collect(),
            confidence: match reduced.confidence {
                crate::capability_evidence::CapabilityConfidence::Hidden => "hidden",
                crate::capability_evidence::CapabilityConfidence::PromptOnly => "prompt_only",
                crate::capability_evidence::CapabilityConfidence::NativePreferred => {
                    "native_preferred"
                }
            }
            .to_owned(),
            route: route_name(reduced.route).to_owned(),
            sources: EvidenceSourceSummaryWire {
                record_count: reduced.sources.record_count,
                provenances: reduced
                    .sources
                    .provenances
                    .iter()
                    .map(|source| provenance_name(*source).to_owned())
                    .collect(),
            },
            evidence_epoch: reduced.evidence_epoch.0,
        }
    }
}

impl From<&CapabilityShadowDiff> for CapabilityShadowDiffWire {
    fn from(diff: &CapabilityShadowDiff) -> Self {
        Self {
            key: (&diff.key).into(),
            legacy_route: route_name(diff.legacy_route).to_owned(),
            reduced_route: route_name(diff.reduced_route).to_owned(),
            reason: diff.reason.clone(),
            unexplained: diff.unexplained,
        }
    }
}

impl TryFrom<CapabilityShadowDiffWire> for CapabilityShadowDiff {
    type Error = String;

    fn try_from(diff: CapabilityShadowDiffWire) -> Result<Self, Self::Error> {
        Ok(Self {
            key: diff.key.into(),
            legacy_route: route_from_name(&diff.legacy_route)
                .ok_or_else(|| format!("unknown legacy route {}", diff.legacy_route))?,
            reduced_route: route_from_name(&diff.reduced_route)
                .ok_or_else(|| format!("unknown reduced route {}", diff.reduced_route))?,
            reason: diff.reason,
            unexplained: diff.unexplained,
        })
    }
}

impl From<&CapabilityEvidenceSnapshot> for CapabilityEvidenceSnapshotWire {
    fn from(snapshot: &CapabilityEvidenceSnapshot) -> Self {
        let identity = snapshot.epoch.identity();
        Self {
            epoch: EvidenceEpochWire {
                id: snapshot.epoch.id().0,
                identity: identity.into(),
                records: snapshot
                    .epoch
                    .records()
                    .iter()
                    .map(|record| EvidenceRecordWire {
                        key: (&record.key).into(),
                        claim: claim_name(record.claim).to_owned(),
                        provenance: provenance_name(record.provenance).to_owned(),
                        observed_at: record.observed_at.0,
                        raw_digest: record.raw_digest.0.clone(),
                        session_scope: (&record.session_scope).into(),
                        choices: record.choices.iter().map(Into::into).collect(),
                    })
                    .collect(),
            },
            completeness: snapshot.completeness,
            reduced: snapshot.reduced.iter().map(Into::into).collect(),
            shadow_diffs: snapshot.shadow_diffs.iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<CapabilityEvidenceSnapshotWire> for CapabilityEvidenceSnapshot {
    type Error = String;

    fn try_from(wire: CapabilityEvidenceSnapshotWire) -> Result<Self, Self::Error> {
        let identity: CliIdentity = wire.epoch.identity.into();
        let records = wire
            .epoch
            .records
            .into_iter()
            .map(|record| {
                Ok(EvidenceRecord {
                    key: record.key.into(),
                    claim: claim_from_name(&record.claim)
                        .ok_or_else(|| format!("unknown evidence claim {}", record.claim))?,
                    provenance: provenance_from_name(&record.provenance).ok_or_else(|| {
                        format!("unknown evidence provenance {}", record.provenance)
                    })?,
                    identity: identity.clone(),
                    observed_at: ObservationTime(record.observed_at),
                    raw_digest: RawEvidenceDigest(record.raw_digest),
                    session_scope: record.session_scope.into(),
                    choices: record.choices.into_iter().map(Into::into).collect(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let epoch = EvidenceEpoch::new(EvidenceEpochId(wire.epoch.id), identity.clone(), records)
            .map_err(|error| {
            format!(
                "evidence record {} has mismatched identity",
                error.record_index
            )
        })?;
        let mut snapshot = Self::from_epoch_with_completeness(epoch, &identity, wire.completeness);
        snapshot.shadow_diffs = wire
            .shadow_diffs
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(snapshot)
    }
}

impl Serialize for CapabilityEvidenceSnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        CapabilityEvidenceSnapshotWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CapabilityEvidenceSnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        CapabilityEvidenceSnapshotWire::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

/// What the agent told spur during `initialize` + `session/new` or `session/load`.
/// Captured at create/load time. ACP 0.12 has no protocol affordance for
/// mid-session capability renegotiation, but consumers may update the Grok
/// display snapshot from its proprietary `model_changed` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpurAgentCaps {
    /// Verbatim `AgentCapabilities` from `InitializeResponse`. Read its
    /// fields directly; future protocol additions land here automatically.
    pub agent: AgentCapabilities,
    /// `NewSessionResponse.modes` (or `LoadSessionResponse.modes`). Some
    /// state with non-empty `available_modes` ⇒ `session/set_mode` is usable.
    pub modes: Option<SessionModeState>,
    /// `NewSessionResponse.config_options`. Non-empty ⇒
    /// `session/set_config_option` is usable.
    pub config_options: Vec<SessionConfigOption>,
    /// Agent identity captured from config at session creation.
    pub agent_kind: AgentKind,
    /// Grok-only model catalog and selected labels derived from proprietary
    /// response meta. Always `None` for other agent kinds and kept separate
    /// from the standard `config_options` capability gates.
    #[serde(default)]
    pub grok_display: Option<GrokSessionDisplay>,
    /// Kiro-only model catalog recovered from the top-level `models` plane
    /// (injected under session meta by the native connection). Always `None`
    /// for other agent kinds; never implies `session/set_config_option`.
    #[serde(default)]
    pub kiro_display: Option<KiroSessionDisplay>,
    /// Provider-neutral evidence captured before typed ACP projection.
    /// Legacy gates remain authoritative during bounded shadow migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_evidence: Option<CapabilityEvidenceSnapshot>,
}

impl SpurAgentCaps {
    /// Build from the two relevant wire responses.
    #[must_use]
    pub fn new(
        initialize: &InitializeResponse,
        new_session: &NewSessionResponse,
        agent_kind: AgentKind,
    ) -> Self {
        let grok_display = extract_grok_session_display(
            agent_kind,
            initialize.meta.as_ref(),
            new_session.meta.as_ref(),
        );
        let kiro_display = extract_kiro_session_display(agent_kind, new_session.meta.as_ref());
        let mut caps = Self {
            agent: initialize.agent_capabilities.clone(),
            modes: new_session.modes.clone(),
            config_options: new_session.config_options.clone().unwrap_or_default(),
            agent_kind,
            grok_display,
            kiro_display,
            capability_evidence: embedded_evidence_snapshot(
                initialize.meta.as_ref(),
                new_session.meta.as_ref(),
            ),
        };
        caps.refresh_evidence_shadow_diffs();
        caps
    }

    /// Build from `initialize` plus the per-session state returned by
    /// `session/load`.
    #[must_use]
    pub fn from_loaded(
        initialize: &InitializeResponse,
        load_session: &LoadSessionResponse,
        agent_kind: AgentKind,
    ) -> Self {
        let grok_display = extract_grok_session_display(
            agent_kind,
            initialize.meta.as_ref(),
            load_session.meta.as_ref(),
        );
        let kiro_display = extract_kiro_session_display(agent_kind, load_session.meta.as_ref());
        let mut caps = Self {
            agent: initialize.agent_capabilities.clone(),
            modes: load_session.modes.clone(),
            config_options: load_session.config_options.clone().unwrap_or_default(),
            agent_kind,
            grok_display,
            kiro_display,
            capability_evidence: embedded_evidence_snapshot(
                initialize.meta.as_ref(),
                load_session.meta.as_ref(),
            ),
        };
        caps.refresh_evidence_shadow_diffs();
        caps
    }

    /// Replace the immutable evidence epoch and recompute shadow diagnostics.
    /// This never changes the legacy routing methods on the facade.
    pub fn apply_evidence_epoch(&mut self, epoch: EvidenceEpoch, current_identity: &CliIdentity) {
        self.capability_evidence = Some(CapabilityEvidenceSnapshot::from_epoch(
            epoch,
            current_identity,
        ));
        self.refresh_evidence_shadow_diffs();
    }

    /// Current provider-neutral reduced capability snapshots.
    #[must_use]
    pub fn reduced_capabilities(&self) -> &[ReducedCapability] {
        self.capability_evidence
            .as_ref()
            .map_or(&[], CapabilityEvidenceSnapshot::reduced_capabilities)
    }

    /// One reduced capability by semantic key.
    #[must_use]
    pub fn reduced_capability(&self, key: &CapabilityKey) -> Option<&ReducedCapability> {
        self.reduced_capabilities()
            .iter()
            .find(|capability| capability.key == *key)
    }

    /// Shadow-only route differences. These never change facade routing.
    #[must_use]
    pub fn capability_shadow_diffs(&self) -> &[CapabilityShadowDiff] {
        self.capability_evidence
            .as_ref()
            .map_or(&[], CapabilityEvidenceSnapshot::shadow_diffs)
    }

    fn refresh_evidence_shadow_diffs(&mut self) {
        let diffs = self
            .capability_evidence
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .reduced_capabilities()
                    .iter()
                    .filter_map(|reduced| {
                        let legacy_route = legacy_route_for(self, &reduced.key)?;
                        if legacy_route == reduced.route {
                            return None;
                        }

                        let provenance_demotion =
                            reduced.sources.provenances.iter().any(|source| {
                                matches!(
                                    source,
                                    EvidenceProvenance::RejectedActiveProbe
                                        | EvidenceProvenance::NativeDispatch
                                )
                            });
                        let vendor_shadow = legacy_route == DispatchRoute::NativePreferred
                            && reduced.route == DispatchRoute::PromptOnly
                            && reduced
                                .sources
                                .provenances
                                .contains(&EvidenceProvenance::VendorAdvertisement);
                        let (reason, unexplained) = if provenance_demotion {
                            ("provenance-aware reducer demotion".to_owned(), false)
                        } else if vendor_shadow {
                            ("bounded legacy native fallback".to_owned(), false)
                        } else {
                            (
                                "unexplained legacy/reducer route difference".to_owned(),
                                true,
                            )
                        };

                        Some(CapabilityShadowDiff {
                            key: reduced.key.clone(),
                            legacy_route,
                            reduced_route: reduced.route,
                            reason,
                            unexplained,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(snapshot) = self.capability_evidence.as_mut() {
            snapshot.shadow_diffs = diffs;
        }
    }

    /// `session/set_mode` is usable when the session has modes to switch between.
    #[must_use]
    pub fn supports_set_mode(&self) -> bool {
        self.modes
            .as_ref()
            .is_some_and(|m| !m.available_modes.is_empty())
    }

    /// `session/set_model` is usable when the session advertises a non-empty
    /// `model` config option. ACP 1.0 removed the dedicated model state and
    /// expresses model choice through `session/set_config_option`.
    #[must_use]
    pub fn supports_set_model(&self) -> bool {
        self.model_option().is_some_and(has_select_choices)
    }

    /// Grok advertises real model ids in proprietary metadata while leaving
    /// standard `config_options` empty.
    #[must_use]
    pub fn supports_grok_set_model(&self) -> bool {
        self.agent_kind == AgentKind::Grok
            && self
                .grok_display
                .as_ref()
                .is_some_and(|display| !display.models().is_empty())
    }

    /// Kiro advertises real model ids on the recovered top-level `models`
    /// plane while leaving standard `config_options` empty. Live probe:
    /// `session/set_model` accepts `{sessionId, modelId}`.
    #[must_use]
    pub fn supports_kiro_set_model(&self) -> bool {
        self.agent_kind == AgentKind::Kiro
            && self
                .kiro_display
                .as_ref()
                .is_some_and(|display| !display.models().is_empty())
    }

    /// Either proprietary DirectSetModel path (Grok or Kiro) is available.
    #[must_use]
    pub fn supports_direct_set_model(&self) -> bool {
        self.supports_grok_set_model() || self.supports_kiro_set_model()
    }

    /// Apply a proven Grok `model_changed` extension notification.
    ///
    /// Standard ACP capability fields and `config_options` remain untouched.
    pub fn apply_grok_model_changed(&mut self, params: &serde_json::Value) -> bool {
        if self.agent_kind != AgentKind::Grok {
            return false;
        }
        self.grok_display
            .as_mut()
            .is_some_and(|display| display.apply_model_changed(params))
    }

    /// Apply a successful Kiro `session/set_model` selection to the frozen
    /// display catalog so status labels track the new model without inventing
    /// config options.
    pub fn apply_kiro_model_selected(&mut self, model_id: &str) -> bool {
        if self.agent_kind != AgentKind::Kiro {
            return false;
        }
        self.kiro_display
            .as_mut()
            .is_some_and(|display| display.apply_selected_model(model_id))
    }

    /// The config option that represents model selection, when advertised.
    ///
    /// ACP 1.1 adds semantic categories; prefer the first option categorized
    /// as `Model`, and retain the legacy `id == "model"` fallback only for
    /// agents that omit `category`.
    #[must_use]
    pub fn model_option(&self) -> Option<&SessionConfigOption> {
        model_option_from(&self.config_options)
    }

    /// `session/set_config_option` is usable when the session advertises
    /// non-empty `config_options`.
    #[must_use]
    pub fn supports_set_config_option(&self) -> bool {
        !self.config_options.is_empty()
    }

    /// `session/load` is announced explicitly on `AgentCapabilities`.
    #[must_use]
    pub fn supports_load_session(&self) -> bool {
        self.agent.load_session
    }

    /// `session/resume` is announced by `AgentCapabilities.session_capabilities.resume`.
    #[must_use]
    pub fn supports_resume_session(&self) -> bool {
        self.agent.session_capabilities.resume.is_some()
    }

    /// `session/delete` is announced by `AgentCapabilities.session_capabilities.delete`.
    #[must_use]
    pub fn supports_delete_session(&self) -> bool {
        self.agent.session_capabilities.delete.is_some()
    }

    /// `session/list` is announced by `AgentCapabilities.session_capabilities.list`.
    #[must_use]
    pub fn supports_list_sessions(&self) -> bool {
        self.agent.session_capabilities.list.is_some()
    }

    /// `session/close` is announced by `AgentCapabilities.session_capabilities.close`.
    #[must_use]
    pub fn supports_close_session(&self) -> bool {
        self.agent.session_capabilities.close.is_some()
    }

    /// Display label for the active model.
    #[must_use]
    pub fn current_model_label(&self) -> Option<String> {
        Self::model_label_from_config_options(&self.config_options)
            .map(str::to_owned)
            .or_else(|| {
                self.grok_display
                    .as_ref()
                    .and_then(|display| display.model_label.clone())
            })
            .or_else(|| {
                self.kiro_display
                    .as_ref()
                    .and_then(|display| display.model_label.clone())
            })
    }

    /// Display label for the active model from a config-options snapshot.
    /// Callers with live session state should pass that fresh snapshot
    /// instead of the frozen caps copy captured at session init.
    #[must_use]
    pub fn model_label_from_config_options(options: &[SessionConfigOption]) -> Option<&str> {
        let option = model_option_from(options)?;

        let SessionConfigKind::Select(select) = &option.kind else {
            return None;
        };
        let current = select.current_value.0.as_ref();
        match &select.options {
            SessionConfigSelectOptions::Ungrouped(options) => options
                .iter()
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.as_str())
                .or(Some(current)),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| group.options.iter())
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.as_str())
                .or(Some(current)),
            _ => Some(current),
        }
    }

    /// Display label for the active reasoning effort from a config-options
    /// snapshot. Callers with live session state should pass that fresh
    /// snapshot instead of the frozen caps copy captured at session init.
    #[must_use]
    pub fn effort_label_from(options: &[SessionConfigOption]) -> Option<String> {
        let option = thought_level_option_from(options)?;

        let SessionConfigKind::Select(select) = &option.kind else {
            return None;
        };
        let current = select.current_value.0.as_ref();
        let name = match &select.options {
            SessionConfigSelectOptions::Ungrouped(options) => options
                .iter()
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.clone()),
            SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| group.options.iter())
                .find(|option| option.value.0.as_ref() == current)
                .map(|option| option.name.clone()),
            _ => None,
        };

        Some(name.unwrap_or_else(|| current.to_owned()))
    }

    /// Display label for the active reasoning effort from this caps snapshot.
    /// This is the initial session state only; TUI status rendering should
    /// prefer live `BrainSession.config_options` snapshots when available.
    #[must_use]
    pub fn current_effort_label(&self) -> Option<String> {
        Self::effort_label_from(&self.config_options).or_else(|| {
            self.grok_display
                .as_ref()
                .and_then(|display| display.effort_label.clone())
        })
    }

    /// Whether this agent is expected to emit usage updates.
    #[must_use]
    pub fn usage_supported(&self) -> bool {
        self.meta_capability_opt("usage_updates")
            .unwrap_or_else(|| crate::agent_quirks::usage_emit_default(self.agent_kind))
    }

    /// Probe a vendor `_meta` extension key and preserve absent/non-bool state.
    #[must_use]
    pub fn meta_capability_opt(&self, key: &str) -> Option<bool> {
        self.agent
            .meta
            .as_ref()
            .and_then(|m| m.get(key))
            .and_then(serde_json::Value::as_bool)
    }

    /// Probe a vendor `_meta` extension key (e.g. `"terminal_output"`).
    /// Returns false for missing keys, non-bool values, or absent meta.
    #[must_use]
    pub fn meta_capability(&self, key: &str) -> bool {
        self.meta_capability_opt(key).unwrap_or(false)
    }
}

fn legacy_route_for(caps: &SpurAgentCaps, key: &CapabilityKey) -> Option<DispatchRoute> {
    let native = match &key.kind {
        CapabilityKind::Model => caps.supports_set_model() || caps.supports_direct_set_model(),
        CapabilityKind::Effort => {
            caps.config_options
                .iter()
                .find(|option| {
                    category_matches(option.category.as_ref(), KnownConfigCategory::ThoughtLevel)
                })
                .is_some_and(has_select_choices)
                || caps.grok_display.as_ref().is_some_and(|display| {
                    display
                        .models()
                        .iter()
                        .any(|model| !model.efforts.is_empty())
                })
        }
        CapabilityKind::Mode => caps.supports_set_mode(),
        CapabilityKind::Custom(kind) if kind == "session_method" => {
            match key.upstream_id.as_str() {
                "session/load" => caps.supports_load_session(),
                "session/resume" => caps.supports_resume_session(),
                "session/delete" => caps.supports_delete_session(),
                "session/list" => caps.supports_list_sessions(),
                "session/close" => caps.supports_close_session(),
                _ => return None,
            }
        }
        CapabilityKind::Command | CapabilityKind::Custom(_) => return None,
    };
    Some(if native {
        DispatchRoute::NativePreferred
    } else {
        DispatchRoute::Hidden
    })
}

pub(crate) fn inject_capability_evidence_epoch(
    meta: &mut Option<Meta>,
    epoch: &EvidenceEpoch,
    completeness: CapabilityEvidenceCompleteness,
) {
    let wire = CapabilityEvidenceSnapshotWire::from(
        &CapabilityEvidenceSnapshot::from_epoch_with_completeness(
            epoch.clone(),
            epoch.identity(),
            completeness,
        ),
    );
    let Ok(value) = serde_json::to_value(EmbeddedCapabilityEvidenceWire {
        epoch: wire.epoch,
        completeness: wire.completeness,
    }) else {
        return;
    };
    meta.get_or_insert_with(Meta::new)
        .insert(CAPABILITY_EVIDENCE_META_KEY.to_owned(), value);
}

fn evidence_snapshot_from_meta(meta: Option<&Meta>) -> Option<CapabilityEvidenceSnapshot> {
    let wire: EmbeddedCapabilityEvidenceWire =
        serde_json::from_value(meta?.get(CAPABILITY_EVIDENCE_META_KEY)?.clone()).ok()?;
    CapabilityEvidenceSnapshot::try_from(CapabilityEvidenceSnapshotWire {
        epoch: wire.epoch,
        completeness: wire.completeness,
        reduced: Vec::new(),
        shadow_diffs: Vec::new(),
    })
    .ok()
}

fn embedded_evidence_snapshot(
    initialize_meta: Option<&Meta>,
    session_meta: Option<&Meta>,
) -> Option<CapabilityEvidenceSnapshot> {
    let initialize = evidence_snapshot_from_meta(initialize_meta);
    let session = evidence_snapshot_from_meta(session_meta);
    match (initialize, session) {
        (Some(initialize), Some(session)) if initialize.epoch().id() > session.epoch().id() => {
            Some(initialize)
        }
        (_, Some(session)) => Some(session),
        (Some(initialize), None) => Some(initialize),
        (None, None) => None,
    }
}

pub(crate) fn build_capability_cli_identity(
    command: &str,
    extra_args: &[String],
    launch_env: &BTreeMap<String, String>,
) -> CliIdentity {
    let resolved_executable = resolve_executable(command, launch_env);
    let mut argv = Vec::with_capacity(extra_args.len().saturating_add(1));
    argv.push(command.to_owned());
    argv.extend(extra_args.iter().cloned());
    let argv = redact_argv(argv);

    let mut environment = BTreeMap::new();
    for key in ["LANG", "LC_ALL", "LC_CTYPE", "PATH", "SHELL"] {
        if let Some(value) = launch_env
            .get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
        {
            environment.insert(key, value);
        }
    }

    CliIdentity {
        resolved_executable,
        upstream_version: None,
        argv_fingerprint: json_digest(&serde_json::json!(argv)),
        environment_fingerprint: json_digest(&serde_json::json!(environment)),
    }
}

pub(crate) fn update_identity_from_initialize_frame(
    identity: &mut CliIdentity,
    frame: &serde_json::Value,
) {
    let result = frame.get("result").unwrap_or(frame);
    identity.upstream_version = result
        .get("agentInfo")
        .or_else(|| result.get("agent_info"))
        .and_then(|info| info.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            result
                .get("agentVersion")
                .or_else(|| result.get("agent_version"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticProbeProjection {
    key: CapabilityKey,
    choices: Vec<CapabilityChoice>,
}

impl SemanticProbeProjection {
    pub(crate) fn retained_bytes(&self) -> usize {
        let kind_bytes = match &self.key.kind {
            CapabilityKind::Custom(kind) => kind.len(),
            _ => 0,
        };
        kind_bytes
            .saturating_add(self.key.upstream_id.len())
            .saturating_add(
                self.choices
                    .iter()
                    .map(|choice| {
                        choice
                            .id
                            .len()
                            .saturating_add(choice.label.len())
                            .saturating_add(
                                choice
                                    .description
                                    .as_ref()
                                    .map_or(0, std::string::String::len),
                            )
                    })
                    .sum::<usize>(),
            )
    }
}

fn projected_string(
    params: &serde_json::Value,
    camel_case: &str,
    snake_case: &str,
) -> Option<String> {
    params
        .get(camel_case)
        .or_else(|| params.get(snake_case))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty() && !value.contains("<redacted>"))
        .map(str::to_owned)
}

fn semantic_config_kind(identifier: &str) -> Option<CapabilityKind> {
    match identifier {
        "model" => Some(CapabilityKind::Model),
        "mode" => Some(CapabilityKind::Mode),
        "thought_level" | "reasoning_effort" | "effort" => Some(CapabilityKind::Effort),
        "command" => Some(CapabilityKind::Command),
        _ => None,
    }
}

pub(crate) fn project_semantic_probe(
    method: &str,
    raw_params: Option<&serde_json::Value>,
) -> Option<SemanticProbeProjection> {
    let params = redact_json(raw_params.unwrap_or(&serde_json::Value::Null));
    let (kind, upstream_id, choice_id) = match method {
        "session/set_model" => (
            CapabilityKind::Model,
            "model".to_owned(),
            projected_string(&params, "modelId", "model_id")?,
        ),
        "session/set_mode" => (
            CapabilityKind::Mode,
            "mode".to_owned(),
            projected_string(&params, "modeId", "mode_id")?,
        ),
        "session/set_config_option" => {
            let config_id = projected_string(&params, "configId", "config_id")?;
            if secret_key(&config_id) {
                return None;
            }
            let category =
                projected_string(&params, "configCategory", "config_category").or_else(|| {
                    params
                        .get("category")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                });
            let kind = semantic_config_kind(category.as_deref().unwrap_or(&config_id))?;
            let choice_id = params
                .get("value")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty() && !value.contains("<redacted>"))
                .map(str::to_owned)?;
            (kind, config_id, choice_id)
        }
        _ => return None,
    };
    Some(SemanticProbeProjection {
        key: CapabilityKey { kind, upstream_id },
        choices: vec![CapabilityChoice {
            id: choice_id.clone(),
            label: choice_id,
            description: None,
        }],
    })
}

pub(crate) fn normalize_raw_acp_observation(
    identity: &CliIdentity,
    method: &str,
    frame: &serde_json::Value,
    session_scope: EvidenceSessionScope,
    probe_projection: Option<&SemanticProbeProjection>,
) -> Vec<EvidenceRecord> {
    let digest = RawEvidenceDigest(json_digest(&redact_json(frame)));
    let observed_at = ObservationTime(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_millis().min(u64::MAX as u128) as u64
            }),
    );
    let mut records = Vec::new();

    if let Some(error) = frame.get("error").filter(|error| !error.is_null()) {
        let failure = failure_record(
            identity,
            method,
            error,
            observed_at,
            digest.clone(),
            session_scope.clone(),
        );
        let rejected = failure.claim == EvidenceClaim::Rejected;
        records.push(failure);
        if rejected {
            if let Some(projection) = probe_projection {
                records.push(make_record(
                    identity,
                    projection.key.kind.clone(),
                    &projection.key.upstream_id,
                    EvidenceClaim::Rejected,
                    EvidenceProvenance::RejectedActiveProbe,
                    observed_at,
                    digest,
                    session_scope,
                    projection.choices.clone(),
                ));
            }
        }
        return records;
    }

    if frame.get("method").is_none() && frame.get("result").is_none() {
        records.push(make_record(
            identity,
            CapabilityKind::Custom("failure".to_owned()),
            &format!("operational:{method}"),
            EvidenceClaim::Unknown,
            EvidenceProvenance::InconclusiveFailure,
            observed_at,
            digest,
            session_scope,
            Vec::new(),
        ));
        return records;
    }

    if frame.get("method").is_none() {
        if let Some(projection) = probe_projection {
            records.push(make_record(
                identity,
                projection.key.kind.clone(),
                &projection.key.upstream_id,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
                observed_at,
                digest.clone(),
                session_scope.clone(),
                projection.choices.clone(),
            ));
        }
    }

    let payload = if frame.get("method").is_some() {
        frame.get("params").unwrap_or(&serde_json::Value::Null)
    } else {
        frame.get("result").unwrap_or(frame)
    };

    match method {
        "initialize" => {
            normalize_initialize_capabilities(
                &mut records,
                identity,
                payload,
                observed_at,
                &digest,
                &session_scope,
            );
            normalize_vendor_planes(
                &mut records,
                identity,
                payload,
                observed_at,
                &digest,
                &session_scope,
            );
            normalize_unknown_top_level_fields(
                &mut records,
                identity,
                payload,
                &[
                    "protocolVersion",
                    "protocol_version",
                    "agentCapabilities",
                    "agent_capabilities",
                    "agentInfo",
                    "agent_info",
                    "authMethods",
                    "auth_methods",
                ],
                EvidenceProvenance::VendorAdvertisement,
                observed_at,
                &digest,
                &session_scope,
            );
        }
        "session/new" | "session/load" => {
            normalize_session_planes(
                &mut records,
                identity,
                payload,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::StandardAdvertisement,
                observed_at,
                &digest,
                &session_scope,
            );
            normalize_vendor_planes(
                &mut records,
                identity,
                payload,
                observed_at,
                &digest,
                &session_scope,
            );
            normalize_unknown_top_level_fields(
                &mut records,
                identity,
                payload,
                &[
                    "sessionId",
                    "session_id",
                    "modes",
                    "configOptions",
                    "config_options",
                ],
                EvidenceProvenance::VendorAdvertisement,
                observed_at,
                &digest,
                &session_scope,
            );
        }
        "session/prompt" => records.push(make_record(
            identity,
            CapabilityKind::Command,
            "session/prompt",
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::PromptFallback,
            observed_at,
            digest,
            session_scope,
            Vec::new(),
        )),
        _ if frame.get("method").is_some() => {
            records.push(make_record(
                identity,
                CapabilityKind::Custom("notification".to_owned()),
                method,
                EvidenceClaim::CandidateObserved,
                EvidenceProvenance::ObservedNotification,
                observed_at,
                digest.clone(),
                session_scope.clone(),
                Vec::new(),
            ));
            normalize_notification_choices(
                &mut records,
                identity,
                payload,
                observed_at,
                &digest,
                &session_scope,
            );
            normalize_unknown_top_level_fields(
                &mut records,
                identity,
                payload,
                &["sessionId", "session_id"],
                EvidenceProvenance::ObservedNotification,
                observed_at,
                &digest,
                &session_scope,
            );
        }
        _ => {}
    }

    records
}

pub(crate) fn operational_failure_record(
    identity: &CliIdentity,
    method: &str,
    error: &str,
    session_scope: EvidenceSessionScope,
) -> EvidenceRecord {
    let redacted = redact_json(&serde_json::json!({"method": method, "error": error}));
    failure_record(
        identity,
        method,
        &redacted["error"],
        ObservationTime(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| {
                    duration.as_millis().min(u64::MAX as u128) as u64
                }),
        ),
        RawEvidenceDigest(json_digest(&redacted)),
        session_scope,
    )
}

pub(crate) fn native_dispatch_failure_record(
    identity: &CliIdentity,
    key: CapabilityKey,
    method: &str,
    error: &str,
    session_scope: EvidenceSessionScope,
) -> EvidenceRecord {
    let redacted = redact_json(&serde_json::json!({"method": method, "error": error}));
    EvidenceRecord {
        key,
        claim: EvidenceClaim::NativeFailed,
        provenance: EvidenceProvenance::NativeDispatch,
        identity: identity.clone(),
        observed_at: ObservationTime(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| {
                    duration.as_millis().min(u64::MAX as u128) as u64
                }),
        ),
        raw_digest: RawEvidenceDigest(json_digest(&redacted)),
        session_scope,
        choices: Vec::new(),
    }
}

fn normalize_initialize_capabilities(
    records: &mut Vec<EvidenceRecord>,
    identity: &CliIdentity,
    payload: &serde_json::Value,
    observed_at: ObservationTime,
    digest: &RawEvidenceDigest,
    scope: &EvidenceSessionScope,
) {
    let capabilities = payload
        .get("agentCapabilities")
        .or_else(|| payload.get("agent_capabilities"));
    let Some(capabilities) = capabilities else {
        return;
    };
    if capabilities
        .get("loadSession")
        .or_else(|| capabilities.get("load_session"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        records.push(make_record(
            identity,
            CapabilityKind::Custom("session_method".to_owned()),
            "session/load",
            EvidenceClaim::NativeVerified,
            EvidenceProvenance::StandardAdvertisement,
            observed_at,
            digest.clone(),
            scope.clone(),
            Vec::new(),
        ));
    }
    let session = capabilities
        .get("sessionCapabilities")
        .or_else(|| capabilities.get("session_capabilities"));
    for (field, method) in [
        ("resume", "session/resume"),
        ("delete", "session/delete"),
        ("list", "session/list"),
        ("close", "session/close"),
    ] {
        if session.and_then(|session| session.get(field)).is_some() {
            records.push(make_record(
                identity,
                CapabilityKind::Custom("session_method".to_owned()),
                method,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::StandardAdvertisement,
                observed_at,
                digest.clone(),
                scope.clone(),
                Vec::new(),
            ));
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "evidence records preserve the full kernel contract"
)]
fn normalize_session_planes(
    records: &mut Vec<EvidenceRecord>,
    identity: &CliIdentity,
    payload: &serde_json::Value,
    claim: EvidenceClaim,
    provenance: EvidenceProvenance,
    observed_at: ObservationTime,
    digest: &RawEvidenceDigest,
    scope: &EvidenceSessionScope,
) {
    if let Some(options) = payload
        .get("configOptions")
        .or_else(|| payload.get("config_options"))
        .and_then(serde_json::Value::as_array)
    {
        for option in options {
            let upstream_id = option
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("config_choice");
            let category = option
                .get("category")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(upstream_id);
            let kind = match category {
                "model" => CapabilityKind::Model,
                "thought_level" | "reasoning_effort" | "effort" => CapabilityKind::Effort,
                "mode" => CapabilityKind::Mode,
                "command" => CapabilityKind::Command,
                other => CapabilityKind::Custom(other.to_owned()),
            };
            let choices = option
                .get("options")
                .map(extract_choices)
                .unwrap_or_default();
            if !choices.is_empty() {
                records.push(make_record(
                    identity,
                    kind,
                    upstream_id,
                    claim,
                    provenance,
                    observed_at,
                    digest.clone(),
                    scope.clone(),
                    choices,
                ));
            }
        }
    }

    let modes = payload.get("modes");
    let available_modes = modes
        .and_then(|modes| {
            modes
                .get("availableModes")
                .or_else(|| modes.get("available_modes"))
        })
        .map(extract_choices)
        .unwrap_or_default();
    if !available_modes.is_empty() {
        records.push(make_record(
            identity,
            CapabilityKind::Mode,
            "mode",
            claim,
            provenance,
            observed_at,
            digest.clone(),
            scope.clone(),
            available_modes,
        ));
    }
}

fn normalize_vendor_planes(
    records: &mut Vec<EvidenceRecord>,
    identity: &CliIdentity,
    payload: &serde_json::Value,
    observed_at: ObservationTime,
    digest: &RawEvidenceDigest,
    scope: &EvidenceSessionScope,
) {
    let meta = payload.get("_meta").unwrap_or(&serde_json::Value::Null);
    for models in [
        payload.get("models"),
        meta.get("modelState"),
        meta.get("model_state"),
    ]
    .into_iter()
    .flatten()
    {
        let choices = models
            .get("availableModels")
            .or_else(|| models.get("available_models"))
            .map(extract_model_choices)
            .unwrap_or_default();
        if !choices.is_empty() {
            records.push(make_record(
                identity,
                CapabilityKind::Model,
                "model",
                EvidenceClaim::CandidateObserved,
                EvidenceProvenance::VendorAdvertisement,
                observed_at,
                digest.clone(),
                scope.clone(),
                choices,
            ));
        }
        let efforts = extract_reasoning_efforts(models);
        if !efforts.is_empty() {
            records.push(make_record(
                identity,
                CapabilityKind::Effort,
                "reasoning_effort",
                EvidenceClaim::CandidateObserved,
                EvidenceProvenance::VendorAdvertisement,
                observed_at,
                digest.clone(),
                scope.clone(),
                efforts,
            ));
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "unknown field evidence preserves the full kernel contract"
)]
fn normalize_unknown_top_level_fields(
    records: &mut Vec<EvidenceRecord>,
    identity: &CliIdentity,
    payload: &serde_json::Value,
    known_fields: &[&str],
    provenance: EvidenceProvenance,
    observed_at: ObservationTime,
    digest: &RawEvidenceDigest,
    scope: &EvidenceSessionScope,
) {
    let Some(object) = payload.as_object() else {
        return;
    };
    let mut paths = BTreeSet::new();
    for (field, value) in object {
        if known_fields.contains(&field.as_str()) {
            continue;
        }
        collect_sanitized_field_paths(&sanitize_field_path_segment(field), value, &mut paths);
    }
    for path in paths {
        records.push(make_record(
            identity,
            CapabilityKind::Custom("unknown_acp_field".to_owned()),
            &path,
            EvidenceClaim::Unknown,
            provenance,
            observed_at,
            digest.clone(),
            scope.clone(),
            Vec::new(),
        ));
    }
}

fn collect_sanitized_field_paths(
    path: &str,
    value: &serde_json::Value,
    paths: &mut BTreeSet<String>,
) {
    paths.insert(path.to_owned());
    match value {
        serde_json::Value::Object(object) => {
            for (field, value) in object {
                let field = sanitize_field_path_segment(field);
                collect_sanitized_field_paths(&format!("{path}.{field}"), value, paths);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_sanitized_field_paths(path, value, paths);
            }
        }
        _ => {}
    }
}

fn sanitize_field_path_segment(segment: &str) -> String {
    let redacted = redact_text(segment);
    if redacted == "<redacted>" {
        return redacted;
    }
    let sanitized = redacted
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "<empty>".to_owned()
    } else {
        sanitized
    }
}

fn normalize_notification_choices(
    records: &mut Vec<EvidenceRecord>,
    identity: &CliIdentity,
    payload: &serde_json::Value,
    observed_at: ObservationTime,
    digest: &RawEvidenceDigest,
    scope: &EvidenceSessionScope,
) {
    let update = payload.get("update").unwrap_or(payload);
    let commands = update
        .get("availableCommands")
        .or_else(|| update.get("available_commands"))
        .or_else(|| update.get("commands"))
        .map(extract_command_choices)
        .unwrap_or_default();
    if !commands.is_empty() {
        records.push(make_record(
            identity,
            CapabilityKind::Command,
            "commands",
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::ObservedNotification,
            observed_at,
            digest.clone(),
            scope.clone(),
            commands,
        ));
    }

    normalize_session_planes(
        records,
        identity,
        update,
        EvidenceClaim::CandidateObserved,
        EvidenceProvenance::ObservedNotification,
        observed_at,
        digest,
        scope,
    );

    let model_id = update
        .get("model_id")
        .or_else(|| update.get("modelId"))
        .and_then(serde_json::Value::as_str);
    if let Some(model_id) = model_id.filter(|id| !id.is_empty()) {
        records.push(make_record(
            identity,
            CapabilityKind::Model,
            "model",
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::ObservedNotification,
            observed_at,
            digest.clone(),
            scope.clone(),
            vec![CapabilityChoice {
                id: model_id.to_owned(),
                label: model_id.to_owned(),
                description: None,
            }],
        ));
    }
}

fn failure_record(
    identity: &CliIdentity,
    method: &str,
    error: &serde_json::Value,
    observed_at: ObservationTime,
    digest: RawEvidenceDigest,
    session_scope: EvidenceSessionScope,
) -> EvidenceRecord {
    let lowered = serde_json::to_string(error)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let authentication = method == "authenticate"
        || [
            "authentication required",
            "unauthenticated",
            "unauthorized",
            "login required",
            "not logged in",
            "sign in required",
        ]
        .iter()
        .any(|marker| lowered.contains(marker));
    let timeout = lowered.contains("timed out") || lowered.contains("timeout");
    let malformed = lowered.contains("malformed")
        || lowered.contains("deserialize")
        || lowered.contains("invalid json");
    let code = error.get("code").and_then(serde_json::Value::as_i64);
    let malformed = malformed
        || code == Some(-32700)
        || lowered.contains("parse error")
        || lowered.contains("invalid request");
    let rejected = matches!(code, Some(-32601 | -32602))
        || lowered.contains("rejected")
        || lowered.contains("method not found")
        || lowered.contains("invalid params")
        || lowered.contains("invalid parameters");
    let (upstream_id, claim, provenance) = if authentication {
        (
            "authentication".to_owned(),
            EvidenceClaim::Inconclusive,
            EvidenceProvenance::InconclusiveFailure,
        )
    } else if timeout {
        (
            "timeout".to_owned(),
            EvidenceClaim::Unknown,
            EvidenceProvenance::InconclusiveFailure,
        )
    } else if malformed {
        (
            "malformed".to_owned(),
            EvidenceClaim::Inconclusive,
            EvidenceProvenance::InconclusiveFailure,
        )
    } else if rejected {
        (
            format!("rejected:{method}"),
            EvidenceClaim::Rejected,
            EvidenceProvenance::NativeDispatch,
        )
    } else {
        (
            format!("operational:{method}"),
            EvidenceClaim::Unknown,
            EvidenceProvenance::InconclusiveFailure,
        )
    };
    make_record(
        identity,
        CapabilityKind::Custom("failure".to_owned()),
        &upstream_id,
        claim,
        provenance,
        observed_at,
        digest,
        session_scope,
        Vec::new(),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "evidence records preserve the full kernel contract"
)]
fn make_record(
    identity: &CliIdentity,
    kind: CapabilityKind,
    upstream_id: &str,
    claim: EvidenceClaim,
    provenance: EvidenceProvenance,
    observed_at: ObservationTime,
    raw_digest: RawEvidenceDigest,
    session_scope: EvidenceSessionScope,
    choices: Vec<CapabilityChoice>,
) -> EvidenceRecord {
    EvidenceRecord {
        key: CapabilityKey {
            kind,
            upstream_id: upstream_id.to_owned(),
        },
        claim,
        provenance,
        identity: identity.clone(),
        observed_at,
        raw_digest,
        session_scope,
        choices,
    }
}

fn extract_choices(value: &serde_json::Value) -> Vec<CapabilityChoice> {
    let mut choices = Vec::new();
    visit_choice_values(value, &mut |item| {
        let Some(id) = item
            .get("value")
            .or_else(|| item.get("id"))
            .or_else(|| item.get("modeId"))
            .or_else(|| item.get("modelId"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let label = item
            .get("name")
            .or_else(|| item.get("label"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(id);
        let description = item
            .get("description")
            .and_then(serde_json::Value::as_str)
            .map(redact_text);
        choices.push(CapabilityChoice {
            id: id.to_owned(),
            label: redact_text(label),
            description,
        });
    });
    choices.sort();
    choices.dedup_by(|left, right| left.id == right.id);
    choices
}

fn extract_model_choices(value: &serde_json::Value) -> Vec<CapabilityChoice> {
    extract_choices(value)
}

fn extract_command_choices(value: &serde_json::Value) -> Vec<CapabilityChoice> {
    let mut choices = Vec::new();
    visit_choice_values(value, &mut |item| {
        let Some(id) = item
            .get("name")
            .or_else(|| item.get("id"))
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        choices.push(CapabilityChoice {
            id: id.to_owned(),
            label: item
                .get("label")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| id.to_owned(), redact_text),
            description: item
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(redact_text),
        });
    });
    choices.sort();
    choices.dedup_by(|left, right| left.id == right.id);
    choices
}

fn visit_choice_values(
    value: &serde_json::Value,
    visit: &mut impl FnMut(&serde_json::Map<String, serde_json::Value>),
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                visit_choice_values(value, visit);
            }
        }
        serde_json::Value::Object(object) => {
            if object.contains_key("value")
                || object.contains_key("id")
                || object.contains_key("modeId")
                || object.contains_key("modelId")
                || object.contains_key("name")
            {
                visit(object);
            } else {
                for value in object.values() {
                    visit_choice_values(value, visit);
                }
            }
        }
        serde_json::Value::String(id) if !id.is_empty() => {
            let mut object = serde_json::Map::new();
            object.insert("id".to_owned(), serde_json::Value::String(id.clone()));
            visit(&object);
        }
        _ => {}
    }
}

fn extract_reasoning_efforts(models: &serde_json::Value) -> Vec<CapabilityChoice> {
    let mut efforts = Vec::new();
    let available = models
        .get("availableModels")
        .or_else(|| models.get("available_models"))
        .and_then(serde_json::Value::as_array);
    for model in available.into_iter().flatten() {
        let effort_values = model
            .get("_meta")
            .and_then(|meta| {
                meta.get("reasoningEfforts")
                    .or_else(|| meta.get("reasoning_efforts"))
            })
            .unwrap_or(&serde_json::Value::Null);
        efforts.extend(extract_choices(effort_values));
    }
    efforts.sort();
    efforts.dedup_by(|left, right| left.id == right.id);
    efforts
}

fn resolve_executable(command: &str, launch_env: &BTreeMap<String, String>) -> PathBuf {
    let candidate = PathBuf::from(command);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        return std::fs::canonicalize(&candidate).unwrap_or(candidate);
    }
    let path = launch_env
        .get("PATH")
        .cloned()
        .or_else(|| std::env::var("PATH").ok());
    if let Some(path) = path {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(command);
            if candidate.is_file() {
                return std::fs::canonicalize(&candidate).unwrap_or(candidate);
            }
        }
    }
    candidate
}

fn redact_argv(argv: Vec<String>) -> Vec<String> {
    let mut redacted = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for argument in argv {
        if redact_next {
            redacted.push("<redacted>".to_owned());
            redact_next = false;
            continue;
        }
        if let Some((flag, _)) = argument.split_once('=') {
            if flag.starts_with('-') && secret_key(flag.trim_start_matches('-')) {
                redacted.push(format!("{flag}=<redacted>"));
                continue;
            }
        }
        if argument.starts_with('-') && secret_key(argument.trim_start_matches('-')) {
            redacted.push(argument);
            redact_next = true;
        } else {
            redacted.push(redact_text(&argument));
        }
    }
    redacted
}

fn redact_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if secret_key(key) {
                            serde_json::Value::String("<redacted>".to_owned())
                        } else {
                            redact_json(value)
                        },
                    )
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(redact_json).collect())
        }
        serde_json::Value::String(value) => serde_json::Value::String(redact_text(value)),
        _ => value.clone(),
    }
}

fn secret_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "secret",
        "token",
        "password",
        "passphrase",
        "authorization",
        "cookie",
        "credential",
        "apikey",
        "privatekey",
        "clientsecret",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn redact_text(value: &str) -> String {
    let lowered = value.to_ascii_lowercase();
    if [
        "bearer ",
        "basic ",
        "token=",
        "password=",
        "passphrase=",
        "secret=",
        "authorization=",
        "cookie=",
        "api_key=",
        "api-key=",
        "client_secret=",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        "<redacted>".to_owned()
    } else {
        value.to_owned()
    }
}

fn json_digest(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let digest = sha256(&bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn sha256(input: &[u8]) -> [u8; 32] {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = sum0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = [0_u8; 32];
    for (index, word) in state.into_iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

fn model_option_from(options: &[SessionConfigOption]) -> Option<&SessionConfigOption> {
    option_by_category_or_absent_id(options, KnownConfigCategory::Model, "model")
}

pub fn thought_level_option_from(options: &[SessionConfigOption]) -> Option<&SessionConfigOption> {
    option_by_category_or_absent_id(
        options,
        KnownConfigCategory::ThoughtLevel,
        "reasoning_effort",
    )
}

/// Current select **value id** (not display label) for a config option.
#[must_use]
pub fn current_select_value_id(option: &SessionConfigOption) -> Option<&str> {
    match &option.kind {
        SessionConfigKind::Select(select) => {
            let v = select.current_value.0.as_ref();
            if v.is_empty() {
                None
            } else {
                Some(v)
            }
        }
        _ => None,
    }
}

/// Active model **id** from config options (for cost.db / metrics; not label).
/// Authority: sol_0225528da3534508 P2 ACP observe.
#[must_use]
pub fn model_id_from_config_options(options: &[SessionConfigOption]) -> Option<String> {
    let option = model_option_from(options)?;
    current_select_value_id(option).map(str::to_owned)
}

/// Active effort / thought-level **id** from config options.
#[must_use]
pub fn effort_id_from_config_options(options: &[SessionConfigOption]) -> Option<String> {
    let option = thought_level_option_from(options)?;
    current_select_value_id(option).map(str::to_owned)
}

#[derive(Clone, Copy)]
enum KnownConfigCategory {
    Model,
    ThoughtLevel,
}

fn option_by_category_or_absent_id<'a>(
    options: &'a [SessionConfigOption],
    category: KnownConfigCategory,
    fallback_id: &str,
) -> Option<&'a SessionConfigOption> {
    options
        .iter()
        .find(|option| category_matches(option.category.as_ref(), category))
        .or_else(|| {
            options
                .iter()
                .find(|option| option.category.is_none() && option.id.0.as_ref() == fallback_id)
        })
}

fn category_matches(
    category: Option<&SessionConfigOptionCategory>,
    expected: KnownConfigCategory,
) -> bool {
    matches!(
        (expected, category),
        (
            KnownConfigCategory::Model,
            Some(SessionConfigOptionCategory::Model)
        ) | (
            KnownConfigCategory::ThoughtLevel,
            Some(SessionConfigOptionCategory::ThoughtLevel)
        )
    )
}

fn has_select_choices(option: &SessionConfigOption) -> bool {
    matches!(
        &option.kind,
        SessionConfigKind::Select(select)
            if match &select.options {
                SessionConfigSelectOptions::Ungrouped(options) => !options.is_empty(),
                SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .any(|group| !group.options.is_empty()),
                _ => false,
            }
    )
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeResponse, NewSessionResponse, SessionCapabilities,
        SessionCloseCapabilities, SessionConfigId, SessionConfigOption,
        SessionConfigOptionCategory, SessionConfigSelectOption, SessionDeleteCapabilities,
        SessionId, SessionListCapabilities, SessionMode, SessionModeId, SessionModeState,
        SessionResumeCapabilities,
    };
    use agent_client_protocol::schema::ProtocolVersion;

    use super::{json_digest, normalize_raw_acp_observation};
    use crate::capability_evidence::{
        CliIdentity, EvidenceClaim, EvidenceProvenance, EvidenceSessionScope,
    };
    use crate::spur_agent_caps::SpurAgentCaps;
    use crate::types::AgentKind;

    fn empty_init_response() -> InitializeResponse {
        InitializeResponse::new(ProtocolVersion::LATEST)
    }

    fn empty_new_session_response() -> NewSessionResponse {
        NewSessionResponse::new(SessionId::new("test-empty"))
    }

    #[test]
    fn evidence_digest_is_stable_sha256() {
        assert_eq!(
            json_digest(&serde_json::Value::Null),
            "sha256:74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b"
        );
    }

    #[test]
    fn raw_acp_errors_are_classified_conservatively() {
        let identity = CliIdentity {
            resolved_executable: "/usr/bin/test-acp".into(),
            upstream_version: Some("1.0.0".to_owned()),
            argv_fingerprint: "sha256:argv".to_owned(),
            environment_fingerprint: "sha256:env".to_owned(),
        };
        let cases = [
            (
                "internal error",
                "session/load",
                -32603,
                "Internal error",
                EvidenceClaim::Unknown,
                EvidenceProvenance::InconclusiveFailure,
                "operational:session/load",
            ),
            (
                "generic server error",
                "session/load",
                -32000,
                "Server error",
                EvidenceClaim::Unknown,
                EvidenceProvenance::InconclusiveFailure,
                "operational:session/load",
            ),
            (
                "timeout",
                "session/load",
                -32000,
                "request timeout",
                EvidenceClaim::Unknown,
                EvidenceProvenance::InconclusiveFailure,
                "timeout",
            ),
            (
                "authentication",
                "authenticate",
                -32001,
                "authentication required",
                EvidenceClaim::Inconclusive,
                EvidenceProvenance::InconclusiveFailure,
                "authentication",
            ),
            (
                "malformed",
                "session/load",
                -32700,
                "Parse error",
                EvidenceClaim::Inconclusive,
                EvidenceProvenance::InconclusiveFailure,
                "malformed",
            ),
            (
                "method not found",
                "session/load",
                -32601,
                "Method not found",
                EvidenceClaim::Rejected,
                EvidenceProvenance::NativeDispatch,
                "rejected:session/load",
            ),
            (
                "invalid params",
                "session/load",
                -32602,
                "Invalid params",
                EvidenceClaim::Rejected,
                EvidenceProvenance::NativeDispatch,
                "rejected:session/load",
            ),
        ];

        for (label, method, code, message, claim, provenance, upstream_id) in cases {
            let frame = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": code, "message": message}
            });
            let records = normalize_raw_acp_observation(
                &identity,
                method,
                &frame,
                EvidenceSessionScope::Global,
                None,
            );
            assert_eq!(records.len(), 1, "{label}");
            let record = &records[0];
            assert_eq!(record.claim, claim, "{label}");
            assert_eq!(record.provenance, provenance, "{label}");
            assert_eq!(record.key.upstream_id, upstream_id, "{label}");
        }
    }

    #[test]
    fn serialized_caps_include_agent_kind() {
        let init = empty_init_response();
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        let value = serde_json::to_value(caps).expect("caps must serialize");

        assert_eq!(
            value.get("agent_kind").and_then(serde_json::Value::as_str),
            Some("codex-acp"),
            "caps snapshots must carry the agent kind that created them"
        );
    }

    fn agent_caps_with_meta(key: &str, val: serde_json::Value) -> AgentCapabilities {
        let mut meta = serde_json::Map::new();
        meta.insert(key.to_string(), val);
        AgentCapabilities::new().meta(meta)
    }

    #[test]
    fn current_model_label_resolves_via_config_option_choices() {
        let init = empty_init_response();
        let mut new = NewSessionResponse::new(SessionId::new("model-label"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gpt-5",
            vec![SessionConfigSelectOption::new("gpt-5", "GPT-5")],
        )]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_model_label().as_deref(), Some("GPT-5"));
    }

    #[test]
    fn model_option_prefers_model_category_over_model_id_allowlist() {
        let init = empty_init_response();
        let mut new = NewSessionResponse::new(SessionId::new("model-category"));
        new.config_options = Some(vec![
            SessionConfigOption::select(
                SessionConfigId::new("model"),
                "Legacy model",
                "legacy-model",
                vec![SessionConfigSelectOption::new(
                    "legacy-model",
                    "Legacy Model",
                )],
            ),
            SessionConfigOption::select(
                SessionConfigId::new("vendor_model"),
                "Vendor model",
                "vendor-sonnet",
                vec![SessionConfigSelectOption::new(
                    "vendor-sonnet",
                    "Vendor Sonnet",
                )],
            )
            .category(SessionConfigOptionCategory::Model),
        ]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        let option = caps.model_option().expect("model option must resolve");
        assert_eq!(option.id.0.as_ref(), "vendor_model");
        assert!(caps.supports_set_model());
        assert_eq!(caps.current_model_label().as_deref(), Some("Vendor Sonnet"));
    }

    #[test]
    fn current_model_label_falls_back_to_raw_id() {
        let init = empty_init_response();
        let mut new = NewSessionResponse::new(SessionId::new("model-label"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gpt-5",
            vec![SessionConfigSelectOption::new("other", "Other")],
        )]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_model_label().as_deref(), Some("gpt-5"));
    }

    #[test]
    fn current_model_label_returns_none_without_model_option() {
        let init = empty_init_response();
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert_eq!(caps.current_model_label(), None);
    }

    #[test]
    fn current_effort_label_resolves_via_select() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("reasoning_effort"),
            "Reasoning effort",
            "medium",
            vec![SessionConfigSelectOption::new("medium", "Medium")],
        )]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_effort_label().as_deref(), Some("Medium"));
    }

    #[test]
    fn current_effort_label_prefers_thought_level_category() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.config_options = Some(vec![
            SessionConfigOption::select(
                SessionConfigId::new("reasoning_effort"),
                "Reasoning effort",
                "low",
                vec![SessionConfigSelectOption::new("low", "Low")],
            ),
            SessionConfigOption::select(
                SessionConfigId::new("thinking_level"),
                "Thinking level",
                "high",
                vec![SessionConfigSelectOption::new("high", "High")],
            )
            .category(SessionConfigOptionCategory::ThoughtLevel),
        ]);
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert_eq!(caps.current_effort_label().as_deref(), Some("High"));
    }

    #[test]
    fn grok_empty_config_options_use_frozen_meta_display_labels() {
        let mut init = empty_init_response();
        init.meta = serde_json::from_value(serde_json::json!({
            "modelState": {
                "currentModelId": "grok-composer-2.5-fast",
                "availableModels": [{
                    "modelId": "grok-composer-2.5-fast",
                    "name": "Grok Composer 2.5 Fast"
                }]
            }
        }))
        .expect("initialize meta fixture must deserialize");
        let mut new = empty_new_session_response();
        new.meta = serde_json::from_value(serde_json::json!({
            "x.ai/sessionConfig": {
                "options": [{
                    "id": "high",
                    "category": "mode",
                    "label": "High Effort",
                    "selected": true
                }]
            }
        }))
        .expect("session meta fixture must deserialize");

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Grok);

        assert_eq!(
            caps.current_model_label().as_deref(),
            Some("Grok Composer 2.5 Fast")
        );
        assert_eq!(caps.current_effort_label().as_deref(), Some("High Effort"));
        assert!(caps.grok_display.is_some());
        assert!(!caps.supports_set_model());
        assert!(!caps.supports_set_config_option());
    }

    #[test]
    fn config_options_win_over_grok_meta_display_labels() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.meta = serde_json::from_value(serde_json::json!({
            "x.ai/sessionConfig": {
                "options": [
                    {
                        "id": "grok-4.5",
                        "category": "model",
                        "label": "Grok 4.5",
                        "selected": true
                    },
                    {
                        "id": "low",
                        "category": "mode",
                        "label": "Low Effort",
                        "selected": true
                    }
                ]
            }
        }))
        .expect("session meta fixture must deserialize");
        new.config_options = Some(vec![
            SessionConfigOption::select(
                SessionConfigId::new("model"),
                "Model",
                "future-grok",
                vec![SessionConfigSelectOption::new("future-grok", "Future Grok")],
            ),
            SessionConfigOption::select(
                SessionConfigId::new("reasoning_effort"),
                "Reasoning effort",
                "high",
                vec![SessionConfigSelectOption::new("high", "Very High")],
            ),
        ]);

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Grok);

        assert_eq!(caps.current_model_label().as_deref(), Some("Future Grok"));
        assert_eq!(caps.current_effort_label().as_deref(), Some("Very High"));
        assert!(caps.supports_set_model());
        assert!(caps.supports_set_config_option());
    }

    #[test]
    fn loaded_grok_session_freezes_display_from_meta() {
        let init = empty_init_response();
        let mut loaded = agent_client_protocol::schema::v1::LoadSessionResponse::new();
        loaded.meta = serde_json::from_value(serde_json::json!({
            "x.ai/sessionConfig": {
                "options": [
                    {
                        "id": "grok-4.5",
                        "category": "model",
                        "label": "Grok 4.5",
                        "selected": true
                    },
                    {
                        "id": "medium",
                        "category": "mode",
                        "label": "Medium Effort",
                        "selected": true
                    }
                ]
            }
        }))
        .expect("load meta fixture must deserialize");

        let caps = SpurAgentCaps::from_loaded(&init, &loaded, AgentKind::Grok);

        assert_eq!(caps.current_model_label().as_deref(), Some("Grok 4.5"));
        assert_eq!(
            caps.current_effort_label().as_deref(),
            Some("Medium Effort")
        );
    }

    #[test]
    fn non_grok_agents_ignore_grok_shaped_meta() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.meta = serde_json::from_value(serde_json::json!({
            "x.ai/sessionConfig": {
                "options": [{
                    "id": "grok-4.5",
                    "category": "model",
                    "label": "Grok 4.5",
                    "selected": true
                }]
            }
        }))
        .expect("session meta fixture must deserialize");

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert_eq!(caps.current_model_label(), None);
        assert_eq!(caps.current_effort_label(), None);
        assert_eq!(caps.grok_display, None);
    }

    #[test]
    fn kiro_recovered_models_meta_drives_label_and_direct_set_gate() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.meta = serde_json::from_value(serde_json::json!({
            "spur.recoveredModels": {
                "availableModels": [
                    {"modelId": "auto", "name": "auto"},
                    {"modelId": "glm-5", "name": "GLM-5"}
                ],
                "currentModelId": "glm-5"
            }
        }))
        .expect("kiro recovered models meta must deserialize");

        let mut caps = SpurAgentCaps::new(&init, &new, AgentKind::Kiro);

        assert_eq!(caps.current_model_label().as_deref(), Some("GLM-5"));
        assert!(caps.supports_kiro_set_model());
        assert!(caps.supports_direct_set_model());
        assert!(!caps.supports_set_model());
        assert!(!caps.supports_set_config_option());
        assert!(caps.apply_kiro_model_selected("auto"));
        assert_eq!(caps.current_model_label().as_deref(), Some("auto"));
    }

    #[test]
    fn non_kiro_agents_ignore_recovered_models_meta() {
        let init = empty_init_response();
        let mut new = empty_new_session_response();
        new.meta = serde_json::from_value(serde_json::json!({
            "spur.recoveredModels": {
                "availableModels": [{"modelId": "x", "name": "X"}],
                "currentModelId": "x"
            }
        }))
        .expect("meta fixture must deserialize");

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);
        assert_eq!(caps.current_model_label(), None);
        assert!(!caps.supports_kiro_set_model());
        assert_eq!(caps.kiro_display, None);
    }

    #[test]
    fn model_label_from_config_options_returns_display_name_and_falls_back() {
        let named = vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "sonnet",
            vec![
                SessionConfigSelectOption::new("sonnet", "Sonnet"),
                SessionConfigSelectOption::new("opus", "Opus"),
            ],
        )];
        assert_eq!(
            SpurAgentCaps::model_label_from_config_options(&named),
            Some("Sonnet")
        );

        let fallback = vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "sonnet",
            vec![SessionConfigSelectOption::new("opus", "Opus")],
        )];
        assert_eq!(
            SpurAgentCaps::model_label_from_config_options(&fallback),
            Some("sonnet")
        );

        let no_model = vec![SessionConfigOption::select(
            SessionConfigId::new("reasoning_effort"),
            "Reasoning effort",
            "medium",
            vec![SessionConfigSelectOption::new("medium", "Medium")],
        )];
        assert_eq!(
            SpurAgentCaps::model_label_from_config_options(&no_model),
            None
        );
    }

    #[test]
    fn usage_supported_delegates_to_quirks() {
        let init = empty_init_response();
        let new = empty_new_session_response();

        let claude = SpurAgentCaps::new(&init, &new, AgentKind::ClaudeCodeAcp);
        let codex = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert!(!claude.usage_supported());
        assert!(codex.usage_supported());
    }

    #[test]
    fn usage_supported_honors_meta_true_for_claude_code() {
        let mut init = empty_init_response();
        init.agent_capabilities =
            agent_caps_with_meta("usage_updates", serde_json::Value::Bool(true));
        let new = empty_new_session_response();

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::ClaudeCodeAcp);

        assert!(caps.usage_supported());
    }

    #[test]
    fn usage_supported_honors_meta_false_for_codex() {
        let mut init = empty_init_response();
        init.agent_capabilities =
            agent_caps_with_meta("usage_updates", serde_json::Value::Bool(false));
        let new = empty_new_session_response();

        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert!(!caps.usage_supported());
    }

    #[test]
    fn usage_supported_ignores_non_bool_meta_and_falls_back_to_quirks() {
        let mut init = empty_init_response();
        init.agent_capabilities = agent_caps_with_meta(
            "usage_updates",
            serde_json::Value::String("false".to_string()),
        );
        let new = empty_new_session_response();

        let claude = SpurAgentCaps::new(&init, &new, AgentKind::ClaudeCodeAcp);
        let codex = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert!(!claude.usage_supported());
        assert!(codex.usage_supported());
    }

    #[test]
    fn empty_responses_yield_all_false() {
        let init = empty_init_response();
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(!caps.supports_set_mode(), "empty modes => no set_mode");
        assert!(
            !caps.supports_set_model(),
            "empty config options => no set_model"
        );
        assert!(
            !caps.supports_set_config_option(),
            "empty config_options => no set_config_option"
        );
        assert!(
            !caps.supports_load_session(),
            "default agent_capabilities => no load_session"
        );
        assert!(
            !caps.supports_resume_session(),
            "default session_capabilities => no session/resume"
        );
        assert!(
            !caps.supports_delete_session(),
            "default session_capabilities => no session/delete"
        );
        assert!(
            !caps.supports_list_sessions(),
            "default session_capabilities => no session/list"
        );
        assert!(
            !caps.supports_close_session(),
            "default session_capabilities => no session/close"
        );
        assert!(
            !caps.meta_capability("terminal_output"),
            "absent meta => terminal_output false"
        );
    }

    #[test]
    fn codex_fixture_yields_all_set_caps_true() {
        let json = include_str!("../tests/data/codex_acp_0_12_new_session_response.json");
        let new: NewSessionResponse =
            serde_json::from_str(json).expect("codex fixture must deserialize");

        // Pair with a default InitializeResponse — set_* gating derives from
        // new_session state, not from AgentCapabilities flags.
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

        assert!(
            caps.supports_set_mode(),
            "codex fixture has 3 modes => set_mode"
        );
        assert!(
            caps.supports_set_model(),
            "codex fixture has model config option => set_model"
        );
        assert!(
            caps.supports_set_config_option(),
            "codex fixture has 3 config_options => set_config_option"
        );
        assert_eq!(caps.config_options.len(), 3);
    }

    #[test]
    fn model_config_option_sets_model_support() {
        let mut new = NewSessionResponse::new(SessionId::new("test-model-config"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gemini-1.5-pro",
            vec![SessionConfigSelectOption::new(
                "gemini-1.5-pro",
                "Gemini 1.5 Pro",
            )],
        )]);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_set_model(), "model config option has choices");
        assert!(
            caps.supports_set_config_option(),
            "model config option is advertised"
        );
        assert!(!caps.supports_set_mode(), "model config has no modes");
    }

    #[test]
    fn model_config_present_but_empty_yields_false() {
        let mut new = NewSessionResponse::new(SessionId::new("test-empty-model-option"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "only-current",
            Vec::<SessionConfigSelectOption>::new(),
        )]);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(
            !caps.supports_set_model(),
            "model config option with empty choices => not usable"
        );
    }

    #[test]
    fn model_config_with_available_yields_true() {
        let mut new = NewSessionResponse::new(SessionId::new("test-models"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "default",
            vec![SessionConfigSelectOption::new("default", "Default")],
        )]);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_set_model());
    }

    #[test]
    fn modes_present_but_empty_yields_false() {
        let modes = SessionModeState::new(SessionModeId::new("only-id"), vec![]);
        let new = NewSessionResponse::new(SessionId::new("test-empty-modes")).modes(modes);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(
            !caps.supports_set_mode(),
            "Some(modes) with empty available_modes => not usable"
        );
    }

    #[test]
    fn modes_with_available_yield_true() {
        let modes = SessionModeState::new(
            SessionModeId::new("default"),
            vec![SessionMode::new(SessionModeId::new("default"), "Default")],
        );
        let new = NewSessionResponse::new(SessionId::new("test-modes")).modes(modes);
        let init = empty_init_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_set_mode());
    }

    #[test]
    fn load_session_capability_propagates_from_agent_capabilities() {
        let mut init = empty_init_response();
        init.agent_capabilities = AgentCapabilities::new().load_session(true);
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_load_session());
    }

    #[test]
    fn session_lifecycle_capabilities_propagate_from_agent_capabilities() {
        let mut init = empty_init_response();
        init.agent_capabilities = AgentCapabilities::new().session_capabilities(
            SessionCapabilities::new()
                .resume(SessionResumeCapabilities::new())
                .delete(SessionDeleteCapabilities::new())
                .list(SessionListCapabilities::new())
                .close(SessionCloseCapabilities::new()),
        );
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.supports_resume_session());
        assert!(caps.supports_delete_session());
        assert!(caps.supports_list_sessions());
        assert!(caps.supports_close_session());
    }

    #[test]
    fn meta_capability_reads_terminal_output_true() {
        let mut init = empty_init_response();
        init.agent_capabilities =
            agent_caps_with_meta("terminal_output", serde_json::Value::Bool(true));
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(caps.meta_capability("terminal_output"));
        assert!(
            !caps.meta_capability("missing_key"),
            "missing keys read as false"
        );
    }

    #[test]
    fn meta_capability_non_bool_value_is_false() {
        let mut init = empty_init_response();
        init.agent_capabilities = agent_caps_with_meta(
            "terminal_output",
            serde_json::Value::String("true".to_string()),
        );
        let new = empty_new_session_response();
        let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);

        assert!(
            !caps.meta_capability("terminal_output"),
            "non-bool meta value is treated as false"
        );
    }
}

#[cfg(test)]
mod p2_model_effort_id_tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        SessionConfigId, SessionConfigOption, SessionConfigOptionCategory,
        SessionConfigSelectOption, SessionConfigValueId,
    };

    fn select_opt(
        id: &str,
        current: &str,
        category: Option<SessionConfigOptionCategory>,
    ) -> SessionConfigOption {
        let mut opt = SessionConfigOption::select(
            SessionConfigId::new(id.to_string()),
            "label".to_string(),
            current.to_string(),
            vec![SessionConfigSelectOption::new(
                SessionConfigValueId::new(current.to_string()),
                "Name".to_string(),
            )],
        );
        if let Some(cat) = category {
            opt = opt.category(cat);
        }
        opt
    }

    #[test]
    fn model_id_from_config_options_reads_select_current() {
        let opts = vec![select_opt(
            "model",
            "gpt-5.5",
            Some(SessionConfigOptionCategory::Model),
        )];
        assert_eq!(
            model_id_from_config_options(&opts).as_deref(),
            Some("gpt-5.5")
        );
    }

    #[test]
    fn effort_id_from_reasoning_effort_option() {
        let opts = vec![select_opt("reasoning_effort", "high", None)];
        assert_eq!(
            effort_id_from_config_options(&opts).as_deref(),
            Some("high")
        );
    }
}
