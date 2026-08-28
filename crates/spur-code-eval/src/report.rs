//! Checksum-verified, deterministic benchmark report rendering.
//!
//! The report keeps deterministic suite-native metrics distinct and treats the
//! model lane as advisory data. Rendering accepts only in-memory artifact bytes
//! and validates them against the existing content-addressed records before any
//! JSON bytes are produced.
//!
//! ```mermaid
//! flowchart LR
//!     P["Pinned sources"] --> V["Validation + isolated materialization"]
//!     V --> Q["Public SPUR query surface"] --> F["Frozen artifacts"]
//!     F --> RQ["RepoQA scorer"]
//!     F --> CC["CrossCodeEval scorer"]
//!     F --> JCG["JCG scorer"]
//!     RQ --> D["Separate deterministic metrics + report"]
//!     CC --> D
//!     JCG --> D
//!     F --> FC["Frozen context"] --> Z["Zero-Mem separated knowledge pack"]
//!     Z --> A["Advisory final answers + model records"]
//!     Z -. "memory ops: 0 LLM calls / input / output tokens" .-> EI["Encoder + index accounting"]
//!     D --> G{"Release projection"}
//!     A --> G
//!     G --> Reject["Reject"]
//!     G --> PD["PublishDeterministic"]
//!     G --> PF["PublishFull"]
//! ```

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use crate::{
    content_sha256,
    metrics::{
        CrossCodeEvalMetrics, Denominators, JcgMetrics, MetricSuite, PublishedMetrics,
        RetrievalMetrics,
    },
    model::{
        ModelCaseStatus, ModelRecord, ModelRecordValidationError, ModelUsage, ZeroMemAccounting,
    },
    ArtifactKind, ArtifactRecord, ContentPin, RepositoryPin,
};

const REPORT_SCHEMA_VERSION: u32 = 1;

/// The three release projections defined by `CODE-EVAL-RELEASE-POLICY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseStatus {
    /// Source or deterministic gates did not all pass.
    Reject,
    /// Deterministic gates passed without a complete passing model lane.
    PublishDeterministic,
    /// Source, deterministic, and model gates all passed.
    PublishFull,
}

/// Complete Boolean inputs to the deterministic-first release policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "CODE-EVAL-RELEASE-POLICY formally defines these five Boolean inputs"
)]
pub struct ReleaseInputs {
    sources_pinned: bool,
    deterministic_complete: bool,
    deterministic_pass: bool,
    model_complete: bool,
    model_pass: bool,
}

impl ReleaseInputs {
    /// Creates one complete release-policy input tuple.
    #[must_use]
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "constructor preserves the formal policy's five named Boolean inputs"
    )]
    pub const fn new(
        sources_pinned: bool,
        deterministic_complete: bool,
        deterministic_pass: bool,
        model_complete: bool,
        model_pass: bool,
    ) -> Self {
        Self {
            sources_pinned,
            deterministic_complete,
            deterministic_pass,
            model_complete,
            model_pass,
        }
    }

    /// Projects the exact deterministic-first release status.
    ///
    /// A model absence, failure, or incomplete result never downgrades a
    /// passing deterministic gate.
    #[must_use]
    pub const fn status(self) -> ReleaseStatus {
        if !(self.sources_pinned && self.deterministic_complete && self.deterministic_pass) {
            ReleaseStatus::Reject
        } else if self.model_complete && self.model_pass {
            ReleaseStatus::PublishFull
        } else {
            ReleaseStatus::PublishDeterministic
        }
    }
}

/// One suite's exact denominators and native metric type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuiteReport<T> {
    denominators: Denominators,
    metrics: Option<T>,
}

impl<T> SuiteReport<T> {
    /// Creates a denominator-visible suite report.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the denominator partition is inconsistent or
    /// metric presence disagrees with the eligible denominator.
    pub fn new(denominators: Denominators, metrics: Option<T>) -> Result<Self, ReportError> {
        validate_denominators(denominators)?;
        if (denominators.eligible == 0) == metrics.is_some() {
            return Err(ReportError::SuiteMetricsPresence {
                eligible: denominators.eligible,
                metrics_present: metrics.is_some(),
            });
        }
        Ok(Self {
            denominators,
            metrics,
        })
    }

