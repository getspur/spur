use std::collections::HashMap;
use std::future::{poll_fn, Future};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier as SyncBarrier, Mutex};
use std::task::Poll;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use spur_context_service::artifact_cache::{
    ArtifactCache, ArtifactCacheFilesystem, ArtifactFetchError, ArtifactFetcher, ArtifactIdentity,
    ArtifactStream,
};
use spur_context_service::serving_registry::ArtifactRef;
use tokio::sync::Notify;

#[derive(Clone)]
enum FetchResponse {
    Body(Vec<u8>),
    Error(String),
}

#[derive(Clone)]
struct FetchPause {
    uri: String,
    started: Arc<SyncBarrier>,
    release: Arc<SyncBarrier>,
}

impl FetchPause {
    fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            started: Arc::new(SyncBarrier::new(2)),
            release: Arc::new(SyncBarrier::new(2)),
        }
    }

    async fn wait_started(&self) {
        let barrier = Arc::clone(&self.started);
        tokio::task::spawn_blocking(move || barrier.wait())
            .await
            .expect("started barrier should not panic");
    }

    async fn release(&self) {
        let barrier = Arc::clone(&self.release);
        tokio::task::spawn_blocking(move || barrier.wait())
            .await
            .expect("release barrier should not panic");
    }
}

#[derive(Clone, Default)]
struct TestFetcher {
    responses: Arc<Mutex<HashMap<String, FetchResponse>>>,
    calls: Arc<AtomicUsize>,
    pause: Arc<Mutex<Option<FetchPause>>>,
}

impl TestFetcher {
    fn with_body(self, uri: &str, body: impl Into<Vec<u8>>) -> Self {
        self.set_response(uri, FetchResponse::Body(body.into()));
        self
    }

    fn with_error(self, uri: &str, message: impl Into<String>) -> Self {
        self.set_response(uri, FetchResponse::Error(message.into()));
        self
    }

    fn with_pause(self, pause: FetchPause) -> Self {
        *self
            .pause
            .lock()
            .expect("pause mutex should not be poisoned") = Some(pause);
        self
    }

    fn set_body(&self, uri: &str, body: impl Into<Vec<u8>>) {
        self.set_response(uri, FetchResponse::Body(body.into()));
    }

    fn set_response(&self, uri: &str, response: FetchResponse) {
        self.responses
            .lock()
            .expect("response mutex should not be poisoned")
            .insert(uri.to_owned(), response);
    }

    fn fetch_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ArtifactFetcher for TestFetcher {
    async fn fetch(&self, uri: &str) -> Result<ArtifactStream, ArtifactFetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = self
            .responses
            .lock()
            .expect("response mutex should not be poisoned")
            .get(uri)
            .cloned()
            .unwrap_or_else(|| FetchResponse::Error(format!("missing test object at {uri}")));
        let pause = self
            .pause
            .lock()
            .expect("pause mutex should not be poisoned")
            .clone()
            .filter(|pause| pause.uri == uri);
        if let Some(pause) = pause {
            let started = Arc::clone(&pause.started);
            tokio::task::spawn_blocking(move || started.wait())
                .await
                .expect("fetch-start barrier should not panic");
            let release = Arc::clone(&pause.release);
            tokio::task::spawn_blocking(move || release.wait())
                .await
                .expect("fetch-release barrier should not panic");
        }

        match response {
            FetchResponse::Body(body) => Ok(Box::pin(Cursor::new(body))),
            FetchResponse::Error(message) => Err(ArtifactFetchError::new(message)),
        }
    }
}

#[derive(Clone)]
struct CleanupPause {
    started: Arc<Notify>,
    armed: Arc<AtomicUsize>,
}

impl CleanupPause {
    fn new() -> Self {
        Self {
            started: Arc::new(Notify::new()),
            armed: Arc::new(AtomicUsize::new(1)),
        }
    }

    async fn wait_started(&self) {
        self.started.notified().await;
    }
}

