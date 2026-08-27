//! Atomic, content-addressed artifacts for one benchmark run.
//!
//! Artifact writes are append-only. Retrieval inputs are writable only while a
//! run is [`RunPhase::Prepared`], deterministic metrics only while it is
//! [`RunPhase::Frozen`], and model records only while it is
//! [`RunPhase::DeterministicScored`]. Sealing a stage verifies its checksums,
//! makes its files read-only, and then advances the manifest.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const CHECKSUM_SCHEMA_VERSION: u32 = 1;
const PENDING_DIRECTORY: &str = ".pending";
static NEXT_SYSTEM_TEMP: AtomicU64 = AtomicU64::new(0);

/// The only legal lifecycle states for one benchmark run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Inputs may be written, but no deterministic input has been sealed.
    Prepared,
    /// Retrieval, context, validation, call-graph, and log inputs are sealed.
    Frozen,
    /// Deterministic metrics are sealed and advisory model records may be written.
    DeterministicScored,
    /// Model records are sealed; the run is terminal.
    ModelScored,
}

/// Stable logical artifact names and their canonical paths below a run root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Store-managed lifecycle manifest.
    Manifest,
    /// Pinned-source and denominator validation record.
    Validation,
    /// Frozen deterministic retrieval rankings.
    Rankings,
    /// Frozen deterministic model/retrieval contexts.
    Contexts,
    /// Frozen normalized call graphs.
    CallGraphs,
    /// Deterministic suite metrics.
    Metrics,
    /// Optional advisory model result records.
    ModelRecords,
    /// Reproducibility and execution logs.
    Logs,
    /// Store-managed content checksum index.
    Checksums,
}

impl ArtifactKind {
    /// Returns the unique canonical path below a run root.
    #[must_use]
    pub const fn relative_path(self) -> &'static str {
        match self {
            Self::Manifest => "manifest.json",
            Self::Validation => "validation.json",
            Self::Rankings => "rankings.jsonl",
            Self::Contexts => "contexts.jsonl",
            Self::CallGraphs => "call-graphs.jsonl",
            Self::Metrics => "metrics.json",
            Self::ModelRecords => "model-records.jsonl",
            Self::Logs => "run.log",
            Self::Checksums => "checksums.json",
        }
    }

    const fn pending_slug(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Validation => "validation",
            Self::Rankings => "rankings",
            Self::Contexts => "contexts",
            Self::CallGraphs => "call-graphs",
            Self::Metrics => "metrics",
            Self::ModelRecords => "model-records",
            Self::Logs => "logs",
            Self::Checksums => "checksums",
        }
    }

    fn from_pending_slug(value: &str) -> Option<Self> {
        match value {
            "manifest" => Some(Self::Manifest),
            "validation" => Some(Self::Validation),
            "rankings" => Some(Self::Rankings),
            "contexts" => Some(Self::Contexts),
            "call-graphs" => Some(Self::CallGraphs),
            "metrics" => Some(Self::Metrics),
            "model-records" => Some(Self::ModelRecords),
            "logs" => Some(Self::Logs),
            "checksums" => Some(Self::Checksums),
            _ => None,
        }
    }

    const fn is_store_managed(self) -> bool {
        matches!(self, Self::Manifest | Self::Checksums)
    }

    const fn writable_in(self, phase: RunPhase) -> bool {
        match self {
            Self::Validation | Self::Rankings | Self::Contexts | Self::CallGraphs | Self::Logs => {
                matches!(phase, RunPhase::Prepared)
            }
            Self::Metrics => matches!(phase, RunPhase::Frozen),
            Self::ModelRecords => matches!(phase, RunPhase::DeterministicScored),
            Self::Manifest | Self::Checksums => false,
        }
    }

    const fn sealed_by(self, phase: RunPhase) -> bool {
        match self {
            Self::Validation | Self::Rankings | Self::Contexts | Self::CallGraphs | Self::Logs => {
                !matches!(phase, RunPhase::Prepared)
            }
            Self::Metrics => matches!(phase, RunPhase::DeterministicScored | RunPhase::ModelScored),
            Self::ModelRecords => matches!(phase, RunPhase::ModelScored),
            Self::Manifest | Self::Checksums => false,
        }
    }
}

/// Content identity retained for one canonical artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    relative_path: String,
    sha256: String,
    bytes: u64,
    frozen: bool,
}