    /// Returns the suite's exact visible denominators.
    #[must_use]
    pub const fn denominators(&self) -> Denominators {
        self.denominators
    }

    /// Returns native metrics, absent only when no case is eligible.
    #[must_use]
    pub const fn metrics(&self) -> Option<&T> {
        self.metrics.as_ref()
    }
}

/// Three separately typed deterministic suite sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeterministicReport {
    repoqa: SuiteReport<RetrievalMetrics>,
    crosscodeeval: SuiteReport<CrossCodeEvalMetrics>,
    jcg: SuiteReport<JcgMetrics>,
    published_metrics: PublishedMetrics,
}

impl DeterministicReport {
    /// Creates an explicitly non-blended deterministic report.
    #[must_use]
    pub const fn new(
        repoqa: SuiteReport<RetrievalMetrics>,
        crosscodeeval: SuiteReport<CrossCodeEvalMetrics>,
        jcg: SuiteReport<JcgMetrics>,
        published_metrics: PublishedMetrics,
    ) -> Self {
        Self {
            repoqa,
            crosscodeeval,
            jcg,
            published_metrics,
        }
    }

    /// Returns the `RepoQA`-native section.
    #[must_use]
    pub const fn repoqa(&self) -> &SuiteReport<RetrievalMetrics> {
        &self.repoqa
    }

    /// Returns the `CrossCodeEval`-native section.
    #[must_use]
    pub const fn crosscodeeval(&self) -> &SuiteReport<CrossCodeEvalMetrics> {
        &self.crosscodeeval
    }

    /// Returns the JCG-native section.
    #[must_use]
    pub const fn jcg(&self) -> &SuiteReport<JcgMetrics> {
        &self.jcg
    }

    /// Returns the typed per-case and operational publication payload.
    #[must_use]
    pub const fn published_metrics(&self) -> &PublishedMetrics {
        &self.published_metrics
    }

    const fn denominators(&self, suite: MetricSuite) -> Denominators {
        match suite {
            MetricSuite::RepoQa => self.repoqa.denominators,
            MetricSuite::CrossCodeEval => self.crosscodeeval.denominators,
            MetricSuite::Jcg => self.jcg.denominators,
        }
    }
}

/// Derived advisory model-lane counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AdvisoryModelSummary {
    total_records: u64,
    completed: u64,
    pending: u64,
    failed: u64,
    model_usage: ModelUsage,
}

impl AdvisoryModelSummary {
    /// Returns the total model-record denominator.
    #[must_use]
    pub const fn total_records(self) -> u64 {
        self.total_records
    }

    /// Returns the number of complete final-answer records.
    #[must_use]
    pub const fn completed(self) -> u64 {
        self.completed
    }

    /// Returns the number of operationally pending records.
    #[must_use]
    pub const fn pending(self) -> u64 {
        self.pending
    }

    /// Returns the number of failed or incomplete records.
    #[must_use]
    pub const fn failed(self) -> u64 {
        self.failed
    }

    /// Returns final-answer model usage, separate from Zero-Mem accounting.
    #[must_use]
    pub const fn model_usage(self) -> ModelUsage {
        self.model_usage
    }
}

/// Optional advisory model records and native Zero-Mem accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvisoryModelReport {
    summary: AdvisoryModelSummary,
    records: Vec<ModelRecord>,
    zero_mem_accounting: ZeroMemAccounting,
}

