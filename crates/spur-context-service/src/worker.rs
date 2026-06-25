//! Fargate worker: fetch source, build graph, translate to DuckLake.

use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::future::Future;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context as _, Result};
use async_trait::async_trait;
use aws_sdk_dynamodb::{types::AttributeValue, Client as DynamoDbClient};
use aws_sdk_s3::primitives::ByteStream;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::abuse;
use crate::jobs::{DynamoDbJobStore, JobStatus, JobStore};
use crate::translate::{translate_artifact_to_ducklake, TranslateOptions, TranslateStats};

const DEFAULT_ARTIFACT_DIR: &str = "/tmp/artifact";
const DEFAULT_CHECKPOINT_BUCKET: &str = "spur-context";
const DEFAULT_TARBALL_SIZE_CAP_BYTES: usize = 500 * 1024 * 1024;
const HTTP_HEADER_CAP_BYTES: usize = 64 * 1024;
const ECS_CREDENTIALS_CAP_BYTES: usize = 64 * 1024;
const JINA_CODE_EMBED_MODEL_NAME: &str = "JinaEmbeddingsV2BaseCode";
const EMBED_MODEL_ENV: &str = "SPUR_EMBEDDING_MODEL";
const GRAPH_SKIP_SECTION_EMBEDDINGS_ENV: &str = "SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS";
const DEFAULT_CATALOG_LEASES_TABLE: &str = "spur-context-catalog-leases";
const CATALOG_LEASE_DURATION_SECS: i64 = 10 * 60;
const CATALOG_LEASE_RENEW_INTERVAL_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEnv {
    pub task_token: String,
    pub job_id: String,
    pub package: String,
    pub revision: String,
    pub source: String,
    pub source_url: String,
    pub source_kind: String,
    pub catalog_dsn: String,
}

impl JobEnv {
    pub fn from_env() -> Result<Self> {
        let catalog_dsn = optional_env("SPUR_CATALOG_DSN")
            .or_else(|| optional_env("SPUR_CATALOG_S3_URI"))
            .ok_or_else(|| anyhow!("SPUR_CATALOG_DSN or SPUR_CATALOG_S3_URI must be set"))?;

        Ok(Self {
            task_token: required_env("TASK_TOKEN")?,
            job_id: required_env("JOB_ID")?,
            package: required_env("PACKAGE")?,
            revision: required_env("REVISION")?,
            source: required_env("SOURCE")?,
            source_url: required_env("SOURCE_URL")?,
            source_kind: required_env("SOURCE_KIND")?,
            catalog_dsn,
        })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkerError {
    #[error("fetch:{0}")]
    Fetch(String),
    #[error("build:{0}")]
    Build(String),
    #[error("translate:{0}")]
    Translate(String),
    #[error("spot_interrupted")]
    SpotInterrupted,
    #[error("sfn_send:{0}")]
    SfnSend(String),
}

#[derive(Clone)]
pub struct StageTracker {
    current: Arc<Mutex<String>>,
    reporter: Option<StageReporter>,
}

impl StageTracker {
    pub fn new() -> Self {
        Self {
            current: Arc::new(Mutex::new("starting".to_owned())),
            reporter: None,
        }
    }

    pub fn with_job_store(job_id: impl Into<String>, jobs: Arc<dyn JobStore>) -> Self {
        Self {
            current: Arc::new(Mutex::new("starting".to_owned())),
            reporter: Some(StageReporter {
                job_id: job_id.into(),
                jobs,
                handle: tokio::runtime::Handle::current(),
            }),
        }
    }

    pub fn set(&self, stage: &str) {
        self.set_current(stage);
        if let Some(reporter) = &self.reporter {
            reporter.record(stage);
        }
    }

    fn set_current(&self, stage: &str) {
        if let Ok(mut current) = self.current.lock() {
            *current = stage.to_owned();
        }
    }

    pub fn get(&self) -> String {
        self.current
            .lock()
            .map(|stage| stage.clone())
            .unwrap_or_else(|_| "unknown".to_owned())
    }
}

#[derive(Clone)]
struct StageReporter {
    job_id: String,
    jobs: Arc<dyn JobStore>,
    handle: tokio::runtime::Handle,
}

impl StageReporter {
    fn record(&self, stage: &str) {
        let result = self.handle.block_on(self.jobs.update_stage(
            &self.job_id,
            JobStatus::Running,
            stage,
        ));
        if let Err(error) = result {
            eprintln!(
                "[worker] warning: failed to record stage `{stage}` for job `{}`: {error:#}",
                self.job_id
            );
        }
    }
}

pub async fn run_from_env() -> Result<(), WorkerError> {
    let env = JobEnv::from_env().map_err(|error| WorkerError::Fetch(error.to_string()))?;
    run_job_and_report(&env).await
}

pub async fn run_job_and_report(env: &JobEnv) -> Result<(), WorkerError> {
    let dynamodb = dynamodb_client();
    let jobs = Arc::new(DynamoDbJobStore::new(dynamodb.clone()));
    let leases = Arc::new(DynamoDbCatalogLeaseStore::new(dynamodb));
    run_job_and_report_with_services(env, jobs, leases).await
}

pub async fn run_job_and_report_with_services(
    env: &JobEnv,
    jobs: Arc<dyn JobStore>,
    leases: Arc<dyn CatalogLeaseStore>,
) -> Result<(), WorkerError> {
    let stage = StageTracker::with_job_store(env.job_id.clone(), jobs.clone());
    let run = run_job_with_stage(env.clone(), stage.clone(), jobs.clone(), leases);

    tokio::select! {
        result = run => {
            match result {
                Ok(stats) => {
                    send_task_success(env, &stats).await?;
                    Ok(())
                }
                Err(error) => {
                    let error_detail = format!("{error:#}");
                    eprintln!("[worker] job failed: {error_detail}");
                    mark_job_failed_best_effort(
                        jobs.as_ref(),
                        env,
                        &failure_error_code(&error),
                        &error_detail,
                    )
                    .await;
                    if let Err(sfn_err) = send_task_failure(env, &failure_error_code(&error), &error_detail).await {
                        eprintln!("[worker] SendTaskFailure also failed: {sfn_err:#}");
                    }
                    return Err(error);
                }
            }
        }
        signal_result = wait_for_sigterm() => {
            if let Err(error) = signal_result {
                let worker_error = WorkerError::SfnSend(error.to_string());
                let error_detail = worker_error.to_string();
                mark_job_failed_best_effort(
                    jobs.as_ref(),
                    env,
                    &failure_error_code(&worker_error),
                    &error_detail,
                )
                .await;
                send_task_failure(env, &failure_error_code(&worker_error), &error_detail).await?;
                return Err(worker_error);
            }
            handle_spot_interruption(env, &stage.get()).await?;
            Err(WorkerError::SpotInterrupted)
        }
    }
}

pub async fn run_job(env: &JobEnv) -> Result<TranslateStats, WorkerError> {
    let dynamodb = dynamodb_client();
    let jobs = Arc::new(DynamoDbJobStore::new(dynamodb.clone()));
    let leases = Arc::new(DynamoDbCatalogLeaseStore::new(dynamodb));
    run_job_with_services(env, jobs, leases).await
}

pub async fn run_job_with_services(
    env: &JobEnv,
    jobs: Arc<dyn JobStore>,
    leases: Arc<dyn CatalogLeaseStore>,
) -> Result<TranslateStats, WorkerError> {
    let stage = StageTracker::with_job_store(env.job_id.clone(), jobs.clone());
    run_job_with_stage(env.clone(), stage, jobs, leases).await
}

async fn run_job_with_stage(
    env: JobEnv,
    stage: StageTracker,
    jobs: Arc<dyn JobStore>,
    leases: Arc<dyn CatalogLeaseStore>,
) -> Result<TranslateStats, WorkerError> {
    let blocking_env = env.clone();
    let stage_clone = stage.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_job_blocking(&blocking_env, &stage_clone)
    })
        .await
        .map_err(|error| WorkerError::Build(format!("worker join failed: {error}")))??;

