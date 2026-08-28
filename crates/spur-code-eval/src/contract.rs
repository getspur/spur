//! Canonical, dataset-independent code-evaluation case contracts.

use std::{error::Error, fmt};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::CONTRACT_VERSION;

/// Validation failure for a canonical code-evaluation case.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContractError {
    /// A required string contained no non-whitespace characters.
    EmptyField {
        /// Stable dotted path to the invalid field.
        field: &'static str,
    },
    /// A revision names mutable content instead of an immutable snapshot.
    MutableRevision {
        /// Stable dotted path to the invalid revision.
        field: &'static str,
        /// Rejected revision.
        revision: String,
    },
    /// A repository revision was not a complete Git object identifier.
    InvalidRepositoryCommit {
        /// Rejected commit identifier.
        revision: String,
    },
    /// A half-open byte span was empty or reversed.
    InvalidSpan {
        /// Inclusive byte offset at which the span starts.
        byte_start: u64,
        /// Exclusive byte offset at which the span ends.
        byte_end: u64,
    },
    /// Gold evidence named neither a source nor a derived identifier.
    EmptyGoldEvidence,
    /// Serialized data used a contract version this crate cannot interpret.
    UnsupportedContractVersion {
        /// Version accepted by this crate.
        expected: &'static str,
        /// Version found in serialized data.
        actual: String,
    },
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(formatter, "{field} must not be empty"),
            Self::MutableRevision { field, revision } => {
                write!(formatter, "{field} must be immutable, got {revision:?}")
            }
            Self::InvalidRepositoryCommit { revision } => write!(
                formatter,
                "repository_pin.commit_sha must be a complete Git object ID, got {revision:?}"
            ),
            Self::InvalidSpan {
                byte_start,
                byte_end,
            } => write!(
                formatter,
                "source span must satisfy byte_start < byte_end, got {byte_start}..{byte_end}"
            ),
            Self::EmptyGoldEvidence => {
                formatter.write_str("gold evidence must contain a source or derived identifier")
            }
            Self::UnsupportedContractVersion { expected, actual } => write!(
                formatter,
                "unsupported contract version {actual:?}; expected {expected:?}"
            ),
        }
    }
}

impl Error for ContractError {}

/// Upstream benchmark suite represented by a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Suite {
    /// `RepoQA` natural-language-to-function retrieval.
    RepoQa,
    /// `CrossCodeEval` cross-file completion evidence retrieval.
    CrossCodeEval,
    /// JCG call-graph expectation evaluation.
    Jcg,
}

/// Source language label retained from the upstream dataset.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Language(String);

impl Language {
    /// Creates a non-empty language label.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError::EmptyField`] when `value` is empty or whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into();
        require_non_empty("language", &value)?;
        Ok(Self(value))
    }

    /// Returns the upstream language label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// Immutable dataset content identity and redistribution metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContentPin {
    uri: String,
    revision: String,
    content_hash: String,
    license: String,
}

impl ContentPin {
    /// Creates an immutable dataset content pin.
    ///
    /// # Errors
    ///
    /// Returns an error when a required field is empty or `revision` names a
    /// mutable branch-like reference.
    pub fn new(
        uri: impl Into<String>,
        revision: impl Into<String>,
        content_hash: impl Into<String>,
        license: impl Into<String>,
    ) -> Result<Self, ContractError> {
        let pin = Self {
            uri: uri.into(),
            revision: revision.into(),
            content_hash: content_hash.into(),
            license: license.into(),
        };
        pin.validate()?;
        Ok(pin)
    }

    /// Returns the dataset URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Returns the immutable dataset revision.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Returns the expected content hash.
    #[must_use]
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    /// Returns the dataset license metadata.
    #[must_use]
    pub fn license(&self) -> &str {
        &self.license
    }

    fn validate(&self) -> Result<(), ContractError> {
        require_non_empty("content_pin.uri", &self.uri)?;
        require_immutable_revision("content_pin.revision", &self.revision)?;
        require_non_empty("content_pin.content_hash", &self.content_hash)?;
        require_non_empty("content_pin.license", &self.license)
    }
}