impl ArtifactRecord {
    fn new(kind: ArtifactKind, sha256: String, bytes: u64) -> Self {
        Self {
            relative_path: kind.relative_path().to_owned(),
            sha256,
            bytes,
            frozen: false,
        }
    }

    /// Returns the canonical relative path.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Returns the lowercase SHA-256 digest.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Returns the explicit content address.
    #[must_use]
    pub fn content_address(&self) -> String {
        format!("sha256:{}", self.sha256)
    }

    /// Returns the byte length recorded with the digest.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns whether the owning lifecycle stage sealed this file.
    #[must_use]
    pub const fn is_frozen(&self) -> bool {
        self.frozen
    }
}

/// Persisted lifecycle and content-address record for one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    schema_version: u32,
    run_id: String,
    phase: RunPhase,
    artifacts: BTreeMap<ArtifactKind, ArtifactRecord>,
}

impl RunManifest {
    /// Creates a prepared manifest with a stable, non-empty run identity.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::InvalidRunId`] for whitespace-only identities
    /// or identities containing path separators or control characters.
    pub fn new(run_id: impl Into<String>) -> Result<Self, ArtifactError> {
        let run_id = run_id.into();
        if run_id.trim().is_empty()
            || run_id
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        {
            return Err(ArtifactError::InvalidRunId { run_id });
        }
        Ok(Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            run_id,
            phase: RunPhase::Prepared,
            artifacts: BTreeMap::new(),
        })
    }

    /// Returns the stable run identity.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Returns the current lifecycle phase.
    #[must_use]
    pub const fn phase(&self) -> RunPhase {
        self.phase
    }

    /// Returns the content record for a logical artifact.
    #[must_use]
    pub fn artifact(&self, kind: ArtifactKind) -> Option<&ArtifactRecord> {
        self.artifacts.get(&kind)
    }

    /// Iterates over canonical artifact records in stable kind order.
    pub fn artifacts(&self) -> impl Iterator<Item = (ArtifactKind, &ArtifactRecord)> {
        self.artifacts.iter().map(|(kind, record)| (*kind, record))
    }

    fn validate_initial(&self, path: &Path) -> Result<(), ArtifactError> {
        if !matches!(self.phase, RunPhase::Prepared) || !self.artifacts.is_empty() {
            return Err(invalid_manifest(
                path,
                format!(
                    "new artifact store requires phase Prepared and an empty artifact index; \
                     got phase {:?} with {} artifact(s)",
                    self.phase,
                    self.artifacts.len()
                ),
            ));
        }
        Ok(())
    }

    fn validate(&self, path: &Path) -> Result<(), ArtifactError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(invalid_manifest(
                path,
                format!(
                    "schema version {} is unsupported; expected {MANIFEST_SCHEMA_VERSION}",
                    self.schema_version
                ),
            ));
        }
        if self.run_id.trim().is_empty()
            || self
                .run_id
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        {
            return Err(invalid_manifest(path, "run_id is not a safe identity"));
        }
        for (kind, record) in &self.artifacts {
            if kind.is_store_managed() {
                return Err(invalid_manifest(
                    path,
                    format!("store-managed artifact {kind:?} cannot be in the content index"),
                ));
            }
            if record.relative_path != kind.relative_path() {
                return Err(invalid_manifest(
                    path,
                    format!(
                        "artifact {kind:?} records noncanonical path {:?}",
                        record.relative_path
                    ),
                ));
            }
            if !is_sha256(&record.sha256) {
                return Err(invalid_manifest(
                    path,
                    format!("artifact {kind:?} has an invalid SHA-256 digest"),
                ));
            }
            if record.frozen != kind.sealed_by(self.phase) {
                return Err(invalid_manifest(
                    path,
                    format!(
                        "artifact {kind:?} frozen={} disagrees with phase {:?}",
                        record.frozen, self.phase
                    ),
                ));
            }
        }
        if matches!(
            self.phase,
            RunPhase::DeterministicScored | RunPhase::ModelScored
        ) && !self.artifacts.contains_key(&ArtifactKind::Metrics)
        {
            return Err(invalid_manifest(
                path,
                "deterministic-scored phase requires metrics.json",
            ));
        }
        if matches!(self.phase, RunPhase::ModelScored)
            && !self.artifacts.contains_key(&ArtifactKind::ModelRecords)
        {
            return Err(invalid_manifest(
                path,
                "model-scored phase requires model-records.jsonl",
            ));
        }
        Ok(())
    }
}

