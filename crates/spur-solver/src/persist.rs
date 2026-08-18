//! Atomic, quota-bounded solver result persistence.
//!
//! Artifacts are a handoff cache for `solve_id` values. They do not replace
//! Beads as SPUR's collaboration source of truth.
//!
//! The cache is a fixed-capacity ring: when a new `persist` would exceed the
//! artifact count or total-byte budget, oldest entries are evicted first until
//! the write fits (or the single new artifact is larger than the whole budget).

use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::types::{SolveConstraintsResponse, SolveModel, SolveStatus, ValidationError};

/// Version of the on-disk solve artifact schema.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;
/// Maximum number of files allowed in one repository's solver cache.
///
/// When a new persist would exceed this count (or the byte budget), the store
/// evicts oldest artifacts first (ring / FIFO) until the write fits.
pub const MAX_ARTIFACTS: usize = 512;
/// Maximum total bytes allowed in one repository's solver cache.
pub const MAX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
/// Placeholder used until a process runner exposes its probed Z3 version.
pub const UNKNOWN_Z3_VERSION: &str = "unknown";

const SOLVE_ID_PREFIX: &str = "sol_";
const SOLVE_ID_HEX_LEN: usize = 16;
const ID_GENERATION_ATTEMPTS: usize = 16;
const LOCK_FILE_NAME: &str = ".lock";

/// Quota dimension that rejected a new artifact.
///
/// Count and byte pressure normally cycle the ring (oldest first). These kinds
/// are returned only when a single new artifact cannot fit even after every
/// existing cache entry is removed (or the new payload alone exceeds the
/// repository byte budget).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactQuotaKind {
    /// The next write cannot fit under [`MAX_ARTIFACTS`] even after eviction.
    ArtifactCount,
    /// The next write cannot fit under [`MAX_ARTIFACT_BYTES`] even after eviction.
    TotalBytes,
}

/// Persistence or retrieval failure for a solve artifact.
#[derive(Debug, Error)]
pub enum PersistError {
    /// A caller supplied an identifier outside the pinned wire format.
    #[error("invalid solve_id `{solve_id}`; expected sol_ followed by 16 lowercase hex digits")]
    InvalidSolveId {
        /// Rejected identifier.
        solve_id: String,
    },
    /// No artifact exists for a well-formed identifier.
    #[error("solve_id `{solve_id}` was not found")]
    SolveIdNotFound {
        /// Requested identifier.
        solve_id: String,
    },
    /// A stored artifact exceeded the repository cache's maximum byte size.
    #[error(
        "solver artifact {} is too large: maximum {limit} bytes, found at least {actual}",
        path.display()
    )]
    ArtifactTooLarge {
        /// Oversized artifact path.
        path: PathBuf,
        /// Configured maximum.
        limit: u64,
        /// Observed size, or the bounded read size when the file grew.
        actual: u64,
    },
    /// A new write would exceed one of the repository-local cache quotas.
    #[error("solver artifact quota exceeded ({kind:?}): limit {limit}, attempted {attempted}")]
    QuotaExceeded {
        /// Quota dimension that was exceeded.
        kind: ArtifactQuotaKind,
        /// Configured maximum.
        limit: u64,
        /// Value that the new artifact would produce.
        attempted: u64,
    },
    /// The artifact JSON could not be serialized or parsed.
    #[error("could not {operation} solver artifact JSON: {source}")]
    Json {
        /// Serialization operation.
        operation: &'static str,
        /// JSON failure.
        #[source]
        source: serde_json::Error,
    },
    /// The stored artifact uses an unsupported schema.
    #[error(
        "unsupported solver artifact schema version {found}; expected {ARTIFACT_SCHEMA_VERSION}"
    )]
    UnsupportedSchema {
        /// Version read from disk.
        found: u32,
    },
    /// The artifact payload does not match the requested identifier.
    #[error("solver artifact id mismatch: requested `{requested}`, stored `{stored}`")]
    SolveIdMismatch {
        /// Identifier used for lookup.
        requested: String,
        /// Identifier in the JSON payload.
        stored: String,
    },
    /// The stored result violates the solver response envelope invariants.
    #[error("invalid solver artifact result: {source}")]
    InvalidResult {
        /// Response invariant failure.
        #[source]
        source: ValidationError,
    },
    /// An unexpected entry would make quota accounting ambiguous.
    #[error("solver artifact directory contains a non-regular entry: {}", path.display())]
    NonRegularEntry {
        /// Unexpected entry.
        path: PathBuf,
    },
    /// A filesystem operation failed.
    #[error("could not {operation} {}: {source}", path.display())]
    Io {
        /// Filesystem operation.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },
    /// The in-process quota lock was poisoned.
    #[error("solver artifact quota lock is poisoned")]
    LockPoisoned,
    /// Repeated UUID collisions prevented allocation of a new identifier.
    #[error("could not allocate a unique solve_id after {ID_GENERATION_ATTEMPTS} attempts")]
    IdGenerationExhausted,
}