    let mut lease = None;
    if env.catalog_dsn.starts_with("s3://") {
        record_job_stage_best_effort(jobs.as_ref(), &env.job_id, "waiting_catalog_lease").await;
        lease = Some(
            leases
                .acquire(&env.catalog_dsn, &env.job_id)
                .await
                .map_err(|error| WorkerError::Translate(format!("acquire catalog lease: {error:#}")))?,
        );
    }

    let result = async {
        // DuckLake cannot open S3 catalog metadata for read-write, so download
        // the catalog locally, translate with the local path, then upload back.
        // Data files go directly to S3 via httpfs (the catalog's stored data_path).
        let catalog_dl = CatalogDownload::fetch(&env.catalog_dsn)
            .await
            .map_err(|e| WorkerError::Translate(format!("download catalog: {e:#}")))?;

        let local_env: JobEnv = if let Some(ref dl) = catalog_dl {
            let mut local = env.clone();
            local.catalog_dsn = dl.local_path.to_string_lossy().to_string();
            // Data files go directly to S3 — the translate step uses FORCE CHECKPOINT
            // to flush all data to S3 before the connection drops.
            std::env::set_var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH", "s3://spur-context/data/");
            local
        } else {
            env.clone()
        };

        stage.set_current("translate");
        record_job_stage_best_effort(jobs.as_ref(), &env.job_id, "translate").await;
        let translate_stage = stage.clone();
        let translate_task = tokio::task::spawn_blocking(move || {
            translate_prepared_blocking(&local_env, &translate_stage, &prepared)
        });
        let (stats, renewed_lease) =
            await_translate_with_lease_renewal(translate_task, leases.clone(), lease.clone()).await?;
        if renewed_lease.is_some() {
            lease = renewed_lease;
        }

        if let Some(ref dl) = catalog_dl {
            if let Some(current_lease) = lease.as_mut() {
                *current_lease = leases
                    .renew(current_lease)
                    .await
                    .map_err(|error| {
                        WorkerError::Translate(format!("renew catalog lease before upload: {error:#}"))
                    })?;
                upload_with_owned_catalog_lease(leases.as_ref(), current_lease, || dl.upload())
                    .await
                    .map_err(|error| WorkerError::Translate(format!("upload catalog: {error:#}")))?;
            } else {
                dl.upload()
                    .await
                    .map_err(|e| WorkerError::Translate(format!("upload catalog: {e:#}")))?;
            }
        }
        stage.set_current("complete");
        Ok(stats)
    }
    .await;

    if let Ok(stats) = &result {
        mark_job_complete_best_effort(jobs.as_ref(), &env, stats).await;
    }

    if let Some(ref current_lease) = lease {
        if let Err(error) = leases.release(current_lease).await {
            eprintln!(
                "[worker] warning: failed to release catalog lease for job `{}`: {error:#}",
                env.job_id
            );
        }
    }

    result
}

fn prepare_job_blocking(env: &JobEnv, stage: &StageTracker) -> Result<PreparedJob, WorkerError> {
    let workspace = TempWorkspace::new(&env.job_id)?;
    let source_dest = workspace.path.join("source");
    let artifact_base = artifact_dir();

    stage.set("fetch_source");
    let stage_started = log_stage_started("fetch_source");
    let source_path = fetch_source(
        &env.source_url,
        &env.source_kind,
        &env.revision,
        &source_dest,
    )?;
    log_stage_completed("fetch_source", stage_started);

    stage.set("build_graph");
    let stage_started = log_stage_started("build_graph");
    prepare_artifact_dir(&artifact_base)?;
    build_graph(&source_path, &artifact_base)?;
    let artifact_dir = resolve_graph_artifact_dir(&artifact_base)?;
    log_stage_completed("build_graph", stage_started);

    Ok(PreparedJob {
        _workspace: workspace,
        source_path,
        artifact_dir,
    })
}

fn translate_prepared_blocking(
    env: &JobEnv,
    stage: &StageTracker,
    prepared: &PreparedJob,
) -> Result<TranslateStats, WorkerError> {
    stage.set_current("translate");
    let stage_started = log_stage_started("translate");
    let stats = translate_with_source_root(&prepared.artifact_dir, Some(&prepared.source_path), env)?;
    log_stage_completed("translate", stage_started);
    stage.set_current("complete");
    Ok(stats)
}

pub fn fetch_source(
    source_url: &str,
    source_kind: &str,
    revision: &str,
    dest: &Path,
) -> Result<PathBuf, WorkerError> {
    if !matches!(
        optional_env("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE").as_deref(),
        Some("1")
    ) {
        let parsed =
            abuse::validate(source_url, &abuse::ValidateOptions::default()).map_err(|error| {
                WorkerError::Fetch(format!("source_url abuse re-validation failed: {error}"))
            })?;
        abuse::resolve_and_check_dns(&parsed)
            .map_err(|error| WorkerError::Fetch(format!("source_url DNS check failed: {error}")))?;
    }

    match source_kind.trim().to_ascii_lowercase().as_str() {
        "git" => fetch_git(source_url, revision, dest),
        "tarball" => fetch_tarball(source_url, dest),
        other => Err(WorkerError::Fetch(format!(
            "unsupported SOURCE_KIND `{other}`"
        ))),
    }
}