/// Typed filesystem, integrity, recovery, and lifecycle failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ArtifactError {
    /// A run identity cannot be represented safely in a manifest.
    #[error("run identity must be non-empty and contain no separators or controls: {run_id:?}")]
    InvalidRunId {
        /// Rejected identity.
        run_id: String,
    },
    /// A run root already contains entries and cannot be initialized over them.
    #[error("artifact root is already initialized or non-empty: {root}")]
    AlreadyInitialized {
        /// Rejected root.
        root: PathBuf,
    },
    /// A filesystem operation failed at an explicit path.
    #[error("cannot {operation} at {path}: {message}")]
    Filesystem {
        /// Stable operation label.
        operation: &'static str,
        /// Path involved in the operation.
        path: PathBuf,
        /// Operating-system error text.
        message: String,
    },
    /// Persisted manifest JSON or an invariant was invalid.
    #[error("invalid artifact manifest at {path}: {message}")]
    InvalidManifest {
        /// Manifest path.
        path: PathBuf,
        /// Decode or invariant diagnostic.
        message: String,
    },
    /// Persisted checksum JSON or an invariant was invalid.
    #[error("invalid artifact checksum index at {path}: {message}")]
    InvalidChecksums {
        /// Checksum path.
        path: PathBuf,
        /// Decode or invariant diagnostic.
        message: String,
    },
    /// Callers attempted to write a store-managed file directly.
    #[error("artifact {kind:?} is managed by ArtifactStore")]
    ManagedArtifact {
        /// Rejected logical artifact.
        kind: ArtifactKind,
    },
    /// The artifact is not writable in the current lifecycle phase.
    #[error("artifact {kind:?} cannot be mutated in phase {phase:?}")]
    MutationForbidden {
        /// Rejected logical artifact.
        kind: ArtifactKind,
        /// Phase checked before touching the filesystem.
        phase: RunPhase,
    },
    /// A lifecycle edge is absent from the solved transition relation.
    #[error("artifact lifecycle transition {from:?} -> {to:?} is not allowed")]
    InvalidTransition {
        /// Current phase.
        from: RunPhase,
        /// Requested phase.
        to: RunPhase,
    },
    /// Append-only publication found an existing canonical artifact.
    #[error("artifact {kind:?} already exists at {path}")]
    ArtifactAlreadyExists {
        /// Logical artifact.
        kind: ArtifactKind,
        /// Existing canonical path.
        path: PathBuf,
    },
    /// A requested content record or file is absent.
    #[error("artifact {kind:?} is missing at {path}")]
    ArtifactMissing {
        /// Logical artifact.
        kind: ArtifactKind,
        /// Expected canonical path.
        path: PathBuf,
    },
    /// A lifecycle transition requires an output that has not been written.
    #[error("transition to {phase:?} requires artifact {kind:?}")]
    RequiredArtifactMissing {
        /// Target phase.
        phase: RunPhase,
        /// Required output.
        kind: ArtifactKind,
    },
    /// A file differs from its content address.
    #[error("checksum mismatch for {kind:?}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Logical artifact.
        kind: ArtifactKind,
        /// Manifest digest.
        expected: String,
        /// Digest computed while opening.
        actual: String,
    },
    /// A file is not sealed for verified reopening.
    #[error("artifact {kind:?} is not frozen in phase {phase:?}")]
    ArtifactNotFrozen {
        /// Logical artifact.
        kind: ArtifactKind,
        /// Current phase.
        phase: RunPhase,
    },
    /// A sealed file's filesystem permissions are writable.
    #[error("frozen artifact {kind:?} is not read-only at {path}")]
    ArtifactNotReadOnly {
        /// Logical artifact.
        kind: ArtifactKind,
        /// Canonical path.
        path: PathBuf,
    },
    /// A caller supplied a malformed content digest.
    #[error("invalid lowercase SHA-256 digest: {digest:?}")]
    InvalidDigest {
        /// Rejected digest.
        digest: String,
    },
    /// A pending recovery filename does not encode one canonical kind and digest.
    #[error("pending artifact path is not canonical: {path}")]
    RecoveryPathMismatch {
        /// Rejected pending path.
        path: PathBuf,
    },
    /// Pending bytes differ from the digest encoded in their path.
    #[error("pending {kind:?} checksum mismatch at {path}: expected {expected}, got {actual}")]
    RecoveryChecksumMismatch {
        /// Logical artifact encoded in the path.
        kind: ArtifactKind,
        /// Pending path.
        path: PathBuf,
        /// Digest encoded in the path.
        expected: String,
        /// Digest computed from available bytes.
        actual: String,
    },
    /// Complete pending bytes conflict with a prior canonical artifact or record.
    #[error("pending {kind:?} at {pending} conflicts with prior artifact {prior}")]
    RecoveryConflict {
        /// Logical artifact.
        kind: ArtifactKind,
        /// Complete pending path.
        pending: PathBuf,
        /// Prior canonical path.
        prior: PathBuf,
    },
    /// A pending artifact exists and must be recovered before another write.
    #[error("pending artifact recovery is required before writing: {path}")]
    PendingRecoveryRequired {
        /// First pending entry.
        path: PathBuf,
    },
}

