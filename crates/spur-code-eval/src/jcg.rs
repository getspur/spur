//! Deterministic JCG call-site normalization and annotation-scoped scoring.
//!
//! Retrieval remains owned by the shared query contract. This module can build
//! a leakage-guarded [`RetrievalRequest`], but scoring accepts only typed,
//! already-frozen [`FrozenCallSiteEvidence`]. It therefore cannot dispatch a
//! backend, query a model, or inspect a repository after retrieval.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{
    CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldCallEdge, GoldEvidence, Language,
    LeakagePolicy, QueryError, QueryPolicy, RepositoryPin, RetrievalRequest, SourceFormat,
    SourceIdentity, SourceKind, SourceSpec, Suite,
};

/// Content identity of the prompt-only JCG retrieval and frozen scoring policy.
pub const JCG_QUERY_POLICY_HASH: &str =
    "sha256:96c4d8870e685a15a78b82e91e32e7254a27a33194bd76d3ea8dcb9e6121adfb";

const INVALID_QUERY_INPUT: &str = "invalid JCG query withheld from retrieval";
const EMPTY_EXPECTATION_EVIDENCE: &str = "jcg:invalid-empty-expectations";

/// Failure to configure or evaluate the JCG adapter.
#[derive(Debug, Error)]
pub enum JcgError {
    /// The selected manifest entry belongs to another suite.
    #[error("JCG adapter requires the jcg source")]
    WrongSuite,
    /// The selected source is not the pinned JCG Markdown archive format.
    #[error("JCG adapter requires tar_gzip_jcg_markdown source format")]
    WrongSourceFormat,
    /// The source omitted required license metadata.
    #[error("JCG source metadata must declare at least one license")]
    MissingLicense,
    /// An unsupported language omitted its denominator-visible reason.
    #[error("unsupported JCG language {language:?} is missing its reason")]
    MissingUnsupportedReason {
        /// Language with the malformed capability declaration.
        language: String,
    },
    /// A frozen call-site row is not canonicalizable.
    #[error("invalid frozen JCG call-site field {field}: {message}")]
    InvalidEvidence {
        /// Invalid field name.
        field: &'static str,
        /// Stable diagnostic.
        message: String,
    },
    /// A shared canonical contract rejected the translated case.
    #[error(transparent)]
    Contract(#[from] ContractError),
    /// The shared retrieval contract rejected request construction or leakage.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Raw upstream provenance could not be serialized.
    #[error("failed to preserve JCG upstream provenance: {0}")]
    Provenance(#[from] serde_json::Error),
}

/// One pinned JCG testcase section with explicit expectation kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JcgRecord {
    case_id: String,
    language: String,
    prompt: String,
    repository_uri: String,
    repository_commit: String,
    materialization_hash: String,
    expectations: Vec<JcgExpectation>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl JcgRecord {
    /// Returns the stable testcase identifier.
    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    /// Returns the upstream language label.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns the prompt-only material visible to shared retrieval.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns pinned, annotation-scoped expectations.
    #[must_use]
    pub fn expectations(&self) -> &[JcgExpectation] {
        &self.expectations
    }
}

/// The distinct semantics of one pinned JCG expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectationKind {
    /// The caller must have a one-hop edge to the target.
    Direct,
    /// The caller must reach the target through at least one one-hop edge.
    Indirect,
    /// The annotated one-hop caller-to-target edge must be absent.
    Prohibited,
}

impl ExpectationKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Indirect => "indirect",
            Self::Prohibited => "prohibited",
        }
    }
}

/// One caller-to-target expectation from the pinned testcase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JcgExpectation {
    kind: ExpectationKind,
    caller: String,
    target: String,
}

impl JcgExpectation {
    /// Returns the expectation semantics.
    #[must_use]
    pub const fn kind(&self) -> ExpectationKind {
        self.kind
    }

    /// Returns the upstream caller name.
    #[must_use]
    pub fn caller(&self) -> &str {
        &self.caller
    }

    /// Returns the upstream target name.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// One typed SPUR call-site observation frozen before scoring starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenCallSiteEvidence {
    caller_method: String,
    call_site_line: u64,
    declared_target: String,
    resolved_targets: Vec<String>,
    source: SourceIdentity,
    source_kinds: Vec<SourceKind>,
    retrieval_rank: usize,
    #[serde(default)]
    unresolved_reason: Option<String>,
}