pub fn build_graph(source_path: &Path, artifact_dir: &Path) -> Result<(), WorkerError> {
    fs::create_dir_all(artifact_dir).map_err(|error| {
        WorkerError::Build(format!(
            "failed to create artifact dir `{}`: {error}",
            artifact_dir.display()
        ))
    })?;

    // Call `spur graph build` as a subprocess. This decouples spur-context-service
    // from spur-cli, allowing duckdb v1.5.4 (for DuckLake) to coexist with
    // spur-cli's duckdb v1.4.4 (for DuckPGQ in spur-analyst) in the same Docker image.
    let _embed_model = EnvVarGuard::set(EMBED_MODEL_ENV, JINA_CODE_EMBED_MODEL_NAME);

    let started = Instant::now();
    eprintln!(
        "[worker] running spur graph build root={} output={}",
        source_path.display(),
        artifact_dir.display()
    );
    let status = Command::new("spur")
        .env(GRAPH_SKIP_SECTION_EMBEDDINGS_ENV, "1")
        .args([
            "graph", "build",
            "--root", &source_path.to_string_lossy(),
            "--output", &artifact_dir.to_string_lossy(),
            "--no-analyst",
        ])
        .status()
        .map_err(|error| WorkerError::Build(format!("failed to run `spur graph build`: {error}")))?;

    if !status.success() {
        return Err(WorkerError::Build(format!(
            "`spur graph build` failed (exit {:?}) after {}",
            status.code(),
            format_duration(started.elapsed())
        )));
    }

    eprintln!(
        "[worker] spur graph build completed in {}",
        format_duration(started.elapsed())
    );
    Ok(())
}

fn log_stage_started(stage: &str) -> Instant {
    eprintln!("[worker] stage {stage} started");
    Instant::now()
}

fn log_stage_completed(stage: &str, started: Instant) {
    eprintln!(
        "[worker] stage {stage} completed in {}",
        format_duration(started.elapsed())
    );
}

fn format_duration(duration: Duration) -> String {
    format!("{}.{:03}s", duration.as_secs(), duration.subsec_millis())
}

pub fn translate(artifact_dir: &Path, env: &JobEnv) -> Result<TranslateStats, WorkerError> {
    translate_with_source_root(artifact_dir, None, env)
}

pub async fn handle_spot_interruption(
    env: &JobEnv,
    last_completed_stage: &str,
) -> Result<(), WorkerError> {
    write_checkpoint(env, last_completed_stage).await?;
    if matches!(
        optional_env("SPUR_CONTEXT_WORKER_SKIP_SFN").as_deref(),
        Some("1")
    ) {
        return Ok(());
    }
    send_task_failure(env, "spot_interrupted", "Fargate Spot interruption").await
}

fn fetch_git(source_url: &str, revision: &str, dest: &Path) -> Result<PathBuf, WorkerError> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|error| {
            WorkerError::Fetch(format!("failed to clear `{}`: {error}", dest.display()))
        })?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            WorkerError::Fetch(format!("failed to create `{}`: {error}", parent.display()))
        })?;
    }

    let clone = Command::new("git")
        .args(["clone", "--filter=blob:none", source_url])
        .arg(dest)
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to run git clone: {error}")))?;
    if !clone.status.success() {
        return Err(WorkerError::Fetch(format!(
            "git clone failed: {}",
            command_stderr(&clone)
        )));
    }

    let checkout = Command::new("git")
        .arg("-C")
        .arg(dest)
        .args(["checkout", revision])
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to run git checkout: {error}")))?;
    if !checkout.status.success() {
        return Err(WorkerError::Fetch(format!(
            "git checkout `{revision}` failed: {}",
            command_stderr(&checkout)
        )));
    }

    Ok(dest.to_path_buf())
}

fn fetch_tarball(source_url: &str, dest: &Path) -> Result<PathBuf, WorkerError> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|error| {
            WorkerError::Fetch(format!("failed to clear `{}`: {error}", dest.display()))
        })?;
    }
    fs::create_dir_all(dest).map_err(|error| {
        WorkerError::Fetch(format!("failed to create `{}`: {error}", dest.display()))
    })?;

    let archive = if source_url.to_ascii_lowercase().contains(".zip") {
        dest.join("__source_archive.zip")
    } else {
        dest.join("__source_archive.tar.gz")
    };
    download_tarball(source_url, &archive)?;
    extract_archive(&archive, dest)?;
    fs::remove_file(&archive).map_err(|error| {
        WorkerError::Fetch(format!("failed to remove `{}`: {error}", archive.display()))
    })?;

    Ok(single_extracted_root(dest).unwrap_or_else(|| dest.to_path_buf()))
}

fn download_tarball(source_url: &str, archive: &Path) -> Result<(), WorkerError> {
    let cap = tarball_size_cap_bytes();
    if source_url.starts_with("http://") {
        let body = http_get_bytes(source_url, cap, &[]).map_err(WorkerError::Fetch)?;
        fs::write(archive, body).map_err(|error| {
            WorkerError::Fetch(format!("failed to write `{}`: {error}", archive.display()))
        })?;
        return Ok(());
    }

    let output = Command::new("curl")
        .args([
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            "--max-filesize",
            &cap.to_string(),
            "-H",
            "User-Agent: spur-context-service/1.0",
            "--output",
        ])
        .arg(archive)
        .arg(source_url)
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to run curl: {error}")))?;
    if !output.status.success() {
        return Err(WorkerError::Fetch(format!(
            "tarball download failed: {}",
            command_stderr(&output)
        )));
    }

    let size = fs::metadata(archive)
        .map_err(|error| {
            WorkerError::Fetch(format!(
                "failed to stat downloaded archive `{}`: {error}",
                archive.display()
            ))
        })?
        .len();
    if size > cap as u64 {
        return Err(WorkerError::Fetch(format!(
            "tarball exceeded size cap: {size} > {cap}"
        )));
    }
    Ok(())
}