/// Stable result body embedded in a schema-v1 artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SolveArtifactResult {
    /// Solver status.
    pub status: SolveStatus,
    /// Concrete model, present only for a satisfiable result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<SolveModel>,
    /// End-to-end solve duration in milliseconds.
    pub duration_ms: u64,
    /// Human-readable diagnostic for non-sat or error outcomes.
    pub reason: Option<String>,
    /// Named hard unsat core surface ids when recorded with the solve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsat_core: Option<Vec<String>>,
}

impl SolveArtifactResult {
    fn from_response(response: &SolveConstraintsResponse) -> Result<Self, PersistError> {
        response
            .validate()
            .map_err(|source| PersistError::InvalidResult { source })?;
        Ok(Self {
            status: response.status,
            model: response.model.clone(),
            duration_ms: response.duration_ms,
            reason: response.reason.clone(),
            unsat_core: response.unsat_core.clone(),
        })
    }

    fn validate(&self) -> Result<(), PersistError> {
        SolveConstraintsResponse {
            status: self.status,
            model: self.model.clone(),
            duration_ms: self.duration_ms,
            solve_id: None,
            reason: self.reason.clone(),
            smt: None,
            unsat_core: self.unsat_core.clone(),
            cached: false,
            session_id: None,
            optimization: None,
            solver_version: None,
        }
        .validate()
        .map_err(|source| PersistError::InvalidResult { source })
    }
}

/// Canonical schema-v1 solve artifact stored beneath `.spur/solver/`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SolveArtifact {
    /// Artifact schema version.
    pub schema_version: u32,
    /// Traversal-safe handoff identifier.
    pub solve_id: String,
    /// UTC creation timestamp in RFC 3339 form.
    pub created_at_wall: String,
    /// Z3 version observed by the hosting service.
    pub z3_version: String,
    /// Canonically serialized `solve_constraints` or `solve_smt` request.
    pub request: Value,
    /// Stable result payload.
    pub result: SolveArtifactResult,
}

/// Result-only response returned by `get_solve_result`.
///
/// The request and creation metadata remain in the handoff cache but are not
/// exposed through the retrieval API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GetSolveResultResponse {
    /// Traversal-safe handoff identifier.
    pub solve_id: String,
    /// Z3 version recorded with the solve.
    pub z3_version: String,
    /// Stable result fields.
    #[serde(flatten)]
    pub result: SolveArtifactResult,
}

impl SolveArtifact {
    fn new<T: Serialize>(
        solve_id: String,
        z3_version: &str,
        request: &T,
        response: &SolveConstraintsResponse,
    ) -> Result<Self, PersistError> {
        let request = serde_json::to_value(request).map_err(|source| PersistError::Json {
            operation: "serialize request for",
            source,
        })?;
        Ok(Self {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            solve_id,
            created_at_wall: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            z3_version: z3_version.to_owned(),
            request,
            result: SolveArtifactResult::from_response(response)?,
        })
    }