#[derive(Clone, Default)]
struct ControlledFilesystem {
    generation_cleanup_calls: Arc<AtomicUsize>,
    fail_generation_cleanups: Arc<AtomicUsize>,
    fail_temp_cleanups: Arc<AtomicUsize>,
    cleanup_pause: Arc<Mutex<Option<CleanupPause>>>,
}

impl ControlledFilesystem {
    fn fail_next_generation_cleanup(&self) {
        self.fail_generation_cleanups.store(1, Ordering::SeqCst);
    }

    fn fail_next_temp_cleanup(&self) {
        self.fail_temp_cleanups.store(1, Ordering::SeqCst);
    }

    fn pause_next_generation_cleanup(&self, pause: CleanupPause) {
        *self
            .cleanup_pause
            .lock()
            .expect("cleanup-pause mutex should not be poisoned") = Some(pause);
    }

    fn generation_cleanup_calls(&self) -> usize {
        self.generation_cleanup_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ArtifactCacheFilesystem for ControlledFilesystem {
    async fn remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
        self.generation_cleanup_calls.fetch_add(1, Ordering::SeqCst);
        if self
            .fail_generation_cleanups
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "deterministic generation cleanup failure",
            ));
        }
        let pause = self
            .cleanup_pause
            .lock()
            .expect("cleanup-pause mutex should not be poisoned")
            .clone();
        if let Some(pause) = pause {
            if pause.armed.swap(0, Ordering::SeqCst) == 1 {
                pause.started.notify_one();
                std::future::pending::<()>().await;
            }
        }
        tokio::fs::remove_dir_all(path).await
    }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        if self
            .fail_temp_cleanups
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "deterministic temp cleanup failure",
            ));
        }
        std::fs::remove_file(path)
    }
}

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "spur-artifact-cache-test-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("test directory should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn artifact(
    generation: i64,
    source: &str,
    package: &str,
    revision: &str,
    uri: &str,
    body: &[u8],
) -> ArtifactIdentity {
    ArtifactIdentity {
        generation,
        source: source.to_owned(),
        package: package.to_owned(),
        revision: revision.to_owned(),
        artifact: ArtifactRef {
            uri: uri.to_owned(),
            sha256: sha256(body),
            bytes: body.len() as u64,
        },
    }
}

fn temp_files(root: &Path) -> Vec<PathBuf> {
    let mut directories = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".tmp"))
            {
                files.push(path);
            }
        }
    }
    files
}

async fn activate(cache: &ArtifactCache, generation: i64) {
    cache
        .activate_generation(generation)
        .await
        .expect("validated live generation should activate");
}

#[tokio::test]
async fn coalesces_same_artifact_download() {
    let root = TestDir::new("coalesce");
    let uri = "s3://artifacts/source/package/revision/graph.json";
    let body = b"generation-scoped artifact";
    let pause = FetchPause::new(uri);
    let fetcher = TestFetcher::default()
        .with_body(uri, body.to_vec())
        .with_pause(pause.clone());
    let artifact = artifact(7, "crates.io", "package", "1.0.0", uri, body);
    let cache = ArtifactCache::new(
        root.path(),
        artifact.artifact.bytes,
        Arc::new(fetcher.clone()),
    )
    .expect("cache should initialize");
    activate(&cache, artifact.generation).await;

    let first_cache = cache.clone();
    let first_artifact = artifact.clone();
    let first = tokio::spawn(async move { first_cache.materialize(&first_artifact).await });
    let second_cache = cache.clone();
    let second_artifact = artifact.clone();
    let second = tokio::spawn(async move { second_cache.materialize(&second_artifact).await });

    pause.wait_started().await;
    assert!(!cache.final_path(&artifact).exists());
    assert_eq!(temp_files(root.path()).len(), 1);
    pause.release().await;

    let first_artifact = first.await.expect("first task should join").unwrap();
    let second_artifact = second.await.expect("second task should join").unwrap();
    assert_eq!(first_artifact.path(), second_artifact.path());
    assert_eq!(std::fs::read(first_artifact.path()).unwrap(), body);
    assert_eq!(fetcher.fetch_count(), 1);
    assert!(temp_files(root.path()).is_empty());
}

