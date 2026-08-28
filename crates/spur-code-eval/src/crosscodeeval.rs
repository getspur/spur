//! Prompt-only `CrossCodeEval` retrieval and post-retrieval evidence derivation.
//!
//! The pinned upstream schema stores the current-file prefix in `prompt`, the
//! hidden continuation in `groundtruth`, and repository/file identity beneath
//! `metadata`. Upstream's ordinary inference tokenizes `prompt` alone; its
//! `query_type="groundtruth"` path is an oracle experiment. This adapter follows
//! the ordinary interpretation and keeps `groundtruth` scoring-only.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    retrieve, CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldEvidence, Language,
    LeakageKind, LeakagePolicy, QueryBackend, QueryError, QueryPolicy, RepositoryPin,
    RetrievalRequest, RetrievalResult, SourceFormat, SourceIdentity, SourceKind, SourceSpec, Suite,
};

/// Version of the deterministic post-retrieval identifier resolver.
pub const SPUR_DERIVED_EVIDENCE_VERSION: &str = "spur-derived-evidence-v1";

/// Content identity of the current-file-prompt-only query policy.
pub const CROSSCODEEVAL_QUERY_POLICY_HASH: &str =
    "sha256:008887aef201ac0348297fb76a4c47d4b21d26da395595c0efbdfca78ccc5512";

const INVALID_QUERY_INPUT: &str = "invalid CrossCodeEval query withheld from retrieval";
const NO_DERIVED_IDENTIFIER: &str = "crosscodeeval:no-derived-identifiers";

/// Failure to configure or execute `CrossCodeEval` translation.
#[derive(Debug, thiserror::Error)]
pub enum CrossCodeError {
    /// The selected manifest source belongs to another suite.
    #[error("CrossCodeEval adapter requires the cross_code_eval source")]
    WrongSuite,
    /// The selected source does not use the pinned XZ/TAR schema.
    #[error("CrossCodeEval adapter requires tar_xz_cross_code_eval source format")]
    WrongSourceFormat,
    /// The source declaration omitted redistribution metadata.
    #[error("CrossCodeEval source metadata must declare at least one license")]
    MissingLicense,
    /// An unsupported language omitted its denominator-visible reason.
    #[error("unsupported CrossCodeEval language {language:?} is missing its reason")]
    MissingUnsupportedReason {
        /// Language with malformed capability metadata.
        language: String,
    },
    /// The materialized repository root could not be canonicalized.
    #[error("failed to canonicalize CrossCodeEval repository root {path}: {source}")]
    RepositoryRoot {
        /// Repository root supplied to retrieval.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: io::Error,
    },
    /// The materialized repository root is not a directory.
    #[error("CrossCodeEval repository root is not a directory: {0}")]
    RepositoryRootNotDirectory(PathBuf),
    /// A shared canonical-contract invariant failed.
    #[error(transparent)]
    Contract(#[from] ContractError),
    /// The shared query boundary rejected or failed the request.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Raw upstream provenance could not be serialized.
    #[error("failed to preserve CrossCodeEval upstream provenance: {0}")]
    Provenance(#[from] serde_json::Error),
}

/// Pinned upstream metadata attached to one line-completion example.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossCodeMetadata {
    task_id: String,
    repository: String,
    file: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl CrossCodeMetadata {
    /// Returns the upstream task identifier.
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Returns the upstream repository label.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the current-file path whose prefix forms the query.
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }
}

/// One flattened record from the pinned `CrossCodeEval` archive.
///
/// `language` and the immutable repository fields are attached while source
/// records are flattened from their language-specific archive member. Unknown
/// upstream fields, including unknown `metadata` fields, are retained through
/// `serde(flatten)` and therefore survive in [`CodeEvalCase::raw_upstream`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossCodeRecord {
    language: String,
    prompt: String,
    groundtruth: String,
    metadata: CrossCodeMetadata,
    repository_uri: String,
    repository_commit: String,
    materialization_hash: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl CrossCodeRecord {
    /// Returns the current-file prompt visible to retrieval.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the hidden upstream completion used only for scoring.
    #[must_use]
    pub fn groundtruth(&self) -> &str {
        &self.groundtruth
    }

    /// Returns the upstream language label.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns the pinned record metadata.
    #[must_use]
    pub const fn metadata(&self) -> &CrossCodeMetadata {
        &self.metadata
    }

    /// Derives the scoring-only identifier set from pinned `groundtruth`.
    ///
    /// The pinned schema has no ordinary qrels or explicit identifier array.
    /// Consequently this is the canonical schema interpretation: scan the
    /// hidden completion for language identifiers, remove reserved words, then
    /// sort and deduplicate. Call this only after [`CrossCodeAdapter::retrieval_case`]
    /// has returned a frozen result.
    #[must_use]
    pub fn scoring_input(&self) -> CrossCodeScoringInput {
        CrossCodeScoringInput::from_hidden_completion(&self.groundtruth)
    }
}

/// Scoring-only identifiers that may be supplied explicitly or derived from
/// the pinned hidden completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossCodeScoringInput {
    identifiers: Vec<String>,
}

