#![cfg(feature = "worker")]

use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde_json::Value;
use sha2::Digest as _;
use spur_context_service::jobs::{
    CreateJobOutcome, CreateJobRequest, JobKey, JobRecord, JobStatus, JobStore, JobsError,
};
use spur_context_service::worker::{
    build_graph, fetch_source, fetch_source_with_bronze_services, handle_spot_interruption,
    retrieve_bronze_source_by_coordinate, run_job_and_record_with_services,
    upload_with_owned_catalog_lease, BronzeArchiveStore, BronzeRawSource, BronzeRawSourceRegistry,
    CatalogDownload, CatalogLease, CatalogLeaseStore, JobEnv, StageTracker, WorkerError,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn job_env_from_env_reads_catalog_dsn() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let _env = EnvGuard::set_all([
        ("TASK_TOKEN", "task-token"),
        ("JOB_ID", "job-123"),
        ("PACKAGE", "serde"),
        ("REVISION", "1.0.197"),
        ("SOURCE", "registry:crates-io"),
        (
            "SOURCE_URL",
            "https://crates.io/api/v1/crates/serde/1.0.197/download",
        ),
        ("SOURCE_KIND", "tarball"),
        ("SPUR_CATALOG_DSN", "sqlite:/tmp/catalog.sqlite"),
    ]);

    let env = JobEnv::from_env()?;

    assert_eq!(env.task_token, "task-token");
    assert_eq!(env.job_id, "job-123");
    assert_eq!(env.package, "serde");
    assert_eq!(env.revision, "1.0.197");
    assert_eq!(env.source, "registry:crates-io");
    assert_eq!(
        env.source_url,
        "https://crates.io/api/v1/crates/serde/1.0.197/download"
    );
    assert_eq!(env.source_kind, "tarball");
    assert_eq!(env.catalog_dsn, "sqlite:/tmp/catalog.sqlite");
    Ok(())
}

#[test]
fn fetch_source_downloads_and_extracts_tarball() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvGuard::set_all([("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE", "1")]);
    let root = unique_temp_dir("worker-tarball")?;
    let fixture = root.join("fixture").join("demo-0.1.0");
    fs::create_dir_all(fixture.join("src")).context("create fixture")?;
    fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .context("write manifest")?;
    fs::write(fixture.join("src/lib.rs"), "pub fn demo() {}\n").context("write lib")?;

    let archive = root.join("demo.tar.gz");
    create_tarball(root.join("fixture").as_path(), &archive)?;
    let source_url = serve_once(fs::read(&archive).context("read archive")?);

    let fetched = fetch_source(&source_url, "tarball", "0.1.0", &root.join("fetch"))?;

    assert!(fetched.join("Cargo.toml").is_file());
    assert_eq!(
        fs::read_to_string(fetched.join("src/lib.rs")).context("read fetched lib")?,
        "pub fn demo() {}\n"
    );
    Ok(())
}

#[tokio::test]
async fn bronze_fetch_uploads_archive_and_registers_raw_source() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvGuard::set_all([("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE", "1")]);
    let root = unique_temp_dir("worker-bronze-upload")?;
    let archive = demo_tarball(&root)?;
    let source_url = serve_once(fs::read(&archive).context("read archive")?);
    let registry = FakeBronzeRegistry::default();
    let store = FakeBronzeArchiveStore::default();
    let env = demo_job_env(&source_url);

    let fetched =
        fetch_source_with_bronze_services(&env, &root.join("fetch"), &registry, &store).await?;

    assert!(fetched.join("Cargo.toml").is_file());
    assert_eq!(
        store.uploaded_keys(),
        ["bronze/registry:crates-io/demo/0.1.0/source.tar.gz"]
    );
    let rows = registry.rows();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.source, "registry:crates-io");
    assert_eq!(row.package, "demo");
    assert_eq!(row.version, "0.1.0");
    assert_eq!(row.revision_kind, "semver");
    assert_eq!(row.semver_major, Some(0));
    assert_eq!(row.semver_minor, Some(1));
    assert_eq!(row.semver_patch, Some(0));
    assert_eq!(row.source_kind, "tarball");
    assert_eq!(row.source_url, source_url);
    assert_eq!(
        row.s3_uri,
        "s3://bronze-test/bronze/registry:crates-io/demo/0.1.0/source.tar.gz"
    );
    assert_eq!(row.bytes, fs::metadata(&archive)?.len());
    assert_eq!(row.fetch_status, "success");
    assert_eq!(
        store.sha256_for(&row.s3_uri),
        Some(row.content_sha256.clone())
    );
    Ok(())
}