#[tokio::test]
async fn cache_identity_paths_distinguish_every_identity_component() {
    let root = TestDir::new("identity");
    let body = b"same immutable bytes";
    let uri = "s3://artifacts/shared/object";
    let fetcher = TestFetcher::default().with_body(uri, body.to_vec());
    let cache = ArtifactCache::new(
        root.path(),
        body.len() as u64 * 2,
        Arc::new(fetcher.clone()),
    )
    .expect("cache should initialize");
    let first = artifact(7, "crates.io", "package", "1.0.0", uri, body);
    let source = artifact(7, "github", "package", "1.0.0", uri, body);
    let package = artifact(7, "crates.io", "other-package", "1.0.0", uri, body);
    let revision = artifact(7, "crates.io", "package", "2.0.0", uri, body);
    let sha = artifact(
        7,
        "crates.io",
        "package",
        "1.0.0",
        uri,
        b"different immutable bytes",
    );
    let next_generation = artifact(8, "crates.io", "package", "1.0.0", uri, body);

    assert_ne!(cache.final_path(&first), cache.final_path(&source));
    assert_ne!(cache.final_path(&first), cache.final_path(&package));
    assert_ne!(cache.final_path(&first), cache.final_path(&revision));
    assert_ne!(cache.final_path(&first), cache.final_path(&sha));
    assert_ne!(cache.final_path(&first), cache.final_path(&next_generation));
    activate(&cache, first.generation).await;
    cache.materialize(&first).await.unwrap();
    cache.materialize(&source).await.unwrap();
    assert_eq!(fetcher.fetch_count(), 2);
}

#[tokio::test]
async fn corrupt_artifact_leaves_no_final_or_reusable_file() {
    let root = TestDir::new("corrupt");
    let uri = "s3://artifacts/corrupt";
    let expected = b"right";
    let fetcher = TestFetcher::default().with_body(uri, b"wrong".to_vec());
    let artifact = artifact(3, "crates.io", "package", "rev", uri, expected);
    let cache = ArtifactCache::new(
        root.path(),
        artifact.artifact.bytes,
        Arc::new(fetcher.clone()),
    )
    .expect("cache should initialize");
    activate(&cache, artifact.generation).await;

    let error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(error.code(), "artifact_integrity_mismatch");
    assert!(error.is_retryable());
    assert_eq!(error.to_string(), "artifact failed integrity validation");
    assert!(!cache.final_path(&artifact).exists());
    assert!(temp_files(root.path()).is_empty());

    let retry_error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(retry_error.code(), "artifact_integrity_mismatch");
    assert_eq!(fetcher.fetch_count(), 2);
}

#[tokio::test]
async fn object_larger_than_declaration_leaves_no_final_or_reusable_temp_file() {
    let root = TestDir::new("oversized-object");
    let uri = "s3://artifacts/oversized-object";
    let declared = b"right";
    let oversized = b"right-extra-bytes";
    let fetcher = TestFetcher::default().with_body(uri, oversized.to_vec());
    let artifact = artifact(4, "crates.io", "package", "rev", uri, declared);
    let cache = ArtifactCache::new(
        root.path(),
        artifact.artifact.bytes,
        Arc::new(fetcher.clone()),
    )
    .expect("cache should initialize");
    activate(&cache, artifact.generation).await;

    let error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(error.code(), "artifact_integrity_mismatch");
    assert!(error.is_retryable());
    assert_eq!(error.to_string(), "artifact failed integrity validation");
    assert!(!cache.final_path(&artifact).exists());
    assert!(temp_files(root.path()).is_empty());

    fetcher.set_body(uri, declared.to_vec());
    let retried = cache.materialize(&artifact).await.unwrap();
    assert_eq!(std::fs::read(retried.path()).unwrap(), declared);
    assert_eq!(fetcher.fetch_count(), 2);
    assert!(temp_files(root.path()).is_empty());
}