/// Frozen provenance retained for a normalized call-site observation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CallSiteProvenance {
    source: SourceIdentity,
    source_kinds: Vec<SourceKind>,
    retrieval_rank: usize,
}

impl CallSiteProvenance {
    /// Returns the stable source identity reported by SPUR.
    #[must_use]
    pub const fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// Returns every public query surface that reported the observation.
    #[must_use]
    pub fn source_kinds(&self) -> &[SourceKind] {
        &self.source_kinds
    }

    /// Returns the rank captured in the frozen response.
    #[must_use]
    pub const fn retrieval_rank(&self) -> usize {
        self.retrieval_rank
    }
}

/// Canonical, deduplicated call-site evidence used by the matcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NormalizedCallSite {
    caller_method: String,
    source_path: String,
    call_site_line: u64,
    declared_target: String,
    resolved_targets: Vec<String>,
    provenance: Vec<CallSiteProvenance>,
    unresolved_reasons: Vec<String>,
}

impl NormalizedCallSite {
    /// Returns the canonical caller method.
    #[must_use]
    pub fn caller_method(&self) -> &str {
        &self.caller_method
    }

    /// Returns the repository-relative source path.
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// Returns the one-based source call-site line.
    #[must_use]
    pub const fn call_site_line(&self) -> u64 {
        self.call_site_line
    }

    /// Returns the canonical declared target label.
    #[must_use]
    pub fn declared_target(&self) -> &str {
        &self.declared_target
    }

    /// Returns canonical resolved targets in stable order without duplicates.
    #[must_use]
    pub fn resolved_targets(&self) -> &[String] {
        &self.resolved_targets
    }

    /// Returns every frozen source observation in canonical order.
    #[must_use]
    pub fn provenance(&self) -> &[CallSiteProvenance] {
        &self.provenance
    }

    /// Returns preserved unresolved diagnostics.
    #[must_use]
    pub fn unresolved_reasons(&self) -> &[String] {
        &self.unresolved_reasons
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CallSiteKey {
    caller_method: String,
    source_path: String,
    call_site_line: u64,
    declared_target: String,
}

#[derive(Default)]
struct CallSiteAccumulator {
    resolved_targets: BTreeSet<String>,
    provenance: BTreeSet<CallSiteProvenance>,
    unresolved_reasons: BTreeSet<String>,
}

/// Normalizes frozen SPUR evidence without querying any external surface.
///
/// Rows are keyed by canonical caller, path, line, and declared target. Their
/// resolved targets, provenance, and unresolved reasons are unioned, sorted,
/// and deduplicated.
///
/// # Errors
///
/// Returns [`JcgError::InvalidEvidence`] for an empty caller/declared target,
/// a zero call-site line, or an empty resolved target.
pub fn normalize_call_sites(
    frozen: &[FrozenCallSiteEvidence],
) -> Result<Vec<NormalizedCallSite>, JcgError> {
    let mut accumulated = BTreeMap::<CallSiteKey, CallSiteAccumulator>::new();
    for row in frozen {
        let caller_method = canonical_method(&row.caller_method);
        require_evidence("caller_method", &caller_method)?;
        if row.call_site_line == 0 {
            return Err(JcgError::InvalidEvidence {
                field: "call_site_line",
                message: "must be one-based".to_owned(),
            });
        }
        let declared_target = canonical_method(&row.declared_target);
        require_evidence("declared_target", &declared_target)?;
        let key = CallSiteKey {
            caller_method,
            source_path: row.source.path().to_owned(),
            call_site_line: row.call_site_line,
            declared_target,
        };
        let entry = accumulated.entry(key).or_default();
        for target in &row.resolved_targets {
            let target = canonical_method(target);
            require_evidence("resolved_targets", &target)?;
            entry.resolved_targets.insert(target);
        }
        let mut source_kinds = row.source_kinds.clone();
        source_kinds.sort_unstable();
        source_kinds.dedup();
        entry.provenance.insert(CallSiteProvenance {
            source: row.source.clone(),
            source_kinds,
            retrieval_rank: row.retrieval_rank,
        });
        if let Some(reason) = row
            .unresolved_reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
        {
            entry.unresolved_reasons.insert(reason.to_owned());
        }
        if row.resolved_targets.is_empty() && entry.unresolved_reasons.is_empty() {
            entry
                .unresolved_reasons
                .insert("frozen call site has no resolved targets".to_owned());
        }
    }

    Ok(accumulated
        .into_iter()
        .map(|(key, entry)| NormalizedCallSite {
            caller_method: key.caller_method,
            source_path: key.source_path,
            call_site_line: key.call_site_line,
            declared_target: key.declared_target,
            resolved_targets: entry.resolved_targets.into_iter().collect(),
            provenance: entry.provenance.into_iter().collect(),
            unresolved_reasons: entry.unresolved_reasons.into_iter().collect(),
        })
        .collect())
}

/// Result of applying one expectation's own semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectationResult {
    /// The required edge/path exists, or the prohibited edge is absent.
    Matched,
    /// A required direct or indirect relation is absent.
    Missing,
    /// An explicitly prohibited direct edge is present.
    Violated,
}

/// One canonical edge that witnesses an outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallPathHop {
    caller_method: String,
    resolved_target: String,
    source_path: String,
    call_site_line: u64,
    declared_target: String,
}