#[tokio::test]
async fn retrieve_bronze_source_by_coordinate_extracts_registered_archive() -> Result<()> {
    let root = unique_temp_dir("worker-bronze-retrieve")?;
    let archive = demo_tarball(&root)?;
    let archive_bytes = fs::read(&archive).context("read archive")?;
    let store = FakeBronzeArchiveStore::default();
    let content_sha256 = store.seed_object(
        "bronze/registry:crates-io/demo/0.1.0/source.tar.gz",
        archive_bytes,
    );
    let registry = FakeBronzeRegistry::with_row(bronze_row(
        "http://127.0.0.1:1/unreachable.tar.gz",
        &content_sha256,
        fs::metadata(&archive)?.len(),
    ));

    let restored = retrieve_bronze_source_by_coordinate(
        "registry:crates-io",
        "demo",
        "0.1.0",
        &root.join("restored"),
        &registry,
        &store,
    )
    .await?
    .context("bronze source should be restored")?;

    assert!(restored.join("Cargo.toml").is_file());
    assert_eq!(
        fs::read_to_string(restored.join("src/lib.rs")).context("read restored lib")?,
        "pub fn demo() {}\n"
    );
    assert_eq!(store.downloads(), 1);
    Ok(())
}

#[tokio::test]
async fn bronze_dedup_skips_upstream_fetch_when_registered_hash_matches() -> Result<()> {
    let root = unique_temp_dir("worker-bronze-dedup")?;
    let archive = demo_tarball(&root)?;
    let archive_bytes = fs::read(&archive).context("read archive")?;
    let store = FakeBronzeArchiveStore::default();
    let content_sha256 = store.seed_object(
        "bronze/registry:crates-io/demo/0.1.0/source.tar.gz",
        archive_bytes,
    );
    let registry = FakeBronzeRegistry::with_row(bronze_row(
        "http://127.0.0.1:1/unreachable.tar.gz",
        &content_sha256,
        fs::metadata(&archive)?.len(),
    ));
    let env = demo_job_env("http://127.0.0.1:1/unreachable.tar.gz");

    let fetched =
        fetch_source_with_bronze_services(&env, &root.join("fetch"), &registry, &store).await?;

    assert!(fetched.join("src/lib.rs").is_file());
    assert_eq!(store.uploads(), 0);
    assert_eq!(store.downloads(), 1);
    assert_eq!(registry.registers(), 0);
    Ok(())
}

#[tokio::test]
async fn bronze_fetch_errors_when_existing_success_row_hash_drifts() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvGuard::set_all([("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE", "1")]);
    let root = unique_temp_dir("worker-bronze-drift")?;
    let archive = demo_tarball(&root)?;
    let source_url = serve_once(fs::read(&archive).context("read archive")?);
    let registry = FakeBronzeRegistry::with_row(bronze_row(
        "http://127.0.0.1:1/stale.tar.gz",
        "old-sha256",
        fs::metadata(&archive)?.len(),
    ));
    let store = FakeBronzeArchiveStore::default();
    let env = demo_job_env(&source_url);

    let err = fetch_source_with_bronze_services(&env, &root.join("fetch"), &registry, &store)
        .await
        .unwrap_err();

    assert!(err.to_string().contains("bronze content drift"));
    assert_eq!(store.uploads(), 0);
    assert_eq!(registry.registers(), 0);
    Ok(())
}