    fn validate(&self, requested_solve_id: &str) -> Result<(), PersistError> {
        if self.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(PersistError::UnsupportedSchema {
                found: self.schema_version,
            });
        }
        validate_solve_id(&self.solve_id)?;
        if self.solve_id != requested_solve_id {
            return Err(PersistError::SolveIdMismatch {
                requested: requested_solve_id.to_owned(),
                stored: self.solve_id.clone(),
            });
        }
        self.result.validate()
    }

    fn into_get_response(self) -> GetSolveResultResponse {
        GetSolveResultResponse {
            solve_id: self.solve_id,
            z3_version: self.z3_version,
            result: self.result,
        }
    }
}

/// Repository-local artifact store shared by cloned solver services.
#[derive(Clone, Debug)]
pub(crate) struct ArtifactStore {
    directory: Arc<PathBuf>,
    write_lock: Arc<Mutex<()>>,
}

impl ArtifactStore {
    pub(crate) fn for_repo_root(repo_root: impl Into<PathBuf>) -> Self {
        let directory = repo_root.into().join(".spur").join("solver");
        Self {
            directory: Arc::new(directory),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn persist<T: Serialize>(
        &self,
        request: &T,
        response: &SolveConstraintsResponse,
        z3_version: &str,
    ) -> Result<SolveArtifact, PersistError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_poisoned| PersistError::LockPoisoned)?;

        ensure_private_directory(&self.directory)?;
        let _repository_guard = RepositoryLock::acquire(&self.directory)?;
        let solve_id = self.allocate_solve_id()?;
        let artifact = SolveArtifact::new(solve_id, z3_version, request, response)?;
        let mut bytes =
            serde_json::to_vec_pretty(&artifact).map_err(|source| PersistError::Json {
                operation: "serialize",
                source,
            })?;
        bytes.push(b'\n');

        self.make_room_for(bytes.len())?;
        let path = self.artifact_path(&artifact.solve_id)?;
        write_atomic(&path, &bytes)?;
        Ok(artifact)
    }