impl CrossCodeScoringInput {
    /// Creates a canonical scoring-only identifier set.
    ///
    /// Empty and duplicate values are discarded. Identifiers are sorted so
    /// caller ordering cannot affect the evidence audit.
    #[must_use]
    pub fn from_identifiers<I, S>(identifiers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let identifiers = identifiers
            .into_iter()
            .map(Into::into)
            .filter(|identifier: &String| !identifier.trim().is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self { identifiers }
    }

    /// Returns canonical scoring-only identifiers.
    #[must_use]
    pub fn identifiers(&self) -> &[String] {
        &self.identifiers
    }

    fn from_hidden_completion(completion: &str) -> Self {
        Self::from_identifiers(
            identifier_tokens(completion)
                .into_iter()
                .filter(|identifier| !is_reserved_identifier(identifier)),
        )
    }
}

/// A frozen rank observation explaining why one source span was positive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedEvidenceMatch {
    source: SourceIdentity,
    rank: usize,
    score: f64,
    source_kinds: Vec<SourceKind>,
}

impl ResolvedEvidenceMatch {
    /// Returns the exact identity copied from the frozen retrieval result.
    #[must_use]
    pub const fn source(&self) -> &SourceIdentity {
        &self.source
    }

    /// Returns the one-based deterministic retrieval rank.
    #[must_use]
    pub const fn rank(&self) -> usize {
        self.rank
    }

    /// Returns the finite retrieval score used for ranking.
    #[must_use]
    pub const fn score(&self) -> f64 {
        self.score
    }

    /// Returns every public retrieval source that contributed the identity.
    #[must_use]
    pub fn source_kinds(&self) -> &[SourceKind] {
        &self.source_kinds
    }
}

/// Complete decision trace for one canonical completion identifier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentifierResolutionTrace {
    identifier: String,
    resolver_version: String,
    matches: Vec<ResolvedEvidenceMatch>,
    unresolved_reason: Option<String>,
}

impl IdentifierResolutionTrace {
    /// Returns the scoring-only identifier considered by the resolver.
    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Returns the resolver version that made this decision.
    #[must_use]
    pub fn resolver_version(&self) -> &str {
        &self.resolver_version
    }

    /// Returns all matched identities with source and rank evidence.
    #[must_use]
    pub fn matches(&self) -> &[ResolvedEvidenceMatch] {
        &self.matches
    }

    /// Returns why this identifier did not produce a cross-file positive.
    #[must_use]
    pub fn unresolved_reason(&self) -> Option<&str> {
        self.unresolved_reason.as_deref()
    }
}

