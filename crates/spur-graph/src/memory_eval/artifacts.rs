//! Immutable benchmark artifacts and publication-state contracts.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use super::metrics::{MetricValue, RetrievalMetrics};
use super::{
    contract::{ContractId, DatasetKind, SourcePin, ValidationReport},
    ranking::{Granularity, QueryOccurrenceId, Ranking, RankingSet, Variant},
};

const CHECKSUM_FILE: &str = "SHA256SUMS";
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The finite run states admitted by the Section-4 lifecycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Blocked,
    Validated,
    RetrievalComplete,
    PublishedRetrieval,
    QaPending,
    QaComplete,
    PublishedFull,
}

/// Events that can advance a validated run without rerunning retrieval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEvent {
    RetrievalComplete,
    PublishRetrieval,
    QaPending,
    QaComplete,
    PublishFull,
}

/// Retrieval evidence not already owned by [`ValidationReport`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetrievalGateEvidence {
    pub gold_leak_free: bool,
    pub denominators_valid: bool,
    pub metrics_finite: bool,
}

/// The exact Boolean inputs of `SECTION-4-RELEASE-GATE`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseGates {
    source_hashes_valid: bool,
    schema_valid: bool,
    ids_valid: bool,
    gold_leak_free: bool,
    denominators_valid: bool,
    metrics_finite: bool,
    retrieval_complete: bool,
    qa_complete: bool,
}

impl ReleaseGates {
    /// Bind source/schema/identity gates to fatal validation rather than
    /// allowing a caller to override a fatal report with hand-written flags.
    pub fn from_validation(validation: &ValidationReport, evidence: RetrievalGateEvidence) -> Self {
        let fatal_free = !validation.has_fatal();
        Self {
            source_hashes_valid: fatal_free,
            schema_valid: fatal_free,
            ids_valid: fatal_free,
            gold_leak_free: evidence.gold_leak_free,
            denominators_valid: evidence.denominators_valid,
            metrics_finite: evidence.metrics_finite,
            retrieval_complete: false,
            qa_complete: false,
        }
    }

    /// Evaluate the exact mutually exclusive Section-4 release branches.
    pub fn release_state(&self) -> RunState {
        if !self.retrieval_gates_pass() {
            RunState::Blocked
        } else if self.qa_complete {
            RunState::PublishedFull
        } else {
            RunState::PublishedRetrieval
        }
    }

    fn pre_retrieval_gates_pass(&self) -> bool {
        self.source_hashes_valid
            && self.schema_valid
            && self.ids_valid
            && self.gold_leak_free
            && self.denominators_valid
            && self.metrics_finite
    }

    fn retrieval_gates_pass(&self) -> bool {
        self.pre_retrieval_gates_pass() && self.retrieval_complete
    }
}

/// Durable QA progress. Eligibility is retained independently from labels so
/// a pending or failed API attempt cannot shrink the denominator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QaProgress {
    eligible_question_ids: Vec<String>,
    completed_question_ids: BTreeSet<String>,
}

impl QaProgress {
    pub fn new<I, S>(question_ids: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let eligible_question_ids = question_ids.into_iter().map(Into::into).collect::<Vec<_>>();
        ensure!(
            !eligible_question_ids.is_empty(),
            "QA denominator must be positive"
        );
        ensure!(
            eligible_question_ids.iter().all(|id| !id.is_empty()),
            "QA question IDs must not be empty"
        );
        ensure!(
            eligible_question_ids.iter().collect::<BTreeSet<_>>().len()
                == eligible_question_ids.len(),
            "QA question IDs must be unique"
        );
        Ok(Self {
            eligible_question_ids,
            completed_question_ids: BTreeSet::new(),
        })
    }

    pub fn denominator(&self) -> usize {
        self.eligible_question_ids.len()
    }

    pub fn completed_question_ids(&self) -> &BTreeSet<String> {
        &self.completed_question_ids
    }

    pub fn mark_completed(&mut self, question_id: &str) -> Result<()> {
        ensure!(
            self.contains(question_id),
            "QA question {question_id:?} is outside the retained denominator"
        );
        self.completed_question_ids.insert(question_id.to_owned());
        Ok(())
    }

    fn contains(&self, question_id: &str) -> bool {
        self.eligible_question_ids
            .iter()
            .any(|eligible| eligible == question_id)
    }

    fn is_complete(&self) -> bool {
        self.completed_question_ids.len() == self.eligible_question_ids.len()
    }