fn extract_archive(archive: &Path, dest: &Path) -> Result<(), WorkerError> {
    if archive.extension().and_then(|ext| ext.to_str()) == Some("zip") {
        validate_zip_entries(archive)?;
        let output = Command::new("unzip")
            .arg("-q")
            .arg(archive)
            .arg("-d")
            .arg(dest)
            .output()
            .map_err(|error| WorkerError::Fetch(format!("failed to run unzip: {error}")))?;
        if !output.status.success() {
            return Err(WorkerError::Fetch(format!(
                "unzip failed: {}",
                command_stderr(&output)
            )));
        }
        return Ok(());
    }

    validate_tar_entries(archive)?;
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to run tar: {error}")))?;
    if !output.status.success() {
        return Err(WorkerError::Fetch(format!(
            "tar extract failed: {}",
            command_stderr(&output)
        )));
    }
    Ok(())
}

fn validate_tar_entries(archive: &Path) -> Result<(), WorkerError> {
    let output = Command::new("tar")
        .arg("-tzf")
        .arg(archive)
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to list tarball: {error}")))?;
    if !output.status.success() {
        return Err(WorkerError::Fetch(format!(
            "tar list failed: {}",
            command_stderr(&output)
        )));
    }
    validate_archive_entries(&String::from_utf8_lossy(&output.stdout))
}

fn validate_zip_entries(archive: &Path) -> Result<(), WorkerError> {
    let output = Command::new("unzip")
        .args(["-Z1"])
        .arg(archive)
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to list zip archive: {error}")))?;
    if !output.status.success() {
        return Err(WorkerError::Fetch(format!(
            "zip list failed: {}",
            command_stderr(&output)
        )));
    }
    validate_archive_entries(&String::from_utf8_lossy(&output.stdout))
}

fn validate_archive_entries(entries: &str) -> Result<(), WorkerError> {
    for entry in entries
        .lines()
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let path = Path::new(entry);
        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(WorkerError::Fetch(format!(
                "archive entry escapes destination: {entry}"
            )));
        }
    }
    Ok(())
}

fn single_extracted_root(dest: &Path) -> Option<PathBuf> {
    let mut entries = fs::read_dir(dest)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    entries.sort();
    if entries.len() == 1 && entries[0].is_dir() {
        Some(entries.remove(0))
    } else {
        None
    }
}

fn prepare_artifact_dir(artifact_dir: &Path) -> Result<(), WorkerError> {
    if artifact_dir.exists() {
        fs::remove_dir_all(artifact_dir).map_err(|error| {
            WorkerError::Build(format!(
                "failed to clear artifact dir `{}`: {error}",
                artifact_dir.display()
            ))
        })?;
    }
    fs::create_dir_all(artifact_dir).map_err(|error| {
        WorkerError::Build(format!(
            "failed to create artifact dir `{}`: {error}",
            artifact_dir.display()
        ))
    })
}

fn resolve_graph_artifact_dir(artifact_base: &Path) -> Result<PathBuf, WorkerError> {
    if artifact_base.join("nodes.parquet").is_file() {
        return Ok(artifact_base.to_path_buf());
    }

    let mut candidates = fs::read_dir(artifact_base)
        .map_err(|error| {
            WorkerError::Build(format!(
                "failed to read artifact dir `{}`: {error}",
                artifact_base.display()
            ))
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_dir() && path.extension().and_then(|ext| ext.to_str()) == Some("parquet")
        })
        .collect::<Vec<_>>();
    candidates.sort();

    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => Err(WorkerError::Build(format!(
            "graph build did not produce a parquet artifact under `{}`",
            artifact_base.display()
        ))),
        count => Err(WorkerError::Build(format!(
            "graph build produced {count} parquet artifacts under `{}`",
            artifact_base.display()
        ))),
    }
}