/// Persistable `spur-derived-evidence-v1` decision record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossCodeEvidenceAudit {
    resolver_version: String,
    positive_spans: Vec<SourceIdentity>,
    unresolved_identifiers: Vec<String>,
    resolution_trace: Vec<IdentifierResolutionTrace>,
    outcome_reason: Option<String>,
}

impl CrossCodeEvidenceAudit {
    /// Returns the resolver contract version.
    #[must_use]
    pub fn resolver_version(&self) -> &str {
        &self.resolver_version
    }

    /// Returns sorted, deduplicated positive identities from frozen retrieval.
    #[must_use]
    pub fn positive_spans(&self) -> &[SourceIdentity] {
        &self.positive_spans
    }

    /// Returns sorted, deduplicated identifiers without a frozen match.
    #[must_use]
    pub fn unresolved_identifiers(&self) -> &[String] {
        &self.unresolved_identifiers
    }

    /// Returns one complete trace entry per canonical candidate identifier.
    #[must_use]
    pub fn resolution_trace(&self) -> &[IdentifierResolutionTrace] {
        &self.resolution_trace
    }

    /// Returns the denominator-visible outcome explanation, when non-eligible.
    #[must_use]
    pub fn outcome_reason(&self) -> Option<&str> {
        self.outcome_reason.as_deref()
    }
}

/// Phase-one output: prompt policy plus an optional frozen retrieval result.
///
/// This type deliberately exposes neither the hidden completion nor derived
/// scoring identifiers.
#[derive(Debug)]
pub struct CrossCodeRetrievalCase {
    record: CrossCodeRecord,
    dataset_pin: ContentPin,
    repository_pin: RepositoryPin,
    query_policy: QueryPolicy,
    retrieval_result: Option<RetrievalResult>,
    phase_status: PhaseStatus,
}

impl CrossCodeRetrievalCase {
    /// Returns the prompt-only policy frozen before scoring derivation.
    #[must_use]
    pub const fn query_policy(&self) -> &QueryPolicy {
        &self.query_policy
    }

    /// Returns frozen retrieval evidence for supported, valid cases.
    #[must_use]
    pub const fn retrieval_result(&self) -> Option<&RetrievalResult> {
        self.retrieval_result.as_ref()
    }
}

#[derive(Debug)]
enum PhaseStatus {
    Supported,
    Unsupported(String),
    Invalid(String),
}

/// Final canonical case paired with its deterministic evidence audit.
#[derive(Debug)]
pub struct CrossCodeTranslation {
    case: CodeEvalCase,
    audit: CrossCodeEvidenceAudit,
}

impl CrossCodeTranslation {
    /// Returns the canonical denominator-visible benchmark case.
    #[must_use]
    pub const fn case(&self) -> &CodeEvalCase {
        &self.case
    }

    /// Returns the persistable post-retrieval resolution audit.
    #[must_use]
    pub const fn audit(&self) -> &CrossCodeEvidenceAudit {
        &self.audit
    }

    /// Returns whether a downstream scoring lane may consume this case.
    #[must_use]
    pub fn scoring_eligible(&self) -> bool {
        matches!(self.case.status(), CaseStatus::Eligible)
    }
}

/// Offline adapter configured from the immutable source manifest.
#[derive(Debug, Clone)]
pub struct CrossCodeAdapter {
    dataset_pin: ContentPin,
    capabilities: BTreeMap<String, Capability>,
}

#[derive(Debug, Clone)]
enum Capability {
    Supported,
    Unsupported(String),
}

