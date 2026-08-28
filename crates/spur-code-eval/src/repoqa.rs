//! Leakage-safe translation of pinned `RepoQA` needles.
//!
//! Translation is deliberately offline and split into two passes. The first
//! pass builds the description-only retrieval query. The second resolves gold
//! identities and prepares native best-target scorer inputs. No query backend,
//! model, or scorer is invoked here.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

use crate::{
    CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldEvidence, Language, QueryPolicy,
    RepositoryPin, SourceFormat, SourceIdentity, SourceSpec, Suite,
};

/// Versioned identity of the `RepoQA` description-only query policy.
pub const REPOQA_QUERY_POLICY_HASH: &str =
    "sha256:b0a2d2fcfa743bba22e6274c49d7615dda2805fe97b1ef69d63f1b70313efbfc";

const INVALID_QUERY_INPUT: &str = "invalid RepoQA query withheld from retrieval";
const INVALID_GOLD_PREFIX: &str = "repoqa-unresolved-target:sha256:";

/// Failure to configure or execute `RepoQA` translation.
#[derive(Debug, thiserror::Error)]
pub enum RepoQaError {
    /// The selected source is not the `RepoQA` source.
    #[error("RepoQA adapter requires the repo_qa source")]
    WrongSuite,
    /// The selected source does not use the pinned `RepoQA` nested JSON format.
    #[error("RepoQA adapter requires gzip_repo_qa_json source format")]
    WrongSourceFormat,
    /// `RepoQA` source metadata did not declare a license.
    #[error("RepoQA source metadata must declare at least one license")]
    MissingLicense,
    /// An unsupported language omitted its denominator-visible reason.
    #[error("unsupported RepoQA language {language:?} is missing its reason")]
    MissingUnsupportedReason {
        /// Language with malformed capability metadata.
        language: String,
    },
    /// A source-symbol name was empty.
    #[error("RepoQA source-symbol name must not be empty")]
    EmptySourceSymbolName,
    /// The repository root could not be canonicalized.
    #[error("failed to canonicalize RepoQA repository root {path}: {source}")]
    RepositoryRoot {
        /// Repository root supplied to translation.
        path: PathBuf,
        /// Underlying filesystem failure.
        source: io::Error,
    },
    /// The repository root is not a directory.
    #[error("RepoQA repository root is not a directory: {0}")]
    RepositoryRootNotDirectory(PathBuf),
    /// A shared canonical-contract invariant failed.
    #[error(transparent)]
    Contract(#[from] ContractError),
    /// Raw upstream provenance could not be serialized.
    #[error("failed to preserve RepoQA upstream provenance: {0}")]
    Provenance(#[from] serde_json::Error),
}

/// One flattened, pinned `RepoQA` needle record.
///
/// The `RepoQA` release groups needles beneath languages and repositories. The
/// source-validation stage flattens each needle and attaches the immutable
/// repository pin represented by this type. Unknown upstream fields remain in
/// the canonical case's raw provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoQaRecord {
    case_id: String,
    language: String,
    #[serde(rename = "repo", alias = "repository")]
    repository: String,
    repository_uri: String,
    repository_commit: String,
    materialization_hash: String,
    path: String,
    name: String,
    start_line: usize,
    end_line: usize,
    description: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl RepoQaRecord {
    /// Returns the stable case identifier.
    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    /// Returns the upstream language label.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns the upstream repository identifier.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the canonical worktree-relative target path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the pinned upstream target name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the inclusive, zero-based upstream start line.
    #[must_use]
    pub const fn start_line(&self) -> usize {
        self.start_line
    }

    /// Returns the exclusive, zero-based upstream end line.
    #[must_use]
    pub const fn end_line(&self) -> usize {
        self.end_line
    }

    /// Returns the natural-language retrieval description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// A graph-resolved symbol name paired with its canonical source identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoQaSourceSymbol {
    name: String,
    source: SourceIdentity,
}

impl RepoQaSourceSymbol {
    /// Creates a source-symbol candidate used by exact `RepoQA` gold resolution.
    ///
    /// # Errors
    ///
    /// Returns [`RepoQaError::EmptySourceSymbolName`] when `name` is blank.
    pub fn new(name: impl Into<String>, source: SourceIdentity) -> Result<Self, RepoQaError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(RepoQaError::EmptySourceSymbolName);
        }
        Ok(Self { name, source })
    }

    /// Returns the graph-qualified symbol name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the canonical SPUR source identity.
    #[must_use]
    pub const fn source(&self) -> &SourceIdentity {
        &self.source
    }
}

/// One pinned target considered by `RepoQA`'s native best-target scorer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RepoQaModelTarget {
    name: String,
    source: SourceIdentity,
}

impl RepoQaModelTarget {
    /// Returns the upstream needle name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact pinned source span used as the reference body.
    #[must_use]
    pub const fn source(&self) -> &SourceIdentity {
        &self.source
    }
}

