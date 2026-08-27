use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::serving_registry::ArtifactRef;

const OWNED_CACHE_DIRECTORY: &str = "spur-artifact-cache";

pub type ArtifactStream = Pin<Box<dyn AsyncRead + Send + 'static>>;

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ArtifactFetchError {
    message: String,
}

impl ArtifactFetchError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait ArtifactFetcher: Send + Sync {
    async fn fetch(&self, uri: &str) -> Result<ArtifactStream, ArtifactFetchError>;
}

#[async_trait]
pub trait ArtifactCacheFilesystem: Send + Sync {
    async fn create_dir_all(&self, path: &Path) -> std::io::Result<()>;
    async fn open_new(&self, path: &Path) -> std::io::Result<tokio::fs::File>;
    async fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    async fn remove_dir_all(&self, path: &Path) -> std::io::Result<()>;
    fn remove_file(&self, path: &Path) -> std::io::Result<()>;
}

#[derive(Debug, Default)]
struct LocalArtifactCacheFilesystem;

#[async_trait]
impl ArtifactCacheFilesystem for LocalArtifactCacheFilesystem {
    async fn create_dir_all(&self, path: &Path) -> std::io::Result<()> {
        tokio::fs::create_dir_all(path).await
    }

    async fn open_new(&self, path: &Path) -> std::io::Result<tokio::fs::File> {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .await
    }

    async fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        tokio::fs::rename(from, to).await
    }

    async fn remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
        tokio::fs::remove_dir_all(path).await
    }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
}

#[cfg(feature = "artifact-cache-s3")]
#[derive(Clone)]
pub struct S3ArtifactFetcher {
    client: aws_sdk_s3::Client,
}

#[cfg(feature = "artifact-cache-s3")]
impl S3ArtifactFetcher {
    pub fn new(client: aws_sdk_s3::Client) -> Self {
        Self { client }
    }
}