impl CrossCodeAdapter {
    /// Creates an adapter from the pinned `CrossCodeEval` source declaration.
    ///
    /// # Errors
    ///
    /// Returns an error for the wrong suite/format, absent license metadata, a
    /// malformed unsupported capability, or an invalid content pin.
    pub fn new(source: &SourceSpec) -> Result<Self, CrossCodeError> {
        if source.suite() != Suite::CrossCodeEval {
            return Err(CrossCodeError::WrongSuite);
        }
        if source.format() != SourceFormat::TarXzCrossCodeEval {
            return Err(CrossCodeError::WrongSourceFormat);
        }
        let license = source
            .licenses()
            .first()
            .ok_or(CrossCodeError::MissingLicense)?;
        let dataset_pin =
            ContentPin::new(source.uri(), source.revision(), source.sha256(), license)?;
        let mut capabilities = BTreeMap::new();
        for capability in source.languages() {
            let status = if capability.supported() {
                Capability::Supported
            } else {
                let reason = capability.reason().ok_or_else(|| {
                    CrossCodeError::MissingUnsupportedReason {
                        language: capability.language().to_owned(),
                    }
                })?;
                Capability::Unsupported(reason.to_owned())
            };
            capabilities.insert(capability.language().to_owned(), status);
        }
        Ok(Self {
            dataset_pin,
            capabilities,
        })
    }

    /// Executes phase one through the injectable shared query boundary.
    ///
    /// Only `record.prompt()` is copied into [`QueryPolicy`], the semantic
    /// query, and the exact-symbol query. Hidden completion material and its
    /// identifiers are used solely as negative leakage guards and are never
    /// serialized into backend arguments or ranking input. Unsupported and
    /// malformed cases do not dispatch.
    ///
    /// # Errors
    ///
    /// Returns an error for an unusable repository root, invalid immutable
    /// repository metadata, query setup/dispatch failures, or shared contract
    /// failures. Leakage is retained as a denominator-visible invalid case.
    pub async fn retrieval_case<B: QueryBackend + ?Sized>(
        &self,
        backend: &B,
        record: &CrossCodeRecord,
        repository_root: &Path,
        top_k: usize,
        exact_followup_limit: usize,
    ) -> Result<CrossCodeRetrievalCase, CrossCodeError> {
        let canonical_root = canonical_repository_root(repository_root)?;
        let repository_pin = RepositoryPin::new(
            &record.repository_uri,
            &record.repository_commit,
            None,
            &record.materialization_hash,
        )?;

        let invalid_reason = record_validation_reason(record).or_else(|| leakage_reason(record));
        let capability = self.capabilities.get(record.language());
        let phase_status = if let Some(reason) = invalid_reason {
            PhaseStatus::Invalid(reason)
        } else {
            match capability {
                Some(Capability::Supported) => PhaseStatus::Supported,
                Some(Capability::Unsupported(reason)) => PhaseStatus::Unsupported(reason.clone()),
                None => PhaseStatus::Invalid(format!(
                    "CrossCodeEval language {:?} is absent from the pinned source capabilities",
                    record.language()
                )),
            }
        };

        if !matches!(phase_status, PhaseStatus::Supported) {
            let input = if matches!(phase_status, PhaseStatus::Unsupported(_)) {
                record.prompt()
            } else {
                INVALID_QUERY_INPUT
            };
            return Ok(CrossCodeRetrievalCase {
                record: record.clone(),
                dataset_pin: self.dataset_pin.clone(),
                repository_pin,
                query_policy: QueryPolicy::new(input, CROSSCODEEVAL_QUERY_POLICY_HASH)?,
                retrieval_result: None,
                phase_status,
            });
        }

        let guard_identifiers = record.scoring_input();
        let leakage = LeakagePolicy::new(
            guard_identifiers.identifiers,
            vec![record.groundtruth().to_owned()],
            Vec::new(),
        )?;
        let request = RetrievalRequest::new(
            &canonical_root,
            record.prompt(),
            record.prompt(),
            top_k,
            exact_followup_limit,
            leakage,
        )?;
        let (query_policy, retrieval_result, phase_status) = match retrieve(backend, &request).await
        {
            Ok(result) => (
                QueryPolicy::new(record.prompt(), CROSSCODEEVAL_QUERY_POLICY_HASH)?,
                Some(result),
                PhaseStatus::Supported,
            ),
            Err(QueryError::ForbiddenLeakage { kind, .. }) => (
                QueryPolicy::new(INVALID_QUERY_INPUT, CROSSCODEEVAL_QUERY_POLICY_HASH)?,
                None,
                PhaseStatus::Invalid(leakage_kind_reason(kind).to_owned()),
            ),
            Err(error) => return Err(error.into()),
        };

        Ok(CrossCodeRetrievalCase {
            record: record.clone(),
            dataset_pin: self.dataset_pin.clone(),
            repository_pin,
            query_policy,
            retrieval_result,
            phase_status,
        })
    }
}