/// Audited result for one pinned expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpectationOutcome {
    kind: ExpectationKind,
    caller: String,
    target: String,
    result: ExpectationResult,
    witness: Vec<CallPathHop>,
    diagnostic: Option<String>,
}

impl ExpectationOutcome {
    /// Returns the applied expectation semantics.
    #[must_use]
    pub const fn kind(&self) -> ExpectationKind {
        self.kind
    }

    /// Returns the canonical outcome.
    #[must_use]
    pub const fn result(&self) -> ExpectationResult {
        self.result
    }

    /// Returns a deterministic direct edge or transitive path witness.
    #[must_use]
    pub fn witness(&self) -> &[CallPathHop] {
        &self.witness
    }

    /// Returns a missing/forbidden diagnostic, when applicable.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

/// Matches only pinned annotations against already-normalized evidence.
///
/// Direct expectations use exactly one edge. Indirect expectations accept any
/// non-empty caller-to-target path. Prohibited expectations inspect only the
/// explicitly annotated direct edge. Unannotated targets never become false
/// positives.
#[must_use]
pub fn match_expectations(
    call_sites: &[NormalizedCallSite],
    expectations: &[JcgExpectation],
) -> Vec<ExpectationOutcome> {
    let (adjacency, edge_witnesses) = edge_index(call_sites);
    canonical_expectations(expectations)
        .into_iter()
        .map(|expectation| {
            let edge_key = (expectation.caller.clone(), expectation.target.clone());
            match expectation.kind {
                ExpectationKind::Direct => edge_witnesses.get(&edge_key).map_or_else(
                    || missing_outcome(&expectation, "required direct edge is absent"),
                    |witness| matched_outcome(&expectation, vec![witness.clone()]),
                ),
                ExpectationKind::Indirect => indirect_path(
                    &expectation.caller,
                    &expectation.target,
                    &adjacency,
                    &edge_witnesses,
                )
                .map_or_else(
                    || missing_outcome(&expectation, "required indirect path is absent"),
                    |witness| matched_outcome(&expectation, witness),
                ),
                ExpectationKind::Prohibited => edge_witnesses.get(&edge_key).map_or_else(
                    || matched_outcome(&expectation, Vec::new()),
                    |witness| ExpectationOutcome {
                        diagnostic: Some(format!(
                            "prohibited annotated edge {} -> {} was present",
                            expectation.caller, expectation.target
                        )),
                        kind: expectation.kind,
                        caller: expectation.caller.clone(),
                        target: expectation.target.clone(),
                        result: ExpectationResult::Violated,
                        witness: vec![witness.clone()],
                    },
                ),
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CanonicalExpectation {
    kind: ExpectationKind,
    caller: String,
    target: String,
}

fn canonical_expectations(expectations: &[JcgExpectation]) -> Vec<CanonicalExpectation> {
    expectations
        .iter()
        .map(|expectation| CanonicalExpectation {
            kind: expectation.kind,
            caller: canonical_method(&expectation.caller),
            target: canonical_method(&expectation.target),
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

type Adjacency = BTreeMap<String, BTreeSet<String>>;
type EdgeWitnesses = BTreeMap<(String, String), CallPathHop>;

fn edge_index(call_sites: &[NormalizedCallSite]) -> (Adjacency, EdgeWitnesses) {
    let mut adjacency = Adjacency::new();
    let mut witnesses = EdgeWitnesses::new();
    for call_site in call_sites {
        for target in &call_site.resolved_targets {
            adjacency
                .entry(call_site.caller_method.clone())
                .or_default()
                .insert(target.clone());
            witnesses
                .entry((call_site.caller_method.clone(), target.clone()))
                .or_insert_with(|| CallPathHop {
                    caller_method: call_site.caller_method.clone(),
                    resolved_target: target.clone(),
                    source_path: call_site.source_path.clone(),
                    call_site_line: call_site.call_site_line,
                    declared_target: call_site.declared_target.clone(),
                });
        }
    }
    (adjacency, witnesses)
}

fn indirect_path(
    caller: &str,
    target: &str,
    adjacency: &Adjacency,
    witnesses: &EdgeWitnesses,
) -> Option<Vec<CallPathHop>> {
    let mut queue = VecDeque::from([(caller.to_owned(), Vec::new())]);
    let mut visited = BTreeSet::from([caller.to_owned()]);
    while let Some((current, path)) = queue.pop_front() {
        for next in adjacency.get(&current).into_iter().flatten() {
            let mut next_path = path.clone();
            let witness = witnesses.get(&(current.clone(), next.clone()))?;
            next_path.push(witness.clone());
            if next == target {
                return Some(next_path);
            }
            if visited.insert(next.clone()) {
                queue.push_back((next.clone(), next_path));
            }
        }
    }
    None
}

fn matched_outcome(
    expectation: &CanonicalExpectation,
    witness: Vec<CallPathHop>,
) -> ExpectationOutcome {
    ExpectationOutcome {
        kind: expectation.kind,
        caller: expectation.caller.clone(),
        target: expectation.target.clone(),
        result: ExpectationResult::Matched,
        witness,
        diagnostic: None,
    }
}

fn missing_outcome(expectation: &CanonicalExpectation, reason: &str) -> ExpectationOutcome {
    ExpectationOutcome {
        diagnostic: Some(format!(
            "{reason}: {} -> {}",
            expectation.caller, expectation.target
        )),
        kind: expectation.kind,
        caller: expectation.caller.clone(),
        target: expectation.target.clone(),
        result: ExpectationResult::Missing,
        witness: Vec::new(),
    }
}

/// Explicit declaration that JCG annotations are not an exhaustive edge set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationScope {
    /// Only annotated expectations may contribute to scoring or diagnostics.
    PartialExpectations,
}

/// Exact numerator and denominator for positive annotated expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AnnotatedRecall {
    matched: usize,
    expected: usize,
}

impl AnnotatedRecall {
    /// Returns matched direct and indirect expectations.
    #[must_use]
    pub const fn matched(self) -> usize {
        self.matched
    }

    /// Returns the direct and indirect annotated denominator.
    #[must_use]
    pub const fn expected(self) -> usize {
        self.expected
    }
}

/// Exact counts for explicitly prohibited direct edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProhibitedSummary {
    expected: usize,
    violated: usize,
}

impl ProhibitedSummary {
    /// Returns the number of explicit prohibited annotations.
    #[must_use]
    pub const fn expected(self) -> usize {
        self.expected
    }

    /// Returns how many explicit prohibited annotations were violated.
    #[must_use]
    pub const fn violated(self) -> usize {
        self.violated
    }
}

/// Denominator-visible status of one JCG scoring audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JcgAuditStatus {
    /// Every frozen call site had a resolved target and no unresolved reason.
    Complete,
    /// Scoring completed from known edges while unresolved evidence remained.
    Partial {
        /// Number of normalized call sites carrying unresolved diagnostics.
        unresolved_call_sites: usize,
    },
    /// The source case is valid but the language extractor is unavailable.
    Unsupported {
        /// Manifest-pinned capability reason.
        reason: String,
    },
    /// The source record cannot participate in scoring.
    Invalid {
        /// Stable validation reason.
        reason: String,
    },
}

/// Complete deterministic audit for one denominator-visible JCG case.
///
/// This type intentionally has no precision field. JCG's partial annotations
/// support positive annotated recall and explicit prohibited-edge diagnostics,
/// not global call-graph precision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JcgAudit {
    case_id: String,
    language: String,
    annotation_scope: AnnotationScope,
    frozen_call_site_count: usize,
    normalized_call_sites: Vec<NormalizedCallSite>,
    unresolved_call_sites: Vec<NormalizedCallSite>,
    expectation_outcomes: Vec<ExpectationOutcome>,
    annotated_positive_recall: Option<AnnotatedRecall>,
    prohibited_summary: ProhibitedSummary,
    status: JcgAuditStatus,
}

impl JcgAudit {
    /// Returns canonical call sites with complete provenance.
    #[must_use]
    pub fn normalized_call_sites(&self) -> &[NormalizedCallSite] {
        &self.normalized_call_sites
    }

    /// Returns the unresolved subset retained for auditing.
    #[must_use]
    pub fn unresolved_call_sites(&self) -> &[NormalizedCallSite] {
        &self.unresolved_call_sites
    }

    /// Returns outcomes in canonical kind/caller/target order.
    #[must_use]
    pub fn expectation_outcomes(&self) -> &[ExpectationOutcome] {
        &self.expectation_outcomes
    }

    /// Returns scoped positive recall, absent for unsupported/invalid cases.
    #[must_use]
    pub const fn annotated_positive_recall(&self) -> Option<AnnotatedRecall> {
        self.annotated_positive_recall
    }

    /// Returns explicit prohibited-edge counts.
    #[must_use]
    pub const fn prohibited_summary(&self) -> ProhibitedSummary {
        self.prohibited_summary
    }

    /// Returns the denominator-visible audit state.
    #[must_use]
    pub const fn status(&self) -> &JcgAuditStatus {
        &self.status
    }
}

/// Canonical shared case and its JCG-specific scoring audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JcgEvaluation {
    case: CodeEvalCase,
    audit: JcgAudit,
}

impl JcgEvaluation {
    /// Returns the canonical shared benchmark case.
    #[must_use]
    pub const fn case(&self) -> &CodeEvalCase {
        &self.case
    }

    /// Returns the complete JCG normalization and scoring audit.
    #[must_use]
    pub const fn audit(&self) -> &JcgAudit {
        &self.audit
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Capability {
    Supported,
    Unsupported(String),
}

/// Adapter configured from the pinned JCG manifest declaration.
#[derive(Debug, Clone)]
pub struct JcgAdapter {
    dataset_pin: ContentPin,
    capabilities: BTreeMap<String, Capability>,
}

impl JcgAdapter {
    /// Creates an adapter from the pinned JCG source declaration.
    ///
    /// # Errors
    ///
    /// Returns an error for the wrong suite/format, missing license metadata,
    /// malformed unsupported capability, or invalid content pin.
    pub fn new(source: &SourceSpec) -> Result<Self, JcgError> {
        if source.suite() != Suite::Jcg {
            return Err(JcgError::WrongSuite);
        }
        if source.format() != SourceFormat::TarGzipJcgMarkdown {
            return Err(JcgError::WrongSourceFormat);
        }
        let license = source.licenses().first().ok_or(JcgError::MissingLicense)?;
        let dataset_pin =
            ContentPin::new(source.uri(), source.revision(), source.sha256(), license)?;
        let mut capabilities = BTreeMap::new();
        for capability in source.languages() {
            let status = if capability.supported() {
                Capability::Supported
            } else {
                Capability::Unsupported(
                    capability
                        .reason()
                        .ok_or_else(|| JcgError::MissingUnsupportedReason {
                            language: capability.language().to_owned(),
                        })?
                        .to_owned(),
                )
            };
            capabilities.insert(capability.language().to_ascii_lowercase(), status);
        }
        Ok(Self {
            dataset_pin,
            capabilities,
        })
    }

    /// Builds a request for the existing shared leakage guard and query APIs.
    ///
    /// The returned request contains only `record.prompt()` as positive query
    /// material. Target names and annotated caller-target pairs are supplied
    /// solely to [`LeakagePolicy`] and are rejected before backend dispatch.
    ///
    /// # Errors
    ///
    /// Returns a shared query error for malformed gold endpoints, an invalid
    /// repository root, empty query material, or invalid retrieval bounds.
    pub fn retrieval_request(
        record: &JcgRecord,
        repository_root: impl AsRef<Path>,
        top_k: usize,
        exact_followup_limit: usize,
    ) -> Result<RetrievalRequest, JcgError> {
        let target_names = record
            .expectations
            .iter()
            .map(|expectation| expectation.target.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let gold_call_edges = record
            .expectations
            .iter()
            .map(|expectation| GoldCallEdge::new(&expectation.caller, &expectation.target))
            .collect::<Result<Vec<_>, _>>()?;
        let leakage = LeakagePolicy::new(target_names, Vec::new(), gold_call_edges)?;
        Ok(RetrievalRequest::new(
            repository_root,
            record.prompt(),
            record.prompt(),
            top_k,
            exact_followup_limit,
            leakage,
        )?)
    }

    /// Normalizes and scores only the supplied frozen call-site evidence.
    ///
    /// This method deliberately has no backend, model, repository root, or
    /// retrieval-result parameter. Unsupported and invalid cases remain in the
    /// shared denominator while producing no expectation outcomes.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid immutable repository metadata, malformed
    /// frozen evidence, shared contract failures, or provenance serialization.
    pub fn evaluate(
        &self,
        record: &JcgRecord,
        frozen: &[FrozenCallSiteEvidence],
    ) -> Result<JcgEvaluation, JcgError> {
        let repository_pin = RepositoryPin::new(
            &record.repository_uri,
            &record.repository_commit,
            None,
            &record.materialization_hash,
        )?;
        let language_key = record.language.trim().to_ascii_lowercase();
        let invalid_reason = record_validation_reason(record);
        let status = if let Some(reason) = invalid_reason {
            CaseStatus::invalid(reason)?
        } else {
            match self.capabilities.get(&language_key) {
                Some(Capability::Supported) => CaseStatus::eligible(),
                Some(Capability::Unsupported(reason)) => CaseStatus::unsupported(reason)?,
                None => CaseStatus::invalid(format!(
                    "JCG language {:?} is absent from the pinned source capabilities",
                    record.language
                ))?,
            }
        };
        let query_input = if matches!(status, CaseStatus::Invalid { .. }) {
            INVALID_QUERY_INPUT
        } else {
            record.prompt()
        };
        let query_policy = QueryPolicy::new(query_input, JCG_QUERY_POLICY_HASH)?;
        let language = if language_key.is_empty() {
            Language::new("invalid")?
        } else {
            Language::new(&language_key)?
        };
        let identifiers = expectation_identifiers(&record.expectations);
        let gold_evidence = GoldEvidence::new(Vec::new(), identifiers)?;
        let raw_upstream = serde_json::to_value(record)?;
        let case_id = if record.case_id.trim().is_empty() {
            "invalid-jcg-case"
        } else {
            record.case_id()
        };
        let case = CodeEvalCase::new(
            Suite::Jcg,
            case_id,
            language,
            self.dataset_pin.clone(),
            repository_pin,
            query_policy,
            gold_evidence,
            status.clone(),
            raw_upstream,
        )?;
        let audit = match status {
            CaseStatus::Eligible => scored_audit(record, frozen)?,
            CaseStatus::Unsupported { reason } => {
                unscored_audit(record, frozen.len(), JcgAuditStatus::Unsupported { reason })
            }
            CaseStatus::Invalid { reason } => {
                unscored_audit(record, frozen.len(), JcgAuditStatus::Invalid { reason })
            }
        };
        Ok(JcgEvaluation { case, audit })
    }
}

fn scored_audit(
    record: &JcgRecord,
    frozen: &[FrozenCallSiteEvidence],
) -> Result<JcgAudit, JcgError> {
    let normalized_call_sites = normalize_call_sites(frozen)?;
    let unresolved_call_sites = normalized_call_sites
        .iter()
        .filter(|call_site| !call_site.unresolved_reasons.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    let expectation_outcomes = match_expectations(&normalized_call_sites, &record.expectations);
    let expected = expectation_outcomes
        .iter()
        .filter(|outcome| outcome.kind != ExpectationKind::Prohibited)
        .count();
    let matched = expectation_outcomes
        .iter()
        .filter(|outcome| {
            outcome.kind != ExpectationKind::Prohibited
                && outcome.result == ExpectationResult::Matched
        })
        .count();
    let prohibited_expected = expectation_outcomes
        .iter()
        .filter(|outcome| outcome.kind == ExpectationKind::Prohibited)
        .count();
    let prohibited_violated = expectation_outcomes
        .iter()
        .filter(|outcome| {
            outcome.kind == ExpectationKind::Prohibited
                && outcome.result == ExpectationResult::Violated
        })
        .count();
    let status = if unresolved_call_sites.is_empty() {
        JcgAuditStatus::Complete
    } else {
        JcgAuditStatus::Partial {
            unresolved_call_sites: unresolved_call_sites.len(),
        }
    };
    Ok(JcgAudit {
        case_id: record.case_id.clone(),
        language: record.language.clone(),
        annotation_scope: AnnotationScope::PartialExpectations,
        frozen_call_site_count: normalized_call_sites
            .iter()
            .map(|call_site| call_site.provenance.len())
            .sum(),
        normalized_call_sites,
        unresolved_call_sites,
        expectation_outcomes,
        annotated_positive_recall: (expected > 0).then_some(AnnotatedRecall { matched, expected }),
        prohibited_summary: ProhibitedSummary {
            expected: prohibited_expected,
            violated: prohibited_violated,
        },
        status,
    })
}

fn unscored_audit(
    record: &JcgRecord,
    frozen_call_site_count: usize,
    status: JcgAuditStatus,
) -> JcgAudit {
    JcgAudit {
        case_id: record.case_id.clone(),
        language: record.language.clone(),
        annotation_scope: AnnotationScope::PartialExpectations,
        frozen_call_site_count,
        normalized_call_sites: Vec::new(),
        unresolved_call_sites: Vec::new(),
        expectation_outcomes: Vec::new(),
        annotated_positive_recall: None,
        prohibited_summary: ProhibitedSummary {
            expected: 0,
            violated: 0,
        },
        status,
    }
}

fn expectation_identifiers(expectations: &[JcgExpectation]) -> Vec<String> {
    let identifiers = canonical_expectations(expectations)
        .into_iter()
        .map(|expectation| {
            format!(
                "{}:{}->{}",
                expectation.kind.label(),
                expectation.caller,
                expectation.target
            )
        })
        .collect::<Vec<_>>();
    if identifiers.is_empty() {
        vec![EMPTY_EXPECTATION_EVIDENCE.to_owned()]
    } else {
        identifiers
    }
}

fn record_validation_reason(record: &JcgRecord) -> Option<String> {
    for (field, value) in [
        ("case_id", record.case_id.as_str()),
        ("language", record.language.as_str()),
        ("prompt", record.prompt.as_str()),
    ] {
        if value.trim().is_empty() {
            return Some(format!("JCG record field {field} must not be empty"));
        }
    }
    if record.expectations.is_empty() {
        return Some("JCG record must retain at least one annotated expectation".to_owned());
    }
    record.expectations.iter().find_map(|expectation| {
        let caller = canonical_method(&expectation.caller);
        let target = canonical_method(&expectation.target);
        (caller.is_empty() || target.is_empty()).then(|| {
            "JCG expectation caller and target must not be empty after normalization".to_owned()
        })
    })
}

fn require_evidence(field: &'static str, value: &str) -> Result<(), JcgError> {
    if value.is_empty() {
        Err(JcgError::InvalidEvidence {
            field,
            message: "must not be empty after normalization".to_owned(),
        })
    } else {
        Ok(())
    }
}

fn canonical_method(value: &str) -> String {
    let trimmed = value.trim();
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "<global>" | "global" | "<module>" | "module"
    ) {
        return "<global>".to_owned();
    }
    trimmed
        .replace("::", ".")
        .replace(['/', '#'], ".")
        .split('.')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(".")
}