    pub(crate) fn get(&self, solve_id: &str) -> Result<GetSolveResultResponse, PersistError> {
        let path = self.artifact_path(solve_id)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(PersistError::SolveIdNotFound {
                    solve_id: solve_id.to_owned(),
                });
            }
            Err(source) => {
                return Err(io_error("inspect", &path, source));
            }
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(PersistError::NonRegularEntry { path });
        }
        if metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(PersistError::ArtifactTooLarge {
                path,
                limit: MAX_ARTIFACT_BYTES,
                actual: metadata.len(),
            });
        }

        let file = open_artifact_for_read(&path)?;
        let file_metadata = file
            .metadata()
            .map_err(|source| io_error("inspect open artifact", &path, source))?;
        if !file_metadata.is_file() {
            return Err(PersistError::NonRegularEntry { path });
        }
        if file_metadata.len() > MAX_ARTIFACT_BYTES {
            return Err(PersistError::ArtifactTooLarge {
                path,
                limit: MAX_ARTIFACT_BYTES,
                actual: file_metadata.len(),
            });
        }

        let mut bytes = Vec::new();
        file.take(MAX_ARTIFACT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| io_error("read", &path, source))?;
        let bytes_read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if bytes_read > MAX_ARTIFACT_BYTES {
            return Err(PersistError::ArtifactTooLarge {
                path,
                limit: MAX_ARTIFACT_BYTES,
                actual: bytes_read,
            });
        }

        let artifact: SolveArtifact =
            serde_json::from_slice(&bytes).map_err(|source| PersistError::Json {
                operation: "parse",
                source,
            })?;
        artifact.validate(solve_id)?;
        Ok(artifact.into_get_response())
    }

    fn artifact_path(&self, solve_id: &str) -> Result<PathBuf, PersistError> {
        validate_solve_id(solve_id)?;
        Ok(self.directory.join(format!("{solve_id}.json")))
    }

    fn allocate_solve_id(&self) -> Result<String, PersistError> {
        for _attempt in 0..ID_GENERATION_ATTEMPTS {
            let uuid = Uuid::new_v4();
            let bytes = uuid.as_bytes();
            let value = u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            let solve_id = format!("{SOLVE_ID_PREFIX}{value:016x}");
            let path = self.artifact_path(&solve_id)?;
            match fs::symlink_metadata(&path) {
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    return Ok(solve_id);
                }
                Err(source) => return Err(io_error("inspect", &path, source)),
                Ok(_) => {}
            }
        }
        Err(PersistError::IdGenerationExhausted)
    }

    /// Evict oldest artifacts until `new_artifact_bytes` fits under both
    /// [`MAX_ARTIFACTS`] and [`MAX_ARTIFACT_BYTES`].
    ///
    /// Callers must hold the repository write lock. A payload larger than the
    /// total byte budget is rejected without deleting existing entries.
    fn make_room_for(&self, new_artifact_bytes: usize) -> Result<(), PersistError> {
        let new_artifact_bytes = u64::try_from(new_artifact_bytes).unwrap_or(u64::MAX);
        let count_limit = u64::try_from(MAX_ARTIFACTS).unwrap_or(u64::MAX);

        if new_artifact_bytes > MAX_ARTIFACT_BYTES {
            return Err(PersistError::QuotaExceeded {
                kind: ArtifactQuotaKind::TotalBytes,
                limit: MAX_ARTIFACT_BYTES,
                attempted: new_artifact_bytes,
            });
        }

        loop {
            let mut entries = self.list_cache_entries()?;
            let artifact_count = u64::try_from(entries.len()).unwrap_or(u64::MAX);
            let total_bytes = entries
                .iter()
                .fold(0_u64, |acc, entry| acc.saturating_add(entry.size));

            let count_fits = artifact_count < count_limit;
            let bytes_fit = total_bytes.saturating_add(new_artifact_bytes) <= MAX_ARTIFACT_BYTES;
            if count_fits && bytes_fit {
                return Ok(());
            }

            if entries.is_empty() {
                // Nothing left to evict; new payload still does not fit.
                if !count_fits {
                    return Err(PersistError::QuotaExceeded {
                        kind: ArtifactQuotaKind::ArtifactCount,
                        limit: count_limit,
                        attempted: artifact_count.saturating_add(1),
                    });
                }
                return Err(PersistError::QuotaExceeded {
                    kind: ArtifactQuotaKind::TotalBytes,
                    limit: MAX_ARTIFACT_BYTES,
                    attempted: total_bytes.saturating_add(new_artifact_bytes),
                });
            }

            // Oldest first (ring): RFC 3339 wall clock when present, else mtime.
            entries.sort_by(|left, right| {
                left.order_key
                    .cmp(&right.order_key)
                    .then_with(|| left.path.cmp(&right.path))
            });
            let victim = &entries[0];
            fs::remove_file(&victim.path)
                .map_err(|source| io_error("evict", &victim.path, source))?;
        }
    }

    fn list_cache_entries(&self) -> Result<Vec<CacheEntry>, PersistError> {
        let mut entries = Vec::new();
        let dir_entries = fs::read_dir(&*self.directory)
            .map_err(|source| io_error("list", &self.directory, source))?;

        for entry in dir_entries {
            let entry = entry.map_err(|source| io_error("list", &self.directory, source))?;
            let file_name = entry.file_name();
            if file_name == OsStr::new(LOCK_FILE_NAME) {
                continue;
            }
            // Skip in-progress atomic write temps (`.{uuid}.tmp`).
            let file_name_lossy = file_name.to_string_lossy();
            if file_name_lossy.starts_with('.') && file_name_lossy.ends_with(".tmp") {
                continue;
            }

            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| io_error("inspect", &path, source))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(PersistError::NonRegularEntry { path });
            }

            let order_key = cache_entry_order_key(&path, &metadata);
            entries.push(CacheEntry {
                path,
                size: metadata.len(),
                order_key,
            });
        }
        Ok(entries)
    }
}

