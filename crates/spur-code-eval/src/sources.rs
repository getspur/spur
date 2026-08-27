//! Immutable benchmark-source manifests and deterministic byte validation.

use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Cursor},
    path::Path,
};

use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tar::Archive;
use thiserror::Error;
use xz2::read::XzDecoder;

use crate::Suite;

/// Version understood by [`SourceManifest`].
pub const SOURCE_MANIFEST_VERSION: u32 = 1;

/// Adapter contract required by every source in the manifest.
pub const SOURCE_ADAPTER_CONTRACT_VERSION: &str = "code-eval-source-v1";

/// Fatal source-manifest or source-byte validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SourceError {
    /// TOML did not satisfy the source-manifest schema.
    #[error("invalid source manifest TOML: {message}")]
    ManifestParse {
        /// Parser diagnostic.
        message: String,
    },
    /// The manifest uses an unsupported schema version.
    #[error("unsupported source manifest version {actual}; expected {expected}")]
    UnsupportedManifestVersion {
        /// Version accepted by this crate.
        expected: u32,
        /// Version found in the manifest.
        actual: u32,
    },
    /// The manifest contains no benchmark sources.
    #[error("source manifest must contain at least one source")]
    EmptyManifest,
    /// A required field is empty.
    #[error("source {source_index} field {field} must not be empty")]
    EmptyField {
        /// Zero-based source position.
        source_index: usize,
        /// Stable field path.
        field: &'static str,
    },
    /// A source lacks redistribution metadata.
    #[error("source {source_index} must declare at least one license")]
    MissingLicenseMetadata {
        /// Zero-based source position.
        source_index: usize,
    },
    /// A revision is not a complete Git object identifier.
    #[error("source {source_index} revision must be an immutable Git object ID, got {revision:?}")]
    MutableRevision {
        /// Zero-based source position.
        source_index: usize,
        /// Rejected revision.
        revision: String,
    },
    /// A SHA-256 pin is not canonical lowercase hexadecimal.
    #[error("source {source_index} has invalid SHA-256 metadata {sha256:?}")]
    InvalidSha256 {
        /// Zero-based source position.
        source_index: usize,
        /// Rejected hash.
        sha256: String,
    },
    /// Two entries identify the same immutable source bytes.
    #[error("duplicate source identity {uri}@{revision}")]
    DuplicateSourceIdentity {
        /// Shared source URI.
        uri: String,
        /// Shared immutable revision.
        revision: String,
    },
    /// A source requires an adapter contract this crate cannot interpret.
    #[error("source {source_index} uses adapter contract {actual:?}; expected {expected:?}")]
    IncompatibleAdapterContractVersion {
        /// Zero-based source position.
        source_index: usize,
        /// Version accepted by this crate.
        expected: &'static str,
        /// Version found in the manifest.
        actual: String,
    },
    /// A source has no denominator-visible language capabilities.
    #[error("source {source_index} must declare at least one language capability")]
    MissingLanguageCapabilities {
        /// Zero-based source position.
        source_index: usize,
    },
    /// A source repeats one language capability.
    #[error("source {source_index} repeats language capability {language:?}")]
    DuplicateLanguageCapability {
        /// Zero-based source position.
        source_index: usize,
        /// Repeated language label.
        language: String,
    },
    /// An unsupported language lacks a denominator-visible reason.
    #[error("source {source_index} unsupported language {language:?} must include a reason")]
    MissingUnsupportedReason {
        /// Zero-based source position.
        source_index: usize,
        /// Language label.
        language: String,
    },
    /// Per-language counts do not cover the complete source denominator.
    #[error(
        "source {source_index} language counts total {actual}, but expected record count is {expected}"
    )]
    LanguageRecordCountMismatch {
        /// Zero-based source position.
        source_index: usize,
        /// Source-level expected count.
        expected: u64,
        /// Sum of language capability counts.
        actual: u64,
    },
    /// No external-index schema evidence was recorded.
    #[error("source {source_index} must record external schema evidence")]
    MissingSchemaEvidence {
        /// Zero-based source position.
        source_index: usize,
    },
    /// The supplied bytes do not match the immutable source hash.
    #[error("source hash mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        /// Manifest SHA-256.
        expected: String,
        /// SHA-256 of the supplied bytes.
        actual: String,
    },
    /// The supplied bytes do not match their declared encoding/schema.
    #[error("invalid source bytes: {message}")]
    InvalidSourceBytes {
        /// Deterministic parser diagnostic.
        message: String,
    },
    /// The source schema produced a different record count.
    #[error("source record count mismatch: expected {expected}, got {actual}")]
    RecordCountMismatch {
        /// Manifest count.
        expected: u64,
        /// Count derived from supplied bytes.
        actual: u64,
    },
    /// A record count exceeded the supported integer range.
    #[error("source record count overflowed u64")]
    RecordCountOverflow,
}