#[tokio::test]
async fn generation_replacement_removes_prior_owned_generation_directory() {
    let root = TestDir::new("generation");
    let first_uri = "s3://artifacts/generation-1";
    let second_uri = "s3://artifacts/generation-2";
    let first_body = b"one";
    let second_body = b"two";
    let fetcher = TestFetcher::default()
        .with_body(first_uri, first_body.to_vec())
        .with_body(second_uri, second_body.to_vec());
    let capacity = first_body.len().max(second_body.len()) as u64;
    let cache = ArtifactCache::new(root.path(), capacity, Arc::new(fetcher))
        .expect("cache should initialize");
    let first = artifact(1, "crates.io", "package", "rev", first_uri, first_body);
    let second = artifact(2, "crates.io", "package", "rev", second_uri, second_body);

    activate(&cache, first.generation).await;
    let first_artifact = cache.materialize(&first).await.unwrap();
    let first_path = first_artifact.path().to_path_buf();
    let first_generation_dir = first_path
        .parent()
        .and_then(Path::parent)
        .expect("final path should be nested below its generation")
        .to_path_buf();
    drop(first_artifact);
    activate(&cache, second.generation).await;
    let second_artifact = cache.materialize(&second).await.unwrap();

    assert!(!first_generation_dir.exists());
    assert!(!first_path.exists());
    assert_eq!(std::fs::read(second_artifact.path()).unwrap(), second_body);
}

#[tokio::test]
async fn explicit_activation_rejects_late_stale_materialization_without_touching_active_bytes() {
    let root = TestDir::new("late-stale");
    let old_uri = "s3://artifacts/generation-red";
    let active_uri = "s3://artifacts/generation-blue";
    let old_body = b"red";
    let active_body = b"blue";
    let fetcher = TestFetcher::default()
        .with_body(old_uri, old_body.to_vec())
        .with_body(active_uri, active_body.to_vec());
    let cache = ArtifactCache::new(
        root.path(),
        active_body.len() as u64,
        Arc::new(fetcher.clone()),
    )
    .expect("cache should initialize");
    let old = artifact(41, "crates.io", "package", "rev", old_uri, old_body);
    let active = artifact(-7, "crates.io", "package", "rev", active_uri, active_body);

    activate(&cache, old.generation).await;
    drop(cache.materialize(&old).await.unwrap());
    activate(&cache, active.generation).await;
    let active_artifact = cache.materialize(&active).await.unwrap();

    let error = cache.materialize(&old).await.unwrap_err();
    assert_eq!(error.code(), "artifact_generation_unavailable");
    assert!(error.is_retryable());
    assert_eq!(std::fs::read(active_artifact.path()).unwrap(), active_body);
    assert_eq!(fetcher.fetch_count(), 2);
}

#[tokio::test]
async fn generation_transition_waits_for_materialized_artifact_lease() {
    let root = TestDir::new("consumer-lease");
    let old_uri = "s3://artifacts/leased-red";
    let next_uri = "s3://artifacts/leased-blue";
    let old_body = b"leased-red";
    let next_body = b"leased-blue";
    let fetcher = TestFetcher::default()
        .with_body(old_uri, old_body.to_vec())
        .with_body(next_uri, next_body.to_vec());
    let cache = ArtifactCache::new(root.path(), next_body.len() as u64, Arc::new(fetcher))
        .expect("cache should initialize");
    let old = artifact(41, "crates.io", "package", "rev", old_uri, old_body);
    let next = artifact(-7, "crates.io", "package", "rev", next_uri, next_body);

    activate(&cache, old.generation).await;
    let old_artifact = cache.materialize(&old).await.unwrap();
    let old_path = old_artifact.path().to_path_buf();
    let mut transition = Box::pin(cache.activate_generation(next.generation));
    poll_fn(|context| match transition.as_mut().poll(context) {
        Poll::Pending => Poll::Ready(()),
        Poll::Ready(result) => panic!("transition completed while lease was held: {result:?}"),
    })
    .await;
    assert!(old_path.exists());

    drop(old_artifact);
    transition.await.unwrap();
    assert!(!old_path.exists());
    let next_artifact = cache.materialize(&next).await.unwrap();
    assert_eq!(std::fs::read(next_artifact.path()).unwrap(), next_body);
}