    fn validate(&self) -> Result<()> {
        ensure!(self.denominator() > 0, "QA denominator must be positive");
        ensure!(
            self.eligible_question_ids.iter().all(|id| !id.is_empty()),
            "QA question IDs must not be empty"
        );
        ensure!(
            self.eligible_question_ids
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                == self.denominator(),
            "QA question IDs must be unique"
        );
        ensure!(
            self.completed_question_ids
                .iter()
                .all(|question_id| self.contains(question_id)),
            "completed QA labels must remain inside the retained denominator"
        );
        Ok(())
    }
}

/// Reproducibility and lifecycle record for one benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunManifest {
    pub run_id: String,
    pub repository_revision: String,
    pub repository_dirty: bool,
    pub sources: Vec<SourcePin>,
    pub contract_id: ContractId,
    pub state: RunState,
    pub qa_state: Option<RunState>,
    pub ranking_hashes: BTreeMap<Variant, String>,
    pub variant_configuration: BTreeMap<Variant, Value>,
    pub deterministic_seeds: BTreeMap<String, u64>,
    pub model: Option<String>,
    pub prompt_hashes: BTreeMap<String, String>,
    pub timestamps: BTreeMap<String, String>,
    pub hardware: BTreeMap<String, String>,
    pub command: Vec<String>,
    pub gates: ReleaseGates,
    pub qa_progress: QaProgress,
}

impl RunManifest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: impl Into<String>,
        repository_revision: impl Into<String>,
        repository_dirty: bool,
        sources: Vec<SourcePin>,
        contract_id: ContractId,
        command: Vec<String>,
        gates: ReleaseGates,
        qa_progress: QaProgress,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            repository_revision: repository_revision.into(),
            repository_dirty,
            sources,
            contract_id,
            state: RunState::Validated,
            qa_state: None,
            ranking_hashes: BTreeMap::new(),
            variant_configuration: BTreeMap::new(),
            deterministic_seeds: BTreeMap::new(),
            model: None,
            prompt_hashes: BTreeMap::new(),
            timestamps: BTreeMap::new(),
            hardware: BTreeMap::new(),
            command,
            gates,
            qa_progress,
        }
    }

    pub fn transition(&mut self, event: RunEvent) -> Result<()> {
        match event {
            RunEvent::RetrievalComplete => {
                ensure!(
                    self.state == RunState::Validated,
                    "retrieval completion requires validated state"
                );
                ensure!(
                    self.gates.pre_retrieval_gates_pass(),
                    "retrieval completion requires every validation, leakage, denominator, and metric gate"
                );
                ensure!(
                    all_variants()
                        .iter()
                        .all(|variant| self.ranking_hashes.contains_key(variant)),
                    "retrieval completion requires every controlled ranking variant"
                );
                ensure!(
                    self.ranking_hashes.values().all(|hash| is_sha256_hex(hash)),
                    "retrieval completion requires valid ranking SHA-256 values"
                );
                self.gates.retrieval_complete = true;
                self.state = RunState::RetrievalComplete;
            }
            RunEvent::PublishRetrieval => {
                ensure!(
                    self.state == RunState::RetrievalComplete,
                    "retrieval publication requires retrieval_complete state"
                );
                ensure!(
                    self.gates.release_state() == RunState::PublishedRetrieval,
                    "retrieval publication requires every Section-4 retrieval gate"
                );
                self.state = RunState::PublishedRetrieval;
            }
            RunEvent::QaPending => {
                ensure!(
                    matches!(
                        self.state,
                        RunState::RetrievalComplete | RunState::PublishedRetrieval
                    ),
                    "qa_pending requires a complete or published retrieval"
                );
                ensure!(
                    self.gates.release_state() == RunState::PublishedRetrieval,
                    "qa_pending requires complete retrieval gates and incomplete QA"
                );
                ensure!(
                    !self.qa_progress.is_complete(),
                    "qa_pending requires at least one retained question without a label"
                );
                self.qa_state = Some(RunState::QaPending);
                if self.state == RunState::RetrievalComplete {
                    self.state = RunState::QaPending;
                }
            }
            RunEvent::QaComplete => {
                ensure!(
                    self.state == RunState::QaPending || self.qa_state == Some(RunState::QaPending),
                    "qa_complete requires qa_pending state"
                );
                ensure!(
                    self.qa_progress.is_complete(),
                    "qa_complete requires one terminal label for every retained denominator"
                );
                self.gates.qa_complete = true;
                self.qa_state = Some(RunState::QaComplete);
                if self.state == RunState::QaPending {
                    self.state = RunState::QaComplete;
                }
            }
            RunEvent::PublishFull => {
                ensure!(
                    self.qa_state == Some(RunState::QaComplete)
                        || self.state == RunState::QaComplete,
                    "published_full requires qa_complete"
                );
                ensure!(
                    self.gates.release_state() == RunState::PublishedFull,
                    "full publication requires every Section-4 gate plus complete QA"
                );
                self.state = RunState::PublishedFull;
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        ensure!(!self.run_id.is_empty(), "run ID must not be empty");
        ensure!(
            !self.repository_revision.is_empty(),
            "repository revision must not be empty"
        );
        ensure!(!self.sources.is_empty(), "manifest must pin a source");
        ensure!(
            self.sources.iter().all(|source| {
                !source.origin.is_empty()
                    && !source.revision.is_empty()
                    && is_sha256_hex(&source.sha256)
            }),
            "every source pin requires origin, revision, and lowercase SHA-256"
        );
        ensure!(
            !self.command.is_empty(),
            "manifest command must not be empty"
        );
        ensure!(
            self.ranking_hashes.values().all(|hash| is_sha256_hex(hash)),
            "recorded ranking hashes must be lowercase SHA-256"
        );
        ensure!(
            self.prompt_hashes.values().all(|hash| is_sha256_hex(hash)),
            "recorded prompt hashes must be lowercase SHA-256"
        );
        self.qa_progress.validate()?;

        match self.state {
            RunState::Blocked => ensure!(
                self.gates.release_state() == RunState::Blocked,
                "blocked state conflicts with passing release gates"
            ),
            RunState::RetrievalComplete | RunState::QaPending => ensure!(
                self.gates.retrieval_gates_pass() && !self.gates.qa_complete,
                "retrieval/qa_pending state conflicts with Section-4 gates"
            ),
            RunState::PublishedRetrieval => ensure!(
                matches!(
                    self.gates.release_state(),
                    RunState::PublishedRetrieval | RunState::PublishedFull
                ),
                "published_retrieval conflicts with Section-4 gates"
            ),
            RunState::QaComplete | RunState::PublishedFull => ensure!(
                self.gates.release_state() == RunState::PublishedFull
                    && self.qa_progress.is_complete(),
                "full/qa_complete state conflicts with Section-4 gates"
            ),
            RunState::Validated => {}
        }
        Ok(())
    }
}

/// The four resumable QA cache namespaces in the approved layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QaArtifactKind {
    Prompt,
    Hypothesis,
    JudgeInput,
    Label,
}

