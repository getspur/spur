#![cfg(feature = "worker")]

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use duckdb::Connection;
use serde_json::{json, Value};
use spur_context_service::jobs::{lookup, JobStatus};
use spur_context_service::worker::{
    build_graph, fetch_source, handle_spot_interruption, update_job_status_with_connection, JobEnv,
    WorkerError,
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

#[test]
fn update_job_status_marks_job_complete_in_catalog() -> Result<()> {
    let conn = initialize_worker_catalog()?;
    let env = JobEnv {
        task_token: "task-token".to_owned(),
        job_id: "job-complete".to_owned(),
        package: "demo".to_owned(),
        revision: "1.0.0".to_owned(),
        source: "registry:crates-io".to_owned(),
        source_url: "https://crates.io/api/v1/crates/demo/1.0.0/download".to_owned(),
        source_kind: "tarball".to_owned(),
        catalog_dsn: "sqlite:/tmp/catalog.sqlite".to_owned(),
    };

    update_job_status_with_connection(
        &conn,
        &env,
        JobStatus::Complete,
        Some(42),
        None,
        Some(json!({ "nodes": 3, "edges": 2 })),
    )?;

    let row = lookup(&conn, "job-complete")?.context("job row should exist")?;
    assert_eq!(row.status, JobStatus::Complete);
    assert_eq!(row.snapshot_id, Some(42));
    assert_eq!(row.error, None);
    assert_eq!(row.row_counts, Some(json!({ "nodes": 3, "edges": 2 })));
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

fn initialize_worker_catalog() -> Result<Connection> {
    let conn = Connection::open_in_memory().context("open in-memory duckdb")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS index_jobs (
            job_id TEXT PRIMARY KEY,
            source TEXT,
            package TEXT,
            revision TEXT,
            source_url TEXT,
            source_url_hash TEXT,
            status TEXT,
            execution_arn TEXT,
            error TEXT,
            snapshot_id BIGINT,
            row_counts JSON,
            created_at TIMESTAMPTZ,
            updated_at TIMESTAMPTZ,
            UNIQUE(source, package, revision, source_url_hash)
        );

        INSERT INTO index_jobs (
            job_id,
            source,
            package,
            revision,
            source_url,
            source_url_hash,
            status,
            execution_arn,
            error,
            snapshot_id,
            row_counts,
            created_at,
            updated_at
        )
        VALUES (
            'job-complete',
            'registry:crates-io',
            'demo',
            '1.0.0',
            'https://crates.io/api/v1/crates/demo/1.0.0/download',
            'sha256:demo',
            'running',
            'arn:running',
            NULL,
            NULL,
            NULL,
            CURRENT_TIMESTAMP,
            CURRENT_TIMESTAMP
        );
        "#,
    )
    .context("create worker catalog fixture")?;
    Ok(conn)
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