#[derive(Deserialize)]
struct ContentPinData {
    uri: String,
    revision: String,
    content_hash: String,
    license: String,
}

impl<'de> Deserialize<'de> for ContentPin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = ContentPinData::deserialize(deserializer)?;
        Self::new(data.uri, data.revision, data.content_hash, data.license)
            .map_err(D::Error::custom)
    }
}

/// Exact repository revision and materialized-tree identity for a case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryPin {
    uri: String,
    commit_sha: String,
    subdirectory: Option<String>,
    materialization_hash: String,
}

impl RepositoryPin {
    /// Creates a repository pin at a complete Git object identifier.
    ///
    /// Both SHA-1 and SHA-256 Git object identifier widths are accepted.
    ///
    /// # Errors
    ///
    /// Returns an error when a required field is empty, the revision is
    /// mutable, or the revision is not a complete hexadecimal object ID.
    pub fn new(
        uri: impl Into<String>,
        commit_sha: impl Into<String>,
        subdirectory: Option<String>,
        materialization_hash: impl Into<String>,
    ) -> Result<Self, ContractError> {
        let pin = Self {
            uri: uri.into(),
            commit_sha: commit_sha.into(),
            subdirectory,
            materialization_hash: materialization_hash.into(),
        };
        pin.validate()?;
        Ok(pin)
    }

    /// Returns the repository URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Returns the exact repository commit.
    #[must_use]
    pub fn commit_sha(&self) -> &str {
        &self.commit_sha
    }

    /// Returns the repository-relative subtree, when one is selected.
    #[must_use]
    pub fn subdirectory(&self) -> Option<&str> {
        self.subdirectory.as_deref()
    }

    /// Returns the expected hash of the materialized repository tree.
    #[must_use]
    pub fn materialization_hash(&self) -> &str {
        &self.materialization_hash
    }

    fn validate(&self) -> Result<(), ContractError> {
        require_non_empty("repository_pin.uri", &self.uri)?;
        require_immutable_revision("repository_pin.commit_sha", &self.commit_sha)?;
        if !is_complete_git_object_id(&self.commit_sha) {
            return Err(ContractError::InvalidRepositoryCommit {
                revision: self.commit_sha.clone(),
            });
        }
        if let Some(subdirectory) = &self.subdirectory {
            require_non_empty("repository_pin.subdirectory", subdirectory)?;
        }
        require_non_empty(
            "repository_pin.materialization_hash",
            &self.materialization_hash,
        )
    }
}

#[derive(Deserialize)]
struct RepositoryPinData {
    uri: String,
    commit_sha: String,
    subdirectory: Option<String>,
    materialization_hash: String,
}

impl<'de> Deserialize<'de> for RepositoryPin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = RepositoryPinData::deserialize(deserializer)?;
        Self::new(
            data.uri,
            data.commit_sha,
            data.subdirectory,
            data.materialization_hash,
        )
        .map_err(D::Error::custom)
    }
}

/// Leakage-safe query input and the versioned policy that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryPolicy {
    input: String,
    policy_hash: String,
}

impl QueryPolicy {
    /// Creates a query and policy identity.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError::EmptyField`] when either value is empty.
    pub fn new(
        input: impl Into<String>,
        policy_hash: impl Into<String>,
    ) -> Result<Self, ContractError> {
        let policy = Self {
            input: input.into(),
            policy_hash: policy_hash.into(),
        };
        policy.validate()?;
        Ok(policy)
    }

    /// Returns the leakage-safe retriever input.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Returns the query-policy content hash.
    #[must_use]
    pub fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    fn validate(&self) -> Result<(), ContractError> {
        require_non_empty("query_policy.input", &self.input)?;
        require_non_empty("query_policy.policy_hash", &self.policy_hash)
    }
}

#[derive(Deserialize)]
struct QueryPolicyData {
    input: String,
    policy_hash: String,
}

impl<'de> Deserialize<'de> for QueryPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = QueryPolicyData::deserialize(deserializer)?;
        Self::new(data.input, data.policy_hash).map_err(D::Error::custom)
    }
}