#[cfg(feature = "artifact-cache-s3")]
#[async_trait]
impl ArtifactFetcher for S3ArtifactFetcher {
    async fn fetch(&self, uri: &str) -> Result<ArtifactStream, ArtifactFetchError> {
        let (bucket, key) = parse_s3_uri(uri).ok_or_else(|| {
            ArtifactFetchError::new("artifact URI is not a complete S3 object URI")
        })?;
        let output = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|error| ArtifactFetchError::new(error.to_string()))?;
        Ok(Box::pin(output.body.into_async_read()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactIdentity {
    pub generation: i64,
    pub source: String,
    pub package: String,
    pub revision: String,
    pub artifact: ArtifactRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheUsage {
    pub resident_bytes: u64,
    pub incoming_temp_bytes: u64,
    pub tmp_capacity_bytes: u64,
    pub poisoned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArtifactCacheError {
    #[error("artifact identity is temporarily unavailable")]
    InvalidIdentity,
    #[error("artifact cache capacity is temporarily unavailable")]
    CapacityExceeded,
    #[error("artifact is temporarily unavailable")]
    Unavailable,
    #[error("artifact generation is temporarily unavailable")]
    GenerationUnavailable,
    #[error("artifact failed integrity validation")]
    IntegrityMismatch,
    #[error("artifact cache is temporarily unavailable")]
    Filesystem,
}

impl ArtifactCacheError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidIdentity => "invalid_artifact_identity",
            Self::CapacityExceeded => "artifact_capacity_exceeded",
            Self::Unavailable => "artifact_unavailable",
            Self::GenerationUnavailable => "artifact_generation_unavailable",
            Self::IntegrityMismatch => "artifact_integrity_mismatch",
            Self::Filesystem => "artifact_cache_unavailable",
        }
    }

    pub const fn is_retryable(self) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct ArtifactCache {
    inner: Arc<ArtifactCacheInner>,
}

struct ArtifactCacheInner {
    owned_root: PathBuf,
    tmp_capacity_bytes: u64,
    fetcher: Arc<dyn ArtifactFetcher>,
    filesystem: Arc<dyn ArtifactCacheFilesystem>,
    generation: Arc<RwLock<GenerationState>>,
    state: Arc<Mutex<CacheState>>,
}

#[derive(Debug, Default)]
struct GenerationState {
    active: Option<i64>,
    pending: Option<PendingGenerationCleanup>,
    epoch: u64,
}

#[derive(Debug, Clone, Copy)]
struct PendingGenerationCleanup {
    previous: i64,
    target: i64,
}

#[derive(Default)]
struct CacheState {
    resident_bytes: u64,
    incoming_temp_bytes: u64,
    poisoned: bool,
    entries: HashMap<CacheKey, Arc<OwnedOperation<Result<PathBuf, ArtifactCacheError>>>>,
}

struct OwnedOperation<T> {
    result: Mutex<Option<T>>,
    completed: Notify,
}

impl<T> OwnedOperation<T> {
    fn new() -> Self {
        Self {
            result: Mutex::new(None),
            completed: Notify::new(),
        }
    }

    fn complete(&self, result: T) {
        *lock_mutex(&self.result) = Some(result);
        self.completed.notify_waiters();
    }
}

impl<T: Clone> OwnedOperation<T> {
    async fn wait(&self) -> T {
        loop {
            let completed = self.completed.notified();
            if let Some(result) = lock_mutex(&self.result).clone() {
                return result;
            }
            completed.await;
        }
    }
}

pub struct MaterializedArtifact {
    path: PathBuf,
    generation: i64,
    _generation_lease: OwnedRwLockReadGuard<GenerationState>,
}

impl MaterializedArtifact {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for MaterializedArtifact {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Deref for MaterializedArtifact {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl fmt::Debug for MaterializedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaterializedArtifact")
            .field("path", &self.path)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Eq)]
struct CacheKey {
    generation: i64,
    source: String,
    package: String,
    revision: String,
    uri: String,
    sha256: String,
    bytes: u64,
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.source == other.source
            && self.package == other.package
            && self.revision == other.revision
            && self.uri == other.uri
            && self.sha256 == other.sha256
            && self.bytes == other.bytes
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.generation.hash(state);
        self.source.hash(state);
        self.package.hash(state);
        self.revision.hash(state);
        self.uri.hash(state);
        self.sha256.hash(state);
        self.bytes.hash(state);
    }
}

impl From<&ArtifactIdentity> for CacheKey {
    fn from(identity: &ArtifactIdentity) -> Self {
        Self {
            generation: identity.generation,
            source: identity.source.clone(),
            package: identity.package.clone(),
            revision: identity.revision.clone(),
            uri: identity.artifact.uri.clone(),
            sha256: identity.artifact.sha256.clone(),
            bytes: identity.artifact.bytes,
        }
    }
}

impl ArtifactCache {
    pub fn new(
        root: impl AsRef<Path>,
        tmp_capacity_bytes: u64,
        fetcher: Arc<dyn ArtifactFetcher>,
    ) -> Result<Self, ArtifactCacheError> {
        Self::new_with_filesystem(
            root,
            tmp_capacity_bytes,
            fetcher,
            Arc::new(LocalArtifactCacheFilesystem),
        )
    }

    pub fn new_with_filesystem(
        root: impl AsRef<Path>,
        tmp_capacity_bytes: u64,
        fetcher: Arc<dyn ArtifactFetcher>,
        filesystem: Arc<dyn ArtifactCacheFilesystem>,
    ) -> Result<Self, ArtifactCacheError> {
        if tmp_capacity_bytes == 0 {
            return Err(ArtifactCacheError::InvalidIdentity);
        }

        let owned_root = root.as_ref().join(OWNED_CACHE_DIRECTORY);
        match std::fs::remove_dir_all(&owned_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ArtifactCacheError::Filesystem),
        }
        std::fs::create_dir_all(&owned_root).map_err(|_| ArtifactCacheError::Filesystem)?;

        Ok(Self {
            inner: Arc::new(ArtifactCacheInner {
                owned_root,
                tmp_capacity_bytes,
                fetcher,
                filesystem,
                generation: Arc::new(RwLock::new(GenerationState::default())),
                state: Arc::new(Mutex::new(CacheState::default())),
            }),
        })
    }

    pub fn final_path(&self, identity: &ArtifactIdentity) -> PathBuf {
        self.generation_dir(identity.generation)
            .join(identity_directory(identity))
            .join(&identity.artifact.sha256)
    }

    pub fn usage(&self) -> CacheUsage {
        let state = lock_state(&self.inner.state);
        CacheUsage {
            resident_bytes: state.resident_bytes,
            incoming_temp_bytes: state.incoming_temp_bytes,
            tmp_capacity_bytes: self.inner.tmp_capacity_bytes,
            poisoned: state.poisoned,
        }
    }

    pub async fn activate_generation(&self, generation: i64) -> Result<(), ArtifactCacheError> {
        let authority = Arc::clone(&self.inner.generation).write_owned().await;
        let poisoned = lock_state(&self.inner.state).poisoned;
        if authority.pending.is_none() && authority.active == Some(generation) && !poisoned {
            return Ok(());
        }

        let operation = Arc::new(OwnedOperation::new());
        let operation_task = Arc::clone(&operation);
        let cache = self.clone();
        tokio::spawn(async move {
            let worker_cache = cache.clone();
            let worker = tokio::spawn(async move {
                worker_cache
                    .reconcile_generation(authority, generation)
                    .await
            });
            let result = match worker.await {
                Ok(result) => result,
                Err(_) => {
                    lock_state(&cache.inner.state).poisoned = true;
                    Err(ArtifactCacheError::Filesystem)
                }
            };
            operation_task.complete(result);
        });
        operation.wait().await
    }

    async fn reconcile_generation(
        &self,
        mut authority: OwnedRwLockWriteGuard<GenerationState>,
        generation: i64,
    ) -> Result<(), ArtifactCacheError> {
        loop {
            let poisoned = lock_state(&self.inner.state).poisoned;
            if authority.pending.is_none() && authority.active == Some(generation) && !poisoned {
                return Ok(());
            }

            let Some(active) = authority.active else {
                if poisoned {
                    match self
                        .inner
                        .filesystem
                        .remove_dir_all(&self.generation_dir(generation))
                        .await
                    {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => return Err(ArtifactCacheError::Filesystem),
                    }
                }
                self.inner
                    .filesystem
                    .create_dir_all(&self.generation_dir(generation))
                    .await
                    .map_err(|_| ArtifactCacheError::Filesystem)?;
                if poisoned {
                    *lock_state(&self.inner.state) = CacheState::default();
                }
                authority.epoch = self.next_generation_epoch(authority.epoch)?;
                authority.active = Some(generation);
                return Ok(());
            };

            if authority.pending.is_none() {
                authority.pending = Some(PendingGenerationCleanup {
                    previous: active,
                    target: generation,
                });
            }
            let pending = authority
                .pending
                .expect("pending generation cleanup was just established");
            let previous_dir = self.generation_dir(pending.previous);
            match self.inner.filesystem.remove_dir_all(&previous_dir).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(ArtifactCacheError::Filesystem),
            }

            {
                let mut state = lock_state(&self.inner.state);
                *state = CacheState::default();
            }
            self.inner
                .filesystem
                .create_dir_all(&self.generation_dir(pending.target))
                .await
                .map_err(|_| ArtifactCacheError::Filesystem)?;
            authority.epoch = self.next_generation_epoch(authority.epoch)?;
            authority.active = Some(pending.target);
            authority.pending = None;
            if pending.target == generation {
                return Ok(());
            }
        }
    }

    fn next_generation_epoch(&self, epoch: u64) -> Result<u64, ArtifactCacheError> {
        epoch.checked_add(1).ok_or_else(|| {
            lock_state(&self.inner.state).poisoned = true;
            ArtifactCacheError::Filesystem
        })
    }

    pub async fn materialize(
        &self,
        identity: &ArtifactIdentity,
    ) -> Result<MaterializedArtifact, ArtifactCacheError> {
        validate_identity(identity)?;
        let initial_lease = Arc::clone(&self.inner.generation).read_owned().await;
        let generation_epoch = self.validate_generation_authority(&initial_lease, identity)?;
        let key = CacheKey::from(identity);
        let (operation, created) = {
            let mut state = lock_state(&self.inner.state);
            if state.poisoned {
                return Err(ArtifactCacheError::Filesystem);
            }
            match state.entries.get(&key) {
                Some(operation) => (Arc::clone(operation), false),
                None => {
                    let operation = Arc::new(OwnedOperation::new());
                    state.entries.insert(key.clone(), Arc::clone(&operation));
                    (operation, true)
                }
            }
        };

        let mut caller_lease = Some(initial_lease);
        if created {
            self.start_materialization_operation(
                key.clone(),
                identity.clone(),
                generation_epoch,
                caller_lease
                    .take()
                    .expect("new operation must transfer its generation lease"),
                Arc::clone(&operation),
            );
        }

        let path = operation.wait().await?;
        let generation_lease = match caller_lease {
            Some(lease) => lease,
            None => Arc::clone(&self.inner.generation).read_owned().await,
        };
        if self.validate_generation_authority(&generation_lease, identity)? != generation_epoch {
            return Err(ArtifactCacheError::GenerationUnavailable);
        }
        let state = lock_state(&self.inner.state);
        if state.poisoned {
            return Err(ArtifactCacheError::Filesystem);
        }
        if !state
            .entries
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &operation))
        {
            return Err(ArtifactCacheError::GenerationUnavailable);
        }
        Ok(MaterializedArtifact {
            path,
            generation: identity.generation,
            _generation_lease: generation_lease,
        })
    }

    fn start_materialization_operation(
        &self,
        key: CacheKey,
        identity: ArtifactIdentity,
        generation_epoch: u64,
        generation_lease: OwnedRwLockReadGuard<GenerationState>,
        operation: Arc<OwnedOperation<Result<PathBuf, ArtifactCacheError>>>,
    ) {
        let cache = self.clone();
        let journal = Arc::new(PublicationJournal::new(
            cache.final_path(&identity),
            Arc::clone(&cache.inner.filesystem),
            Arc::clone(&cache.inner.state),
        ));
        tokio::spawn(async move {
            let worker_cache = cache.clone();
            let worker_journal = Arc::clone(&journal);
            let worker = tokio::spawn(async move {
                worker_cache
                    .fetch_and_publish(
                        &identity,
                        generation_epoch,
                        &generation_lease,
                        &worker_journal,
                    )
                    .await
            });
            let (result, panicked) = match worker.await {
                Ok(result) => (result, false),
                Err(_) => (Err(ArtifactCacheError::Filesystem), true),
            };
            if panicked {
                lock_state(&cache.inner.state).poisoned = true;
            }
            if result.is_err() {
                journal.reconcile_failure();
            }
            {
                let mut state = lock_state(&cache.inner.state);
                if result.is_err()
                    && state
                        .entries
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &operation))
                {
                    state.entries.remove(&key);
                }
            }
            operation.complete(result);
        });
    }

    fn validate_generation_authority(
        &self,
        generation: &GenerationState,
        identity: &ArtifactIdentity,
    ) -> Result<u64, ArtifactCacheError> {
        if generation.pending.is_some() || generation.active != Some(identity.generation) {
            return Err(ArtifactCacheError::GenerationUnavailable);
        }
        Ok(generation.epoch)
    }

    fn validate_materialization_admission(
        &self,
        generation: &GenerationState,
        identity: &ArtifactIdentity,
        generation_epoch: u64,
    ) -> Result<(), ArtifactCacheError> {
        if self.validate_generation_authority(generation, identity)? != generation_epoch {
            return Err(ArtifactCacheError::GenerationUnavailable);
        }
        if lock_state(&self.inner.state).poisoned {
            return Err(ArtifactCacheError::Filesystem);
        }
        Ok(())
    }

    async fn fetch_and_publish(
        &self,
        identity: &ArtifactIdentity,
        generation_epoch: u64,
        generation_lease: &GenerationState,
        journal: &PublicationJournal,
    ) -> Result<PathBuf, ArtifactCacheError> {
        self.validate_materialization_admission(generation_lease, identity, generation_epoch)?;
        let final_path = journal.final_path();
        let parent = final_path.parent().ok_or(ArtifactCacheError::Filesystem)?;
        self.inner
            .filesystem
            .create_dir_all(parent)
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;

        self.validate_materialization_admission(generation_lease, identity, generation_epoch)?;
        let reservation = CapacityReservation::reserve(
            Arc::clone(&self.inner.state),
            self.inner.tmp_capacity_bytes,
            identity.artifact.bytes,
        )?;
        let temp_path = parent.join(format!(
            ".{}.{}.tmp",
            identity.artifact.sha256,
            uuid::Uuid::new_v4()
        ));
        journal.track_temp(temp_path.clone(), reservation);
        self.validate_materialization_admission(generation_lease, identity, generation_epoch)?;
        let file = self
            .inner
            .filesystem
            .open_new(&temp_path)
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;
        self.validate_materialization_admission(generation_lease, identity, generation_epoch)?;
        let mut stream = self
            .inner
            .fetcher
            .fetch(&identity.artifact.uri)
            .await
            .map_err(|_| ArtifactCacheError::Unavailable)?;
        let mut writer = HashingWriter::new(file, identity.artifact.bytes);
        if let Err(error) = tokio::io::copy(&mut stream, &mut writer).await {
            return if error.kind() == std::io::ErrorKind::InvalidData {
                Err(ArtifactCacheError::IntegrityMismatch)
            } else {
                Err(ArtifactCacheError::Filesystem)
            };
        }
        writer
            .flush()
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;
        let (file, actual_bytes, actual_sha256) = writer.finish();
        file.sync_all()
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;
        drop(file);

        if actual_bytes != identity.artifact.bytes
            || !actual_sha256.eq_ignore_ascii_case(&identity.artifact.sha256)
        {
            return Err(ArtifactCacheError::IntegrityMismatch);
        }

        self.validate_materialization_admission(generation_lease, identity, generation_epoch)?;
        journal.start_publication();
        self.inner
            .filesystem
            .rename(&temp_path, &final_path)
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;
        if self.validate_generation_authority(generation_lease, identity)? != generation_epoch {
            journal.reconcile_failure();
            return Err(ArtifactCacheError::GenerationUnavailable);
        }
        if let Err(error) = journal.commit_if_available() {
            journal.reconcile_failure();
            return Err(error);
        }
        journal.complete_success();
        Ok(final_path)
    }

    fn generation_dir(&self, generation: i64) -> PathBuf {
        self.inner
            .owned_root
            .join(format!("generation-{generation}"))
    }
}

