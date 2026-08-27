use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier as SyncBarrier, Mutex};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use spur_context_service::artifact_cache::{
    ArtifactCache, ArtifactFetchError, ArtifactFetcher, ArtifactIdentity, ArtifactStream,
};
use spur_context_service::serving_registry::ArtifactRef;

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

    let first_path = first.await.expect("first task should join").unwrap();
    let second_path = second.await.expect("second task should join").unwrap();
    assert_eq!(first_path, second_path);
    assert_eq!(std::fs::read(first_path).unwrap(), body);
    assert_eq!(fetcher.fetch_count(), 1);
    assert!(temp_files(root.path()).is_empty());
}

#[tokio::test]
async fn cache_identity_contains_generation_full_package_identity_and_sha256() {
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
    let second = artifact(7, "github", "package", "1.0.0", uri, body);
    let next_generation = artifact(8, "crates.io", "package", "1.0.0", uri, body);

    assert_ne!(cache.final_path(&first), cache.final_path(&second));
    assert_ne!(cache.final_path(&first), cache.final_path(&next_generation));
    cache.materialize(&first).await.unwrap();
    cache.materialize(&second).await.unwrap();
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

    let error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(error.code(), "artifact_integrity_mismatch");
    assert!(error.is_retryable());
    assert!(!cache.final_path(&artifact).exists());
    assert!(temp_files(root.path()).is_empty());

    let retry_error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(retry_error.code(), "artifact_integrity_mismatch");
    assert_eq!(fetcher.fetch_count(), 2);
}

#[tokio::test]
async fn mismatched_declared_bytes_leave_no_final_file() {
    let root = TestDir::new("size-mismatch");
    let uri = "s3://artifacts/size-mismatch";
    let body = b"short";
    let fetcher = TestFetcher::default().with_body(uri, body.to_vec());
    let mut artifact = artifact(4, "crates.io", "package", "rev", uri, body);
    artifact.artifact.bytes += 1;
    let cache = ArtifactCache::new(root.path(), artifact.artifact.bytes, Arc::new(fetcher))
        .expect("cache should initialize");

    let error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(error.code(), "artifact_integrity_mismatch");
    assert!(error.is_retryable());
    assert!(!cache.final_path(&artifact).exists());
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

    let first_path = cache.materialize(&first).await.unwrap();
    let first_generation_dir = first_path
        .parent()
        .and_then(Path::parent)
        .expect("final path should be nested below its generation")
        .to_path_buf();
    let second_path = cache.materialize(&second).await.unwrap();

    assert!(!first_generation_dir.exists());
    assert!(!first_path.exists());
    assert_eq!(std::fs::read(second_path).unwrap(), second_body);
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
    let old_path = cache.materialize(&old).await.unwrap();

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
    let retried_path = cache.materialize(&new).await.unwrap();
    assert_eq!(std::fs::read(retried_path).unwrap(), new_body);
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

    let error = cache.materialize(&artifact).await.unwrap_err();
    assert_eq!(error.code(), "invalid_artifact_identity");
    assert!(error.is_retryable());
    assert_eq!(fetcher.fetch_count(), 0);
    assert!(!cache.final_path(&artifact).exists());
}