/// Canonical identity of one half-open source byte span.
///
/// Derived ordering is the canonical order: path, start byte, end byte, then
/// optional symbol ID. It is also used to deterministically deduplicate gold
/// evidence.
///
/// # Examples
///
/// ```
/// use spur_code_eval::SourceIdentity;
///
/// let identity = SourceIdentity::new("src/lib.rs", 4, 12, Some("symbol-7".to_owned()))?;
/// assert_eq!(identity.path(), "src/lib.rs");
/// assert_eq!(identity.symbol_id(), Some("symbol-7"));
/// # Ok::<(), spur_code_eval::ContractError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct SourceIdentity {
    path: String,
    byte_start: u64,
    byte_end: u64,
    symbol_id: Option<String>,
}

impl SourceIdentity {
    /// Creates a canonical source identity.
    ///
    /// # Errors
    ///
    /// Returns an error when `path` or a supplied `symbol_id` is empty, or
    /// when the half-open byte span is empty or reversed.
    pub fn new(
        path: impl Into<String>,
        byte_start: u64,
        byte_end: u64,
        symbol_id: Option<String>,
    ) -> Result<Self, ContractError> {
        let identity = Self {
            path: path.into(),
            byte_start,
            byte_end,
            symbol_id,
        };
        identity.validate()?;
        Ok(identity)
    }

    /// Returns the repository-relative source path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the inclusive start byte.
    #[must_use]
    pub const fn byte_start(&self) -> u64 {
        self.byte_start
    }

    /// Returns the exclusive end byte.
    #[must_use]
    pub const fn byte_end(&self) -> u64 {
        self.byte_end
    }

    /// Returns the resolved SPUR symbol ID, when available.
    #[must_use]
    pub fn symbol_id(&self) -> Option<&str> {
        self.symbol_id.as_deref()
    }

    fn validate(&self) -> Result<(), ContractError> {
        require_non_empty("source_identity.path", &self.path)?;
        if self.byte_start >= self.byte_end {
            return Err(ContractError::InvalidSpan {
                byte_start: self.byte_start,
                byte_end: self.byte_end,
            });
        }
        if let Some(symbol_id) = &self.symbol_id {
            require_non_empty("source_identity.symbol_id", symbol_id)?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct SourceIdentityData {
    path: String,
    byte_start: u64,
    byte_end: u64,
    symbol_id: Option<String>,
}

impl<'de> Deserialize<'de> for SourceIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = SourceIdentityData::deserialize(deserializer)?;
        Self::new(data.path, data.byte_start, data.byte_end, data.symbol_id)
            .map_err(D::Error::custom)
    }
}

/// Canonical source spans and upstream-derived evidence identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GoldEvidence {
    sources: Vec<SourceIdentity>,
    derived_identifiers: Vec<String>,
}

impl GoldEvidence {
    /// Creates sorted, deduplicated gold evidence.
    ///
    /// # Errors
    ///
    /// Returns an error when both collections are empty or a derived identifier
    /// contains no non-whitespace characters.
    pub fn new(
        mut sources: Vec<SourceIdentity>,
        mut derived_identifiers: Vec<String>,
    ) -> Result<Self, ContractError> {
        if sources.is_empty() && derived_identifiers.is_empty() {
            return Err(ContractError::EmptyGoldEvidence);
        }
        for identifier in &derived_identifiers {
            require_non_empty("gold_evidence.derived_identifier", identifier)?;
        }
        sources.sort_unstable();
        sources.dedup();
        derived_identifiers.sort_unstable();
        derived_identifiers.dedup();
        Ok(Self {
            sources,
            derived_identifiers,
        })
    }

    /// Returns source identities in canonical order without duplicates.
    #[must_use]
    pub fn sources(&self) -> &[SourceIdentity] {
        &self.sources
    }

    /// Returns derived identifiers in canonical order without duplicates.
    #[must_use]
    pub fn derived_identifiers(&self) -> &[String] {
        &self.derived_identifiers
    }
}