impl QaArtifactKind {
    fn directory(self) -> &'static str {
        match self {
            Self::Prompt => "qa/prompts",
            Self::Hypothesis => "qa/hypotheses",
            Self::JudgeInput => "qa/judge-inputs",
            Self::Label => "qa/labels",
        }
    }
}

/// The path and digest committed by one successful atomic write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDigest {
    pub relative_path: PathBuf,
    pub sha256: String,
}

/// Atomic writer for one `results/<run-id>/` directory.
#[derive(Debug, Clone)]
pub struct ArtifactWriter {
    root: PathBuf,
}

impl ArtifactWriter {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)
            .with_context(|| format!("create artifact root {}", root.display()))?;
        for directory in [
            "rankings",
            "metrics",
            "qa/prompts",
            "qa/hypotheses",
            "qa/judge-inputs",
            "qa/labels",
        ] {
            fs::create_dir_all(root.join(directory))
                .with_context(|| format!("create artifact directory {directory}"))?;
        }
        Ok(Self { root })
    }

    pub fn write_manifest(&self, manifest: &RunManifest) -> Result<ArtifactDigest> {
        manifest.validate()?;
        for (variant, recorded_hash) in &manifest.ranking_hashes {
            let relative = ranking_path(*variant);
            let path = self.root.join(&relative);
            ensure!(
                path.is_file(),
                "manifest records missing ranking {}",
                relative.display()
            );
            ensure!(
                sha256_file(&path)? == *recorded_hash,
                "manifest ranking hash does not match {}",
                relative.display()
            );
        }
        self.write_json(Path::new("manifest.json"), manifest)
    }

    pub fn write_validation(&self, validation: &ValidationReport) -> Result<ArtifactDigest> {
        self.write_json(Path::new("validation.json"), validation)
    }

    pub fn write_rankings(
        &self,
        manifest: &mut RunManifest,
        variant: Variant,
        rankings: &RankingSet,
    ) -> Result<ArtifactDigest> {
        let mut bytes = Vec::new();
        let mut count = 0usize;
        for ((question_id, key_variant, key_granularity), ranking) in rankings {
            if *key_variant != variant {
                continue;
            }
            validate_ranking(*key_variant, *key_granularity, ranking)?;
            serde_json::to_writer(
                &mut bytes,
                &PersistedRanking {
                    question_id,
                    ranking,
                },
            )?;
            bytes.push(b'\n');
            count += 1;
        }
        ensure!(count > 0, "ranking artifact must contain a record");

        let relative = ranking_path(variant);
        let sha256 = sha256_bytes(&bytes);
        if let Some(recorded) = manifest.ranking_hashes.get(&variant) {
            ensure!(
                recorded == &sha256,
                "immutable ranking hash for {} cannot change",
                relative.display()
            );
        }
        if let Some(recorded) = self.recorded_checksum(&relative)? {
            ensure!(
                recorded == sha256,
                "immutable ranking hash for {} cannot change",
                relative.display()
            );
        }

        let target = self.root.join(&relative);
        if target.is_file() && sha256_file(&target)? == sha256 {
            manifest.ranking_hashes.insert(variant, sha256.clone());
            return Ok(ArtifactDigest {
                relative_path: relative,
                sha256,
            });
        }

        let digest = self.write_atomic(&relative, &bytes)?;
        manifest
            .ranking_hashes
            .insert(variant, digest.sha256.clone());
        Ok(digest)
    }

    /// Write one file per dataset/granularity, with every variant bound to the
    /// exact immutable ranking bytes recorded in the manifest.
    pub fn write_metrics(
        &self,
        manifest: &RunManifest,
        metrics: &[RetrievalMetrics],
    ) -> Result<Vec<ArtifactDigest>> {
        ensure!(!metrics.is_empty(), "metric artifact must not be empty");
        let mut groups = BTreeMap::<PathBuf, Vec<&RetrievalMetrics>>::new();
        for metric in metrics {
            groups
                .entry(metric_path(metric.dataset, metric.granularity))
                .or_default()
                .push(metric);
        }

        groups
            .into_iter()
            .map(|(relative, mut group)| {
                group.sort_by_key(|metric| metric.variant);
                let first = group[0];
                let mut variants = BTreeSet::new();
                let persisted = group
                    .into_iter()
                    .map(|metric| {
                        ensure!(
                            metric.dataset == first.dataset
                                && metric.granularity == first.granularity,
                            "metric file cannot mix datasets or granularities"
                        );
                        ensure!(
                            variants.insert(metric.variant),
                            "metric file contains duplicate variant {:?}",
                            metric.variant
                        );
                        let source_ranking_hash = manifest
                            .ranking_hashes
                            .get(&metric.variant)
                            .with_context(|| {
                                format!(
                                    "metric variant {:?} has no recorded ranking hash",
                                    metric.variant
                                )
                            })?;
                        Ok(PersistedMetrics {
                            source_ranking_hash,
                            metrics: metric,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                self.write_json(
                    &relative,
                    &MetricFile {
                        dataset: first.dataset,
                        granularity: first.granularity,
                        variants: persisted,
                    },
                )
            })
            .collect()
    }

    pub fn write_qa_json<T: Serialize>(
        &self,
        kind: QaArtifactKind,
        question_id: &str,
        value: &T,
    ) -> Result<ArtifactDigest> {
        ensure!(
            kind != QaArtifactKind::Label,
            "use write_qa_label for labels"
        );
        ensure!(!question_id.is_empty(), "QA question ID must not be empty");
        self.write_json(&qa_path(kind, question_id), value)
    }

    /// Persist a terminal label once, then advance only that retained question.
    /// A repeated identical write is idempotent; a changed label is rejected.
    pub fn write_qa_label<T: Serialize>(
        &self,
        manifest: &mut RunManifest,
        question_id: &str,
        label: &T,
    ) -> Result<ArtifactDigest> {
        ensure!(
            manifest.qa_progress.contains(question_id),
            "QA question {question_id:?} is outside the retained denominator"
        );
        let value = serde_json::to_value(label)?;
        ensure!(!value.is_null(), "terminal QA label must not be null");
        let bytes = serde_json::to_vec_pretty(&value)?;
        let relative = qa_path(QaArtifactKind::Label, question_id);
        let sha256 = sha256_bytes(&bytes);

        if let Some(recorded) = self.recorded_checksum(&relative)? {
            ensure!(
                recorded == sha256,
                "immutable QA label for {question_id:?} cannot change"
            );
        }
        let target = self.root.join(&relative);
        let digest = if target.is_file() {
            ensure!(
                sha256_file(&target)? == sha256,
                "immutable QA label for {question_id:?} cannot change"
            );
            ArtifactDigest {
                relative_path: relative,
                sha256,
            }
        } else {
            self.write_atomic(&relative, &bytes)?
        };
        manifest.qa_progress.mark_completed(question_id)?;
        Ok(digest)
    }

    pub fn write_report(&self, report: &str) -> Result<ArtifactDigest> {
        self.write_atomic(Path::new("report.md"), report.as_bytes())
    }

    /// Verify both digest values and coverage: every regular artifact file
    /// except `SHA256SUMS` itself must have exactly one checksum entry.
    pub fn verify_checksums(&self) -> Result<()> {
        let checksum_path = self.root.join(CHECKSUM_FILE);
        let contents = fs::read_to_string(&checksum_path)
            .with_context(|| format!("read {}", checksum_path.display()))?;
        let mut recorded = BTreeMap::new();
        for (line_index, line) in contents.lines().enumerate() {
            let (hash, relative) = line
                .split_once("  ")
                .with_context(|| format!("malformed SHA256SUMS line {}", line_index + 1))?;
            ensure!(
                is_sha256_hex(hash),
                "invalid checksum on line {}",
                line_index + 1
            );
            let relative = PathBuf::from(relative);
            validate_relative_path(&relative)?;
            ensure!(
                recorded.insert(relative.clone(), hash.to_owned()).is_none(),
                "duplicate checksum entry {}",
                relative.display()
            );
            let actual = sha256_file(&self.root.join(&relative))?;
            ensure!(
                actual == hash,
                "checksum mismatch for {}",
                relative.display()
            );
        }

        let actual_paths = collect_artifact_files(&self.root)?
            .into_iter()
            .map(|(relative, _)| relative)
            .collect::<BTreeSet<_>>();
        let recorded_paths = recorded.into_keys().collect::<BTreeSet<_>>();
        ensure!(
            actual_paths == recorded_paths,
            "SHA256SUMS does not cover every published file"
        );
        Ok(())
    }

    fn write_json<T: Serialize>(&self, relative: &Path, value: &T) -> Result<ArtifactDigest> {
        self.write_atomic(relative, &serde_json::to_vec_pretty(value)?)
    }

    fn write_atomic(&self, relative: &Path, bytes: &[u8]) -> Result<ArtifactDigest> {
        validate_relative_path(relative)?;
        replace_atomic(&self.root, relative, bytes)?;
        self.refresh_checksums()?;
        Ok(ArtifactDigest {
            relative_path: relative.to_path_buf(),
            sha256: sha256_bytes(bytes),
        })
    }

    fn refresh_checksums(&self) -> Result<()> {
        let mut contents = String::new();
        for (relative, path) in collect_artifact_files(&self.root)? {
            contents.push_str(&sha256_file(&path)?);
            contents.push_str("  ");
            contents.push_str(&path_to_slash_string(&relative)?);
            contents.push('\n');
        }
        replace_atomic(&self.root, Path::new(CHECKSUM_FILE), contents.as_bytes())
    }

    fn recorded_checksum(&self, relative: &Path) -> Result<Option<String>> {
        let checksum_path = self.root.join(CHECKSUM_FILE);
        if !checksum_path.is_file() {
            return Ok(None);
        }
        let wanted = path_to_slash_string(relative)?;
        for line in fs::read_to_string(&checksum_path)?.lines() {
            let (hash, path) = line
                .split_once("  ")
                .context("malformed SHA256SUMS entry")?;
            ensure!(is_sha256_hex(hash), "invalid SHA256SUMS digest");
            if path == wanted {
                return Ok(Some(hash.to_owned()));
            }
        }
        Ok(None)
    }
}

#[derive(Serialize)]
struct PersistedRanking<'a> {
    question_id: &'a QueryOccurrenceId,
    #[serde(flatten)]
    ranking: &'a Ranking,
}

#[derive(Serialize)]
struct MetricFile<'a> {
    dataset: DatasetKind,
    granularity: Granularity,
    variants: Vec<PersistedMetrics<'a>>,
}

#[derive(Serialize)]
struct PersistedMetrics<'a> {
    source_ranking_hash: &'a str,
    #[serde(flatten)]
    metrics: &'a RetrievalMetrics,
}