/// Atomic artifact writer and verified read boundary for one run root.
#[derive(Debug)]
pub struct ArtifactStore {
    root: PathBuf,
    manifest: RunManifest,
}

impl ArtifactStore {
    /// Initializes an empty run root from a prepared manifest without artifacts.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the root is non-empty or cannot be created,
    /// canonicalized, synchronized, or initialized atomically.
    pub fn create(root: impl AsRef<Path>, manifest: RunManifest) -> Result<Self, ArtifactError> {
        let requested_root = root.as_ref();
        let requested_manifest_path = requested_root.join(ArtifactKind::Manifest.relative_path());
        manifest.validate_initial(&requested_manifest_path)?;
        if requested_root.exists()
            && fs::read_dir(requested_root)
                .map_err(|error| filesystem("read directory", requested_root, &error))?
                .next()
                .is_some()
        {
            return Err(ArtifactError::AlreadyInitialized {
                root: requested_root.to_path_buf(),
            });
        }
        fs::create_dir_all(requested_root)
            .map_err(|error| filesystem("create directory", requested_root, &error))?;
        let root = fs::canonicalize(requested_root)
            .map_err(|error| filesystem("canonicalize directory", requested_root, &error))?;
        let manifest_path = root.join(ArtifactKind::Manifest.relative_path());
        manifest.validate(&manifest_path)?;
        let pending = root.join(PENDING_DIRECTORY);
        fs::create_dir(&pending)
            .map_err(|error| filesystem("create pending directory", &pending, &error))?;
        let store = Self { root, manifest };
        store.persist_state()?;
        Ok(store)
    }