#[derive(Debug)]
struct CacheEntry {
    path: PathBuf,
    size: u64,
    /// Lexicographic key: prefer `created_at_wall` (RFC 3339), else mtime nanos.
    order_key: String,
}

fn cache_entry_order_key(path: &Path, metadata: &fs::Metadata) -> String {
    if let Some(created_at) = peek_created_at_wall(path) {
        return format!("t:{created_at}");
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("m:{modified:032}")
}

/// Best-effort read of `created_at_wall` without full artifact validation.
fn peek_created_at_wall(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    // Cap parse work for oversized/corrupt stubs used in tests.
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("created_at_wall")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Validates the pinned `sol_` plus 16-lowercase-hex identifier format.
///
/// Callers must invoke this before joining an untrusted identifier to a path.
///
/// # Errors
///
/// Returns [`PersistError::InvalidSolveId`] for any other shape, including
/// path separators, traversal components, uppercase hex, or the wrong length.
pub fn validate_solve_id(solve_id: &str) -> Result<(), PersistError> {
    let valid = solve_id
        .strip_prefix(SOLVE_ID_PREFIX)
        .is_some_and(|suffix| {
            suffix.len() == SOLVE_ID_HEX_LEN
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
    if valid {
        Ok(())
    } else {
        Err(PersistError::InvalidSolveId {
            solve_id: solve_id.to_owned(),
        })
    }
}

#[derive(Debug)]
struct RepositoryLock {
    file: File,
}

impl RepositoryLock {
    fn acquire(directory: &Path) -> Result<Self, PersistError> {
        let path = directory.join(LOCK_FILE_NAME);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }

        let file = options
            .open(&path)
            .map_err(|source| io_error("open repository lock", &path, source))?;
        let metadata = file
            .metadata()
            .map_err(|source| io_error("inspect repository lock", &path, source))?;
        if !metadata.is_file() {
            return Err(PersistError::NonRegularEntry { path });
        }
        file.lock_exclusive()
            .map_err(|source| io_error("acquire repository lock", &path, source))?;
        Ok(Self { file })
    }
}

impl Drop for RepositoryLock {
    fn drop(&mut self) {
        let _ignored = fs2::FileExt::unlock(&self.file);
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), PersistError> {
    fs::create_dir_all(path).map_err(|source| io_error("create", path, source))?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect", path, source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PersistError::NonRegularEntry {
            path: path.to_path_buf(),
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error("set permissions on", path, source))?;
    }
    Ok(())
}

fn open_artifact_for_read(path: &Path) -> Result<File, PersistError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(file) => Ok(file),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            Err(PersistError::SolveIdNotFound {
                solve_id: path
                    .file_stem()
                    .and_then(OsStr::to_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        }
        Err(source) => Err(io_error("open", path, source)),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            "resolve parent for",
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "artifact path has no parent"),
        )
    })?;
    let temporary_path = parent.join(format!(".{}.tmp", Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let mut temporary = options
        .open(&temporary_path)
        .map_err(|source| io_error("create temporary file", &temporary_path, source))?;
    if let Err(source) = write_and_sync(&mut temporary, bytes) {
        drop(temporary);
        remove_temporary(&temporary_path);
        return Err(io_error("write temporary file", &temporary_path, source));
    }
    drop(temporary);

    if let Err(source) = fs::rename(&temporary_path, path) {
        remove_temporary(&temporary_path);
        return Err(io_error("rename temporary file to", path, source));
    }
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync parent directory for", path, source))?;
    Ok(())
}

fn write_and_sync(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

fn remove_temporary(path: &Path) {
    let _ignored = fs::remove_file(path);
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> PersistError {
    PersistError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