/// Scoring-only inputs required by `RepoQA`'s native best-target evaluator.
///
/// This type intentionally contains no model output, similarity, verdict, or
/// deterministic retrieval result. A later model lane can read the pinned
/// target bodies and invoke the upstream scorer without mutating retrieval
/// artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoQaModelScoreInput {
    language: Language,
    repository: String,
    ground_truth_name: String,
    targets: Vec<RepoQaModelTarget>,
}

impl RepoQaModelScoreInput {
    /// Returns the language passed to the native sanitizer/scorer.
    #[must_use]
    pub const fn language(&self) -> &Language {
        &self.language
    }

    /// Returns the repository whose needles form the best-target set.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the target name whose best-match verdict determines success.
    #[must_use]
    pub fn ground_truth_name(&self) -> &str {
        &self.ground_truth_name
    }

    /// Returns every pinned candidate target in the same repository/language.
    #[must_use]
    pub fn targets(&self) -> &[RepoQaModelTarget] {
        &self.targets
    }
}

/// Result of translating one `RepoQA` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoQaTranslation {
    case: CodeEvalCase,
    model_score_input: Option<RepoQaModelScoreInput>,
}

impl RepoQaTranslation {
    /// Returns the deterministic retrieval/gold contract.
    #[must_use]
    pub const fn case(&self) -> &CodeEvalCase {
        &self.case
    }

    /// Returns scoring-only model input for an eligible case.
    #[must_use]
    pub const fn model_score_input(&self) -> Option<&RepoQaModelScoreInput> {
        self.model_score_input.as_ref()
    }
}

/// Offline `RepoQA` record translator configured from the pinned source manifest.
#[derive(Debug, Clone)]
pub struct RepoQaAdapter {
    dataset_pin: ContentPin,
    capabilities: BTreeMap<String, Capability>,
}

#[derive(Debug, Clone)]
enum Capability {
    Supported,
    Unsupported(String),
}