#[test]
fn fetch_source_rejects_localhost_url_without_escape_hatch() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvGuard::set_all([("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE", "0")]);
    let root = unique_temp_dir("worker-localhost-rejected")?;

    let err = fetch_source(
        "https://127.0.0.1/archive.tar.gz",
        "tarball",
        "0.1.0",
        &root.join("fetch"),
    )
    .unwrap_err();

    match err {
        WorkerError::Fetch(detail) => {
            assert!(detail.contains("source_url abuse re-validation failed"));
            assert!(detail.contains("localhost"));
        }
        other => panic!("expected fetch error, got {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn spot_interruption_handler_writes_checkpoint() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = unique_temp_dir("worker-checkpoint")?;
    let _env_guard = EnvGuard::set_all([
        ("SPUR_CONTEXT_WORKER_CHECKPOINT_DIR", root.to_str().unwrap()),
        ("SPUR_CONTEXT_WORKER_SKIP_SFN", "1"),
    ]);
    let env = JobEnv {
        task_token: "task-token".to_owned(),
        job_id: "job-spot".to_owned(),
        package: "demo".to_owned(),
        revision: "deadbeef".to_owned(),
        source: "git:custom".to_owned(),
        source_url: "https://github.com/example/demo.git".to_owned(),
        source_kind: "git".to_owned(),
        catalog_dsn: "sqlite:/tmp/catalog.sqlite".to_owned(),
    };

    handle_spot_interruption(&env, "translate").await?;

    let checkpoint_path = root.join("jobs").join("job-spot").join("checkpoint.json");
    let checkpoint: Value =
        serde_json::from_str(&fs::read_to_string(&checkpoint_path).context("read checkpoint")?)
            .context("parse checkpoint")?;

    assert_eq!(checkpoint["job_id"], "job-spot");
    assert_eq!(checkpoint["last_completed_stage"], "translate");
    assert_eq!(checkpoint["error"], "spot_interrupted");
    assert_eq!(checkpoint["package"], "demo");
    assert_eq!(checkpoint["revision"], "deadbeef");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_updates_job_stages_around_fetch_build_and_translate() -> Result<()> {
    let store = Arc::new(FakeJobStore::default());
    store.seed_job("job-stages");
    let tracker = StageTracker::with_job_store("job-stages", store.clone());

    tokio::task::spawn_blocking(move || {
        tracker.set("fetch_source");
        tracker.set("build_graph");
        tracker.set("translate");
    })
    .await
    .context("stage reporter task")?;

    assert_eq!(
        store.stage_updates(),
        ["fetch_source", "build_graph", "translate"]
    );
    let record = store
        .lookup_job("job-stages")
        .await?
        .context("job should exist")?;
    assert_eq!(record.status, JobStatus::Running);
    assert_eq!(record.stage.as_deref(), Some("translate"));
    Ok(())
}

#[tokio::test]
async fn lambda_worker_records_failed_job_without_sfn_callback() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvGuard::set_all([("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE", "1")]);
    let store = Arc::new(FakeJobStore::default());
    store.seed_job("job-lambda-fail");
    let leases = Arc::new(FakeCatalogLeaseStore::lost());
    let env = JobEnv {
        task_token: String::new(),
        job_id: "job-lambda-fail".to_owned(),
        package: "demo".to_owned(),
        revision: "deadbeef".to_owned(),
        source: "git:custom".to_owned(),
        source_url: "https://github.com/example/demo".to_owned(),
        source_kind: "unsupported".to_owned(),
        catalog_dsn: "sqlite:/tmp/catalog.sqlite".to_owned(),
    };

    let err = run_job_and_record_with_services(&env, store.clone(), leases)
        .await
        .unwrap_err();

    assert!(matches!(err, WorkerError::Fetch(_)));
    let record = store
        .lookup_job("job-lambda-fail")
        .await?
        .context("job should exist")?;
    assert_eq!(record.status, JobStatus::Failed);
    assert_eq!(record.error_code.as_deref(), Some("fetch"));
    assert!(record
        .error_detail
        .as_deref()
        .is_some_and(|detail| detail.contains("unsupported SOURCE_KIND")));
    Ok(())
}

#[tokio::test]
async fn catalog_lease_blocks_upload_when_token_is_lost() -> Result<()> {
    let leases = FakeCatalogLeaseStore::lost();
    let lease = CatalogLease {
        catalog_uri: "s3://bucket/catalog.ducklake".to_owned(),
        owner_job_id: "job-lost".to_owned(),
        lease_token: "token-1".to_owned(),
        expires_at_unix_secs: 1_900_000_000,
        fencing_counter: 7,
    };
    let upload_calls = AtomicUsize::new(0);

    let err = upload_with_owned_catalog_lease(&leases, &lease, || {
        upload_calls.fetch_add(1, Ordering::SeqCst);
        async { Ok(()) }
    })
    .await
    .unwrap_err();

    assert!(err.to_string().contains("lease lost"));
    assert_eq!(upload_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn catalog_download_upload_uses_conditional_s3_write_metadata() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let server = FakeS3Server::start("catalog-before", "\"etag-before\"", "version-before")?;
    let _env = EnvGuard::set_all([
        ("AWS_ENDPOINT_URL_S3", server.endpoint.as_str()),
        ("AWS_ACCESS_KEY_ID", "test"),
        ("AWS_SECRET_ACCESS_KEY", "test"),
        ("AWS_REGION", "us-east-1"),
    ]);

    let download = CatalogDownload::fetch("s3://catalog-bucket/catalog.ducklake")
        .await?
        .context("s3 catalog should be downloaded")?;
    fs::write(download.local_path(), "catalog-after").context("mutate local catalog")?;
    download.upload().await?;

    let put = server.put_request();
    assert!(put.contains("if-match: \"etag-before\""));
    assert!(!put.contains("if-none-match"));
    assert!(put.contains("/catalog-bucket/catalog.ducklake"));
    Ok(())
}

#[test]
fn build_graph_invokes_spur_with_progress_visible() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = unique_temp_dir("worker-build-graph")?;
    let bin_dir = root.join("bin");
    fs::create_dir_all(&bin_dir).context("create fake bin dir")?;
    let record = root.join("spur-record.txt");
    let fake_spur = bin_dir.join("spur");
    fs::write(
        &fake_spur,
        format!(
            "#!/usr/bin/env bash\n{{\n  printf 'args=%s\\n' \"$*\"\n  printf 'skip_embeddings=%s\\n' \"${{SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS-}}\"\n}} > {}\n",
            record.display()
        ),
    )
    .context("write fake spur")?;
    let mut perms = fs::metadata(&fake_spur)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        perms.set_mode(0o755);
    }
    fs::set_permissions(&fake_spur, perms).context("chmod fake spur")?;

    let source = root.join("source");
    fs::create_dir_all(&source).context("create source")?;
    let artifact = root.join("artifact");
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _env = EnvGuard::set_all([("PATH", path.as_str())]);

    build_graph(&source, &artifact)?;

    let record = fs::read_to_string(&record).context("read fake spur record")?;
    assert!(record.contains("args=graph build"));
    assert!(record.contains("--no-analyst"));
    assert!(!record.contains("--quiet"));
    assert!(record.contains("skip_embeddings=1"));
    Ok(())
}