#[tokio::test]
async fn generation_cleanup_failure_preserves_state_and_retry_cannot_skip_cleanup() {
    let root = TestDir::new("cleanup-retry");
    let old_uri = "s3://artifacts/cleanup-old";
    let next_uri = "s3://artifacts/cleanup-next";
    let old_body = b"old";
    let next_body = b"next";
    let fetcher = TestFetcher::default()
        .with_body(old_uri, old_body.to_vec())
        .with_body(next_uri, next_body.to_vec());
    let filesystem = ControlledFilesystem::default();
    let cache = ArtifactCache::new_with_filesystem(
        root.path(),
        next_body.len() as u64,
        Arc::new(fetcher.clone()),
        Arc::new(filesystem.clone()),
    )
    .expect("cache should initialize");
    let old = artifact(1, "crates.io", "package", "rev", old_uri, old_body);
    let next = artifact(2, "crates.io", "package", "rev", next_uri, next_body);

    activate(&cache, old.generation).await;
    let old_artifact = cache.materialize(&old).await.unwrap();
    let old_path = old_artifact.path().to_path_buf();
    drop(old_artifact);
    filesystem.fail_next_generation_cleanup();

    let error = cache
        .activate_generation(next.generation)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "artifact_cache_unavailable");
    assert!(old_path.exists());
    assert_eq!(cache.usage().resident_bytes, old_body.len() as u64);
    assert_eq!(filesystem.generation_cleanup_calls(), 1);
    assert_eq!(
        cache.materialize(&old).await.unwrap_err().code(),
        "artifact_generation_unavailable"
    );
    assert_eq!(
        cache.materialize(&next).await.unwrap_err().code(),
        "artifact_generation_unavailable"
    );
    assert_eq!(fetcher.fetch_count(), 1);

    activate(&cache, next.generation).await;
    assert_eq!(filesystem.generation_cleanup_calls(), 2);
    assert!(!old_path.exists());
    assert_eq!(cache.usage().resident_bytes, 0);
    let next_artifact = cache.materialize(&next).await.unwrap();
    assert_eq!(std::fs::read(next_artifact.path()).unwrap(), next_body);
}

#[tokio::test]
async fn cancelled_generation_cleanup_remains_pending_until_retry() {
    let root = TestDir::new("cleanup-cancel");
    let old_uri = "s3://artifacts/cancel-old";
    let next_uri = "s3://artifacts/cancel-next";
    let old_body = b"old";
    let next_body = b"next";
    let fetcher = TestFetcher::default()
        .with_body(old_uri, old_body.to_vec())
        .with_body(next_uri, next_body.to_vec());
    let filesystem = ControlledFilesystem::default();
    let pause = CleanupPause::new();
    let cache = ArtifactCache::new_with_filesystem(
        root.path(),
        next_body.len() as u64,
        Arc::new(fetcher.clone()),
        Arc::new(filesystem.clone()),
    )
    .expect("cache should initialize");
    let old = artifact(1, "crates.io", "package", "rev", old_uri, old_body);
    let next = artifact(2, "crates.io", "package", "rev", next_uri, next_body);

    activate(&cache, old.generation).await;
    let old_artifact = cache.materialize(&old).await.unwrap();
    let old_path = old_artifact.path().to_path_buf();
    drop(old_artifact);
    filesystem.pause_next_generation_cleanup(pause.clone());
    let transition_cache = cache.clone();
    let transition =
        tokio::spawn(async move { transition_cache.activate_generation(next.generation).await });
    pause.wait_started().await;
    transition.abort();
    assert!(transition.await.unwrap_err().is_cancelled());

    assert!(old_path.exists());
    assert_eq!(cache.usage().resident_bytes, old_body.len() as u64);
    assert_eq!(
        cache.materialize(&old).await.unwrap_err().code(),
        "artifact_generation_unavailable"
    );

    activate(&cache, next.generation).await;
    assert!(!old_path.exists());
    assert_eq!(cache.usage().resident_bytes, 0);
    assert_eq!(fetcher.fetch_count(), 1);
}