impl AdvisoryModelReport {
    /// Creates a canonically ordered advisory section and derives its summary.
    ///
    /// # Errors
    ///
    /// Returns a typed error for duplicate case/variant identities or counter
    /// overflow.
    pub fn new(
        mut records: Vec<ModelRecord>,
        zero_mem_accounting: ZeroMemAccounting,
    ) -> Result<Self, ReportError> {
        for record in &records {
            record
                .validate()
                .map_err(|source| ReportError::InvalidAdvisoryModelRecord {
                    case_id: record.identity().context().case_id().to_owned(),
                    source,
                })?;
        }
        records.sort_by(|left, right| {
            left.identity()
                .context()
                .case_id()
                .cmp(right.identity().context().case_id())
                .then_with(|| {
                    left.identity()
                        .context()
                        .variant()
                        .cmp(&right.identity().context().variant())
                })
        });
        for pair in records.windows(2) {
            let left = pair[0].identity().context();
            let right = pair[1].identity().context();
            if left.case_id() == right.case_id() && left.variant() == right.variant() {
                return Err(ReportError::DuplicateModelRecord {
                    case_id: left.case_id().to_owned(),
                });
            }
        }

        let mut completed = 0_u64;
        let mut pending = 0_u64;
        let mut failed = 0_u64;
        let mut llm_calls = 0_u64;
        let mut input_tokens = 0_u64;
        let mut output_tokens = 0_u64;
        for record in &records {
            match record.status() {
                ModelCaseStatus::Completed => completed = checked_increment(completed)?,
                ModelCaseStatus::ModelPending(_) => pending = checked_increment(pending)?,
                ModelCaseStatus::ModelFailed(_) => failed = checked_increment(failed)?,
            }
            let usage = record.usage();
            llm_calls = checked_add(llm_calls, usage.llm_calls())?;
            input_tokens = checked_add(input_tokens, usage.input_tokens())?;
            output_tokens = checked_add(output_tokens, usage.output_tokens())?;
        }
        let total_records = u64::try_from(records.len())
            .map_err(|_error| ReportError::NumericOverflow { field: "records" })?;
        let summary = AdvisoryModelSummary {
            total_records,
            completed,
            pending,
            failed,
            model_usage: ModelUsage::new(llm_calls, input_tokens, output_tokens),
        };
        Ok(Self {
            summary,
            records,
            zero_mem_accounting,
        })
    }

    /// Returns counters derived from the canonical records.
    #[must_use]
    pub const fn summary(&self) -> AdvisoryModelSummary {
        self.summary
    }

    /// Returns records in canonical case/variant order.
    #[must_use]
    pub fn records(&self) -> &[ModelRecord] {
        &self.records
    }

    /// Returns native Zero-Mem memory-operation accounting.
    #[must_use]
    pub const fn zero_mem_accounting(&self) -> &ZeroMemAccounting {
        &self.zero_mem_accounting
    }
}

/// Complete metadata required to reproduce and audit a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReproducibilityMetadata {
    /// Exact SPUR Git object ID.
    pub spur_revision: String,
    /// Whether that SPUR worktree contained uncommitted changes.
    pub spur_dirty: bool,
    /// Stable platform or target-triple identity.
    pub platform: String,
    /// Exact argument vector, including the invoked program at index zero.
    pub command_argv: Vec<String>,
    /// Per-phase elapsed microseconds in canonical phase-name order.
    pub phase_timings_micros: BTreeMap<String, u64>,
    /// Peak resident memory in bytes.
    pub peak_rss_bytes: u64,
    /// Final index size in bytes.
    pub index_bytes: u64,
    /// Every immutable dataset/source pin, keyed by stable source identity.
    pub source_pins: BTreeMap<String, ContentPin>,
    /// Every immutable repository pin, keyed by stable repository identity.
    pub repository_pins: BTreeMap<String, RepositoryPin>,
    /// Content identity of the exact leakage-safe query policy.
    pub query_policy_hash: String,
    /// Exact scorer versions in stable scorer-name order.
    pub scorer_versions: BTreeMap<String, String>,
    /// Exact adapter versions in stable adapter-name order.
    pub adapter_versions: BTreeMap<String, String>,
    /// Exact eligible, unsupported, and invalid suite denominators.
    pub suite_denominators: BTreeMap<MetricSuite, Denominators>,
    /// Existing content-addressed identities for every referenced artifact.
    pub artifact_records: BTreeMap<ArtifactKind, ArtifactRecord>,
}