impl RepoQaAdapter {
    /// Creates an adapter from the validated `RepoQA` source declaration.
    ///
    /// # Errors
    ///
    /// Returns an error when the source selects another suite/format, omits
    /// license metadata, or cannot construct the canonical content pin.
    pub fn new(source: &SourceSpec) -> Result<Self, RepoQaError> {
        if source.suite() != Suite::RepoQa {
            return Err(RepoQaError::WrongSuite);
        }
        if source.format() != SourceFormat::GzipRepoQaJson {
            return Err(RepoQaError::WrongSourceFormat);
        }
        let license = source
            .licenses()
            .first()
            .ok_or(RepoQaError::MissingLicense)?;
        let dataset_pin =
            ContentPin::new(source.uri(), source.revision(), source.sha256(), license)?;
        let mut capabilities = BTreeMap::new();
        for capability in source.languages() {
            let status = if capability.supported() {
                Capability::Supported
            } else {
                let reason =
                    capability
                        .reason()
                        .ok_or_else(|| RepoQaError::MissingUnsupportedReason {
                            language: capability.language().to_owned(),
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

    /// Translates records without fetching sources, querying SPUR, or running a model.
    ///
    /// `source_symbols` must contain the graph-resolved `RepoQA` needle targets
    /// for the supplied repository root. Exact path/name/span resolution is
    /// required for eligibility. Every input record yields one denominator-
    /// visible case, including unsupported and invalid records.
    ///
    /// # Errors
    ///
    /// Returns an error only for adapter configuration, an unusable repository
    /// root, provenance serialization, or a shared contract invariant. Target
    /// resolution failures are represented as [`CaseStatus::Invalid`].
    pub fn translate(
        &self,
        records: &[RepoQaRecord],
        repository_root: &Path,
        source_symbols: &[RepoQaSourceSymbol],
    ) -> Result<Vec<RepoQaTranslation>, RepoQaError> {
        let canonical_root = canonical_repository_root(repository_root)?;

        // Pass 1 is retrieval-visible: it uses descriptions and a static policy
        // identity only. Gold path/name/span material is not accepted here.
        let query_pass = records
            .iter()
            .map(build_query_policy)
            .collect::<Result<Vec<_>, _>>()?;

        // Pass 2 is scoring-only: resolve exact gold and prepare native target
        // sets. It cannot mutate the already-built retrieval queries.
        let resolutions = records
            .iter()
            .map(|record| resolve_target(record, &canonical_root, source_symbols))
            .collect::<Vec<_>>();
        let model_targets = model_target_groups(records, &resolutions);

        records
            .iter()
            .zip(query_pass)
            .zip(resolutions)
            .map(|((record, query), resolution)| {
                self.finish_translation(record, query, resolution, &model_targets)
            })
            .collect()
    }

    fn finish_translation(
        &self,
        record: &RepoQaRecord,
        query: QueryPass,
        resolution: TargetResolution,
        model_targets: &BTreeMap<ModelGroupKey, Vec<RepoQaModelTarget>>,
    ) -> Result<RepoQaTranslation, RepoQaError> {
        let language = Language::new(record.language.clone())?;
        let capability = self.capabilities.get(record.language());
        let target_is_resolved = matches!(&resolution, TargetResolution::Resolved(_));
        let (gold_evidence, resolution_reason) = match resolution {
            TargetResolution::Resolved(source) => {
                (GoldEvidence::new(vec![source], Vec::new())?, None)
            }
            TargetResolution::Invalid(reason) => (
                GoldEvidence::new(Vec::new(), vec![invalid_gold_identifier(record)])?,
                Some(reason),
            ),
        };

        let status = if let Some(reason) = resolution_reason.or(query.invalid_reason) {
            CaseStatus::invalid(reason)?
        } else {
            match capability {
                Some(Capability::Supported) => CaseStatus::eligible(),
                Some(Capability::Unsupported(reason)) => CaseStatus::unsupported(reason)?,
                None => CaseStatus::invalid(format!(
                    "RepoQA language {:?} is absent from the pinned source capabilities",
                    record.language
                ))?,
            }
        };
        let eligible = matches!(&status, CaseStatus::Eligible);
        let repository_pin = RepositoryPin::new(
            &record.repository_uri,
            &record.repository_commit,
            None,
            &record.materialization_hash,
        )?;
        let raw_upstream = serde_json::to_value(record)?;
        let case = CodeEvalCase::new(
            Suite::RepoQa,
            &record.case_id,
            language.clone(),
            self.dataset_pin.clone(),
            repository_pin,
            query.policy,
            gold_evidence,
            status,
            raw_upstream,
        )?;

        let model_score_input = if eligible && target_is_resolved {
            let key = ModelGroupKey::from_record(record);
            Some(RepoQaModelScoreInput {
                language,
                repository: record.repository.clone(),
                ground_truth_name: record.name.clone(),
                targets: model_targets.get(&key).cloned().unwrap_or_default(),
            })
        } else {
            None
        };

        Ok(RepoQaTranslation {
            case,
            model_score_input,
        })
    }
}

struct QueryPass {
    policy: QueryPolicy,
    invalid_reason: Option<String>,
}

fn build_query_policy(record: &RepoQaRecord) -> Result<QueryPass, ContractError> {
    let invalid_reason = query_leakage_reason(record);
    let input = if invalid_reason.is_some() {
        INVALID_QUERY_INPUT
    } else {
        record.description()
    };
    Ok(QueryPass {
        policy: QueryPolicy::new(input, REPOQA_QUERY_POLICY_HASH)?,
        invalid_reason,
    })
}

fn query_leakage_reason(record: &RepoQaRecord) -> Option<String> {
    let query_key = canonical_leakage_key(record.description());
    let target_key = canonical_leakage_key(target_leaf(record.name()));
    if !target_key.is_empty() && query_key.contains(&target_key) {
        return Some("RepoQA description query leaks the target name".to_owned());
    }

    let path_key = canonical_leakage_key(record.path());
    let path_leaks = !path_key.is_empty() && query_key.contains(&path_key);
    let span_leaks = [
        format!("{}..{}", record.start_line(), record.end_line()),
        format!("{}:{}", record.start_line(), record.end_line()),
        format!("{}-{}", record.start_line(), record.end_line()),
    ]
    .iter()
    .any(|span| record.description().contains(span));
    (path_leaks || span_leaks)
        .then(|| "RepoQA description query leaks gold path/span material".to_owned())
}

fn canonical_leakage_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn target_leaf(name: &str) -> &str {
    name.rsplit([':', '.', '/', '#'])
        .find(|part| !part.is_empty())
        .unwrap_or(name)
}

#[derive(Clone)]
enum TargetResolution {
    Resolved(SourceIdentity),
    Invalid(String),
}

fn resolve_target(
    record: &RepoQaRecord,
    repository_root: &Path,
    source_symbols: &[RepoQaSourceSymbol],
) -> TargetResolution {
    let Some(relative_path) = canonical_relative_path(record.path()) else {
        return TargetResolution::Invalid(
            "RepoQA target path is not a canonical worktree-relative path".to_owned(),
        );
    };
    let joined = repository_root.join(&relative_path);
    let Ok(canonical_file) = fs::canonicalize(&joined) else {
        return TargetResolution::Invalid("RepoQA target path is not a regular file".to_owned());
    };
    let Ok(canonical_relative) = canonical_file.strip_prefix(repository_root) else {
        return TargetResolution::Invalid(
            "RepoQA target path escapes the repository root through a symlink".to_owned(),
        );
    };
    if canonical_relative != relative_path {
        return TargetResolution::Invalid(
            "RepoQA target path does not resolve to its canonical worktree-relative path"
                .to_owned(),
        );
    }
    if !canonical_file.is_file() {
        return TargetResolution::Invalid("RepoQA target path is not a regular file".to_owned());
    }
    let Ok(source) = fs::read(&canonical_file) else {
        return TargetResolution::Invalid(
            "RepoQA target path is not a readable regular file".to_owned(),
        );
    };
    let Some((byte_start, byte_end)) = line_span(&source, record.start_line(), record.end_line())
    else {
        return TargetResolution::Invalid(
            "RepoQA target line span is malformed or out of range".to_owned(),
        );
    };

    let named = source_symbols
        .iter()
        .filter(|symbol| symbol.name() == record.name())
        .collect::<Vec<_>>();
    let exact = named
        .iter()
        .filter(|symbol| {
            let source = symbol.source();
            source.path() == record.path()
                && source.byte_start() == byte_start
                && source.byte_end() == byte_end
                && source.symbol_id().is_some()
        })
        .collect::<Vec<_>>();
    match exact.as_slice() {
        [symbol] => TargetResolution::Resolved(symbol.source().clone()),
        [] if named.is_empty() => TargetResolution::Invalid(
            "RepoQA target did not resolve to a canonical SPUR source identity".to_owned(),
        ),
        [] if named.iter().any(|symbol| {
            let source = symbol.source();
            source.path() == record.path()
                && source.byte_start() == byte_start
                && source.byte_end() == byte_end
                && source.symbol_id().is_none()
        }) =>
        {
            TargetResolution::Invalid(
                "RepoQA target did not resolve to a symbol-backed canonical SPUR source identity"
                    .to_owned(),
            )
        }
        [] => TargetResolution::Invalid(
            "RepoQA target path or span does not match its canonical SPUR source identity"
                .to_owned(),
        ),
        _ => TargetResolution::Invalid(
            "RepoQA target resolved to multiple canonical SPUR source identities".to_owned(),
        ),
    }
}

fn canonical_repository_root(root: &Path) -> Result<PathBuf, RepoQaError> {
    let canonical = fs::canonicalize(root).map_err(|source| RepoQaError::RepositoryRoot {
        path: root.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(RepoQaError::RepositoryRootNotDirectory(canonical));
    }
    Ok(canonical)
}

fn canonical_relative_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return None;
    }
    let mut canonical = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => canonical.push(component),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => {
                return None;
            }
        }
    }
    (canonical.to_string_lossy() == path.to_string_lossy()).then_some(canonical)
}

fn line_span(source: &[u8], start_line: usize, end_line: usize) -> Option<(u64, u64)> {
    if start_line >= end_line {
        return None;
    }
    let mut starts = vec![0_usize];
    for (index, byte) in source.iter().enumerate() {
        if *byte == b'\n' {
            starts.push(index + 1);
        }
    }
    if end_line > starts.len() {
        return None;
    }
    let start = *starts.get(start_line)?;
    let end = starts.get(end_line).copied().unwrap_or(source.len());
    let start = u64::try_from(start).ok()?;
    let end = u64::try_from(end).ok()?;
    (start < end).then_some((start, end))
}

fn invalid_gold_identifier(record: &RepoQaRecord) -> String {
    let mut digest = Sha256::new();
    digest.update(record.case_id.as_bytes());
    digest.update([0]);
    digest.update(record.path.as_bytes());
    digest.update([0]);
    digest.update(record.name.as_bytes());
    digest.update([0]);
    digest.update(record.start_line.to_le_bytes());
    digest.update(record.end_line.to_le_bytes());
    format!("{INVALID_GOLD_PREFIX}{:x}", digest.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModelGroupKey {
    language: String,
    repository: String,
    repository_commit: String,
}

impl ModelGroupKey {
    fn from_record(record: &RepoQaRecord) -> Self {
        Self {
            language: record.language.clone(),
            repository: record.repository.clone(),
            repository_commit: record.repository_commit.clone(),
        }
    }
}

fn model_target_groups(
    records: &[RepoQaRecord],
    resolutions: &[TargetResolution],
) -> BTreeMap<ModelGroupKey, Vec<RepoQaModelTarget>> {
    let mut groups = BTreeMap::<ModelGroupKey, Vec<RepoQaModelTarget>>::new();
    for (record, resolution) in records.iter().zip(resolutions) {
        if let TargetResolution::Resolved(source) = resolution {
            groups
                .entry(ModelGroupKey::from_record(record))
                .or_default()
                .push(RepoQaModelTarget {
                    name: record.name.clone(),
                    source: source.clone(),
                });
        }
    }
    for targets in groups.values_mut() {
        targets.sort_unstable();
        targets.dedup();
    }
    groups
}