fn translate_with_source_root(
    artifact_dir: &Path,
    source_root: Option<&Path>,
    env: &JobEnv,
) -> Result<TranslateStats, WorkerError> {
    let revision_kind = if env.revision.contains('.') { "semver" } else { "git_sha" };

    let actual_artifact = if artifact_dir.join("nodes.parquet").is_file() {
        artifact_dir.to_path_buf()
    } else {
        let mut candidates: Vec<_> = fs::read_dir(artifact_dir)
            .map_err(|e| WorkerError::Translate(format!("read artifact dir: {e}")))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && p.join("nodes.parquet").is_file())
            .collect();
        candidates.sort();
        candidates.into_iter().next()
            .ok_or_else(|| WorkerError::Translate("no nodes.parquet found in artifact".into()))?
    };

    eprintln!("[worker] artifact: {}", actual_artifact.display());

    let opts = TranslateOptions {
        source: env.source.clone(),
        package: env.package.clone(),
        revision: env.revision.clone(),
        revision_kind: revision_kind.to_owned(),
        artifact_dir: actual_artifact,
        source_root: source_root.map(|p| p.to_path_buf()),
        catalog_dsn: env.catalog_dsn.clone(),
    };

    eprintln!("[worker] running Rust API translate...");
    translate_artifact_to_ducklake(&opts)
        .map_err(|e| WorkerError::Translate(format!("{e:#}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogLease {
    pub catalog_uri: String,
    pub owner_job_id: String,
    pub lease_token: String,
    pub expires_at_unix_secs: i64,
    pub fencing_counter: i64,
}

#[async_trait]
pub trait CatalogLeaseStore: Send + Sync {
    async fn acquire(&self, catalog_uri: &str, owner_job_id: &str) -> Result<CatalogLease>;
    async fn renew(&self, lease: &CatalogLease) -> Result<CatalogLease>;
    async fn assert_owned(&self, lease: &CatalogLease) -> Result<()>;
    async fn release(&self, lease: &CatalogLease) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct DynamoDbCatalogLeaseStore {
    client: DynamoDbClient,
    table_name: String,
}

impl DynamoDbCatalogLeaseStore {
    pub fn new(client: DynamoDbClient) -> Self {
        let table_name = env::var("SPUR_CATALOG_LEASES_TABLE")
            .unwrap_or_else(|_| DEFAULT_CATALOG_LEASES_TABLE.to_owned());
        Self { client, table_name }
    }

    pub fn with_table_name(client: DynamoDbClient, table_name: impl Into<String>) -> Self {
        Self {
            client,
            table_name: table_name.into(),
        }
    }

    pub fn table_name(&self) -> &str {
        &self.table_name
    }

    async fn update_lease(
        &self,
        catalog_uri: &str,
        owner_job_id: &str,
        lease_token: &str,
        condition_expression: &str,
    ) -> Result<CatalogLease> {
        let now = unix_secs_i64();
        let expires_at = now + CATALOG_LEASE_DURATION_SECS;
        let output = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(catalog_lease_pk(catalog_uri)))
            .update_expression(
                "SET #catalog_uri = :catalog_uri, \
                 #owner_job_id = :owner_job_id, \
                 #lease_token = :lease_token, \
                 #expires_at = :expires_at, \
                 #updated_at = :now, \
                 #created_at = if_not_exists(#created_at, :now), \
                 #fencing_counter = if_not_exists(#fencing_counter, :zero) + :one",
            )
            .condition_expression(condition_expression)
            .expression_attribute_names("#catalog_uri", "catalog_uri")
            .expression_attribute_names("#owner_job_id", "owner_job_id")
            .expression_attribute_names("#lease_token", "lease_token")
            .expression_attribute_names("#expires_at", "expires_at_unix_secs")
            .expression_attribute_names("#updated_at", "updated_at_unix_secs")
            .expression_attribute_names("#created_at", "created_at_unix_secs")
            .expression_attribute_names("#fencing_counter", "fencing_counter")
            .expression_attribute_values(":catalog_uri", AttributeValue::S(catalog_uri.to_owned()))
            .expression_attribute_values(":owner_job_id", AttributeValue::S(owner_job_id.to_owned()))
            .expression_attribute_values(":lease_token", AttributeValue::S(lease_token.to_owned()))
            .expression_attribute_values(":expires_at", AttributeValue::N(expires_at.to_string()))
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .expression_attribute_values(":zero", AttributeValue::N("0".to_owned()))
            .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
            .return_values(aws_sdk_dynamodb::types::ReturnValue::AllNew)
            .send()
            .await
            .with_context(|| format!("update catalog lease for {catalog_uri}"))?;
        let attributes = output
            .attributes
            .ok_or_else(|| anyhow!("DynamoDB catalog lease update returned no attributes"))?;
        catalog_lease_from_item(&attributes)
    }
}

#[async_trait]
impl CatalogLeaseStore for DynamoDbCatalogLeaseStore {
    async fn acquire(&self, catalog_uri: &str, owner_job_id: &str) -> Result<CatalogLease> {
        let token = Uuid::new_v4().to_string();
        let now = unix_secs_i64();
        self.update_lease(
            catalog_uri,
            owner_job_id,
            &token,
            "attribute_not_exists(pk) OR #expires_at < :now",
        )
        .await
        .with_context(|| {
            format!(
                "catalog lease for {catalog_uri} is held by another worker at {now}"
            )
        })
    }

    async fn renew(&self, lease: &CatalogLease) -> Result<CatalogLease> {
        self.update_lease(
            &lease.catalog_uri,
            &lease.owner_job_id,
            &lease.lease_token,
            "#owner_job_id = :owner_job_id AND #lease_token = :lease_token",
        )
        .await
    }

    async fn assert_owned(&self, lease: &CatalogLease) -> Result<()> {
        let output = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(catalog_lease_pk(&lease.catalog_uri)))
            .consistent_read(true)
            .send()
            .await
            .with_context(|| format!("read catalog lease for {}", lease.catalog_uri))?;
        let item = output
            .item
            .ok_or_else(|| anyhow!("catalog lease no longer exists for {}", lease.catalog_uri))?;
        let current = catalog_lease_from_item(&item)?;
        if current.owner_job_id != lease.owner_job_id || current.lease_token != lease.lease_token {
            bail!("catalog lease lost for {}", lease.catalog_uri);
        }
        if current.expires_at_unix_secs <= unix_secs_i64() {
            bail!("catalog lease expired for {}", lease.catalog_uri);
        }
        Ok(())
    }

    async fn release(&self, lease: &CatalogLease) -> Result<()> {
        let result = self
            .client
            .delete_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(catalog_lease_pk(&lease.catalog_uri)))
            .condition_expression("#owner_job_id = :owner_job_id AND #lease_token = :lease_token")
            .expression_attribute_names("#owner_job_id", "owner_job_id")
            .expression_attribute_names("#lease_token", "lease_token")
            .expression_attribute_values(":owner_job_id", AttributeValue::S(lease.owner_job_id.clone()))
            .expression_attribute_values(":lease_token", AttributeValue::S(lease.lease_token.clone()))
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if is_dynamodb_conditional_error(&error) => Ok(()),
            Err(error) => Err(anyhow!("release catalog lease: {error}")),
        }
    }
}