/// Executes phase two against already-frozen evidence.
///
/// Positives are copied only from canonical [`SourceIdentity`] values present
/// in `retrieval_case.retrieval_result()`. The resolver performs no backend,
/// model, repository, or oracle-file access.
///
/// # Errors
///
/// Returns an error only when a shared canonical contract invariant fails or
/// raw upstream provenance cannot be serialized.
pub fn derive_evidence_after_retrieval(
    retrieval_case: CrossCodeRetrievalCase,
    scoring_input: CrossCodeScoringInput,
) -> Result<CrossCodeTranslation, CrossCodeError> {
    let CrossCodeRetrievalCase {
        record,
        dataset_pin,
        repository_pin,
        query_policy,
        retrieval_result,
        phase_status,
    } = retrieval_case;
    let retrieval_corruption = retrieval_result
        .as_ref()
        .is_some_and(|result| !result.issues().is_empty());
    let audit = resolve_evidence_audit(
        &record,
        retrieval_result.as_ref(),
        &phase_status,
        scoring_input,
        retrieval_corruption,
    );

    let status = match phase_status {
        PhaseStatus::Invalid(reason) => CaseStatus::invalid(reason)?,
        PhaseStatus::Unsupported(reason) => CaseStatus::unsupported(reason)?,
        PhaseStatus::Supported if retrieval_corruption || audit.positive_spans.is_empty() => {
            CaseStatus::invalid(
                audit
                    .outcome_reason
                    .as_deref()
                    .unwrap_or("supported CrossCodeEval case has no valid resolved evidence"),
            )?
        }
        PhaseStatus::Supported => CaseStatus::eligible(),
    };
    let derived_identifiers = audit
        .resolution_trace
        .iter()
        .map(|trace| trace.identifier.clone())
        .collect();
    let gold_evidence = GoldEvidence::new(audit.positive_spans.clone(), derived_identifiers)?;
    let language = if record.language().trim().is_empty() {
        Language::new("invalid")?
    } else {
        Language::new(record.language())?
    };
    let raw_upstream = serde_json::to_value(&record)?;
    let case_id = if record.metadata.task_id().trim().is_empty() {
        "invalid-crosscodeeval-case"
    } else {
        record.metadata.task_id()
    };
    let case = CodeEvalCase::new(
        Suite::CrossCodeEval,
        case_id,
        language,
        dataset_pin,
        repository_pin,
        query_policy,
        gold_evidence,
        status,
        raw_upstream,
    )?;

    Ok(CrossCodeTranslation { case, audit })
}