#[test]
#[ignore = "requires git on PATH; run with: scripts/spur-cargo test -p spur-context-service --features worker --test worker_test fetch_source_clones_git_repo -- --ignored"]
fn fetch_source_clones_git_repo() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvGuard::set_all([("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE", "1")]);
    let root = unique_temp_dir("worker-git")?;
    let repo = root.join("repo");
    fs::create_dir_all(repo.join("src")).context("create repo")?;
    fs::write(repo.join("src/lib.rs"), "pub fn demo() {}\n").context("write lib")?;
    run_git(&root, ["init", "repo"])?;
    run_git(&repo, ["add", "."])?;
    run_git(&repo, ["commit", "-m", "initial"])?;
    let revision = git_output(&repo, ["rev-parse", "HEAD"])?;

    let fetched = fetch_source(
        repo.to_str().context("repo path is utf-8")?,
        "git",
        revision.trim(),
        &root.join("fetch"),
    )?;

    assert!(fetched.join("src/lib.rs").is_file());
    Ok(())
}

#[tokio::test]
#[ignore = "requires AWS credentials and SPUR_CONTEXT_WORKER_AWS_CHECKPOINT_URI=s3://bucket/prefix/checkpoint.json; run with --ignored"]
async fn spot_interruption_handler_writes_checkpoint_to_s3() -> Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let uri = std::env::var("SPUR_CONTEXT_WORKER_AWS_CHECKPOINT_URI")
        .context("set SPUR_CONTEXT_WORKER_AWS_CHECKPOINT_URI")?;
    let _env_guard = EnvGuard::set_all([("SPUR_CONTEXT_WORKER_CHECKPOINT_URI", uri.as_str())]);
    let env = JobEnv {
        task_token: "task-token".to_owned(),
        job_id: "job-spot-aws".to_owned(),
        package: "demo".to_owned(),
        revision: "deadbeef".to_owned(),
        source: "git:custom".to_owned(),
        source_url: "https://github.com/example/demo.git".to_owned(),
        source_kind: "git".to_owned(),
        catalog_dsn: "sqlite:/tmp/catalog.sqlite".to_owned(),
    };

    handle_spot_interruption(&env, "fetch").await?;
    Ok(())
}