pub async fn upload_with_owned_catalog_lease<F, Fut>(
    lease_store: &dyn CatalogLeaseStore,
    lease: &CatalogLease,
    upload: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    lease_store.assert_owned(lease).await?;
    upload().await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogUploadCondition {
    IfMatch {
        e_tag: String,
        version_id: Option<String>,
    },
    IfNoneMatch,
}

/// Downloads the DuckLake catalog from S3 to a local file so it can be
/// opened in read-write mode. DuckLake cannot open S3 catalog metadata
/// files for writing ("Cannot open an HTTP file for both reading and
/// writing"), so we download → modify locally → upload back.
/// Data files go directly to S3 via httpfs during translate.
pub struct CatalogDownload {
    local_path: PathBuf,
    s3_bucket: String,
    s3_key: String,
    upload_condition: CatalogUploadCondition,
}

impl CatalogDownload {
    pub async fn fetch(catalog_dsn: &str) -> Result<Option<Self>> {
        if !catalog_dsn.starts_with("s3://") {
            return Ok(None);
        }
        let parsed = parse_s3_uri(catalog_dsn).map_err(|e| anyhow!("{e}"))?;
        let bucket = parsed.bucket;
        let key = parsed.key;
        let local_path = PathBuf::from("/tmp/catalog.ducklake");

        let client = s3_client();

        eprintln!("[worker] downloading catalog from s3://{bucket}/{key}");
        let resp = client
            .get_object()
            .bucket(&bucket)
            .key(&key)
            .send()
            .await
            .context("failed to download DuckLake catalog from S3")?;
        let e_tag = resp.e_tag().map(str::to_owned);
        let version_id = resp.version_id().map(str::to_owned);
        let upload_condition = match e_tag {
            Some(e_tag) => CatalogUploadCondition::IfMatch { e_tag, version_id },
            None => bail!(
                "downloaded S3 catalog had no ETag; refusing to allow unconditional upload"
            ),
        };
        let body = resp
            .body
            .collect()
            .await
            .context("failed to read catalog body")?;
        fs::write(&local_path, body.into_bytes())
            .with_context(|| format!("failed to write catalog to {}", local_path.display()))?;

        Ok(Some(Self {
            local_path,
            s3_bucket: bucket,
            s3_key: key,
            upload_condition,
        }))
    }

    pub fn local_path(&self) -> &Path {
        &self.local_path
    }

    pub fn upload_condition(&self) -> &CatalogUploadCondition {
        &self.upload_condition
    }

    pub async fn upload(&self) -> Result<()> {
        self.upload_with_client(&s3_client()).await
    }

    pub async fn upload_with_client(&self, client: &aws_sdk_s3::Client) -> Result<()> {
        let data = fs::read(&self.local_path).with_context(|| {
            format!("failed to read catalog from {}", self.local_path.display())
        })?;

        eprintln!(
            "[worker] uploading catalog ({} bytes) to s3://{}/{}",
            data.len(),
            self.s3_bucket,
            self.s3_key
        );
        let request = client
            .put_object()
            .bucket(&self.s3_bucket)
            .key(&self.s3_key)
            .body(ByteStream::from(data));
        let request = match &self.upload_condition {
            CatalogUploadCondition::IfMatch { e_tag, .. } => request.if_match(e_tag),
            CatalogUploadCondition::IfNoneMatch => request.if_none_match("*"),
        };
        request
            .send()
            .await
            .context("failed to upload DuckLake catalog to S3")?;
        Ok(())
    }
}

async fn await_translate_with_lease_renewal(
    mut translate_task: tokio::task::JoinHandle<Result<TranslateStats, WorkerError>>,
    leases: Arc<dyn CatalogLeaseStore>,
    mut lease: Option<CatalogLease>,
) -> Result<(TranslateStats, Option<CatalogLease>), WorkerError> {
    let Some(mut current_lease) = lease.take() else {
        let stats = translate_task
            .await
            .map_err(|error| WorkerError::Build(format!("worker join failed: {error}")))??;
        return Ok((stats, None));
    };

    let mut interval = tokio::time::interval(Duration::from_secs(CATALOG_LEASE_RENEW_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;
    loop {
        tokio::select! {
            result = &mut translate_task => {
                let stats = result
                    .map_err(|error| WorkerError::Build(format!("worker join failed: {error}")))??;
                return Ok((stats, Some(current_lease)));
            }
            _ = interval.tick() => {
                current_lease = leases.renew(&current_lease).await.map_err(|error| {
                    WorkerError::Translate(format!("renew catalog lease while translating: {error:#}"))
                })?;
            }
        }
    }
}

async fn record_job_stage_best_effort(jobs: &dyn JobStore, job_id: &str, stage: &str) {
    if let Err(error) = jobs.update_stage(job_id, JobStatus::Running, stage).await {
        eprintln!(
            "[worker] warning: failed to record stage `{stage}` for job `{job_id}`: {error:#}"
        );
    }
}

async fn mark_job_complete_best_effort(jobs: &dyn JobStore, env: &JobEnv, stats: &TranslateStats) {
    if let Err(error) = jobs
        .mark_complete(&env.job_id, stats.snapshot_id, json!(&stats.rows_inserted))
        .await
    {
        eprintln!(
            "[worker] warning: failed to mark job `{}` complete in JobStore: {error:#}",
            env.job_id
        );
    }
}

async fn mark_job_failed_best_effort(
    jobs: &dyn JobStore,
    env: &JobEnv,
    error_code: &str,
    error_detail: &str,
) {
    if let Err(error) = jobs
        .mark_failed(&env.job_id, error_code, error_detail)
        .await
    {
        eprintln!(
            "[worker] warning: failed to mark job `{}` failed in JobStore: {error:#}",
            env.job_id
        );
    }
}

fn catalog_lease_pk(catalog_uri: &str) -> String {
    format!("CATALOG#{}", sha256_hex(catalog_uri))
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

fn catalog_lease_from_item(
    item: &std::collections::HashMap<String, AttributeValue>,
) -> Result<CatalogLease> {
    Ok(CatalogLease {
        catalog_uri: dynamodb_string_attr(item, "catalog_uri")?,
        owner_job_id: dynamodb_string_attr(item, "owner_job_id")?,
        lease_token: dynamodb_string_attr(item, "lease_token")?,
        expires_at_unix_secs: dynamodb_i64_attr(item, "expires_at_unix_secs")?,
        fencing_counter: dynamodb_i64_attr(item, "fencing_counter")?,
    })
}

fn dynamodb_string_attr(
    item: &std::collections::HashMap<String, AttributeValue>,
    name: &str,
) -> Result<String> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Ok(value.clone()),
        Some(_) => bail!("DynamoDB catalog lease attribute `{name}` is not a string"),
        None => bail!("DynamoDB catalog lease missing string attribute `{name}`"),
    }
}

fn dynamodb_i64_attr(
    item: &std::collections::HashMap<String, AttributeValue>,
    name: &str,
) -> Result<i64> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value
            .parse()
            .with_context(|| format!("parse DynamoDB catalog lease number `{name}`")),
        Some(_) => bail!("DynamoDB catalog lease attribute `{name}` is not a number"),
        None => bail!("DynamoDB catalog lease missing number attribute `{name}`"),
    }
}

fn is_dynamodb_conditional_error(error: &impl std::fmt::Display) -> bool {
    let message = error.to_string();
    message.contains("ConditionalCheckFailed") || message.contains("TransactionCanceledException")
}

async fn write_checkpoint(env: &JobEnv, last_completed_stage: &str) -> Result<(), WorkerError> {
    let checkpoint = Checkpoint {
        job_id: &env.job_id,
        package: &env.package,
        revision: &env.revision,
        source: &env.source,
        source_url: &env.source_url,
        source_kind: &env.source_kind,
        last_completed_stage,
        fetched_source_bytes: None,
        build_completed: matches!(last_completed_stage, "translate" | "complete"),
        translate_partial: last_completed_stage == "translate",
        error: "spot_interrupted",
        written_at_unix_secs: unix_secs(),
    };
    let payload = serde_json::to_vec_pretty(&checkpoint)
        .map_err(|error| WorkerError::SfnSend(format!("serialize checkpoint: {error}")))?;

    if let Some(dir) = optional_env("SPUR_CONTEXT_WORKER_CHECKPOINT_DIR") {
        let path = PathBuf::from(dir)
            .join("jobs")
            .join(&env.job_id)
            .join("checkpoint.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                WorkerError::SfnSend(format!(
                    "failed to create checkpoint dir `{}`: {error}",
                    parent.display()
                ))
            })?;
        }
        fs::write(&path, payload).map_err(|error| {
            WorkerError::SfnSend(format!(
                "failed to write checkpoint `{}`: {error}",
                path.display()
            ))
        })?;
        return Ok(());
    }

    let uri = optional_env("SPUR_CONTEXT_WORKER_CHECKPOINT_URI").unwrap_or_else(|| {
        format!(
            "s3://{DEFAULT_CHECKPOINT_BUCKET}/jobs/{}/checkpoint.json",
            env.job_id
        )
    });
    let s3_uri = parse_s3_uri(&uri)?;
    let client = s3_client();
    client
        .put_object()
        .bucket(s3_uri.bucket)
        .key(s3_uri.key)
        .body(ByteStream::from(payload))
        .send()
        .await
        .map_err(|error| WorkerError::SfnSend(format!("write checkpoint: {error}")))?;
    Ok(())
}