fn resolve_evidence_audit(
    record: &CrossCodeRecord,
    retrieval_result: Option<&RetrievalResult>,
    phase_status: &PhaseStatus,
    scoring_input: CrossCodeScoringInput,
    retrieval_corruption: bool,
) -> CrossCodeEvidenceAudit {
    let mut identifiers = scoring_input.identifiers;
    let no_identifiers = identifiers.is_empty();
    if no_identifiers {
        identifiers.push(NO_DERIVED_IDENTIFIER.to_owned());
    }
    let phase_reason = match phase_status {
        PhaseStatus::Supported => None,
        PhaseStatus::Unsupported(reason) | PhaseStatus::Invalid(reason) => Some(reason.clone()),
    };
    let mut resolver = IdentifierResolver {
        record,
        retrieval_result,
        phase_reason: phase_reason.as_deref(),
        no_identifiers,
        positives: BTreeSet::new(),
        unresolved: BTreeSet::new(),
    };
    let resolution_trace = identifiers
        .into_iter()
        .map(|identifier| resolver.resolve(identifier))
        .collect::<Vec<_>>();
    let positive_spans = resolver.positives.into_iter().collect::<Vec<_>>();
    let outcome_reason = match phase_status {
        PhaseStatus::Invalid(reason) | PhaseStatus::Unsupported(reason) => Some(reason.clone()),
        PhaseStatus::Supported if retrieval_corruption => {
            Some("frozen retrieval contains invalid evidence diagnostics".to_owned())
        }
        PhaseStatus::Supported if positive_spans.is_empty() => Some(
            "supported CrossCodeEval case has no resolved positive evidence in frozen retrieval"
                .to_owned(),
        ),
        PhaseStatus::Supported => None,
    };
    CrossCodeEvidenceAudit {
        resolver_version: SPUR_DERIVED_EVIDENCE_VERSION.to_owned(),
        positive_spans,
        unresolved_identifiers: resolver.unresolved.into_iter().collect(),
        resolution_trace,
        outcome_reason,
    }
}

struct IdentifierResolver<'a> {
    record: &'a CrossCodeRecord,
    retrieval_result: Option<&'a RetrievalResult>,
    phase_reason: Option<&'a str>,
    no_identifiers: bool,
    positives: BTreeSet<SourceIdentity>,
    unresolved: BTreeSet<String>,
}

impl IdentifierResolver<'_> {
    fn resolve(&mut self, identifier: String) -> IdentifierResolutionTrace {
        let mut matches = Vec::new();
        let mut matched_target_file = false;
        if let Some(result) = self.retrieval_result {
            for (index, hit) in result.hits().iter().enumerate() {
                if identity_matches_identifier(hit.identity(), &identifier) {
                    if same_path(hit.identity().path(), self.record.metadata.file()) {
                        matched_target_file = true;
                        continue;
                    }
                    self.positives.insert(hit.identity().clone());
                    matches.push(ResolvedEvidenceMatch {
                        source: hit.identity().clone(),
                        rank: index + 1,
                        score: hit.score(),
                        source_kinds: hit.source_kinds().to_vec(),
                    });
                }
            }
        }
        let unresolved_reason = matches.is_empty().then(|| {
            self.unresolved.insert(identifier.clone());
            if self.no_identifiers && identifier == NO_DERIVED_IDENTIFIER {
                "hidden completion contained no resolvable identifier".to_owned()
            } else if let Some(reason) = self.phase_reason {
                reason.to_owned()
            } else if matched_target_file {
                "identifier matched only current-file retrieval evidence".to_owned()
            } else {
                "identifier is absent from frozen retrieval evidence".to_owned()
            }
        });
        IdentifierResolutionTrace {
            identifier,
            resolver_version: SPUR_DERIVED_EVIDENCE_VERSION.to_owned(),
            matches,
            unresolved_reason,
        }
    }
}

fn canonical_repository_root(repository_root: &Path) -> Result<PathBuf, CrossCodeError> {
    let canonical = std::fs::canonicalize(repository_root).map_err(|source| {
        CrossCodeError::RepositoryRoot {
            path: repository_root.to_path_buf(),
            source,
        }
    })?;
    if !canonical.is_dir() {
        return Err(CrossCodeError::RepositoryRootNotDirectory(canonical));
    }
    Ok(canonical)
}