#[tokio::test]
async fn failed_temp_cleanup_poisons_serving_until_explicit_cleanup() {
    let root = TestDir::new("temp-cleanup-failure");
    let uri = "s3://artifacts/temp-cleanup";
    let expected = b"right";
    let fetcher = TestFetcher::default().with_body(uri, b"wrong".to_vec());
    let filesystem = ControlledFilesystem::default();
    let cache = ArtifactCache::new_with_filesystem(
        root.path(),
        expected.len() as u64,
        Arc::new(fetcher.clone()),
        Arc::new(filesystem.clone()),
    )
    .expect("cache should initialize");
    let artifact = artifact(3, "crates.io", "package", "rev", uri, expected);

    activate(&cache, artifact.generation).await;
    filesystem.fail_next_temp_cleanup();
    assert_eq!(
        cache.materialize(&artifact).await.unwrap_err().code(),
        "artifact_integrity_mismatch"
    );
    assert_eq!(temp_files(root.path()).len(), 1);
    assert!(cache.usage().poisoned);

    let blocked = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(blocked.code(), "artifact_cache_unavailable");
    assert_eq!(fetcher.fetch_count(), 1);

    activate(&cache, artifact.generation).await;
    assert!(temp_files(root.path()).is_empty());
    assert!(!cache.usage().poisoned);
    fetcher.set_body(uri, expected.to_vec());
    let recovered = cache.materialize(&artifact).await.unwrap();
    assert_eq!(std::fs::read(recovered.path()).unwrap(), expected);
}

#[tokio::test]
async fn exact_capacity_boundary_counts_the_in_flight_temp_file() {
    // Direct witnesses from PRE solves sol_855f809bc87a44e2 and sol_6e504aea270b40a0.
    const RESIDENT_BYTES: usize = 1000;
    const INCOMING_BYTES: usize = 500;
    const OVER_CAPACITY_BYTES: usize = 501;
    const TMP_CAPACITY_BYTES: u64 = 1500;

    let exact_root = TestDir::new("capacity-exact");
    let resident_uri = "s3://artifacts/resident";
    let incoming_uri = "s3://artifacts/incoming";
    let resident_body = vec![b'r'; RESIDENT_BYTES];
    let incoming_body = vec![b'i'; INCOMING_BYTES];
    let pause = FetchPause::new(incoming_uri);
    let fetcher = TestFetcher::default()
        .with_body(resident_uri, resident_body.clone())
        .with_body(incoming_uri, incoming_body.clone())
        .with_pause(pause.clone());
    let cache = ArtifactCache::new(exact_root.path(), TMP_CAPACITY_BYTES, Arc::new(fetcher))
        .expect("cache should initialize");
    let resident = artifact(
        11,
        "crates.io",
        "resident",
        "rev",
        resident_uri,
        &resident_body,
    );
    let incoming = artifact(
        11,
        "crates.io",
        "incoming",
        "rev",
        incoming_uri,
        &incoming_body,
    );
    activate(&cache, resident.generation).await;
    cache.materialize(&resident).await.unwrap();

    let incoming_cache = cache.clone();
    let incoming_clone = incoming.clone();
    let incoming_task =
        tokio::spawn(async move { incoming_cache.materialize(&incoming_clone).await });
    pause.wait_started().await;
    let usage = cache.usage();
    assert_eq!(usage.resident_bytes, RESIDENT_BYTES as u64);
    assert_eq!(usage.incoming_temp_bytes, INCOMING_BYTES as u64);
    assert_eq!(
        usage.resident_bytes + usage.incoming_temp_bytes,
        TMP_CAPACITY_BYTES
    );
    assert!(!cache.final_path(&incoming).exists());
    pause.release().await;
    incoming_task
        .await
        .expect("incoming task should join")
        .unwrap();
    assert_eq!(cache.usage().resident_bytes, TMP_CAPACITY_BYTES);
    assert_eq!(cache.usage().incoming_temp_bytes, 0);

    let over_root = TestDir::new("capacity-over");
    let over_uri = "s3://artifacts/over";
    let over_body = vec![b'o'; OVER_CAPACITY_BYTES];
    let over_fetcher = TestFetcher::default()
        .with_body(resident_uri, resident_body.clone())
        .with_body(over_uri, over_body.clone());
    let over_cache = ArtifactCache::new(
        over_root.path(),
        TMP_CAPACITY_BYTES,
        Arc::new(over_fetcher.clone()),
    )
    .expect("cache should initialize");
    let over = artifact(11, "crates.io", "over", "rev", over_uri, &over_body);
    activate(&over_cache, resident.generation).await;
    over_cache.materialize(&resident).await.unwrap();
    let error = over_cache.materialize(&over).await.unwrap_err();
    assert_eq!(error.code(), "artifact_capacity_exceeded");
    assert!(error.is_retryable());
    assert_eq!(over_fetcher.fetch_count(), 1);
    assert!(!over_cache.final_path(&over).exists());
    assert!(temp_files(over_root.path()).is_empty());
}

