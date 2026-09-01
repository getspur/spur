//! Provider-neutral capability evidence and deterministic route reduction.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Semantic capability family independent of any ACP provider.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityKind {
    Model,
    Effort,
    Mode,
    Command,
    Custom(String),
}

/// Stable identity of one semantic capability surface.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityKey {
    pub kind: CapabilityKind,
    pub upstream_id: String,
}

/// One choice learned from upstream evidence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

/// Non-secret executable identity to which native evidence is bound.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CliIdentity {
    pub resolved_executable: PathBuf,
    pub upstream_version: Option<String>,
    pub argv_fingerprint: String,
    pub environment_fingerprint: String,
}

/// Scope in which an observation was made.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceSessionScope {
    Global,
    Session(String),
    IsolatedProbe,
}

/// Milliseconds since the Unix epoch at which evidence was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObservationTime(pub u64);

/// Stable digest of the raw payload retained by the artifact policy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawEvidenceDigest(pub String);

/// Origin of an evidence claim. Probe recipes are deliberately not provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceProvenance {
    StandardAdvertisement,
    VendorAdvertisement,
    AcceptedActiveProbe,
    RejectedActiveProbe,
    ObservedNotification,
    PromptFallback,
    InconclusiveFailure,
    NativeDispatch,
}

/// What one observation says about a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceClaim {
    CandidateObserved,
    NativeVerified,
    Rejected,
    Inconclusive,
    Unknown,
    NativeFailed,
}

/// Append-only evidence retained inside one immutable epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRecord {
    pub key: CapabilityKey,
    pub claim: EvidenceClaim,
    pub provenance: EvidenceProvenance,
    pub identity: CliIdentity,
    pub observed_at: ObservationTime,
    pub raw_digest: RawEvidenceDigest,
    pub session_scope: EvidenceSessionScope,
    pub choices: Vec<CapabilityChoice>,
}

/// Monotonic identity of one immutable evidence snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvidenceEpochId(pub u64);

/// Failure to bind every record in an epoch to the same CLI identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceEpochError {
    pub record_index: usize,
}

/// Immutable records for one CLI identity and evidence epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceEpoch {
    id: EvidenceEpochId,
    identity: CliIdentity,
    records: Box<[EvidenceRecord]>,
}

impl EvidenceEpoch {
    pub fn new(
        id: EvidenceEpochId,
        identity: CliIdentity,
        records: Vec<EvidenceRecord>,
    ) -> Result<Self, EvidenceEpochError> {
        if let Some(record_index) = records
            .iter()
            .position(|record| record.identity != identity)
        {
            return Err(EvidenceEpochError { record_index });
        }
        Ok(Self {
            id,
            identity,
            records: records.into_boxed_slice(),
        })
    }

    #[must_use]
    pub fn id(&self) -> EvidenceEpochId {
        self.id
    }

    #[must_use]
    pub fn identity(&self) -> &CliIdentity {
        &self.identity
    }

    #[must_use]
    pub fn records(&self) -> &[EvidenceRecord] {
        &self.records
    }
}

/// Confidence lifecycle derived for one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityConfidence {
    Hidden,
    PromptOnly,
    NativePreferred,
}

impl CapabilityConfidence {
    #[must_use]
    pub fn native_allowed(self) -> bool {
        self == Self::NativePreferred
    }
}

/// The single dispatch route selected for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DispatchRoute {
    Hidden,
    PromptOnly,
    NativePreferred,
}

/// Bounded summary of evidence sources considered by the reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSourceSummary {
    pub record_count: usize,
    pub provenances: Vec<EvidenceProvenance>,
}

/// Pure reduced snapshot for one capability and epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReducedCapability {
    pub key: CapabilityKey,
    pub choices: Vec<CapabilityChoice>,
    pub confidence: CapabilityConfidence,
    pub route: DispatchRoute,
    pub sources: EvidenceSourceSummary,
    pub evidence_epoch: EvidenceEpochId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RouterInputs {
    prompt_advertised: bool,
    native_verified: bool,
    identity_matches: bool,
    native_failed: bool,
}