fn record_validation_reason(record: &CrossCodeRecord) -> Option<String> {
    for (field, value) in [
        ("language", record.language()),
        ("prompt", record.prompt()),
        ("groundtruth", record.groundtruth()),
        ("metadata.task_id", record.metadata.task_id()),
        ("metadata.repository", record.metadata.repository()),
        ("metadata.file", record.metadata.file()),
    ] {
        if value.trim().is_empty() {
            return Some(format!(
                "CrossCodeEval record field {field} must not be empty"
            ));
        }
    }
    let file = Path::new(record.metadata.file());
    if file.is_absolute()
        || file.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Some("CrossCodeEval metadata.file must be worktree-relative".to_owned());
    }
    None
}

fn leakage_reason(record: &CrossCodeRecord) -> Option<String> {
    let prompt_tokens = identifier_tokens(record.prompt());
    let completion_tokens = identifier_tokens(record.groundtruth());
    let target_leakage = record
        .scoring_input()
        .identifiers()
        .iter()
        .find(|identifier| prompt_tokens.iter().any(|token| token == *identifier))
        .map(|_| leakage_kind_reason(LeakageKind::TargetName).to_owned());
    if target_leakage.is_some() {
        return target_leakage;
    }
    (!completion_tokens.is_empty()
        && token_sequence_position(&prompt_tokens, &completion_tokens).is_some())
    .then(|| leakage_kind_reason(LeakageKind::HiddenCompletion).to_owned())
}

const fn leakage_kind_reason(kind: LeakageKind) -> &'static str {
    match kind {
        LeakageKind::TargetName => "CrossCodeEval prompt leaks a target identifier",
        LeakageKind::HiddenCompletion => "CrossCodeEval prompt leaks the hidden completion",
        LeakageKind::GoldCallEdge => "CrossCodeEval prompt leaks gold call-edge material",
    }
}

fn identity_matches_identifier(identity: &SourceIdentity, identifier: &str) -> bool {
    identity
        .symbol_id()
        .into_iter()
        .chain(std::iter::once(identity.path()))
        .flat_map(identifier_tokens)
        .any(|token| token == identifier)
}

fn same_path(left: &str, right: &str) -> bool {
    left.replace('\\', "/") == right.replace('\\', "/")
}

fn identifier_tokens(input: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut current = String::new();
    for character in input.chars() {
        if character == '_' || character == '$' || character.is_alphanumeric() {
            if current.is_empty()
                && character != '_'
                && character != '$'
                && !character.is_alphabetic()
            {
                continue;
            }
            current.push(character);
        } else if !current.is_empty() {
            identifiers.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        identifiers.push(current);
    }
    identifiers
}

fn token_sequence_position(haystack: &[String], needle: &[String]) -> Option<usize> {
    (!needle.is_empty() && needle.len() <= haystack.len())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}

fn is_reserved_identifier(identifier: &str) -> bool {
    matches!(
        identifier,
        "abstract"
            | "as"
            | "async"
            | "await"
            | "bool"
            | "boolean"
            | "break"
            | "case"
            | "catch"
            | "char"
            | "class"
            | "const"
            | "continue"
            | "def"
            | "default"
            | "del"
            | "do"
            | "double"
            | "elif"
            | "else"
            | "enum"
            | "except"
            | "export"
            | "extends"
            | "false"
            | "final"
            | "finally"
            | "float"
            | "for"
            | "from"
            | "function"
            | "global"
            | "if"
            | "implements"
            | "import"
            | "in"
            | "instanceof"
            | "int"
            | "interface"
            | "is"
            | "lambda"
            | "let"
            | "long"
            | "namespace"
            | "native"
            | "new"
            | "none"
            | "null"
            | "number"
            | "object"
            | "of"
            | "override"
            | "package"
            | "pass"
            | "private"
            | "protected"
            | "public"
            | "raise"
            | "readonly"
            | "return"
            | "short"
            | "static"
            | "str"
            | "string"
            | "super"
            | "switch"
            | "synchronized"
            | "this"
            | "throw"
            | "throws"
            | "transient"
            | "true"
            | "try"
            | "type"
            | "typeof"
            | "var"
            | "void"
            | "volatile"
            | "while"
            | "with"
            | "yield"
    )
}