fn create_tarball(source_dir: &Path, archive: &Path) -> Result<()> {
    let status = Command::new("tar")
        .arg("-czf")
        .arg(archive)
        .arg("-C")
        .arg(source_dir)
        .arg(".")
        .status()
        .context("run tar")?;
    anyhow::ensure!(status.success(), "tar exited with {status}");
    Ok(())
}

fn serve_once(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock http");
    let addr = listener.local_addr().expect("mock http addr");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept mock http");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/gzip\r\n\r\n",
            body.len()
        );
        stream
            .write_all(headers.as_bytes())
            .expect("write mock headers");
        stream.write_all(&body).expect("write mock body");
    });
    format!("http://{addr}/demo.tar.gz")
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<()> {
    let status = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .status()
        .context("run git")?;
    anyhow::ensure!(status.success(), "git exited with {status}");
    Ok(())
}

fn git_output<const N: usize>(cwd: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .context("run git")?;
    anyhow::ensure!(output.status.success(), "git exited with {}", output.status);
    String::from_utf8(output.stdout).context("git output utf-8")
}

fn unique_temp_dir(name: &str) -> Result<PathBuf> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "spur-context-service-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("clock before unix epoch")?
            .as_nanos()
    ));
    fs::create_dir_all(&path).context("create temp dir")?;
    Ok(path)
}

fn demo_tarball(root: &Path) -> Result<PathBuf> {
    let fixture = root.join("fixture").join("demo-0.1.0");
    fs::create_dir_all(fixture.join("src")).context("create fixture")?;
    fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .context("write manifest")?;
    fs::write(fixture.join("src/lib.rs"), "pub fn demo() {}\n").context("write lib")?;
    let archive = root.join("demo.tar.gz");
    create_tarball(root.join("fixture").as_path(), &archive)?;
    Ok(archive)
}

fn demo_job_env(source_url: &str) -> JobEnv {
    JobEnv {
        task_token: "task-token".to_owned(),
        job_id: "job-bronze".to_owned(),
        package: "demo".to_owned(),
        revision: "0.1.0".to_owned(),
        source: "registry:crates-io".to_owned(),
        source_url: source_url.to_owned(),
        source_kind: "tarball".to_owned(),
        catalog_dsn: "sqlite:/tmp/catalog.sqlite".to_owned(),
    }
}

fn bronze_row(source_url: &str, content_sha256: &str, bytes: u64) -> BronzeRawSource {
    BronzeRawSource {
        source: "registry:crates-io".to_owned(),
        package: "demo".to_owned(),
        version: "0.1.0".to_owned(),
        revision_kind: "semver".to_owned(),
        semver_major: Some(0),
        semver_minor: Some(1),
        semver_patch: Some(0),
        source_kind: "tarball".to_owned(),
        source_url: source_url.to_owned(),
        s3_uri: "s3://bronze-test/bronze/registry:crates-io/demo/0.1.0/source.tar.gz".to_owned(),
        content_sha256: content_sha256.to_owned(),
        bytes,
        fetched_at: 1_800_000_000,
        fetch_status: "success".to_owned(),
    }
}