fn select_route(inputs: RouterInputs) -> DispatchRoute {
    if inputs.native_verified && inputs.identity_matches && !inputs.native_failed {
        DispatchRoute::NativePreferred
    } else if inputs.prompt_advertised {
        DispatchRoute::PromptOnly
    } else {
        DispatchRoute::Hidden
    }
}

/// Reduce one capability without mutating its evidence epoch.
#[must_use]
pub fn reduce_capability(
    epoch: &EvidenceEpoch,
    current_identity: &CliIdentity,
    key: &CapabilityKey,
) -> ReducedCapability {
    let mut choices = BTreeMap::<String, CapabilityChoice>::new();
    let mut provenances = BTreeSet::new();
    let mut record_count = 0;
    let mut prompt_advertised = false;
    let mut native_verified = false;
    let mut native_failed = false;

    for record in epoch.records().iter().filter(|record| record.key == *key) {
        record_count += 1;
        provenances.insert(record.provenance);

        let passive_advertisement = record.claim == EvidenceClaim::CandidateObserved
            && matches!(
                record.provenance,
                EvidenceProvenance::StandardAdvertisement
                    | EvidenceProvenance::VendorAdvertisement
                    | EvidenceProvenance::ObservedNotification
            );
        let observed_prompt_fallback = record.claim == EvidenceClaim::CandidateObserved
            && record.provenance == EvidenceProvenance::PromptFallback;
        let verified_native = record.claim == EvidenceClaim::NativeVerified
            && matches!(
                record.provenance,
                EvidenceProvenance::StandardAdvertisement | EvidenceProvenance::AcceptedActiveProbe
            );

        prompt_advertised |= passive_advertisement || observed_prompt_fallback;
        native_verified |= verified_native;
        native_failed |= record.claim == EvidenceClaim::NativeFailed
            || (record.claim == EvidenceClaim::Rejected
                && record.provenance == EvidenceProvenance::RejectedActiveProbe);

        if passive_advertisement || observed_prompt_fallback || verified_native {
            for choice in &record.choices {
                choices
                    .entry(choice.id.clone())
                    .and_modify(|current| {
                        if choice < current {
                            current.clone_from(choice);
                        }
                    })
                    .or_insert_with(|| choice.clone());
            }
        }
    }

    let route = select_route(RouterInputs {
        prompt_advertised,
        native_verified,
        identity_matches: epoch.identity() == current_identity,
        native_failed,
    });
    let confidence = match route {
        DispatchRoute::Hidden => CapabilityConfidence::Hidden,
        DispatchRoute::PromptOnly => CapabilityConfidence::PromptOnly,
        DispatchRoute::NativePreferred => CapabilityConfidence::NativePreferred,
    };

    ReducedCapability {
        key: key.clone(),
        choices: if route == DispatchRoute::Hidden {
            Vec::new()
        } else {
            choices.into_values().collect()
        },
        confidence,
        route,
        sources: EvidenceSourceSummary {
            record_count,
            provenances: provenances.into_iter().collect(),
        },
        evidence_epoch: epoch.id(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(version: &str) -> CliIdentity {
        CliIdentity {
            resolved_executable: PathBuf::from("/usr/bin/example-acp"),
            upstream_version: Some(version.to_owned()),
            argv_fingerprint: "argv-sha256".to_owned(),
            environment_fingerprint: "env-sha256".to_owned(),
        }
    }

    fn key() -> CapabilityKey {
        CapabilityKey {
            kind: CapabilityKind::Effort,
            upstream_id: "reasoning_effort".to_owned(),
        }
    }

    fn choice(id: &str) -> CapabilityChoice {
        CapabilityChoice {
            id: id.to_owned(),
            label: id.to_uppercase(),
            description: None,
        }
    }

    fn record(
        identity: &CliIdentity,
        claim: EvidenceClaim,
        provenance: EvidenceProvenance,
        observed_at: u64,
        choices: Vec<CapabilityChoice>,
    ) -> EvidenceRecord {
        EvidenceRecord {
            key: key(),
            claim,
            provenance,
            identity: identity.clone(),
            observed_at: ObservationTime(observed_at),
            raw_digest: RawEvidenceDigest(format!("digest-{observed_at}")),
            session_scope: EvidenceSessionScope::Session("session-1".to_owned()),
            choices,
        }
    }

    fn epoch(id: u64, identity: &CliIdentity, records: Vec<EvidenceRecord>) -> EvidenceEpoch {
        EvidenceEpoch::new(EvidenceEpochId(id), identity.clone(), records).unwrap()
    }

    #[test]
    fn router_matches_every_input_row_exclusively_and_deterministically() {
        for prompt_advertised in [false, true] {
            for native_verified in [false, true] {
                for identity_matches in [false, true] {
                    for native_failed in [false, true] {
                        let native = native_verified && identity_matches && !native_failed;
                        let expected = if native {
                            DispatchRoute::NativePreferred
                        } else if prompt_advertised {
                            DispatchRoute::PromptOnly
                        } else {
                            DispatchRoute::Hidden
                        };
                        let inputs = RouterInputs {
                            prompt_advertised,
                            native_verified,
                            identity_matches,
                            native_failed,
                        };
                        let actual = select_route(inputs);

                        assert_eq!(actual, expected);
                        assert_eq!(actual, select_route(inputs));
                        assert_eq!(
                            [
                                actual == DispatchRoute::Hidden,
                                actual == DispatchRoute::PromptOnly,
                                actual == DispatchRoute::NativePreferred,
                            ]
                            .into_iter()
                            .filter(|selected| *selected)
                            .count(),
                            1
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lifecycle_follows_hidden_prompt_native_prompt_and_preserves_safety() {
        let current = identity("1.0.0");
        let candidate = record(
            &current,
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::VendorAdvertisement,
            1,
            vec![choice("xhigh")],
        );
        let verified = record(
            &current,
            EvidenceClaim::NativeVerified,
            EvidenceProvenance::AcceptedActiveProbe,
            2,
            vec![choice("xhigh")],
        );
        let failed = record(
            &current,
            EvidenceClaim::NativeFailed,
            EvidenceProvenance::NativeDispatch,
            3,
            Vec::new(),
        );
        let snapshots = [
            reduce_capability(&epoch(0, &current, vec![]), &current, &key()),
            reduce_capability(
                &epoch(1, &current, vec![candidate.clone()]),
                &current,
                &key(),
            ),
            reduce_capability(
                &epoch(2, &current, vec![candidate.clone(), verified.clone()]),
                &current,
                &key(),
            ),
            reduce_capability(
                &epoch(3, &current, vec![candidate, verified, failed]),
                &current,
                &key(),
            ),
        ];

        assert_eq!(
            snapshots
                .iter()
                .map(|snapshot| snapshot.confidence)
                .collect::<Vec<_>>(),
            vec![
                CapabilityConfidence::Hidden,
                CapabilityConfidence::PromptOnly,
                CapabilityConfidence::NativePreferred,
                CapabilityConfidence::PromptOnly,
            ]
        );
        for snapshot in &snapshots {
            assert_eq!(
                snapshot.confidence.native_allowed(),
                snapshot.confidence == CapabilityConfidence::NativePreferred
            );
            assert_eq!(
                snapshot.route == DispatchRoute::NativePreferred,
                snapshot.confidence.native_allowed()
            );
        }
    }

    #[test]
    fn identity_mismatch_and_native_failure_demote_verified_evidence() {
        let original = identity("1.0.0");
        let changed = identity("2.0.0");
        let candidate = record(
            &original,
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::VendorAdvertisement,
            1,
            vec![choice("xhigh")],
        );
        let verified = record(
            &original,
            EvidenceClaim::NativeVerified,
            EvidenceProvenance::AcceptedActiveProbe,
            2,
            vec![choice("xhigh")],
        );
        let failed = record(
            &original,
            EvidenceClaim::NativeFailed,
            EvidenceProvenance::NativeDispatch,
            3,
            Vec::new(),
        );

        assert_eq!(
            reduce_capability(
                &epoch(1, &original, vec![candidate.clone(), verified.clone()]),
                &changed,
                &key(),
            )
            .route,
            DispatchRoute::PromptOnly
        );
        assert_eq!(
            reduce_capability(
                &epoch(2, &original, vec![candidate, verified, failed]),
                &original,
                &key(),
            )
            .route,
            DispatchRoute::PromptOnly
        );
    }

    #[test]
    fn unknown_rejected_inconclusive_and_recipe_only_never_enable_native() {
        let current = identity("1.0.0");
        for (claim, provenance) in [
            (
                EvidenceClaim::Unknown,
                EvidenceProvenance::AcceptedActiveProbe,
            ),
            (
                EvidenceClaim::Rejected,
                EvidenceProvenance::RejectedActiveProbe,
            ),
            (
                EvidenceClaim::Inconclusive,
                EvidenceProvenance::AcceptedActiveProbe,
            ),
        ] {
            let result = reduce_capability(
                &epoch(
                    1,
                    &current,
                    vec![record(
                        &current,
                        claim,
                        provenance,
                        1,
                        vec![choice("xhigh")],
                    )],
                ),
                &current,
                &key(),
            );
            assert_eq!(result.route, DispatchRoute::Hidden);
            assert!(result.choices.is_empty());
        }

        let recipe_exists = true;
        let result = reduce_capability(&epoch(2, &current, vec![]), &current, &key());
        assert!(recipe_exists);
        assert_eq!(result.route, DispatchRoute::Hidden);
    }

    #[test]
    fn inconclusive_failure_provenance_is_retained_and_never_native_enables() {
        let current = identity("1.0.0");
        let inconclusive_failure = record(
            &current,
            EvidenceClaim::Inconclusive,
            EvidenceProvenance::InconclusiveFailure,
            1,
            vec![choice("xhigh")],
        );
        let snapshot = epoch(1, &current, vec![inconclusive_failure]);

        assert_eq!(
            snapshot.records()[0].provenance,
            EvidenceProvenance::InconclusiveFailure
        );

        let result = reduce_capability(&snapshot, &current, &key());
        assert_eq!(result.route, DispatchRoute::Hidden);
        assert!(result.choices.is_empty());
        assert_eq!(
            result.sources.provenances,
            vec![EvidenceProvenance::InconclusiveFailure]
        );
    }

    #[test]
    fn rejected_evidence_demotes_while_inconclusive_evidence_preserves_prior_routes() {
        let current = identity("1.0.0");
        let candidate = record(
            &current,
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::VendorAdvertisement,
            1,
            vec![choice("xhigh")],
        );
        let verified = record(
            &current,
            EvidenceClaim::NativeVerified,
            EvidenceProvenance::AcceptedActiveProbe,
            2,
            vec![choice("xhigh")],
        );

        for claim in [EvidenceClaim::Unknown, EvidenceClaim::Inconclusive] {
            let result = reduce_capability(
                &epoch(
                    3,
                    &current,
                    vec![
                        candidate.clone(),
                        verified.clone(),
                        record(
                            &current,
                            claim,
                            EvidenceProvenance::AcceptedActiveProbe,
                            3,
                            Vec::new(),
                        ),
                    ],
                ),
                &current,
                &key(),
            );
            assert_eq!(result.route, DispatchRoute::NativePreferred);
        }

        let rejected = reduce_capability(
            &epoch(
                4,
                &current,
                vec![
                    candidate,
                    verified,
                    record(
                        &current,
                        EvidenceClaim::Rejected,
                        EvidenceProvenance::RejectedActiveProbe,
                        4,
                        Vec::new(),
                    ),
                ],
            ),
            &current,
            &key(),
        );
        assert_eq!(rejected.route, DispatchRoute::PromptOnly);
    }

    #[test]
    fn rejected_prompt_fallback_does_not_demote_verified_native_evidence() {
        let current = identity("1.0.0");
        let verified = record(
            &current,
            EvidenceClaim::NativeVerified,
            EvidenceProvenance::AcceptedActiveProbe,
            1,
            vec![choice("xhigh")],
        );
        let rejected_prompt_fallback = record(
            &current,
            EvidenceClaim::Rejected,
            EvidenceProvenance::PromptFallback,
            2,
            Vec::new(),
        );

        let result = reduce_capability(
            &epoch(1, &current, vec![verified, rejected_prompt_fallback]),
            &current,
            &key(),
        );

        assert_eq!(result.route, DispatchRoute::NativePreferred);
    }

    #[test]
    fn observed_prompt_fallback_enables_prompt_only_route() {
        let current = identity("1.0.0");
        let prompt_fallback = record(
            &current,
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::PromptFallback,
            1,
            Vec::new(),
        );

        let result =
            reduce_capability(&epoch(1, &current, vec![prompt_fallback]), &current, &key());

        assert_eq!(result.route, DispatchRoute::PromptOnly);
    }

    #[test]
    fn standard_advertisement_and_accepted_probe_are_native_evidence() {
        let current = identity("1.0.0");
        for provenance in [
            EvidenceProvenance::StandardAdvertisement,
            EvidenceProvenance::AcceptedActiveProbe,
        ] {
            let result = reduce_capability(
                &epoch(
                    1,
                    &current,
                    vec![record(
                        &current,
                        EvidenceClaim::NativeVerified,
                        provenance,
                        1,
                        vec![choice("xhigh")],
                    )],
                ),
                &current,
                &key(),
            );
            assert_eq!(result.route, DispatchRoute::NativePreferred);
        }
    }

    #[test]
    fn records_retain_provider_neutral_provenance_identity_digest_and_scope() {
        let current = identity("1.0.0");
        let evidence = record(
            &current,
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::ObservedNotification,
            42,
            vec![choice("new-upstream-value")],
        );
        let snapshot = epoch(7, &current, vec![evidence.clone()]);

        assert_eq!(snapshot.id(), EvidenceEpochId(7));
        assert_eq!(snapshot.identity(), &current);
        assert_eq!(snapshot.records(), &[evidence.clone()]);
        assert_eq!(evidence.observed_at, ObservationTime(42));
        assert_eq!(
            evidence.raw_digest,
            RawEvidenceDigest("digest-42".to_owned())
        );
        assert_eq!(
            evidence.session_scope,
            EvidenceSessionScope::Session("session-1".to_owned())
        );
    }

    #[test]
    fn choices_are_learned_without_an_allowlist_and_reduced_deterministically() {
        let current = identity("1.0.0");
        let records = vec![
            record(
                &current,
                EvidenceClaim::CandidateObserved,
                EvidenceProvenance::VendorAdvertisement,
                1,
                vec![choice("xhigh"), choice("future-effort")],
            ),
            record(
                &current,
                EvidenceClaim::CandidateObserved,
                EvidenceProvenance::ObservedNotification,
                2,
                vec![choice("future-effort"), choice("low")],
            ),
        ];
        let snapshot = epoch(1, &current, records);

        let first = reduce_capability(&snapshot, &current, &key());
        let second = reduce_capability(&snapshot, &current, &key());
        assert_eq!(first, second);
        assert_eq!(
            first
                .choices
                .into_iter()
                .map(|choice| choice.id)
                .collect::<Vec<_>>(),
            vec!["future-effort", "low", "xhigh"]
        );
    }

    #[test]
    fn evidence_epochs_are_identity_bound_and_immutable_during_reduction() {
        let current = identity("1.0.0");
        let changed = identity("2.0.0");
        let mixed = EvidenceEpoch::new(
            EvidenceEpochId(1),
            current.clone(),
            vec![record(
                &changed,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
                1,
                Vec::new(),
            )],
        );
        assert_eq!(mixed.unwrap_err(), EvidenceEpochError { record_index: 0 });

        let snapshot = epoch(
            2,
            &current,
            vec![record(
                &current,
                EvidenceClaim::CandidateObserved,
                EvidenceProvenance::VendorAdvertisement,
                1,
                vec![choice("xhigh")],
            )],
        );
        let before = snapshot.clone();
        let _ = reduce_capability(&snapshot, &current, &key());
        assert_eq!(snapshot, before);
    }
}