    /// Opens an existing run, recovers complete pending artifacts, and verifies
    /// manifest/checksum agreement.
    ///
    /// # Errors
    ///
    /// Returns a typed decoding, recovery, filesystem, or integrity error.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ArtifactError> {
        let requested_root = root.as_ref();
        let root = fs::canonicalize(requested_root)
            .map_err(|error| filesystem("canonicalize directory", requested_root, &error))?;
        let manifest_path = root.join(ArtifactKind::Manifest.relative_path());
        let manifest: RunManifest = read_json(&manifest_path, true)?;
        let mut store = Self { root, manifest };
        store.manifest.validate(&manifest_path)?;
        store.recover()?;
        store.validate_metadata_consistency()?;
        store.validate_phase_files()?;
        Ok(store)
    }

    /// Returns the canonical run root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the in-memory manifest last persisted by this store.
    #[must_use]
    pub const fn manifest(&self) -> &RunManifest {
        &self.manifest
    }

    /// Returns the canonical path for one logical artifact.
    #[must_use]
    pub fn artifact_path(&self, kind: ArtifactKind) -> PathBuf {
        self.root.join(kind.relative_path())
    }

    /// Returns the canonical content-addressed pending path used by crash recovery.
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::ManagedArtifact`] for store metadata and
    /// [`ArtifactError::InvalidDigest`] for a malformed digest.
    pub fn pending_path(&self, kind: ArtifactKind, sha256: &str) -> Result<PathBuf, ArtifactError> {
        if kind.is_store_managed() {
            return Err(ArtifactError::ManagedArtifact { kind });
        }
        if !is_sha256(sha256) {
            return Err(ArtifactError::InvalidDigest {
                digest: sha256.to_owned(),
            });
        }
        Ok(self
            .root
            .join(PENDING_DIRECTORY)
            .join(format!("{}.{sha256}.tmp", kind.pending_slug())))
    }

    /// Atomically publishes one content-addressed, append-only artifact.
    ///
    /// Lifecycle and existing-entry checks finish before creating a pending
    /// file. A crash after the pending file is synchronized can be completed by
    /// [`Self::recover`].
    ///
    /// # Errors
    ///
    /// Returns a typed lifecycle, conflict, recovery, or filesystem error.
    pub fn write_atomic(
        &mut self,
        kind: ArtifactKind,
        bytes: &[u8],
    ) -> Result<ArtifactRecord, ArtifactError> {
        self.validate_write(kind)?;
        let sha256 = content_sha256(bytes);
        let pending = self.pending_path(kind, &sha256)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&pending)
            .map_err(|error| filesystem("create pending artifact", &pending, &error))?;
        file.write_all(bytes)
            .map_err(|error| filesystem("write pending artifact", &pending, &error))?;
        file.sync_all()
            .map_err(|error| filesystem("synchronize pending artifact", &pending, &error))?;
        drop(file);

        let bytes = u64::try_from(bytes.len()).map_err(|error| ArtifactError::Filesystem {
            operation: "record artifact length",
            path: pending.clone(),
            message: error.to_string(),
        })?;
        let record = ArtifactRecord::new(kind, sha256, bytes);
        self.manifest.artifacts.insert(kind, record.clone());
        self.persist_state()?;

        let target = self.artifact_path(kind);
        fs::rename(&pending, &target)
            .map_err(|error| filesystem("promote pending artifact", &pending, &error))?;
        sync_directory(self.root.join(PENDING_DIRECTORY).as_path())?;
        sync_directory(&self.root)?;
        Ok(record)
    }

    /// Recovers every complete content-addressed pending artifact.
    ///
    /// Recovery first validates the complete pending set. Therefore a partial,
    /// malformed, phase-invalid, or conflicting entry is rejected before any
    /// pending entry or prior canonical artifact changes.
    ///
    /// # Errors
    ///
    /// Returns a typed mismatch, conflict, lifecycle, or filesystem error.
    pub fn recover(&mut self) -> Result<(), ArtifactError> {
        let candidates = self.preflight_recovery()?;
        for candidate in candidates {
            let record =
                ArtifactRecord::new(candidate.kind, candidate.sha256.clone(), candidate.bytes);
            self.manifest
                .artifacts
                .entry(candidate.kind)
                .or_insert(record);
            self.persist_state()?;
            if candidate.target_exists {
                fs::remove_file(&candidate.pending).map_err(|error| {
                    filesystem("remove recovered duplicate", &candidate.pending, &error)
                })?;
            } else {
                fs::rename(&candidate.pending, &candidate.target).map_err(|error| {
                    filesystem("promote recovered artifact", &candidate.pending, &error)
                })?;
            }
        }
        if !self.pending_entries()?.is_empty() {
            return Err(ArtifactError::PendingRecoveryRequired {
                path: self.pending_entries()?.remove(0),
            });
        }
        if !self.manifest.artifacts.is_empty() {
            sync_directory(self.root.join(PENDING_DIRECTORY).as_path())?;
            sync_directory(&self.root)?;
        }
        Ok(())
    }

    /// Verifies and seals prepared deterministic inputs, then transitions to
    /// [`RunPhase::Frozen`].
    ///
    /// # Errors
    ///
    /// Returns [`ArtifactError::InvalidTransition`] unless the current phase is
    /// prepared, or an integrity/filesystem error when sealing fails.
    pub fn freeze(&mut self) -> Result<(), ArtifactError> {
        if self.manifest.phase != RunPhase::Prepared {
            return Err(ArtifactError::InvalidTransition {
                from: self.manifest.phase,
                to: RunPhase::Frozen,
            });
        }
        self.ensure_no_pending()?;
        let kinds = self
            .manifest
            .artifacts
            .keys()
            .copied()
            .filter(|kind| kind.sealed_by(RunPhase::Frozen))
            .collect::<Vec<_>>();
        self.preflight_seal(&kinds)?;
        self.seal(&kinds)?;
        self.manifest.phase = RunPhase::Frozen;
        self.persist_state()
    }

    /// Advances along one solved scoring edge and seals that stage's output.
    ///
    /// `Prepared → Frozen` is intentionally owned by [`Self::freeze`] so the
    /// lifecycle cannot advance without verifying retrieval inputs.
    ///
    /// # Errors
    ///
    /// Returns a typed invalid-transition, missing-output, integrity, or
    /// filesystem error.
    pub fn transition(&mut self, to: RunPhase) -> Result<(), ArtifactError> {
        let (required, seal_kind) = match (self.manifest.phase, to) {
            (RunPhase::Frozen, RunPhase::DeterministicScored) => {
                (ArtifactKind::Metrics, ArtifactKind::Metrics)
            }
            (RunPhase::DeterministicScored, RunPhase::ModelScored) => {
                (ArtifactKind::ModelRecords, ArtifactKind::ModelRecords)
            }
            (from, to) => return Err(ArtifactError::InvalidTransition { from, to }),
        };
        if !self.manifest.artifacts.contains_key(&required) {
            return Err(ArtifactError::RequiredArtifactMissing {
                phase: to,
                kind: required,
            });
        }
        self.ensure_no_pending()?;
        self.preflight_seal(&[seal_kind])?;
        self.seal(&[seal_kind])?;
        self.manifest.phase = to;
        self.persist_state()
    }

    /// Verifies one sealed artifact's path, type, digest, length, and read-only
    /// permissions before returning a read-only file handle.
    ///
    /// # Errors
    ///
    /// Returns a typed missing, unfrozen, checksum, path, permission, or
    /// filesystem error.
    pub fn open_verified(&self, kind: ArtifactKind) -> Result<File, ArtifactError> {
        let record =
            self.manifest
                .artifacts
                .get(&kind)
                .ok_or_else(|| ArtifactError::ArtifactMissing {
                    kind,
                    path: self.artifact_path(kind),
                })?;
        if !record.frozen {
            return Err(ArtifactError::ArtifactNotFrozen {
                kind,
                phase: self.manifest.phase,
            });
        }
        self.verify_record(kind, record)?;
        let path = self.artifact_path(kind);
        let metadata = fs::metadata(&path)
            .map_err(|error| filesystem("read artifact metadata", &path, &error))?;
        if !metadata.permissions().readonly() {
            return Err(ArtifactError::ArtifactNotReadOnly { kind, path });
        }
        File::open(&path).map_err(|error| filesystem("open verified artifact", &path, &error))
    }

    fn validate_write(&self, kind: ArtifactKind) -> Result<(), ArtifactError> {
        if kind.is_store_managed() {
            return Err(ArtifactError::ManagedArtifact { kind });
        }
        if !kind.writable_in(self.manifest.phase) {
            return Err(ArtifactError::MutationForbidden {
                kind,
                phase: self.manifest.phase,
            });
        }
        if let Some(pending) = self.pending_entries()?.into_iter().next() {
            return Err(ArtifactError::PendingRecoveryRequired { path: pending });
        }
        let path = self.artifact_path(kind);
        if self.manifest.artifacts.contains_key(&kind) || path.exists() {
            return Err(ArtifactError::ArtifactAlreadyExists { kind, path });
        }
        Ok(())
    }

    fn preflight_recovery(&self) -> Result<Vec<RecoveryCandidate>, ArtifactError> {
        let mut candidates = Vec::new();
        let mut seen_kinds = BTreeMap::new();
        for pending in self.pending_entries()? {
            let file_name = pending
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| ArtifactError::RecoveryPathMismatch {
                    path: pending.clone(),
                })?;
            let stem = file_name.strip_suffix(".tmp").ok_or_else(|| {
                ArtifactError::RecoveryPathMismatch {
                    path: pending.clone(),
                }
            })?;
            let (slug, expected) =
                stem.rsplit_once('.')
                    .ok_or_else(|| ArtifactError::RecoveryPathMismatch {
                        path: pending.clone(),
                    })?;
            let expected = expected.to_owned();
            let kind = ArtifactKind::from_pending_slug(slug).ok_or_else(|| {
                ArtifactError::RecoveryPathMismatch {
                    path: pending.clone(),
                }
            })?;
            if kind.is_store_managed() || !is_sha256(&expected) {
                return Err(ArtifactError::RecoveryPathMismatch { path: pending });
            }
            if !kind.writable_in(self.manifest.phase) {
                return Err(ArtifactError::MutationForbidden {
                    kind,
                    phase: self.manifest.phase,
                });
            }
            let metadata = fs::symlink_metadata(&pending)
                .map_err(|error| filesystem("read pending metadata", &pending, &error))?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(ArtifactError::RecoveryPathMismatch { path: pending });
            }
            let actual = sha256_file(&pending)?;
            if actual != expected {
                return Err(ArtifactError::RecoveryChecksumMismatch {
                    kind,
                    path: pending,
                    expected,
                    actual,
                });
            }
            if let Some(prior) = seen_kinds.insert(kind, pending.clone()) {
                return Err(ArtifactError::RecoveryConflict {
                    kind,
                    pending,
                    prior,
                });
            }
            let target = self.artifact_path(kind);
            let target_exists = target.exists();
            if let Some(record) = self.manifest.artifacts.get(&kind) {
                if record.sha256 != expected
                    || record.relative_path != kind.relative_path()
                    || record.bytes != metadata.len()
                {
                    return Err(ArtifactError::RecoveryConflict {
                        kind,
                        pending,
                        prior: target,
                    });
                }
            }
            if target_exists {
                let target_metadata = fs::symlink_metadata(&target)
                    .map_err(|error| filesystem("read prior metadata", &target, &error))?;
                if !target_metadata.file_type().is_file()
                    || target_metadata.file_type().is_symlink()
                    || sha256_file(&target)? != expected
                {
                    return Err(ArtifactError::RecoveryConflict {
                        kind,
                        pending,
                        prior: target,
                    });
                }
            }
            candidates.push(RecoveryCandidate {
                kind,
                pending,
                target,
                sha256: expected,
                bytes: metadata.len(),
                target_exists,
            });
        }
        Ok(candidates)
    }

    fn pending_entries(&self) -> Result<Vec<PathBuf>, ArtifactError> {
        let pending_directory = self.root.join(PENDING_DIRECTORY);
        let mut entries = fs::read_dir(&pending_directory)
            .map_err(|error| filesystem("read pending directory", &pending_directory, &error))?
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| filesystem("read pending entry", &pending_directory, &error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort();
        Ok(entries)
    }

    fn ensure_no_pending(&self) -> Result<(), ArtifactError> {
        if let Some(path) = self.pending_entries()?.into_iter().next() {
            return Err(ArtifactError::PendingRecoveryRequired { path });
        }
        Ok(())
    }

    fn preflight_seal(&self, kinds: &[ArtifactKind]) -> Result<(), ArtifactError> {
        for kind in kinds {
            let record = self.manifest.artifacts.get(kind).ok_or_else(|| {
                ArtifactError::ArtifactMissing {
                    kind: *kind,
                    path: self.artifact_path(*kind),
                }
            })?;
            self.verify_record(*kind, record)?;
        }
        Ok(())
    }

    fn seal(&mut self, kinds: &[ArtifactKind]) -> Result<(), ArtifactError> {
        for kind in kinds {
            let path = self.artifact_path(*kind);
            let mut permissions = fs::metadata(&path)
                .map_err(|error| filesystem("read artifact permissions", &path, &error))?
                .permissions();
            permissions.set_readonly(true);
            fs::set_permissions(&path, permissions)
                .map_err(|error| filesystem("make artifact read-only", &path, &error))?;
            self.manifest
                .artifacts
                .get_mut(kind)
                .expect("preflight guarantees the record exists")
                .frozen = true;
        }
        sync_directory(&self.root)
    }

    fn verify_record(
        &self,
        kind: ArtifactKind,
        record: &ArtifactRecord,
    ) -> Result<(), ArtifactError> {
        let path = self.artifact_path(kind);
        if record.relative_path != kind.relative_path() {
            return Err(invalid_manifest(
                self.artifact_path(ArtifactKind::Manifest).as_path(),
                format!("artifact {kind:?} records a noncanonical path"),
            ));
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                ArtifactError::ArtifactMissing {
                    kind,
                    path: path.clone(),
                }
            } else {
                filesystem("read artifact metadata", &path, &error)
            }
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(ArtifactError::ArtifactMissing { kind, path });
        }
        let actual = sha256_file(&path)?;
        if actual != record.sha256 || metadata.len() != record.bytes {
            return Err(ArtifactError::ChecksumMismatch {
                kind,
                expected: record.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }

    fn validate_phase_files(&self) -> Result<(), ArtifactError> {
        for (kind, record) in &self.manifest.artifacts {
            self.verify_record(*kind, record)?;
            if record.frozen {
                let path = self.artifact_path(*kind);
                let readonly = fs::metadata(&path)
                    .map_err(|error| filesystem("read artifact permissions", &path, &error))?
                    .permissions()
                    .readonly();
                if !readonly {
                    return Err(ArtifactError::ArtifactNotReadOnly { kind: *kind, path });
                }
            }
        }
        Ok(())
    }

    fn validate_metadata_consistency(&self) -> Result<(), ArtifactError> {
        let path = self.artifact_path(ArtifactKind::Checksums);
        let index: ChecksumIndex = read_json(&path, false)?;
        if index.schema_version != CHECKSUM_SCHEMA_VERSION {
            return Err(invalid_checksums(
                &path,
                format!(
                    "schema version {} is unsupported; expected {CHECKSUM_SCHEMA_VERSION}",
                    index.schema_version
                ),
            ));
        }
        let expected = self.checksum_entries();
        if index.artifacts != expected {
            return Err(invalid_checksums(
                &path,
                "entries disagree with manifest content records",
            ));
        }
        Ok(())
    }

    fn checksum_entries(&self) -> BTreeMap<String, String> {
        self.manifest
            .artifacts
            .values()
            .map(|record| (record.relative_path.clone(), record.sha256.clone()))
            .collect()
    }

    fn persist_state(&self) -> Result<(), ArtifactError> {
        let checksum_index = ChecksumIndex {
            schema_version: CHECKSUM_SCHEMA_VERSION,
            artifacts: self.checksum_entries(),
        };
        write_json_atomic(
            &self.root,
            ArtifactKind::Checksums.relative_path(),
            &checksum_index,
        )?;
        write_json_atomic(
            &self.root,
            ArtifactKind::Manifest.relative_path(),
            &self.manifest,
        )
    }
}

#[derive(Debug)]
struct RecoveryCandidate {
    kind: ArtifactKind,
    pending: PathBuf,
    target: PathBuf,
    sha256: String,
    bytes: u64,
    target_exists: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChecksumIndex {
    schema_version: u32,
    artifacts: BTreeMap<String, String>,
}

/// Computes the lowercase SHA-256 content address for `bytes`.
#[must_use]
pub fn content_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String, ArtifactError> {
    let bytes = fs::read(path).map_err(|error| filesystem("read artifact", path, &error))?;
    Ok(content_sha256(&bytes))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn write_json_atomic<T: Serialize>(
    root: &Path,
    relative_path: &str,
    value: &T,
) -> Result<(), ArtifactError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|error| ArtifactError::Filesystem {
            operation: "serialize store metadata",
            path: root.join(relative_path),
            message: error.to_string(),
        })?;
    bytes.push(b'\n');
    replace_atomic(root, relative_path, &bytes)
}

fn read_json<T: DeserializeOwned>(path: &Path, manifest: bool) -> Result<T, ArtifactError> {
    let bytes = fs::read(path).map_err(|error| filesystem("read store metadata", path, &error))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        if manifest {
            invalid_manifest(path, error.to_string())
        } else {
            invalid_checksums(path, error.to_string())
        }
    })
}

fn replace_atomic(root: &Path, relative_path: &str, bytes: &[u8]) -> Result<(), ArtifactError> {
    let target = root.join(relative_path);
    let sequence = NEXT_SYSTEM_TEMP.fetch_add(1, Ordering::Relaxed);
    let temporary = root.join(format!(
        ".{relative_path}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| filesystem("create metadata temporary", &temporary, &error))?;
        file.write_all(bytes)
            .map_err(|error| filesystem("write metadata temporary", &temporary, &error))?;
        file.sync_all()
            .map_err(|error| filesystem("synchronize metadata temporary", &temporary, &error))?;
        drop(file);
        fs::rename(&temporary, &target)
            .map_err(|error| filesystem("promote store metadata", &temporary, &error))?;
        sync_directory(root)
    })();
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn sync_directory(path: &Path) -> Result<(), ArtifactError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| filesystem("synchronize directory", path, &error))
}

fn filesystem(operation: &'static str, path: &Path, error: &io::Error) -> ArtifactError {
    ArtifactError::Filesystem {
        operation,
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn invalid_manifest(path: &Path, message: impl Into<String>) -> ArtifactError {
    ArtifactError::InvalidManifest {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

fn invalid_checksums(path: &Path, message: impl Into<String>) -> ArtifactError {
    ArtifactError::InvalidChecksums {
        path: path.to_path_buf(),
        message: message.into(),
    }
}
