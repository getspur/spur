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
use tokio::sync::{OnceCell, OwnedRwLockReadGuard, RwLock};

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
    async fn remove_dir_all(&self, path: &Path) -> std::io::Result<()>;
    fn remove_file(&self, path: &Path) -> std::io::Result<()>;
}

#[derive(Debug, Default)]
struct LocalArtifactCacheFilesystem;

#[async_trait]
impl ArtifactCacheFilesystem for LocalArtifactCacheFilesystem {
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
    entries: HashMap<CacheKey, Arc<OnceCell<Result<PathBuf, ArtifactCacheError>>>>,
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
        let mut authority = self.inner.generation.write().await;
        loop {
            let poisoned = lock_state(&self.inner.state).poisoned;
            if authority.pending.is_none() && authority.active == Some(generation) && !poisoned {
                return Ok(());
            }

            let Some(active) = authority.active else {
                tokio::fs::create_dir_all(self.generation_dir(generation))
                    .await
                    .map_err(|_| ArtifactCacheError::Filesystem)?;
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
            tokio::fs::create_dir_all(self.generation_dir(pending.target))
                .await
                .map_err(|_| ArtifactCacheError::Filesystem)?;
            authority.active = Some(pending.target);
            authority.pending = None;
            if pending.target == generation {
                return Ok(());
            }
        }
    }

    pub async fn materialize(
        &self,
        identity: &ArtifactIdentity,
    ) -> Result<MaterializedArtifact, ArtifactCacheError> {
        validate_identity(identity)?;
        let generation_lease = Arc::clone(&self.inner.generation).read_owned().await;
        if generation_lease.pending.is_some()
            || generation_lease.active != Some(identity.generation)
        {
            return Err(ArtifactCacheError::GenerationUnavailable);
        }
        let key = CacheKey::from(identity);
        let cell = {
            let mut state = lock_state(&self.inner.state);
            if state.poisoned {
                return Err(ArtifactCacheError::Filesystem);
            }
            Arc::clone(
                state
                    .entries
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(OnceCell::new())),
            )
        };

        let result = cell
            .get_or_init(|| async { self.fetch_and_publish(identity).await })
            .await
            .clone();
        if result.is_err() {
            let mut state = lock_state(&self.inner.state);
            if state
                .entries
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &cell))
            {
                state.entries.remove(&key);
            }
        }
        result.map(|path| MaterializedArtifact {
            path,
            generation: identity.generation,
            _generation_lease: generation_lease,
        })
    }

    async fn fetch_and_publish(
        &self,
        identity: &ArtifactIdentity,
    ) -> Result<PathBuf, ArtifactCacheError> {
        let final_path = self.final_path(identity);
        let parent = final_path.parent().ok_or(ArtifactCacheError::Filesystem)?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;

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
        let mut temp_guard = TempFileGuard::new(
            temp_path.clone(),
            Arc::clone(&self.inner.filesystem),
            Arc::clone(&self.inner.state),
        );
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;
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

        tokio::fs::rename(&temp_path, &final_path)
            .await
            .map_err(|_| ArtifactCacheError::Filesystem)?;
        temp_guard.disarm();
        reservation.commit();
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

fn lock_state(state: &Mutex<CacheState>) -> MutexGuard<'_, CacheState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

    fn commit(mut self) {
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

struct TempFileGuard {
    path: PathBuf,
    filesystem: Arc<dyn ArtifactCacheFilesystem>,
    state: Arc<Mutex<CacheState>>,
    active: bool,
}

impl TempFileGuard {
    fn new(
        path: PathBuf,
        filesystem: Arc<dyn ArtifactCacheFilesystem>,
        state: Arc<Mutex<CacheState>>,
    ) -> Self {
        Self {
            path,
            filesystem,
            state,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            match self.filesystem.remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => lock_state(&self.state).poisoned = true,
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