/// Complete versioned collection of benchmark sources.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifest {
    manifest_version: u32,
    sources: Vec<SourceSpec>,
}

impl SourceManifest {
    /// Parses TOML and validates all metadata without accessing the network.
    ///
    /// # Examples
    ///
    /// ```
    /// use spur_code_eval::SourceManifest;
    ///
    /// let manifest = SourceManifest::from_toml(include_str!(
    ///     "../benchmarks/code_eval.toml"
    /// ))?;
    /// assert_eq!(manifest.sources().len(), 3);
    /// # Ok::<(), spur_code_eval::SourceError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a typed [`SourceError`] for malformed TOML or invalid source
    /// metadata.
    pub fn from_toml(input: &str) -> Result<Self, SourceError> {
        let manifest: Self = toml::from_str(input).map_err(|error| SourceError::ManifestParse {
            message: error.to_string(),
        })?;
        manifest.validate_metadata()?;
        Ok(manifest)
    }

    /// Returns the manifest schema version.
    #[must_use]
    pub const fn manifest_version(&self) -> u32 {
        self.manifest_version
    }

    /// Returns sources in their declared deterministic order.
    #[must_use]
    pub fn sources(&self) -> &[SourceSpec] {
        &self.sources
    }

    fn validate_metadata(&self) -> Result<(), SourceError> {
        if self.manifest_version != SOURCE_MANIFEST_VERSION {
            return Err(SourceError::UnsupportedManifestVersion {
                expected: SOURCE_MANIFEST_VERSION,
                actual: self.manifest_version,
            });
        }
        if self.sources.is_empty() {
            return Err(SourceError::EmptyManifest);
        }

        let mut identities = BTreeSet::new();
        for (source_index, source) in self.sources.iter().enumerate() {
            source.validate_metadata(source_index)?;
            let identity = (source.uri.clone(), source.revision.clone());
            if !identities.insert(identity) {
                return Err(SourceError::DuplicateSourceIdentity {
                    uri: source.uri.clone(),
                    revision: source.revision.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Encoding and record-count schema of immutable source bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    /// One JSON value per non-empty UTF-8 line.
    JsonLines,
    /// `RepoQA` gzip-compressed nested JSON with records in repository `needles`.
    GzipRepoQaJson,
    /// `CrossCodeEval` XZ/TAR with canonical `line_completion.jsonl` members.
    TarXzCrossCodeEval,
    /// JCG gzip/TAR with one `##` testcase section per record.
    TarGzipJcgMarkdown,
}

/// One immutable upstream benchmark source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpec {
    suite: Suite,
    uri: String,
    revision: String,
    sha256: String,
    licenses: Vec<String>,
    expected_record_count: u64,
    format: SourceFormat,
    adapter_contract_version: String,
    languages: Vec<LanguageCapability>,
    evidence: Vec<SchemaEvidence>,
}

impl SourceSpec {
    /// Returns the benchmark suite.
    #[must_use]
    pub const fn suite(&self) -> Suite {
        self.suite
    }

    /// Returns the immutable source URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Returns the complete upstream Git object identifier.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Returns the lowercase SHA-256 source pin.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns the upstream license identifiers.
    #[must_use]
    pub fn licenses(&self) -> &[String] {
        &self.licenses
    }

    /// Returns the expected number of upstream records.
    #[must_use]
    pub const fn expected_record_count(&self) -> u64 {
        self.expected_record_count
    }

    /// Returns the deterministic source-byte encoding and count schema.
    #[must_use]
    pub const fn format(&self) -> SourceFormat {
        self.format
    }

    /// Returns the adapter contract required to interpret this source.
    #[must_use]
    pub fn adapter_contract_version(&self) -> &str {
        &self.adapter_contract_version
    }

    /// Returns every source language, including unsupported capabilities.
    #[must_use]
    pub fn languages(&self) -> &[LanguageCapability] {
        &self.languages
    }

    /// Returns the external-index evidence behind schema/count assumptions.
    #[must_use]
    pub fn evidence(&self) -> &[SchemaEvidence] {
        &self.evidence
    }

    fn validate_metadata(&self, source_index: usize) -> Result<(), SourceError> {
        require_non_empty(source_index, "uri", &self.uri)?;
        if !is_complete_git_object_id(&self.revision) {
            return Err(SourceError::MutableRevision {
                source_index,
                revision: self.revision.clone(),
            });
        }
        if !is_canonical_sha256(&self.sha256) {
            return Err(SourceError::InvalidSha256 {
                source_index,
                sha256: self.sha256.clone(),
            });
        }
        if self.licenses.is_empty()
            || self
                .licenses
                .iter()
                .any(|license| license.trim().is_empty())
        {
            return Err(SourceError::MissingLicenseMetadata { source_index });
        }
        if self.adapter_contract_version != SOURCE_ADAPTER_CONTRACT_VERSION {
            return Err(SourceError::IncompatibleAdapterContractVersion {
                source_index,
                expected: SOURCE_ADAPTER_CONTRACT_VERSION,
                actual: self.adapter_contract_version.clone(),
            });
        }
        self.validate_languages(source_index)?;
        self.validate_evidence(source_index)
    }

    fn validate_languages(&self, source_index: usize) -> Result<(), SourceError> {
        if self.languages.is_empty() {
            return Err(SourceError::MissingLanguageCapabilities { source_index });
        }
        let mut languages = BTreeSet::new();
        let mut total = 0_u64;
        for capability in &self.languages {
            require_non_empty(source_index, "languages.language", &capability.language)?;
            if !languages.insert(capability.language.clone()) {
                return Err(SourceError::DuplicateLanguageCapability {
                    source_index,
                    language: capability.language.clone(),
                });
            }
            if !capability.supported
                && capability
                    .reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(SourceError::MissingUnsupportedReason {
                    source_index,
                    language: capability.language.clone(),
                });
            }
            total = total
                .checked_add(capability.expected_record_count)
                .ok_or(SourceError::RecordCountOverflow)?;
        }
        if total != self.expected_record_count {
            return Err(SourceError::LanguageRecordCountMismatch {
                source_index,
                expected: self.expected_record_count,
                actual: total,
            });
        }
        Ok(())
    }

    fn validate_evidence(&self, source_index: usize) -> Result<(), SourceError> {
        if self.evidence.is_empty() {
            return Err(SourceError::MissingSchemaEvidence { source_index });
        }
        for evidence in &self.evidence {
            require_non_empty(
                source_index,
                "evidence.external_package",
                &evidence.external_package,
            )?;
            require_non_empty(
                source_index,
                "evidence.external_source",
                &evidence.external_source,
            )?;
            if let Some(index_job_id) = &evidence.index_job_id {
                require_non_empty(source_index, "evidence.index_job_id", index_job_id)?;
            }
            require_non_empty(source_index, "evidence.observation", &evidence.observation)?;
            if !is_complete_git_object_id(&evidence.revision) {
                return Err(SourceError::MutableRevision {
                    source_index,
                    revision: evidence.revision.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Denominator-visible support status for one upstream language.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageCapability {
    language: String,
    supported: bool,
    reason: Option<String>,
    expected_record_count: u64,
}

impl LanguageCapability {
    /// Returns the upstream language label.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns whether this crate can evaluate the language.
    #[must_use]
    pub const fn supported(&self) -> bool {
        self.supported
    }

    /// Returns the denominator-visible unsupported reason.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Returns records attributed to this language.
    #[must_use]
    pub const fn expected_record_count(&self) -> u64 {
        self.expected_record_count
    }
}

/// External-index/code evidence used to establish a source schema.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaEvidence {
    external_package: String,
    external_source: String,
    revision: String,
    index_job_id: Option<String>,
    selectors: Vec<String>,
    observation: String,
}

impl SchemaEvidence {
    /// Returns the indexed package name.
    #[must_use]
    pub fn external_package(&self) -> &str {
        &self.external_package
    }

    /// Returns the external catalog source namespace.
    #[must_use]
    pub fn external_source(&self) -> &str {
        &self.external_source
    }

    /// Returns the exact indexed revision.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Returns the completed external-index job identifier.
    #[must_use]
    pub fn index_job_id(&self) -> Option<&str> {
        self.index_job_id.as_deref()
    }

    /// Returns selectors read through the external code tools.
    #[must_use]
    pub fn selectors(&self) -> &[String] {
        &self.selectors
    }

    /// Returns the observed schema/count evidence.
    #[must_use]
    pub fn observation(&self) -> &str {
        &self.observation
    }
}

/// Successful deterministic validation of supplied source bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSource {
    suite: Suite,
    sha256: String,
    record_count: u64,
}

impl ValidatedSource {
    /// Returns the validated suite.
    #[must_use]
    pub const fn suite(&self) -> Suite {
        self.suite
    }

    /// Returns the validated SHA-256.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns the validated record count.
    #[must_use]
    pub const fn record_count(&self) -> u64 {
        self.record_count
    }
}

/// Validates caller-supplied bytes against an immutable source specification.
///
/// This function performs no filesystem or network access. It checks the hash
/// before parsing so corrupted compressed bytes always fail as a hash mismatch.
///
/// # Errors
///
/// Returns [`SourceError::HashMismatch`] when bytes differ from the pin,
/// [`SourceError::InvalidSourceBytes`] when the declared schema cannot be
/// decoded, or [`SourceError::RecordCountMismatch`] when the decoded
/// denominator differs.
pub fn validate_bytes(source: &SourceSpec, bytes: &[u8]) -> Result<ValidatedSource, SourceError> {
    let actual_hash = format!("{:x}", Sha256::digest(bytes));
    if actual_hash != source.sha256 {
        return Err(SourceError::HashMismatch {
            expected: source.sha256.clone(),
            actual: actual_hash,
        });
    }

    let actual_count = match source.format {
        SourceFormat::JsonLines => count_json_lines(BufReader::new(Cursor::new(bytes)))?,
        SourceFormat::GzipRepoQaJson => count_repoqa(bytes)?,
        SourceFormat::TarXzCrossCodeEval => count_cross_code_eval(source, bytes)?,
        SourceFormat::TarGzipJcgMarkdown => count_jcg(source, bytes)?,
    };
    if actual_count != source.expected_record_count {
        return Err(SourceError::RecordCountMismatch {
            expected: source.expected_record_count,
            actual: actual_count,
        });
    }

    Ok(ValidatedSource {
        suite: source.suite,
        sha256: actual_hash,
        record_count: actual_count,
    })
}

fn count_repoqa(bytes: &[u8]) -> Result<u64, SourceError> {
    let dataset: Value =
        serde_json::from_reader(GzDecoder::new(bytes)).map_err(invalid_source_bytes)?;
    let languages = dataset
        .as_object()
        .ok_or_else(|| SourceError::InvalidSourceBytes {
            message: "RepoQA root must be a JSON object".to_owned(),
        })?;

    let mut count = 0_u64;
    for repositories in languages.values() {
        let repositories =
            repositories
                .as_array()
                .ok_or_else(|| SourceError::InvalidSourceBytes {
                    message: "RepoQA language value must be an array".to_owned(),
                })?;
        for repository in repositories {
            let needles = repository
                .get("needles")
                .and_then(Value::as_array)
                .ok_or_else(|| SourceError::InvalidSourceBytes {
                    message: "RepoQA repository must contain a needles array".to_owned(),
                })?;
            count = count
                .checked_add(u64::try_from(needles.len()).map_err(record_count_overflow)?)
                .ok_or(SourceError::RecordCountOverflow)?;
        }
    }
    Ok(count)
}

fn count_cross_code_eval(source: &SourceSpec, bytes: &[u8]) -> Result<u64, SourceError> {
    let languages: BTreeSet<&str> = source
        .languages
        .iter()
        .map(|capability| capability.language.as_str())
        .collect();
    let mut archive = Archive::new(XzDecoder::new(bytes));
    let entries = archive.entries().map_err(invalid_source_bytes)?;
    let mut matched_files = 0_u64;
    let mut count = 0_u64;

    for entry in entries {
        let entry = entry.map_err(invalid_source_bytes)?;
        let path = entry.path().map_err(invalid_source_bytes)?;
        if path.file_name().and_then(|name| name.to_str()) != Some("line_completion.jsonl") {
            continue;
        }
        let Some(language) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        if !languages.contains(language) {
            continue;
        }

        matched_files = matched_files
            .checked_add(1)
            .ok_or(SourceError::RecordCountOverflow)?;
        count = count
            .checked_add(count_json_lines(BufReader::new(entry))?)
            .ok_or(SourceError::RecordCountOverflow)?;
    }

    if matched_files == 0 {
        return Err(SourceError::InvalidSourceBytes {
            message: "CrossCodeEval archive has no canonical line_completion.jsonl members"
                .to_owned(),
        });
    }
    Ok(count)
}

fn count_jcg(source: &SourceSpec, bytes: &[u8]) -> Result<u64, SourceError> {
    let languages: BTreeSet<&str> = source
        .languages
        .iter()
        .map(|capability| match capability.language.as_str() {
            "javascript" => "js",
            language => language,
        })
        .collect();
    let mut archive = Archive::new(GzDecoder::new(bytes));
    let entries = archive.entries().map_err(invalid_source_bytes)?;
    let mut matched_files = 0_u64;
    let mut count = 0_u64;

    for entry in entries {
        let entry = entry.map_err(invalid_source_bytes)?;
        let path = entry.path().map_err(invalid_source_bytes)?;
        if !is_jcg_testcase_path(&path, &languages) {
            continue;
        }

        matched_files = matched_files
            .checked_add(1)
            .ok_or(SourceError::RecordCountOverflow)?;
        count = count
            .checked_add(count_markdown_testcases(BufReader::new(entry))?)
            .ok_or(SourceError::RecordCountOverflow)?;
    }

    if matched_files == 0 {
        return Err(SourceError::InvalidSourceBytes {
            message: "JCG archive has no language testcase Markdown members".to_owned(),
        });
    }
    Ok(count)
}

fn is_jcg_testcase_path(path: &Path, languages: &BTreeSet<&str>) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
        return false;
    }
    let components: Vec<&str> = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();
    let in_resources = components
        .windows(4)
        .any(|window| window == ["jcg_testcases", "src", "main", "resources"]);
    let in_language = components
        .windows(2)
        .any(|window| window[0] == "resources" && languages.contains(window[1]));
    in_resources && in_language
}

fn count_markdown_testcases<R: BufRead>(reader: R) -> Result<u64, SourceError> {
    let mut count = 0_u64;
    for line in reader.lines() {
        let line = line.map_err(invalid_source_bytes)?;
        if line
            .strip_prefix("## ")
            .is_some_and(|heading| !heading.trim().is_empty())
        {
            count = count
                .checked_add(1)
                .ok_or(SourceError::RecordCountOverflow)?;
        }
    }
    Ok(count)
}

fn count_json_lines<R: BufRead>(mut reader: R) -> Result<u64, SourceError> {
    let mut count = 0_u64;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(invalid_source_bytes)?;
        if read == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<Value>(&line).map_err(invalid_source_bytes)?;
        count = count
            .checked_add(1)
            .ok_or(SourceError::RecordCountOverflow)?;
    }
    Ok(count)
}

fn require_non_empty(
    source_index: usize,
    field: &'static str,
    value: &str,
) -> Result<(), SourceError> {
    if value.trim().is_empty() {
        Err(SourceError::EmptyField {
            source_index,
            field,
        })
    } else {
        Ok(())
    }
}

fn is_complete_git_object_id(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64) && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_source_bytes(error: impl std::fmt::Display) -> SourceError {
    SourceError::InvalidSourceBytes {
        message: error.to_string(),
    }
}

fn record_count_overflow(_error: std::num::TryFromIntError) -> SourceError {
    SourceError::RecordCountOverflow
}