fn validate_identity(identity: &ArtifactIdentity) -> Result<(), ArtifactCacheError> {
    if identity.source.trim().is_empty()
        || identity.package.trim().is_empty()
        || identity.revision.trim().is_empty()
        || identity.artifact.bytes == 0
        || identity.artifact.sha256.len() != 64
        || !identity
            .artifact
            .sha256
            .as_bytes()
            .iter()
            .all(u8::is_ascii_hexdigit)
        || parse_s3_uri(&identity.artifact.uri).is_none()
    {
        return Err(ArtifactCacheError::InvalidIdentity);
    }
    Ok(())
}

fn parse_s3_uri(uri: &str) -> Option<(&str, &str)> {
    let without_scheme = uri.strip_prefix("s3://")?;
    let (bucket, key) = without_scheme.split_once('/')?;
    (!bucket.is_empty() && !key.is_empty()).then_some((bucket, key))
}

fn identity_directory(identity: &ArtifactIdentity) -> String {
    let mut hasher = Sha256::new();
    for part in [
        identity.source.as_bytes(),
        identity.package.as_bytes(),
        identity.revision.as_bytes(),
        identity.artifact.uri.as_bytes(),
        identity.artifact.sha256.as_bytes(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.update(identity.artifact.bytes.to_be_bytes());
    format!("{:x}", hasher.finalize())
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_state(state: &Mutex<CacheState>) -> MutexGuard<'_, CacheState> {
    lock_mutex(state)
}

struct CapacityReservation {
    state: Arc<Mutex<CacheState>>,
    bytes: u64,
    active: bool,
}

impl CapacityReservation {
    fn reserve(
        state: Arc<Mutex<CacheState>>,
        tmp_capacity_bytes: u64,
        bytes: u64,
    ) -> Result<Self, ArtifactCacheError> {
        {
            let mut usage = lock_state(&state);
            if usage.poisoned {
                return Err(ArtifactCacheError::Filesystem);
            }
            let required = usage
                .resident_bytes
                .checked_add(usage.incoming_temp_bytes)
                .and_then(|used| used.checked_add(bytes))
                .ok_or(ArtifactCacheError::CapacityExceeded)?;
            if required > tmp_capacity_bytes {
                return Err(ArtifactCacheError::CapacityExceeded);
            }
            usage.incoming_temp_bytes += bytes;
        }
        Ok(Self {
            state,
            bytes,
            active: true,
        })
    }

    fn commit_if_available(&mut self) -> Result<(), ArtifactCacheError> {
        let mut state = lock_state(&self.state);
        if state.poisoned {
            return Err(ArtifactCacheError::Filesystem);
        }
        state.incoming_temp_bytes -= self.bytes;
        state.resident_bytes += self.bytes;
        self.active = false;
        Ok(())
    }

    fn commit(&mut self) {
        let mut state = lock_state(&self.state);
        state.incoming_temp_bytes -= self.bytes;
        state.resident_bytes += self.bytes;
        self.active = false;
    }
}

impl Drop for CapacityReservation {
    fn drop(&mut self) {
        if self.active {
            let mut state = lock_state(&self.state);
            state.incoming_temp_bytes -= self.bytes;
        }
    }
}

struct PublicationJournal {
    filesystem: Arc<dyn ArtifactCacheFilesystem>,
    cache_state: Arc<Mutex<CacheState>>,
    state: Mutex<PublicationJournalState>,
}

struct PublicationJournalState {
    final_path: PathBuf,
    temp_path: Option<PathBuf>,
    reservation: Option<CapacityReservation>,
    publication_started: bool,
    committed: bool,
}

impl PublicationJournal {
    fn new(
        final_path: PathBuf,
        filesystem: Arc<dyn ArtifactCacheFilesystem>,
        cache_state: Arc<Mutex<CacheState>>,
    ) -> Self {
        Self {
            filesystem,
            cache_state,
            state: Mutex::new(PublicationJournalState {
                final_path,
                temp_path: None,
                reservation: None,
                publication_started: false,
                committed: false,
            }),
        }
    }

    fn final_path(&self) -> PathBuf {
        lock_mutex(&self.state).final_path.clone()
    }

    fn track_temp(&self, temp_path: PathBuf, reservation: CapacityReservation) {
        let mut state = lock_mutex(&self.state);
        state.temp_path = Some(temp_path);
        state.reservation = Some(reservation);
    }

    fn start_publication(&self) {
        lock_mutex(&self.state).publication_started = true;
    }

    fn commit_if_available(&self) -> Result<(), ArtifactCacheError> {
        let mut state = lock_mutex(&self.state);
        state
            .reservation
            .as_mut()
            .ok_or(ArtifactCacheError::Filesystem)?
            .commit_if_available()?;
        state.committed = true;
        Ok(())
    }

    fn complete_success(&self) {
        let mut state = lock_mutex(&self.state);
        state.temp_path = None;
        state.reservation = None;
    }

    fn reconcile_failure(&self) {
        let (final_path, temp_path, publication_started, committed) = {
            let state = lock_mutex(&self.state);
            (
                state.final_path.clone(),
                state.temp_path.clone(),
                state.publication_started,
                state.committed,
            )
        };
        if committed {
            return;
        }

        let final_cleanup_failed = publication_started && !self.remove_file(&final_path);
        if let Some(temp_path) = temp_path {
            self.remove_file(&temp_path);
        }

        let mut state = lock_mutex(&self.state);
        if state.committed {
            return;
        }
        state.temp_path = None;
        if final_cleanup_failed {
            if let Some(mut reservation) = state.reservation.take() {
                reservation.commit();
            }
            state.committed = true;
        } else {
            state.reservation = None;
        }
    }

    fn remove_file(&self, path: &Path) -> bool {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.filesystem.remove_file(path)
        })) {
            Ok(Ok(())) => true,
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => true,
            Ok(Err(_)) | Err(_) => {
                lock_state(&self.cache_state).poisoned = true;
                false
            }
        }
    }
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    written: u64,
    expected: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W, expected: u64) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
            expected,
        }
    }

    fn finish(self) -> (W, u64, String) {
        (
            self.inner,
            self.written,
            format!("{:x}", self.hasher.finalize()),
        )
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for HashingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let remaining = self.expected.saturating_sub(self.written);
        if remaining == 0 && !buffer.is_empty() {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "artifact exceeded its declared size",
            )));
        }
        let allowed = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        match Pin::new(&mut self.inner).poll_write(context, &buffer[..allowed]) {
            Poll::Ready(Ok(written)) => {
                self.hasher.update(&buffer[..written]);
                self.written += written as u64;
                Poll::Ready(Ok(written))
            }
            result => result,
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

impl fmt::Debug for ArtifactCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactCache")
            .field("owned_root", &self.inner.owned_root)
            .field("tmp_capacity_bytes", &self.inner.tmp_capacity_bytes)
            .finish_non_exhaustive()
    }
}