#[derive(Deserialize)]
struct GoldEvidenceData {
    sources: Vec<SourceIdentity>,
    derived_identifiers: Vec<String>,
}

impl<'de> Deserialize<'de> for GoldEvidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = GoldEvidenceData::deserialize(deserializer)?;
        Self::new(data.sources, data.derived_identifiers).map_err(D::Error::custom)
    }
}

/// Denominator-visible eligibility state for one upstream case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaseStatus {
    /// The case participates in suite-native scoring.
    Eligible,
    /// The source is valid, but a required product capability is unavailable.
    Unsupported {
        /// Stable, human-readable explanation retained in reports.
        reason: String,
    },
    /// The case violates a source, repository, corpus, or gold invariant.
    Invalid {
        /// Stable, human-readable explanation retained in reports.
        reason: String,
    },
}

impl CaseStatus {
    /// Creates an eligible status.
    #[must_use]
    pub const fn eligible() -> Self {
        Self::Eligible
    }

    /// Creates an unsupported status with a denominator-visible reason.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError::EmptyField`] when `reason` is empty.
    pub fn unsupported(reason: impl Into<String>) -> Result<Self, ContractError> {
        Self::with_reason(reason.into(), true)
    }

    /// Creates an invalid status with a denominator-visible reason.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError::EmptyField`] when `reason` is empty.
    pub fn invalid(reason: impl Into<String>) -> Result<Self, ContractError> {
        Self::with_reason(reason.into(), false)
    }

    /// Returns the non-eligible reason, if one is required by this status.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Eligible => None,
            Self::Unsupported { reason } | Self::Invalid { reason } => Some(reason),
        }
    }

    /// Returns whether the status must remain represented in report counts.
    #[must_use]
    pub const fn is_denominator_visible(&self) -> bool {
        matches!(
            self,
            Self::Eligible | Self::Unsupported { .. } | Self::Invalid { .. }
        )
    }

    fn with_reason(reason: String, unsupported: bool) -> Result<Self, ContractError> {
        require_non_empty("case_status.reason", &reason)?;
        if unsupported {
            Ok(Self::Unsupported { reason })
        } else {
            Ok(Self::Invalid { reason })
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CaseStatusData {
    Eligible,
    Unsupported { reason: String },
    Invalid { reason: String },
}

impl<'de> Deserialize<'de> for CaseStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match CaseStatusData::deserialize(deserializer)? {
            CaseStatusData::Eligible => Ok(Self::eligible()),
            CaseStatusData::Unsupported { reason } => {
                Self::unsupported(reason).map_err(D::Error::custom)
            }
            CaseStatusData::Invalid { reason } => Self::invalid(reason).map_err(D::Error::custom),
        }
    }
}

/// Canonical case shared by every code-intelligence benchmark adapter.
///
/// Construction and deserialization validate all local invariants. Unknown
/// upstream fields belong in [`Self::raw_upstream`] and survive Serde round
/// trips unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeEvalCase {
    suite: Suite,
    case_id: String,
    language: Language,
    contract_version: String,
    dataset_pin: ContentPin,
    repository_pin: RepositoryPin,
    query_policy: QueryPolicy,
    gold_evidence: GoldEvidence,
    status: CaseStatus,
    raw_upstream: Value,
}