#[derive(Default)]
struct FakeBronzeRegistry {
    rows: Mutex<Vec<BronzeRawSource>>,
    registers: AtomicUsize,
}

impl FakeBronzeRegistry {
    fn with_row(row: BronzeRawSource) -> Self {
        Self {
            rows: Mutex::new(vec![row]),
            registers: AtomicUsize::new(0),
        }
    }

    fn rows(&self) -> Vec<BronzeRawSource> {
        self.rows.lock().expect("bronze registry lock").clone()
    }

    fn registers(&self) -> usize {
        self.registers.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BronzeRawSourceRegistry for FakeBronzeRegistry {
    async fn lookup(
        &self,
        source: &str,
        package: &str,
        version: &str,
    ) -> Result<Option<BronzeRawSource>, WorkerError> {
        Ok(self
            .rows
            .lock()
            .expect("bronze registry lock")
            .iter()
            .rev()
            .find(|row| row.source == source && row.package == package && row.version == version)
            .cloned())
    }

    async fn register(&self, row: &BronzeRawSource) -> Result<(), WorkerError> {
        self.registers.fetch_add(1, Ordering::SeqCst);
        self.rows
            .lock()
            .expect("bronze registry lock")
            .push(row.clone());
        Ok(())
    }
}

#[derive(Default)]
struct FakeBronzeArchiveStore {
    objects: Mutex<HashMap<String, FakeBronzeObject>>,
    uploaded_keys: Mutex<Vec<String>>,
    uploads: AtomicUsize,
    downloads: AtomicUsize,
}

#[derive(Clone)]
struct FakeBronzeObject {
    content_sha256: String,
    bytes: Vec<u8>,
}

impl FakeBronzeArchiveStore {
    fn seed_object(&self, key: &str, bytes: Vec<u8>) -> String {
        let content_sha256 = sha256_hex(&bytes);
        self.objects.lock().expect("bronze store lock").insert(
            format!("s3://bronze-test/{key}"),
            FakeBronzeObject {
                content_sha256: content_sha256.clone(),
                bytes,
            },
        );
        content_sha256
    }

    fn uploaded_keys(&self) -> Vec<String> {
        self.uploaded_keys
            .lock()
            .expect("uploaded keys lock")
            .clone()
    }

    fn sha256_for(&self, uri: &str) -> Option<String> {
        self.objects
            .lock()
            .expect("bronze store lock")
            .get(uri)
            .map(|object| object.content_sha256.clone())
    }

    fn uploads(&self) -> usize {
        self.uploads.load(Ordering::SeqCst)
    }

    fn downloads(&self) -> usize {
        self.downloads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BronzeArchiveStore for FakeBronzeArchiveStore {
    async fn content_sha256(&self, s3_uri: &str) -> Result<Option<String>, WorkerError> {
        Ok(self.sha256_for(s3_uri))
    }

    async fn download_to_path(&self, s3_uri: &str, path: &Path) -> Result<(), WorkerError> {
        let object = self
            .objects
            .lock()
            .expect("bronze store lock")
            .get(s3_uri)
            .cloned()
            .ok_or_else(|| WorkerError::Fetch(format!("missing fake bronze object {s3_uri}")))?;
        fs::write(path, object.bytes)
            .map_err(|error| WorkerError::Fetch(format!("write fake bronze download: {error}")))?;
        self.downloads.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn upload_path(
        &self,
        key: &str,
        content_sha256: &str,
        path: &Path,
    ) -> Result<String, WorkerError> {
        let bytes = fs::read(path)
            .map_err(|error| WorkerError::Fetch(format!("read fake bronze upload: {error}")))?;
        let uri = format!("s3://bronze-test/{key}");
        self.objects.lock().expect("bronze store lock").insert(
            uri.clone(),
            FakeBronzeObject {
                content_sha256: content_sha256.to_owned(),
                bytes,
            },
        );
        self.uploaded_keys
            .lock()
            .expect("uploaded keys lock")
            .push(key.to_owned());
        self.uploads.fetch_add(1, Ordering::SeqCst);
        Ok(uri)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    format!("{digest:x}")
}

#[derive(Default)]
struct FakeJobStore {
    next_id: AtomicU64,
    state: Mutex<FakeJobState>,
}

#[derive(Default)]
struct FakeJobState {
    jobs: std::collections::HashMap<String, JobRecord>,
    dedupe: std::collections::HashMap<JobKey, String>,
    stage_updates: Vec<String>,
}

#[async_trait]
impl JobStore for FakeJobStore {
    async fn create_or_get_active_job(
        &self,
        request: CreateJobRequest,
    ) -> spur_context_service::jobs::Result<CreateJobOutcome> {
        let key = request.key();
        let mut state = self.state.lock().expect("fake store lock");
        if let Some(job_id) = state.dedupe.get(&key) {
            if let Some(record) = state.jobs.get(job_id) {
                return Ok(CreateJobOutcome::Existing(record.clone()));
            }
        }

        let job_id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let record = JobRecord {
            job_id: job_id.clone(),
            status: JobStatus::Queued,
            source: request.source,
            package: request.package,
            revision: request.revision,
            source_url: request.source_url,
            source_url_hash: request.source_url_hash,
            source_kind: request.source_kind,
            caller_id: request.caller_id,
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: "now".to_owned(),
            updated_at: "now".to_owned(),
        };
        state.dedupe.insert(key, job_id.clone());
        state.jobs.insert(job_id, record.clone());
        Ok(CreateJobOutcome::Created(record))
    }

    async fn record_execution_started(
        &self,
        job_id: &str,
        execution_arn: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        self.update_job(job_id, |record| {
            record.execution_arn = Some(execution_arn.to_owned());
        })
    }

    async fn update_stage(
        &self,
        job_id: &str,
        status: JobStatus,
        stage: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let mut state = self.state.lock().expect("fake store lock");
        let record = state
            .jobs
            .get_mut(job_id)
            .ok_or(spur_context_service::jobs::JobsError::NotFound)?;
        record.status = status;
        record.stage = Some(stage.to_owned());
        record.updated_at = format!("stage:{stage}");
        let updated = record.clone();
        state.stage_updates.push(stage.to_owned());
        Ok(updated)
    }

    async fn mark_complete(
        &self,
        job_id: &str,
        snapshot_id: i64,
        row_counts: Value,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let record = self.update_job(job_id, |record| {
            record.status = JobStatus::Complete;
            record.snapshot_id = Some(snapshot_id);
            record.row_counts = Some(row_counts);
            record.error_code = None;
            record.error_detail = None;
        })?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn mark_failed(
        &self,
        job_id: &str,
        code: &str,
        detail: &str,
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let record = self.update_job(job_id, |record| {
            record.status = JobStatus::Failed;
            record.error_code = Some(code.to_owned());
            record.error_detail = Some(detail.to_owned());
        })?;
        self.release_dedupe_if_owner(&record).await?;
        Ok(record)
    }

    async fn lookup_job(
        &self,
        job_id: &str,
    ) -> spur_context_service::jobs::Result<Option<JobRecord>> {
        Ok(self
            .state
            .lock()
            .expect("fake store lock")
            .jobs
            .get(job_id)
            .cloned())
    }

    async fn release_dedupe_if_owner(
        &self,
        record: &JobRecord,
    ) -> spur_context_service::jobs::Result<()> {
        let mut state = self.state.lock().expect("fake store lock");
        let key = record.key();
        if state
            .dedupe
            .get(&key)
            .is_some_and(|job_id| job_id == &record.job_id)
        {
            state.dedupe.remove(&key);
        }
        Ok(())
    }
}

impl FakeJobStore {
    fn seed_job(&self, job_id: &str) {
        let record = JobRecord {
            job_id: job_id.to_owned(),
            status: JobStatus::Queued,
            source: "git:custom".to_owned(),
            package: "demo".to_owned(),
            revision: "main".to_owned(),
            source_url: "https://github.com/example/demo".to_owned(),
            source_url_hash: "sha256:demo".to_owned(),
            source_kind: "git".to_owned(),
            caller_id: "test".to_owned(),
            execution_arn: None,
            attempt: 1,
            stage: None,
            snapshot_id: None,
            row_counts: None,
            error_code: None,
            error_detail: None,
            created_at: "now".to_owned(),
            updated_at: "now".to_owned(),
        };
        let mut state = self.state.lock().expect("fake store lock");
        state.dedupe.insert(record.key(), job_id.to_owned());
        state.jobs.insert(job_id.to_owned(), record);
    }

    fn stage_updates(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("fake store lock")
            .stage_updates
            .clone()
    }

    fn update_job(
        &self,
        job_id: &str,
        update: impl FnOnce(&mut JobRecord),
    ) -> spur_context_service::jobs::Result<JobRecord> {
        let mut state = self.state.lock().expect("fake store lock");
        let record = state.jobs.get_mut(job_id).ok_or(JobsError::NotFound)?;
        update(record);
        Ok(record.clone())
    }
}

struct FakeCatalogLeaseStore {
    lost: AtomicBool,
}

impl FakeCatalogLeaseStore {
    fn lost() -> Self {
        Self {
            lost: AtomicBool::new(true),
        }
    }
}

#[async_trait]
impl CatalogLeaseStore for FakeCatalogLeaseStore {
    async fn acquire(&self, catalog_uri: &str, owner_job_id: &str) -> Result<CatalogLease> {
        Ok(CatalogLease {
            catalog_uri: catalog_uri.to_owned(),
            owner_job_id: owner_job_id.to_owned(),
            lease_token: "token".to_owned(),
            expires_at_unix_secs: 1_900_000_000,
            fencing_counter: 1,
        })
    }

    async fn renew(&self, lease: &CatalogLease) -> Result<CatalogLease> {
        Ok(lease.clone())
    }

    async fn assert_owned(&self, _lease: &CatalogLease) -> Result<()> {
        if self.lost.load(Ordering::SeqCst) {
            anyhow::bail!("lease lost");
        }
        Ok(())
    }

    async fn release(&self, _lease: &CatalogLease) -> Result<()> {
        Ok(())
    }
}

struct FakeS3Server {
    endpoint: String,
    put_request: Arc<Mutex<Option<String>>>,
}

impl FakeS3Server {
    fn start(body: &'static str, etag: &'static str, version: &'static str) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("bind fake s3")?;
        let endpoint = format!("http://{}", listener.local_addr().context("fake s3 addr")?);
        let put_request = Arc::new(Mutex::new(None));
        let put_request_thread = put_request.clone();
        thread::spawn(move || {
            let (mut get_stream, _) = listener.accept().expect("accept fake s3 get");
            let _get = read_http_request(&mut get_stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: {etag}\r\nx-amz-version-id: {version}\r\n\r\n{body}",
                body.len()
            );
            get_stream
                .write_all(response.as_bytes())
                .expect("write fake s3 get");

            let (mut put_stream, _) = listener.accept().expect("accept fake s3 put");
            let put = read_http_request(&mut put_stream).to_ascii_lowercase();
            *put_request_thread.lock().expect("put request lock") = Some(put);
            put_stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .expect("write fake s3 put");
        });
        Ok(Self {
            endpoint,
            put_request,
        })
    }

    fn put_request(&self) -> String {
        self.put_request
            .lock()
            .expect("put request lock")
            .clone()
            .expect("put request should be captured")
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).expect("read fake s3 request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&request).to_string()
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set_all<const N: usize>(vars: [(&'static str, &str); N]) -> Self {
        let mut previous = Vec::with_capacity(vars.len());
        for (key, value) in vars {
            previous.push((key, std::env::var(key).ok()));
            std::env::set_var(key, value);
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