async fn send_task_success(env: &JobEnv, stats: &TranslateStats) -> Result<(), WorkerError> {
    let output = serde_json::to_string(&json!({
        "snapshot_id": stats.snapshot_id,
        "rows_inserted": stats.rows_inserted,
    }))
    .map_err(|error| WorkerError::SfnSend(format!("serialize success output: {error}")))?;

    sfn_client()
        .send_task_success()
        .task_token(env.task_token.clone())
        .output(output)
        .send()
        .await
        .map_err(|error| WorkerError::SfnSend(format!("SendTaskSuccess: {error}")))?;
    Ok(())
}

async fn send_task_failure(env: &JobEnv, error_code: &str, cause: &str) -> Result<(), WorkerError> {
    if matches!(
        optional_env("SPUR_CONTEXT_WORKER_SKIP_SFN").as_deref(),
        Some("1")
    ) {
        return Ok(());
    }
    sfn_client()
        .send_task_failure()
        .task_token(env.task_token.clone())
        .error(error_code.to_owned())
        .cause(cause.to_owned())
        .send()
        .await
        .map_err(|error| WorkerError::SfnSend(format!("SendTaskFailure: {error}")))?;
    Ok(())
}

fn sfn_client() -> aws_sdk_sfn::Client {
    let mut builder = aws_sdk_sfn::Config::builder()
        .behavior_version(aws_sdk_sfn::config::BehaviorVersion::latest())
        .region(aws_sdk_sfn::config::Region::new(aws_region()));
    if let Some(endpoint) = aws_endpoint_url("SFN") {
        builder = builder.endpoint_url(endpoint);
    }
    if let Some(credentials) = aws_credentials_for_sfn() {
        builder = builder.credentials_provider(credentials);
    } else if aws_endpoint_url("SFN").is_some() {
        builder = builder.credentials_provider(aws_sdk_sfn::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "LocalEndpoint",
        ));
    }
    aws_sdk_sfn::Client::from_conf(builder.build())
}

fn s3_client() -> aws_sdk_s3::Client {
    let mut builder = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(aws_region()));
    if let Some(endpoint) = aws_endpoint_url("S3") {
        builder = builder.endpoint_url(endpoint).force_path_style(true);
    }
    if let Some(credentials) = aws_credentials_for_s3() {
        builder = builder.credentials_provider(credentials);
    } else if aws_endpoint_url("S3").is_some() {
        builder = builder.credentials_provider(aws_sdk_s3::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "LocalEndpoint",
        ));
    }
    aws_sdk_s3::Client::from_conf(builder.build())
}

fn dynamodb_client() -> DynamoDbClient {
    let mut builder = aws_sdk_dynamodb::Config::builder()
        .behavior_version(aws_sdk_dynamodb::config::BehaviorVersion::latest())
        .region(aws_sdk_dynamodb::config::Region::new(aws_region()));
    if let Some(endpoint) = aws_endpoint_url("DYNAMODB") {
        builder = builder.endpoint_url(endpoint);
    }
    if let Some(credentials) = aws_credentials_for_dynamodb() {
        builder = builder.credentials_provider(credentials);
    } else if aws_endpoint_url("DYNAMODB").is_some() {
        builder = builder.credentials_provider(aws_sdk_dynamodb::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "LocalEndpoint",
        ));
    }
    DynamoDbClient::from_conf(builder.build())
}

fn aws_credentials_for_sfn() -> Option<aws_sdk_sfn::config::Credentials> {
    credential_parts().map(|parts| {
        aws_sdk_sfn::config::Credentials::new(
            parts.access_key_id,
            parts.secret_access_key,
            parts.session_token,
            None,
            parts.provider_name,
        )
    })
}

fn aws_credentials_for_s3() -> Option<aws_sdk_s3::config::Credentials> {
    credential_parts().map(|parts| {
        aws_sdk_s3::config::Credentials::new(
            parts.access_key_id,
            parts.secret_access_key,
            parts.session_token,
            None,
            parts.provider_name,
        )
    })
}

fn aws_credentials_for_dynamodb() -> Option<aws_sdk_dynamodb::config::Credentials> {
    credential_parts().map(|parts| {
        aws_sdk_dynamodb::config::Credentials::new(
            parts.access_key_id,
            parts.secret_access_key,
            parts.session_token,
            None,
            parts.provider_name,
        )
    })
}

fn credential_parts() -> Option<CredentialParts> {
    if let (Some(access_key_id), Some(secret_access_key)) = (
        optional_env("AWS_ACCESS_KEY_ID"),
        optional_env("AWS_SECRET_ACCESS_KEY"),
    ) {
        return Some(CredentialParts {
            access_key_id,
            secret_access_key,
            session_token: optional_env("AWS_SESSION_TOKEN"),
            provider_name: "Env",
        });
    }
    ecs_credential_parts()
}

fn ecs_credential_parts() -> Option<CredentialParts> {
    let url = optional_env("AWS_CONTAINER_CREDENTIALS_FULL_URI").or_else(|| {
        optional_env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            .map(|path| format!("http://169.254.170.2{path}"))
    })?;
    let authorization = optional_env("AWS_CONTAINER_AUTHORIZATION_TOKEN").or_else(|| {
        optional_env("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE")
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|value| value.trim().to_owned())
    });
    let mut headers = Vec::new();
    if let Some(token) = authorization {
        headers.push(("Authorization", token));
    }
    let body = http_get_bytes(&url, ECS_CREDENTIALS_CAP_BYTES, &headers).ok()?;
    let credentials: EcsCredentials = serde_json::from_slice(&body).ok()?;
    Some(CredentialParts {
        access_key_id: credentials.access_key_id,
        secret_access_key: credentials.secret_access_key,
        session_token: credentials.token,
        provider_name: "EcsContainer",
    })
}

