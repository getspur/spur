//! Fargate worker: fetch source, build graph, translate to DuckLake.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context as _, Result};
use aws_sdk_s3::primitives::ByteStream;
use duckdb::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::catalog::connect_ducklake;
use crate::jobs::{update_status, JobStatus};
use crate::translate::{translate_artifact_to_ducklake, TranslateOptions, TranslateStats};

const DEFAULT_ARTIFACT_DIR: &str = "/tmp/artifact";
const DEFAULT_CHECKPOINT_BUCKET: &str = "spur-context";
const DEFAULT_TARBALL_SIZE_CAP_BYTES: usize = 500 * 1024 * 1024;
const HTTP_HEADER_CAP_BYTES: usize = 64 * 1024;
const ECS_CREDENTIALS_CAP_BYTES: usize = 64 * 1024;
const JINA_CODE_EMBED_MODEL_NAME: &str = "JinaEmbeddingsV2BaseCode";
const EMBED_MODEL_ENV: &str = "SPUR_EMBEDDING_MODEL";

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

#[derive(Debug, Clone)]
struct StageTracker(Arc<Mutex<String>>);

impl StageTracker {
    fn new() -> Self {
        Self(Arc::new(Mutex::new("starting".to_owned())))
    }

    fn set(&self, stage: &str) {
        if let Ok(mut current) = self.0.lock() {
            *current = stage.to_owned();
        }
    }

    fn get(&self) -> String {
        self.0
            .lock()
            .map(|stage| stage.clone())
            .unwrap_or_else(|_| "unknown".to_owned())
    }
}

pub async fn run_from_env() -> Result<(), WorkerError> {
    let env = JobEnv::from_env().map_err(|error| WorkerError::Fetch(error.to_string()))?;
    run_job_and_report(&env).await
}

pub async fn run_job_and_report(env: &JobEnv) -> Result<(), WorkerError> {
    let stage = StageTracker::new();
    let run = run_job_with_stage(env.clone(), stage.clone());

    tokio::select! {
        result = run => {
            match result {
                Ok(stats) => {
                    update_job_status(
                        env,
                        JobStatus::Complete,
                        Some(stats.snapshot_id),
                        None,
                        Some(json!(&stats.rows_inserted)),
                    );
                    send_task_success(env, &stats).await?;
                    Ok(())
                }
                Err(error) => {
                    let error_detail = error.to_string();
                    update_job_status(env, JobStatus::Failed, None, Some(&error_detail), None);
                    send_task_failure(env, &failure_error_code(&error), &error_detail).await?;
                    Err(error)
                }
            }
        }
        signal_result = wait_for_sigterm() => {
            if let Err(error) = signal_result {
                let worker_error = WorkerError::SfnSend(error.to_string());
                let error_detail = worker_error.to_string();
                update_job_status(env, JobStatus::Failed, None, Some(&error_detail), None);
                send_task_failure(env, &failure_error_code(&worker_error), &error_detail).await?;
                return Err(worker_error);
            }
            handle_spot_interruption(env, &stage.get()).await?;
            Err(WorkerError::SpotInterrupted)
        }
    }
}

pub async fn run_job(env: &JobEnv) -> Result<TranslateStats, WorkerError> {
    run_job_with_stage(env.clone(), StageTracker::new()).await
}

pub fn update_job_status(
    env: &JobEnv,
    status: JobStatus,
    snapshot_id: Option<i64>,
    error: Option<&str>,
    row_counts: Option<Value>,
) {
    let status_label = status.to_string();
    let result = (|| -> Result<()> {
        let conn = connect_ducklake(&env.catalog_dsn)
            .with_context(|| format!("connect catalog for index job `{}`", env.job_id))?;
        update_job_status_with_connection(&conn, env, status, snapshot_id, error, row_counts)
            .with_context(|| format!("update index_jobs for job `{}`", env.job_id))?;
        Ok(())
    })();

    if let Err(update_error) = result {
        eprintln!(
            "[worker] warning: failed to update index_jobs status to `{status_label}` for job `{}`: {update_error:#}",
            env.job_id
        );
    }
}

pub fn update_job_status_with_connection(
    conn: &Connection,
    env: &JobEnv,
    status: JobStatus,
    snapshot_id: Option<i64>,
    error: Option<&str>,
    row_counts: Option<Value>,
) -> crate::jobs::Result<()> {
    update_status(conn, &env.job_id, status, snapshot_id, error, row_counts)
}

async fn run_job_with_stage(
    env: JobEnv,
    stage: StageTracker,
) -> Result<TranslateStats, WorkerError> {
    tokio::task::spawn_blocking(move || run_job_blocking(&env, &stage))
        .await
        .map_err(|error| WorkerError::Build(format!("worker join failed: {error}")))?
}

fn run_job_blocking(env: &JobEnv, stage: &StageTracker) -> Result<TranslateStats, WorkerError> {
    let workspace = TempWorkspace::new(&env.job_id)?;
    let source_dest = workspace.path.join("source");
    let artifact_base = artifact_dir();

    stage.set("fetch_source");
    let source_path = fetch_source(
        &env.source_url,
        &env.source_kind,
        &env.revision,
        &source_dest,
    )?;

    stage.set("build_graph");
    prepare_artifact_dir(&artifact_base)?;
    build_graph(&source_path, &artifact_base)?;
    let artifact_dir = resolve_graph_artifact_dir(&artifact_base)?;

    stage.set("translate");
    let stats = translate_with_source_root(&artifact_dir, Some(&source_path), env)?;
    stage.set("complete");
    Ok(stats)
}

pub fn fetch_source(
    source_url: &str,
    source_kind: &str,
    revision: &str,
    dest: &Path,
) -> Result<PathBuf, WorkerError> {
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

    let _embed_model = EnvVarGuard::set(EMBED_MODEL_ENV, JINA_CODE_EMBED_MODEL_NAME);
    let options = spur_cli::commands::graph::GraphBuildOptions {
        root: Some(source_path.to_path_buf()),
        workspace: false,
        output: Some(artifact_dir.to_path_buf()),
        quiet: true,
        skip_analyst: true,
        with_temporal: false,
        temporal_shard_config: Default::default(),
    };

    spur_cli::commands::graph::build(options).map_err(|error| WorkerError::Build(error.to_string()))
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
    let opts = TranslateOptions {
        source: env.source.clone(),
        package: env.package.clone(),
        revision: env.revision.clone(),
        revision_kind: revision_kind_for_revision(&env.revision).to_owned(),
        artifact_dir: artifact_dir.to_path_buf(),
        source_root: source_root.map(Path::to_path_buf),
        catalog_dsn: env.catalog_dsn.clone(),
    };
    translate_artifact_to_ducklake(&opts).map_err(|error| WorkerError::Translate(error.to_string()))
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
        builder = builder.endpoint_url(endpoint);
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
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
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

fn revision_kind_for_revision(revision: &str) -> &'static str {
    if revision.contains('.') {
        "semver"
    } else {
        "git_sha"
    }
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

fn command_stderr(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("exit status {}", output.status)
    } else {
        stderr
    }
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