#[tokio::test]
async fn sanitized_retryable_error_never_falls_back_to_stale_generation() {
    let root = TestDir::new("no-stale-fallback");
    let first_uri = "s3://artifacts/old";
    let missing_uri = "s3://private-bucket/customer-secret/new";
    let old_body = b"old";
    let new_body = b"new";
    let fetcher = TestFetcher::default()
        .with_body(first_uri, old_body.to_vec())
        .with_error(missing_uri, "credential secret-token leaked by backend");
    let cache = ArtifactCache::new(
        root.path(),
        new_body.len() as u64,
        Arc::new(fetcher.clone()),
    )
    .expect("cache should initialize");
    let old = artifact(20, "crates.io", "package", "rev", first_uri, old_body);
    let new = artifact(21, "crates.io", "package", "rev", missing_uri, new_body);
    activate(&cache, old.generation).await;
    let old_artifact = cache.materialize(&old).await.unwrap();
    let old_path = old_artifact.path().to_path_buf();
    drop(old_artifact);
    activate(&cache, new.generation).await;

    let error = cache.materialize(&new).await.unwrap_err();
    assert_eq!(error.code(), "artifact_unavailable");
    assert!(error.is_retryable());
    assert_eq!(error.to_string(), "artifact is temporarily unavailable");
    assert!(!error.to_string().contains("secret"));
    assert!(!error.to_string().contains("private-bucket"));
    assert!(!old_path.exists());
    assert!(!cache.final_path(&new).exists());
    assert!(temp_files(root.path()).is_empty());

    fetcher.set_body(missing_uri, new_body.to_vec());
    let retried = cache.materialize(&new).await.unwrap();
    assert_eq!(std::fs::read(retried.path()).unwrap(), new_body);
    assert_eq!(fetcher.fetch_count(), 3);
}

#[tokio::test]
async fn invalid_identity_is_rejected_before_fetch() {
    let root = TestDir::new("invalid-identity");
    let uri = "s3://artifacts/identity";
    let body = b"identity";
    let fetcher = TestFetcher::default().with_body(uri, body.to_vec());
    let mut artifact = artifact(5, "crates.io", "package", "rev", uri, body);
    artifact.source.clear();
    let cache = ArtifactCache::new(root.path(), body.len() as u64, Arc::new(fetcher.clone()))
        .expect("cache should initialize");
    activate(&cache, artifact.generation).await;

    let error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(error.code(), "invalid_artifact_identity");
    assert!(error.is_retryable());
    assert_eq!(fetcher.fetch_count(), 0);
    assert!(!cache.final_path(&artifact).exists());
}

#[cfg(feature = "artifact-cache-s3")]
#[test]
fn s3_fetcher_is_importable_without_service_feature() {
    use spur_context_service::artifact_cache::S3ArtifactFetcher;

    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<S3ArtifactFetcher>();
}