/// One immutable, non-blended benchmark report.
///
/// Report bytes can only be produced by [`BenchmarkReport::render_json`],
/// which requires the exact frozen artifact payloads. Direct serde
/// serialization is intentionally unavailable:
///
/// ```compile_fail
/// use spur_code_eval::report::BenchmarkReport;
///
/// fn direct_serde_serialization_cannot_bypass_artifact_payload_validation(
///     report: &BenchmarkReport,
/// ) {
///     let _bytes = serde_json::to_vec(report).unwrap();
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkReport {
    schema_version: u32,
    release_inputs: ReleaseInputs,
    release_status: ReleaseStatus,
    reproducibility: ReproducibilityMetadata,
    deterministic: DeterministicReport,
    advisory_model: Option<AdvisoryModelReport>,
}

#[derive(Serialize)]
struct BenchmarkReportWire<'a> {
    schema_version: u32,
    release_inputs: &'a ReleaseInputs,
    release_status: ReleaseStatus,
    reproducibility: &'a ReproducibilityMetadata,
    deterministic: &'a DeterministicReport,
    advisory_model: Option<&'a AdvisoryModelReport>,
}

impl BenchmarkReport {
    /// Validates and creates a benchmark report before rendering.
    ///
    /// # Errors
    ///
    /// Returns a typed metadata, denominator, artifact-record, or advisory-lane
    /// consistency error.
    pub fn new(
        release_inputs: ReleaseInputs,
        reproducibility: ReproducibilityMetadata,
        deterministic: DeterministicReport,
        advisory_model: Option<AdvisoryModelReport>,
    ) -> Result<Self, ReportError> {
        validate_reproducibility(&reproducibility, &deterministic)?;
        let release_status = release_inputs.status();
        if release_status == ReleaseStatus::PublishFull {
            let advisory = advisory_model
                .as_ref()
                .ok_or(ReportError::MissingCompleteAdvisoryModel)?;
            let summary = advisory.summary();
            if summary.total_records == 0 || summary.completed != summary.total_records {
                return Err(ReportError::MissingCompleteAdvisoryModel);
            }
        }
        Ok(Self {
            schema_version: REPORT_SCHEMA_VERSION,
            release_inputs,
            release_status,
            reproducibility,
            deterministic,
            advisory_model,
        })
    }

    /// Returns the release projection computed from the immutable inputs.
    #[must_use]
    pub const fn release_status(&self) -> ReleaseStatus {
        self.release_status
    }