fn validate_ranking(
    key_variant: Variant,
    key_granularity: Granularity,
    ranking: &Ranking,
) -> Result<()> {
    ensure!(
        ranking.variant == key_variant && ranking.granularity == key_granularity,
        "ranking key disagrees with serialized variant or granularity"
    );
    ensure!(
        ranking.hits.len() <= ranking.k,
        "ranking contains more hits than its declared k"
    );
    ensure!(
        ranking.hits.iter().all(|hit| {
            !hit.occurrence_id.is_empty()
                && hit.score.is_finite()
                && hit.provenance_id.as_ref().is_none_or(|id| !id.is_empty())
        }),
        "ranking contains an invalid occurrence, provenance, or non-finite score"
    );
    ensure!(
        ranking
            .hits
            .iter()
            .map(|hit| hit.occurrence_id.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            == ranking.hits.len(),
        "ranking contains duplicate occurrence IDs"
    );
    ensure!(
        [
            &ranking.query_sha256,
            &ranking.corpus_sha256,
            &ranking.serialization_sha256,
        ]
        .into_iter()
        .all(|hash| is_sha256_hex(hash)),
        "ranking provenance hashes must be lowercase SHA-256"
    );
    Ok(())
}

fn replace_atomic(root: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    validate_relative_path(relative)?;
    let target = root.join(relative);
    let parent = target
        .parent()
        .context("artifact target must have a parent")?;
    fs::create_dir_all(parent)?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .context("artifact filename must be UTF-8")?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("create temporary artifact {}", temporary.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &target).with_context(|| {
            format!(
                "rename temporary artifact {} to {}",
                temporary.display(),
                target.display()
            )
        })?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();

    if result.is_err() && temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn collect_artifact_files(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                visit(root, &entry.path(), files)?;
            } else if file_type.is_file() {
                let path = entry.path();
                let relative = path.strip_prefix(root)?.to_path_buf();
                let file_name = relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("artifact filename must be UTF-8")?;
                if relative == Path::new(CHECKSUM_FILE) || file_name.contains(".tmp-") {
                    continue;
                }
                files.push((relative, path));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty(),
        "artifact path must not be empty"
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "artifact path must be a traversal-safe relative path"
    );
    Ok(())
}

fn path_to_slash_string(path: &Path) -> Result<String> {
    validate_relative_path(path)?;
    path.components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .map(str::to_owned)
                .context("artifact path must be UTF-8"),
            _ => unreachable!("validated normal path component"),
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

fn sha256_file(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open artifact {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            break;
        }
        let consumed = bytes.len();
        hasher.update(bytes);
        reader.consume(consumed);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ranking_path(variant: Variant) -> PathBuf {
    PathBuf::from(format!("rankings/{}.jsonl", variant_name(variant)))
}

fn metric_path(dataset: DatasetKind, granularity: Granularity) -> PathBuf {
    PathBuf::from(format!(
        "metrics/{}-{}.json",
        dataset_name(dataset),
        granularity_name(granularity)
    ))
}

fn qa_path(kind: QaArtifactKind, question_id: &str) -> PathBuf {
    PathBuf::from(kind.directory()).join(format!("{}.json", sha256_bytes(question_id.as_bytes())))
}

fn all_variants() -> [Variant; 5] {
    [
        Variant::Oracle,
        Variant::Recent,
        Variant::FlatBm25,
        Variant::GraphIndexOnly,
        Variant::GraphTraversal,
    ]
}

fn variant_name(variant: Variant) -> &'static str {
    match variant {
        Variant::Oracle => "oracle",
        Variant::Recent => "recent",
        Variant::FlatBm25 => "flat_bm25",
        Variant::GraphIndexOnly => "graph_index_only",
        Variant::GraphTraversal => "graph_traversal",
    }
}

fn dataset_name(dataset: DatasetKind) -> &'static str {
    match dataset {
        DatasetKind::Locomo => "locomo",
        DatasetKind::LongMemEval => "longmemeval",
    }
}

fn granularity_name(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Turn => "turn",
        Granularity::Session => "session",
    }
}