impl CodeEvalCase {
    /// Creates and validates a canonical case at [`CONTRACT_VERSION`].
    ///
    /// # Errors
    ///
    /// Returns [`ContractError::EmptyField`] when `case_id` is empty.
    #[expect(
        clippy::too_many_arguments,
        reason = "the constructor mirrors the canonical serialized contract"
    )]
    pub fn new(
        suite: Suite,
        case_id: impl Into<String>,
        language: Language,
        dataset_pin: ContentPin,
        repository_pin: RepositoryPin,
        query_policy: QueryPolicy,
        gold_evidence: GoldEvidence,
        status: CaseStatus,
        raw_upstream: Value,
    ) -> Result<Self, ContractError> {
        Self::try_from(CodeEvalCaseData {
            suite,
            case_id: case_id.into(),
            language,
            contract_version: CONTRACT_VERSION.to_owned(),
            dataset_pin,
            repository_pin,
            query_policy,
            gold_evidence,
            status,
            raw_upstream,
        })
    }

    /// Returns the upstream suite.
    #[must_use]
    pub const fn suite(&self) -> Suite {
        self.suite
    }

    /// Returns the stable case identifier within the suite.
    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    /// Returns the upstream language label.
    #[must_use]
    pub const fn language(&self) -> &Language {
        &self.language
    }

    /// Returns the serialized contract version.
    #[must_use]
    pub fn contract_version(&self) -> &str {
        &self.contract_version
    }

    /// Returns the immutable dataset content pin.
    #[must_use]
    pub const fn dataset_pin(&self) -> &ContentPin {
        &self.dataset_pin
    }

    /// Returns the immutable repository pin.
    #[must_use]
    pub const fn repository_pin(&self) -> &RepositoryPin {
        &self.repository_pin
    }

    /// Returns the leakage-safe query and policy identity.
    #[must_use]
    pub const fn query_policy(&self) -> &QueryPolicy {
        &self.query_policy
    }

    /// Returns canonical gold evidence.
    #[must_use]
    pub const fn gold_evidence(&self) -> &GoldEvidence {
        &self.gold_evidence
    }

    /// Returns the denominator-visible case status.
    #[must_use]
    pub const fn status(&self) -> &CaseStatus {
        &self.status
    }

    /// Returns the unmodified upstream record retained for auditing.
    #[must_use]
    pub const fn raw_upstream(&self) -> &Value {
        &self.raw_upstream
    }

    /// Returns whether this case remains represented in report counts.
    #[must_use]
    pub const fn is_denominator_visible(&self) -> bool {
        self.status.is_denominator_visible()
    }
}

#[derive(Deserialize)]
struct CodeEvalCaseData {
    suite: Suite,
    case_id: String,
    language: Language,
    contract_version: String,
    dataset_pin: ContentPin,
    repository_pin: RepositoryPin,
    query_policy: QueryPolicy,
    gold_evidence: GoldEvidence,
    status: CaseStatus,
    raw_upstream: Value,
}

impl TryFrom<CodeEvalCaseData> for CodeEvalCase {
    type Error = ContractError;

    fn try_from(data: CodeEvalCaseData) -> Result<Self, Self::Error> {
        require_non_empty("case_id", &data.case_id)?;
        if data.contract_version != CONTRACT_VERSION {
            return Err(ContractError::UnsupportedContractVersion {
                expected: CONTRACT_VERSION,
                actual: data.contract_version,
            });
        }
        Ok(Self {
            suite: data.suite,
            case_id: data.case_id,
            language: data.language,
            contract_version: data.contract_version,
            dataset_pin: data.dataset_pin,
            repository_pin: data.repository_pin,
            query_policy: data.query_policy,
            gold_evidence: data.gold_evidence,
            status: data.status,
            raw_upstream: data.raw_upstream,
        })
    }
}

impl<'de> Deserialize<'de> for CodeEvalCase {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let data = CodeEvalCaseData::deserialize(deserializer)?;
        Self::try_from(data).map_err(D::Error::custom)
    }
}

fn require_non_empty(field: &'static str, value: &str) -> Result<(), ContractError> {
    if value.trim().is_empty() {
        Err(ContractError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn require_immutable_revision(field: &'static str, revision: &str) -> Result<(), ContractError> {
    require_non_empty(field, revision)?;
    if is_mutable_revision(revision) {
        Err(ContractError::MutableRevision {
            field,
            revision: revision.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn is_mutable_revision(revision: &str) -> bool {
    let normalized = revision.trim().to_ascii_lowercase();
    if normalized.starts_with("refs/heads/") || normalized.starts_with("refs/remotes/") {
        return true;
    }
    matches!(
        normalized.rsplit('/').next(),
        Some("head" | "latest" | "main" | "master" | "tip" | "trunk")
    )
}

fn is_complete_git_object_id(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}