    /// Verifies every referenced checksum and renders deterministic JSON.
    ///
    /// The artifact map must exactly cover the report's content-addressed
    /// records. No filesystem path is accepted or opened.
    ///
    /// # Errors
    ///
    /// Returns a typed error before serialization for missing, unexpected,
    /// non-frozen, malformed, wrong-length, or checksum-mismatched artifacts.
    /// JSON serialization errors are preserved as [`ReportError::Serialization`].
    pub fn render_json(
        &self,
        artifact_payloads: &BTreeMap<ArtifactKind, Vec<u8>>,
    ) -> Result<Vec<u8>, ReportError> {
        validate_artifacts(&self.reproducibility.artifact_records, artifact_payloads)?;
        let wire = BenchmarkReportWire {
            schema_version: self.schema_version,
            release_inputs: &self.release_inputs,
            release_status: self.release_status,
            reproducibility: &self.reproducibility,
            deterministic: &self.deterministic,
            advisory_model: self.advisory_model.as_ref(),
        };
        let mut bytes = serde_json::to_vec_pretty(&wire)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// Typed construction, integrity, and serialization failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReportError {
    /// A required reproducibility field was empty or malformed.
    #[error("invalid reproducibility field `{field}`")]
    InvalidMetadata {
        /// Stable metadata field name.
        field: &'static str,
    },
    /// Exact denominator counters did not form a valid partition.
    #[error("invalid exact denominator partition")]
    InvalidDenominators,
    /// Native metric presence did not agree with eligible cases.
    #[error("suite metrics presence disagrees with eligible denominator {eligible}")]
    SuiteMetricsPresence {
        /// Eligible case count.
        eligible: u64,
        /// Whether the caller supplied native metrics.
        metrics_present: bool,
    },
    /// Reproducibility denominators disagreed with a native suite section.
    #[error("reproducibility denominators disagree with {suite:?}")]
    DenominatorMismatch {
        /// Suite whose exact counts disagreed.
        suite: MetricSuite,
    },
    /// A full release did not include complete advisory model records.
    #[error("publish_full requires non-empty, complete advisory model records")]
    MissingCompleteAdvisoryModel,
    /// More than one advisory record used the same case/variant identity.
    #[error("duplicate advisory model record for case {case_id:?}")]
    DuplicateModelRecord {
        /// Duplicated case identity.
        case_id: String,
    },
    /// A deserialized advisory record violated model-layer invariants.
    #[error("invalid advisory model record for case {case_id:?}: {source}")]
    InvalidAdvisoryModelRecord {
        /// Invalid case identity, when one was available.
        case_id: String,
        /// Exact model-layer validation failure.
        #[source]
        source: ModelRecordValidationError,
    },
    /// A referenced checksum had no supplied in-memory payload.
    #[error("artifact {kind:?} has no supplied payload")]
    MissingArtifactPayload {
        /// Missing logical artifact.
        kind: ArtifactKind,
    },
    /// A supplied payload had no referenced checksum record.
    #[error("artifact {kind:?} has no referenced checksum")]
    MissingArtifactChecksum {
        /// Unreferenced logical artifact.
        kind: ArtifactKind,
    },
    /// A referenced artifact record did not use its canonical logical path.
    #[error("artifact {kind:?} has a noncanonical content record")]
    NonCanonicalArtifact {
        /// Malformed logical artifact.
        kind: ArtifactKind,
    },
    /// A referenced artifact record did not contain a lowercase SHA-256 digest.
    #[error("artifact {kind:?} has an invalid SHA-256 checksum")]
    InvalidArtifactChecksum {
        /// Malformed logical artifact.
        kind: ArtifactKind,
    },
    /// A referenced artifact was not lifecycle-frozen.
    #[error("artifact {kind:?} is not frozen")]
    ArtifactNotFrozen {
        /// Mutable logical artifact.
        kind: ArtifactKind,
    },
    /// Supplied payload length disagreed with its content record.
    #[error("artifact {kind:?} length mismatch: expected {expected}, got {actual}")]
    ArtifactLengthMismatch {
        /// Mismatched logical artifact.
        kind: ArtifactKind,
        /// Content-record byte length.
        expected: u64,
        /// Supplied payload byte length.
        actual: u64,
    },
    /// Supplied payload bytes differed from their content address.
    #[error("artifact {kind:?} checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Mismatched logical artifact.
        kind: ArtifactKind,
        /// Content-record digest.
        expected: String,
        /// Digest computed from the supplied bytes.
        actual: String,
    },
    /// An exact counter could not be represented.
    #[error("numeric field `{field}` overflowed")]
    NumericOverflow {
        /// Stable counter name.
        field: &'static str,
    },
    /// Deterministic JSON serialization failed.
    #[error("cannot serialize benchmark report: {0}")]
    Serialization(#[from] serde_json::Error),
}

fn validate_reproducibility(
    metadata: &ReproducibilityMetadata,
    deterministic: &DeterministicReport,
) -> Result<(), ReportError> {
    if !is_git_oid(&metadata.spur_revision) {
        return Err(ReportError::InvalidMetadata {
            field: "spur_revision",
        });
    }
    require_nonempty("platform", &metadata.platform)?;
    if metadata.command_argv.is_empty() || metadata.command_argv[0].trim().is_empty() {
        return Err(ReportError::InvalidMetadata {
            field: "command_argv",
        });
    }
    validate_nonempty_map(
        "phase_timings_micros",
        &metadata.phase_timings_micros,
        |_| true,
    )?;
    validate_nonempty_map("source_pins", &metadata.source_pins, |_| true)?;
    validate_nonempty_map("repository_pins", &metadata.repository_pins, |_| true)?;
    require_nonempty("query_policy_hash", &metadata.query_policy_hash)?;
    validate_nonempty_map("scorer_versions", &metadata.scorer_versions, |value| {
        !value.trim().is_empty()
    })?;
    validate_nonempty_map("adapter_versions", &metadata.adapter_versions, |value| {
        !value.trim().is_empty()
    })?;
    if metadata.artifact_records.is_empty() {
        return Err(ReportError::InvalidMetadata {
            field: "artifact_records",
        });
    }

    for suite in [
        MetricSuite::RepoQa,
        MetricSuite::CrossCodeEval,
        MetricSuite::Jcg,
    ] {
        let denominators =
            metadata
                .suite_denominators
                .get(&suite)
                .ok_or(ReportError::InvalidMetadata {
                    field: "suite_denominators",
                })?;
        validate_denominators(*denominators)?;
        if *denominators != deterministic.denominators(suite) {
            return Err(ReportError::DenominatorMismatch { suite });
        }
    }
    for (kind, record) in &metadata.artifact_records {
        validate_artifact_record(*kind, record)?;
    }
    Ok(())
}

fn validate_artifacts(
    records: &BTreeMap<ArtifactKind, ArtifactRecord>,
    payloads: &BTreeMap<ArtifactKind, Vec<u8>>,
) -> Result<(), ReportError> {
    for (kind, record) in records {
        validate_artifact_record(*kind, record)?;
        let payload = payloads
            .get(kind)
            .ok_or(ReportError::MissingArtifactPayload { kind: *kind })?;
        let actual_bytes =
            u64::try_from(payload.len()).map_err(|_error| ReportError::NumericOverflow {
                field: "artifact_payload_bytes",
            })?;
        let actual = content_sha256(payload);
        if record.sha256() != actual {
            return Err(ReportError::ChecksumMismatch {
                kind: *kind,
                expected: record.sha256().to_owned(),
                actual,
            });
        }
        if record.bytes() != actual_bytes {
            return Err(ReportError::ArtifactLengthMismatch {
                kind: *kind,
                expected: record.bytes(),
                actual: actual_bytes,
            });
        }
    }
    for kind in payloads.keys() {
        if !records.contains_key(kind) {
            return Err(ReportError::MissingArtifactChecksum { kind: *kind });
        }
    }
    Ok(())
}

fn validate_artifact_record(
    kind: ArtifactKind,
    record: &ArtifactRecord,
) -> Result<(), ReportError> {
    if record.relative_path() != kind.relative_path() {
        return Err(ReportError::NonCanonicalArtifact { kind });
    }
    if !is_sha256(record.sha256()) {
        return Err(ReportError::InvalidArtifactChecksum { kind });
    }
    if !record.is_frozen() {
        return Err(ReportError::ArtifactNotFrozen { kind });
    }
    Ok(())
}

fn validate_denominators(denominators: Denominators) -> Result<(), ReportError> {
    let partition = denominators
        .eligible
        .checked_add(denominators.unsupported)
        .and_then(|value| value.checked_add(denominators.invalid))
        .ok_or(ReportError::NumericOverflow {
            field: "suite_denominators",
        })?;
    if partition != denominators.total
        || [
            denominators.answered,
            denominators.unresolved,
            denominators.ambiguous,
            denominators.stale,
        ]
        .into_iter()
        .any(|count| count > denominators.eligible)
    {
        return Err(ReportError::InvalidDenominators);
    }
    Ok(())
}

fn validate_nonempty_map<T>(
    field: &'static str,
    values: &BTreeMap<String, T>,
    valid_value: impl Fn(&T) -> bool,
) -> Result<(), ReportError> {
    if values.is_empty()
        || values
            .iter()
            .any(|(key, value)| key.trim().is_empty() || !valid_value(value))
    {
        Err(ReportError::InvalidMetadata { field })
    } else {
        Ok(())
    }
}

fn require_nonempty(field: &'static str, value: &str) -> Result<(), ReportError> {
    if value.trim().is_empty() {
        Err(ReportError::InvalidMetadata { field })
    } else {
        Ok(())
    }
}

fn checked_increment(value: u64) -> Result<u64, ReportError> {
    checked_add(value, 1)
}

fn checked_add(left: u64, right: u64) -> Result<u64, ReportError> {
    left.checked_add(right).ok_or(ReportError::NumericOverflow {
        field: "model_summary",
    })
}

fn is_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