fn aws_region() -> String {
    optional_env("AWS_REGION")
        .or_else(|| optional_env("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|| "us-east-1".to_owned())
}

fn aws_endpoint_url(service: &str) -> Option<String> {
    optional_env(&format!("AWS_ENDPOINT_URL_{service}"))
        .or_else(|| optional_env("AWS_ENDPOINT_URL"))
}

#[cfg(unix)]
async fn wait_for_sigterm() -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    sigterm.recv().await;
    Ok(())
}

#[cfg(not(unix))]
async fn wait_for_sigterm() -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("install ctrl-c handler")
}

fn http_get_bytes(
    url: &str,
    body_cap_bytes: usize,
    headers: &[(&str, String)],
) -> std::result::Result<Vec<u8>, String> {
    let parsed = parse_http_url(url)?;
    let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))
        .map_err(|error| format!("connect to {}:{} failed: {error}", parsed.host, parsed.port))?;
    let mut request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: spur-context-service/1.0\r\nConnection: close\r\n",
        parsed.path, parsed.authority
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write http request failed: {error}"))?;

    let mut response = Vec::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("read http response failed: {error}"))?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..read]);
        if response.len() > body_cap_bytes + HTTP_HEADER_CAP_BYTES {
            return Err(format!(
                "HTTP response exceeded size cap: > {} bytes",
                body_cap_bytes
            ));
        }
    }

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| "HTTP response missing header terminator".to_owned())?;
    let headers = String::from_utf8_lossy(&response[..header_end]);
    let status = headers.lines().next().unwrap_or_default();
    if !(status.starts_with("HTTP/1.1 200") || status.starts_with("HTTP/1.0 200")) {
        return Err(format!("HTTP GET failed: {status}"));
    }
    let body = response[header_end..].to_vec();
    if body.len() > body_cap_bytes {
        return Err(format!(
            "HTTP body exceeded size cap: {} > {}",
            body.len(),
            body_cap_bytes
        ));
    }
    Ok(body)
}

fn parse_http_url(url: &str) -> std::result::Result<ParsedHttpUrl, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("only plain http URL is supported by internal client: {url}"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, format!("/{path}")),
        None => (rest, "/".to_owned()),
    };
    if authority.is_empty() {
        return Err("HTTP URL missing host".to_owned());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port
                .parse::<u16>()
                .map_err(|error| format!("invalid HTTP port `{port}`: {error}"))?;
            (host.to_owned(), port)
        }
        _ => (authority.to_owned(), 80),
    };
    Ok(ParsedHttpUrl {
        authority: authority.to_owned(),
        host,
        port,
        path,
    })
}

fn parse_s3_uri(uri: &str) -> Result<S3Uri, WorkerError> {
    let without_scheme = uri
        .strip_prefix("s3://")
        .ok_or_else(|| WorkerError::SfnSend(format!("checkpoint URI must be s3://: {uri}")))?;
    let (bucket, key) = without_scheme.split_once('/').ok_or_else(|| {
        WorkerError::SfnSend(format!("checkpoint URI must include bucket and key: {uri}"))
    })?;
    if bucket.is_empty() || key.is_empty() {
        return Err(WorkerError::SfnSend(format!(
            "checkpoint URI must include bucket and key: {uri}"
        )));
    }
    Ok(S3Uri {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    })
}

fn artifact_dir() -> PathBuf {
    env::var_os("SPUR_CONTEXT_WORKER_ARTIFACT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ARTIFACT_DIR))
}

fn tarball_size_cap_bytes() -> usize {
    optional_env("SPUR_CONTEXT_WORKER_TARBALL_CAP_BYTES")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TARBALL_SIZE_CAP_BYTES)
}

fn failure_error_code(error: &WorkerError) -> String {
    match error {
        WorkerError::Fetch(detail) => format!("fetch:{detail}"),
        WorkerError::Build(detail) => format!("build:{detail}"),
        WorkerError::Translate(detail) => format!("commit:{detail}"),
        WorkerError::SpotInterrupted => "spot_interrupted".to_owned(),
        WorkerError::SfnSend(detail) => format!("sfn_send:{detail}"),
    }
}

fn required_env(name: &'static str) -> Result<String> {
    let value = env::var(name).with_context(|| format!("{name} must be set"))?;
    if value.trim().is_empty() {
        bail!("{name} must be non-empty");
    }
    Ok(value)
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn unix_secs_i64() -> i64 {
    i64::try_from(unix_secs()).unwrap_or(i64::MAX)
}

fn command_stderr(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("exit status {}", output.status)
    } else {
        stderr
    }
}

#[derive(Debug)]
struct PreparedJob {
    _workspace: TempWorkspace,
    source_path: PathBuf,
    artifact_dir: PathBuf,
}

#[derive(Debug)]
struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new(job_id: &str) -> Result<Self, WorkerError> {
        let mut path = env::temp_dir();
        path.push(format!(
            "spur-context-worker-{}-{}-{}",
            sanitize_path_part(job_id),
            std::process::id(),
            unix_secs()
        ));
        fs::create_dir_all(&path).map_err(|error| {
            WorkerError::Fetch(format!(
                "failed to create workspace `{}`: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn sanitize_path_part(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Debug)]
struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var_os(key);
        env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => env::set_var(self.key, value),
            None => env::remove_var(self.key),
        }
    }
}

#[derive(Debug)]
struct ParsedHttpUrl {
    authority: String,
    host: String,
    port: u16,
    path: String,
}

#[derive(Debug)]
struct S3Uri {
    bucket: String,
    key: String,
}

#[derive(Debug)]
struct CredentialParts {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    provider_name: &'static str,
}

#[derive(Debug, Deserialize)]
struct EcsCredentials {
    #[serde(rename = "AccessKeyId")]
    access_key_id: String,
    #[serde(rename = "SecretAccessKey")]
    secret_access_key: String,
    #[serde(rename = "Token")]
    token: Option<String>,
}

#[derive(Debug, Serialize)]
struct Checkpoint<'a> {
    job_id: &'a str,
    package: &'a str,
    revision: &'a str,
    source: &'a str,
    source_url: &'a str,
    source_kind: &'a str,
    last_completed_stage: &'a str,
    fetched_source_bytes: Option<u64>,
    build_completed: bool,
    translate_partial: bool,
    error: &'a str,
    written_at_unix_secs: u64,
}
