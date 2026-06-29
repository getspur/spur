//! Fargate worker: fetch source, build graph, translate to DuckLake.

use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::future::Future;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context as _, Result};
use async_trait::async_trait;
use aws_sdk_dynamodb::{types::AttributeValue, Client as DynamoDbClient};
use aws_sdk_s3::primitives::ByteStream;
use duckdb::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::abuse;
use crate::catalog::{connect_ducklake_with_data_path, ducklake_data_path};
use crate::jobs::{DynamoDbJobStore, JobStatus, JobStore};
use crate::medallion::{SilverManifest, SilverManifestFile, SILVER_PREFIX};
use crate::translate::{
    translate_artifact_to_ducklake, TranslateLineage, TranslateOptions, TranslateStats,
    CATALOG_TABLES_SQL, DEFAULT_EMBED_TEXT_VERSION, DEFAULT_TRANSLATE_SCHEMA_VERSION,
};

const DEFAULT_ARTIFACT_DIR: &str = "/tmp/artifact";
const DEFAULT_CHECKPOINT_BUCKET: &str = "spur-context";
const DEFAULT_BRONZE_BUCKET: &str = "spur-context";
const DEFAULT_SILVER_BUCKET: &str = "spur-context";
const DEFAULT_TARBALL_SIZE_CAP_BYTES: usize = 500 * 1024 * 1024;
const DEFAULT_GIT_SIZE_CAP_BYTES: usize = 2 * 1024 * 1024 * 1024;
const DEFAULT_MAX_BUILD_SECONDS: u64 = 30 * 60;
const HTTP_HEADER_CAP_BYTES: usize = 64 * 1024;
const ECS_CREDENTIALS_CAP_BYTES: usize = 64 * 1024;
const EMBEDDING_GEMMA_EMBED_MODEL_NAME: &str = "EmbeddingGemma300M";
const EMBED_MODEL_ENV: &str = "SPUR_EMBEDDING_MODEL";
const GRAPH_SKIP_SECTION_EMBEDDINGS_ENV: &str = "SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS";
// Graviton2/neoverse-n1 Lambda cannot run the prebuilt ONNX Runtime's SVE/SME
// kernels; skip code-symbol embeddings too so `spur graph build` never loads ORT.
const GRAPH_SKIP_CODE_SYMBOL_EMBEDDINGS_ENV: &str = "SPUR_GRAPH_SKIP_CODE_SYMBOL_EMBEDDINGS";
const DEFAULT_CATALOG_LEASES_TABLE: &str = "spur-context-catalog-leases";
const CATALOG_PASSWORD_ENV: &str = "SPUR_CATALOG_PASSWORD";
const CATALOG_PASSWORD_SECRET_ARN_ENV: &str = "SPUR_CATALOG_PASSWORD_SECRET_ARN";
const CATALOG_LEASE_DURATION_SECS: i64 = 10 * 60;
const CATALOG_LEASE_RENEW_INTERVAL_SECS: u64 = 60;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobFromLayer {
    #[default]
    Source,
    Bronze,
    Silver,
}

impl JobFromLayer {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "source" => Ok(Self::Source),
            "bronze" => Ok(Self::Bronze),
            "silver" => Ok(Self::Silver),
            other => bail!("unsupported --from-layer `{other}`"),
        }
    }
}

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
    pub from_layer: JobFromLayer,
}

impl JobEnv {
    pub fn from_env() -> Result<Self> {
        let from_layer = optional_env("SPUR_CONTEXT_FROM_LAYER")
            .or_else(|| optional_env("FROM_LAYER"))
            .map(|value| JobFromLayer::parse(&value))
            .transpose()?
            .unwrap_or_default();
        Self::from_env_with_layer(from_layer)
    }

    pub fn from_env_args<I, S>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut from_layer = optional_env("SPUR_CONTEXT_FROM_LAYER")
            .or_else(|| optional_env("FROM_LAYER"))
            .map(|value| JobFromLayer::parse(&value))
            .transpose()?
            .unwrap_or_default();

        let mut args = args.into_iter().map(Into::into).skip(1).peekable();
        while let Some(arg) = args.next() {
            let arg = arg.to_string_lossy();
            if let Some(value) = arg.strip_prefix("--from-layer=") {
                from_layer = JobFromLayer::parse(value)?;
            } else if arg == "--from-layer" {
                let Some(value) = args.next() else {
                    bail!("--from-layer requires a value");
                };
                from_layer = JobFromLayer::parse(&value.to_string_lossy())?;
            }
        }

        Self::from_env_with_layer(from_layer)
    }

    fn from_env_with_layer(from_layer: JobFromLayer) -> Result<Self> {
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
            from_layer,
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

    async fn set_async(&self, stage: &str) {
        self.set_current(stage);
        if let Some(reporter) = &self.reporter {
            reporter.record_async(stage).await;
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

impl Default for StageTracker {
    fn default() -> Self {
        Self::new()
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

    async fn record_async(&self, stage: &str) {
        let result = self
            .jobs
            .update_stage(&self.job_id, JobStatus::Running, stage)
            .await;
        if let Err(error) = result {
            eprintln!(
                "[worker] warning: failed to record stage `{stage}` for job `{}`: {error:#}",
                self.job_id
            );
        }
    }
}

pub async fn run_from_env() -> Result<(), WorkerError> {
    let env = JobEnv::from_env_args(env::args_os())
        .map_err(|error| WorkerError::Fetch(error.to_string()))?;
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
                    let error_code = failure_error_code(&error);
                    eprintln!("[worker] job failed: {error_detail}");
                    mark_job_failed_best_effort(
                        jobs.as_ref(),
                        env,
                        &error_code,
                        &error_detail,
                    )
                    .await;
                    if let Err(sfn_err) = send_task_failure(env, &error_code, &error_detail).await {
                        eprintln!("[worker] SendTaskFailure also failed: {sfn_err:#}");
                    }
                    Err(error)
                }
            }
        }
        signal_result = wait_for_sigterm() => {
            if let Err(error) = signal_result {
                let worker_error = WorkerError::SfnSend(error.to_string());
                let error_detail = worker_error.to_string();
                let error_code = failure_error_code(&worker_error);
                mark_job_failed_best_effort(
                    jobs.as_ref(),
                    env,
                    &error_code,
                    &error_detail,
                )
                .await;
                send_task_failure(env, &error_code, &error_detail).await?;
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

pub async fn run_job_and_record(env: &JobEnv) -> Result<TranslateStats, WorkerError> {
    let dynamodb = dynamodb_client();
    let jobs = Arc::new(DynamoDbJobStore::new(dynamodb.clone()));
    let leases = Arc::new(DynamoDbCatalogLeaseStore::new(dynamodb));
    run_job_and_record_with_services(env, jobs, leases).await
}

pub async fn run_job_and_record_with_services(
    env: &JobEnv,
    jobs: Arc<dyn JobStore>,
    leases: Arc<dyn CatalogLeaseStore>,
) -> Result<TranslateStats, WorkerError> {
    match run_job_with_services(env, jobs.clone(), leases).await {
        Ok(stats) => Ok(stats),
        Err(error) => {
            let error_detail = format!("{error:#}");
            let error_code = failure_error_code(&error);
            eprintln!("[worker] job failed: {error_detail}");
            mark_job_failed_best_effort(jobs.as_ref(), env, &error_code, &error_detail).await;
            Err(error)
        }
    }
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
    ensure_catalog_password_env().await?;
    let prepared = prepare_job(&env, &stage, leases.as_ref()).await?;

    let mut lease = None;
    if env.catalog_dsn.starts_with("s3://") {
        record_job_stage_best_effort(jobs.as_ref(), &env.job_id, "waiting_catalog_lease").await;
        lease = Some(
            leases
                .acquire(&env.catalog_dsn, &env.job_id)
                .await
                .map_err(|error| {
                    WorkerError::Translate(format!("acquire catalog lease: {error:#}"))
                })?,
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
            await_translate_with_lease_renewal(translate_task, leases.clone(), lease.clone())
                .await?;
        if renewed_lease.is_some() {
            lease = renewed_lease;
        }

        if let Some(ref dl) = catalog_dl {
            if let Some(current_lease) = lease.as_mut() {
                *current_lease = leases.renew(current_lease).await.map_err(|error| {
                    WorkerError::Translate(format!("renew catalog lease before upload: {error:#}"))
                })?;
                upload_with_owned_catalog_lease(leases.as_ref(), current_lease, || dl.upload())
                    .await
                    .map_err(|error| {
                        WorkerError::Translate(format!("upload catalog: {error:#}"))
                    })?;
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

async fn prepare_job(
    env: &JobEnv,
    stage: &StageTracker,
    leases: &dyn CatalogLeaseStore,
) -> Result<PreparedJob, WorkerError> {
    match env.from_layer {
        JobFromLayer::Source => prepare_job_from_source(env, stage, leases).await,
        JobFromLayer::Bronze => prepare_job_from_bronze(env, stage, leases).await,
        JobFromLayer::Silver => prepare_job_from_silver(env, stage, leases).await,
    }
}

async fn prepare_job_from_source(
    env: &JobEnv,
    stage: &StageTracker,
    leases: &dyn CatalogLeaseStore,
) -> Result<PreparedJob, WorkerError> {
    let workspace = TempWorkspace::new(&env.job_id)?;
    let source_dest = workspace.path.join("source");

    stage.set_async("fetch_source").await;
    let stage_started = log_stage_started("fetch_source");
    let source_path = fetch_source_with_default_bronze(env, &source_dest, leases).await?;
    log_stage_completed("fetch_source", stage_started);

    build_persist_and_prepare_default(env, stage, workspace, Some(source_path), leases).await
}

async fn prepare_job_from_bronze(
    env: &JobEnv,
    stage: &StageTracker,
    leases: &dyn CatalogLeaseStore,
) -> Result<PreparedJob, WorkerError> {
    let workspace = TempWorkspace::new(&env.job_id)?;
    let source_dest = workspace.path.join("source");

    stage.set_async("restore_bronze").await;
    let stage_started = log_stage_started("restore_bronze");
    let source_path = restore_bronze_source_with_default_services(env, &source_dest, leases)
        .await?
        .ok_or_else(|| {
            WorkerError::Fetch(format!(
                "bronze raw source missing for {}/{}/{}",
                env.source, env.package, env.revision
            ))
        })?;
    log_stage_completed("restore_bronze", stage_started);

    build_persist_and_prepare_default(env, stage, workspace, Some(source_path), leases).await
}

async fn prepare_job_from_silver(
    env: &JobEnv,
    stage: &StageTracker,
    _leases: &dyn CatalogLeaseStore,
) -> Result<PreparedJob, WorkerError> {
    let workspace = TempWorkspace::new(&env.job_id)?;
    let silver_store = S3SilverArtifactStore::new(silver_bucket());

    stage.set_async("restore_silver").await;
    let stage_started = log_stage_started("restore_silver");
    let row = lookup_silver_artifact_with_default_services(env)
        .await?
        .ok_or_else(|| {
            WorkerError::Build(format!(
                "silver graph artifact missing for {}/{}/{}",
                env.source, env.package, env.revision
            ))
        })?;
    let prepared = prepare_from_silver_row(env, workspace, None, row, &silver_store).await?;
    log_stage_completed("restore_silver", stage_started);
    Ok(prepared)
}

async fn build_persist_and_prepare_default(
    env: &JobEnv,
    stage: &StageTracker,
    workspace: TempWorkspace,
    source_path: Option<PathBuf>,
    leases: &dyn CatalogLeaseStore,
) -> Result<PreparedJob, WorkerError> {
    let source_path_for_build = source_path
        .as_ref()
        .ok_or_else(|| WorkerError::Build("source path is required to build silver".to_owned()))?;
    let artifact_base = artifact_dir();

    stage.set_async("build_graph").await;
    let stage_started = log_stage_started("build_graph");
    let artifact_dir = SpurGraphArtifactBuilder
        .build(source_path_for_build, &artifact_base)
        .await?;
    log_stage_completed("build_graph", stage_started);

    stage.set_async("persist_silver").await;
    let stage_started = log_stage_started("persist_silver");
    let persisted_silver =
        persist_silver_graph_artifact_with_default_services(env, &artifact_dir, leases).await?;
    let silver_artifact_dir = workspace.path.join("silver_artifact");
    let silver_store = S3SilverArtifactStore::new(silver_bucket());
    download_silver_artifact_from_manifest(
        &persisted_silver.row.manifest_uri,
        &persisted_silver.manifest,
        &silver_artifact_dir,
        &silver_store,
    )
    .await?;
    log_stage_completed("persist_silver", stage_started);

    Ok(prepared_job_from_silver(
        workspace,
        source_path,
        silver_artifact_dir,
        persisted_silver.row,
        persisted_silver.manifest,
    ))
}

pub async fn prepare_job_with_services(
    env: &JobEnv,
    stage: &StageTracker,
    bronze_registry: &dyn BronzeRawSourceRegistry,
    bronze_store: &dyn BronzeArchiveStore,
    silver_registry: &dyn SilverGraphArtifactRegistry,
    silver_store: &dyn SilverArtifactStore,
    graph_builder: &dyn GraphArtifactBuilder,
) -> Result<PreparedJob, WorkerError> {
    let workspace = TempWorkspace::new(&env.job_id)?;
    let build_services = BuildPersistServices {
        bronze_registry,
        silver_registry,
        silver_store,
        graph_builder,
    };
    match env.from_layer {
        JobFromLayer::Source => {
            let source_dest = workspace.path.join("source");
            stage.set_async("fetch_source").await;
            let stage_started = log_stage_started("fetch_source");
            let source_path =
                fetch_source_with_bronze_services(env, &source_dest, bronze_registry, bronze_store)
                    .await?;
            log_stage_completed("fetch_source", stage_started);
            build_persist_and_prepare_with_services(
                env,
                stage,
                workspace,
                source_path,
                &build_services,
            )
            .await
        }
        JobFromLayer::Bronze => {
            let source_dest = workspace.path.join("source");
            stage.set_async("restore_bronze").await;
            let stage_started = log_stage_started("restore_bronze");
            let source_path = retrieve_bronze_source_by_coordinate(
                &env.source,
                &env.package,
                &env.revision,
                &source_dest,
                bronze_registry,
                bronze_store,
            )
            .await?
            .ok_or_else(|| {
                WorkerError::Fetch(format!(
                    "bronze raw source missing for {}/{}/{}",
                    env.source, env.package, env.revision
                ))
            })?;
            log_stage_completed("restore_bronze", stage_started);
            build_persist_and_prepare_with_services(
                env,
                stage,
                workspace,
                source_path,
                &build_services,
            )
            .await
        }
        JobFromLayer::Silver => {
            stage.set_async("restore_silver").await;
            let stage_started = log_stage_started("restore_silver");
            let row = silver_registry
                .lookup(&env.source, &env.package, &env.revision)
                .await?
                .ok_or_else(|| {
                    WorkerError::Build(format!(
                        "silver graph artifact missing for {}/{}/{}",
                        env.source, env.package, env.revision
                    ))
                })?;
            let prepared = prepare_from_silver_row(env, workspace, None, row, silver_store).await?;
            log_stage_completed("restore_silver", stage_started);
            Ok(prepared)
        }
    }
}

struct BuildPersistServices<'a> {
    bronze_registry: &'a dyn BronzeRawSourceRegistry,
    silver_registry: &'a dyn SilverGraphArtifactRegistry,
    silver_store: &'a dyn SilverArtifactStore,
    graph_builder: &'a dyn GraphArtifactBuilder,
}

async fn build_persist_and_prepare_with_services(
    env: &JobEnv,
    stage: &StageTracker,
    workspace: TempWorkspace,
    source_path: PathBuf,
    services: &BuildPersistServices<'_>,
) -> Result<PreparedJob, WorkerError> {
    let artifact_base = artifact_dir();
    stage.set_async("build_graph").await;
    let stage_started = log_stage_started("build_graph");
    let artifact_dir = services
        .graph_builder
        .build(&source_path, &artifact_base)
        .await?;
    log_stage_completed("build_graph", stage_started);

    stage.set_async("persist_silver").await;
    let stage_started = log_stage_started("persist_silver");
    let bronze_content_sha256 = services
        .bronze_registry
        .lookup(&env.source, &env.package, &env.revision)
        .await?
        .map(|row| row.content_sha256)
        .ok_or_else(|| {
            WorkerError::Build(format!(
                "bronze raw source missing after restore for {}/{}/{}",
                env.source, env.package, env.revision
            ))
        })?;
    let builder_version = silver_builder_version(&artifact_dir)?;
    let persisted_silver = persist_silver_graph_artifact_with_manifest(
        env,
        &artifact_dir,
        &bronze_content_sha256,
        &builder_version,
        services.silver_store,
        services.silver_registry,
    )
    .await?;
    let silver_artifact_dir = workspace.path.join("silver_artifact");
    download_silver_artifact_from_manifest(
        &persisted_silver.row.manifest_uri,
        &persisted_silver.manifest,
        &silver_artifact_dir,
        services.silver_store,
    )
    .await?;
    log_stage_completed("persist_silver", stage_started);

    Ok(prepared_job_from_silver(
        workspace,
        Some(source_path),
        silver_artifact_dir,
        persisted_silver.row,
        persisted_silver.manifest,
    ))
}

async fn prepare_from_silver_row(
    env: &JobEnv,
    workspace: TempWorkspace,
    source_path: Option<PathBuf>,
    row: SilverGraphArtifact,
    silver_store: &dyn SilverArtifactStore,
) -> Result<PreparedJob, WorkerError> {
    let manifest = silver_store.download_manifest(&row.manifest_uri).await?;
    silver_store
        .validate_manifest(&row.manifest_uri, &manifest)
        .await?;
    let silver_artifact_dir = workspace.path.join("silver_artifact");
    download_silver_artifact_from_manifest(
        &row.manifest_uri,
        &manifest,
        &silver_artifact_dir,
        silver_store,
    )
    .await?;

    if row.source != env.source || row.package != env.package || row.version != env.revision {
        return Err(WorkerError::Build(format!(
            "silver graph artifact coordinate mismatch: expected {}/{}/{} got {}/{}/{}",
            env.source, env.package, env.revision, row.source, row.package, row.version
        )));
    }

    Ok(prepared_job_from_silver(
        workspace,
        source_path,
        silver_artifact_dir,
        row,
        manifest,
    ))
}

fn prepared_job_from_silver(
    workspace: TempWorkspace,
    source_path: Option<PathBuf>,
    artifact_dir: PathBuf,
    row: SilverGraphArtifact,
    manifest: SilverManifest,
) -> PreparedJob {
    let allow_missing_embeddings = row.embedding_count == 0;
    PreparedJob {
        _workspace: workspace,
        source_path,
        artifact_dir,
        artifact_manifest: Some(manifest),
        lineage: Some(TranslateLineage {
            bronze_content_sha256: row.bronze_content_sha256,
            silver_graph_content_hash: row.graph_content_hash,
            builder_version: row.builder_version,
            translate_schema_version: DEFAULT_TRANSLATE_SCHEMA_VERSION.to_owned(),
            embed_text_version: DEFAULT_EMBED_TEXT_VERSION.to_owned(),
        }),
        allow_missing_embeddings,
    }
}

fn translate_prepared_blocking(
    env: &JobEnv,
    stage: &StageTracker,
    prepared: &PreparedJob,
) -> Result<TranslateStats, WorkerError> {
    stage.set_current("translate");
    let stage_started = log_stage_started("translate");
    let stats = translate_with_source_root(
        &prepared.artifact_dir,
        prepared.source_path.as_deref(),
        prepared.artifact_manifest.clone(),
        prepared.lineage.clone(),
        prepared.allow_missing_embeddings,
        env,
    )?;
    log_stage_completed("translate", stage_started);
    stage.set_current("complete");
    Ok(stats)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BronzeRawSource {
    pub source: String,
    pub package: String,
    pub version: String,
    pub revision_kind: String,
    pub semver_major: Option<i32>,
    pub semver_minor: Option<i32>,
    pub semver_patch: Option<i32>,
    pub source_kind: String,
    pub source_url: String,
    pub s3_uri: String,
    pub content_sha256: String,
    pub bytes: u64,
    pub fetched_at: i64,
    pub fetch_status: String,
}

#[async_trait]
pub trait BronzeRawSourceRegistry: Send + Sync {
    async fn lookup(
        &self,
        source: &str,
        package: &str,
        version: &str,
    ) -> Result<Option<BronzeRawSource>, WorkerError>;
    async fn register(&self, row: &BronzeRawSource) -> Result<(), WorkerError>;
}

#[async_trait]
pub trait BronzeArchiveStore: Send + Sync {
    async fn content_sha256(&self, s3_uri: &str) -> Result<Option<String>, WorkerError>;
    async fn download_to_path(&self, s3_uri: &str, path: &Path) -> Result<(), WorkerError>;
    async fn upload_path(
        &self,
        key: &str,
        content_sha256: &str,
        path: &Path,
    ) -> Result<String, WorkerError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilverGraphArtifact {
    pub source: String,
    pub package: String,
    pub version: String,
    pub revision_kind: String,
    pub semver_major: Option<i32>,
    pub semver_minor: Option<i32>,
    pub semver_patch: Option<i32>,
    pub bronze_content_sha256: String,
    pub builder_version: String,
    pub graph_content_hash: String,
    pub artifact_s3_prefix: String,
    pub manifest_uri: String,
    pub manifest_schema_hash: String,
    pub node_count: u64,
    pub edge_count: u64,
    pub file_count: u64,
    pub embedding_count: u64,
    pub built_at: i64,
    pub build_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilverUploadedFile {
    pub s3_uri: String,
    pub etag: String,
    pub size_bytes: u64,
}

#[async_trait]
pub trait SilverArtifactStore: Send + Sync {
    async fn upload_file(&self, key: &str, path: &Path) -> Result<SilverUploadedFile, WorkerError>;
    async fn upload_manifest(
        &self,
        key: &str,
        manifest: &SilverManifest,
    ) -> Result<String, WorkerError>;
    async fn validate_manifest(
        &self,
        manifest_uri: &str,
        manifest: &SilverManifest,
    ) -> Result<(), WorkerError>;
    async fn download_manifest(&self, manifest_uri: &str) -> Result<SilverManifest, WorkerError>;
    async fn download_manifest_file(
        &self,
        manifest_uri: &str,
        relative_path: &str,
        dest: &Path,
    ) -> Result<(), WorkerError>;
}

#[async_trait]
pub trait SilverGraphArtifactRegistry: Send + Sync {
    async fn lookup(
        &self,
        source: &str,
        package: &str,
        version: &str,
    ) -> Result<Option<SilverGraphArtifact>, WorkerError>;
    async fn register(&self, row: &SilverGraphArtifact) -> Result<(), WorkerError>;
}

#[async_trait]
pub trait GraphArtifactBuilder: Send + Sync {
    async fn build(&self, source_path: &Path, artifact_base: &Path)
        -> Result<PathBuf, WorkerError>;
}

#[derive(Debug, Clone, Copy)]
pub struct SpurGraphArtifactBuilder;

#[async_trait]
impl GraphArtifactBuilder for SpurGraphArtifactBuilder {
    async fn build(
        &self,
        source_path: &Path,
        artifact_base: &Path,
    ) -> Result<PathBuf, WorkerError> {
        let source_path = source_path.to_path_buf();
        let artifact_base = artifact_base.to_path_buf();
        tokio::task::spawn_blocking(move || {
            prepare_artifact_dir(&artifact_base)?;
            build_graph(&source_path, &artifact_base)?;
            resolve_graph_artifact_dir(&artifact_base)
        })
        .await
        .map_err(|error| WorkerError::Build(format!("worker join failed: {error}")))?
    }
}

#[derive(Debug)]
struct FetchedSourceArchive {
    source_path: PathBuf,
    archive_path: PathBuf,
    extension: &'static str,
    content_sha256: String,
    bytes: u64,
}

pub async fn fetch_source_with_bronze_services(
    env: &JobEnv,
    dest: &Path,
    registry: &dyn BronzeRawSourceRegistry,
    archive_store: &dyn BronzeArchiveStore,
) -> Result<PathBuf, WorkerError> {
    fetch_source_with_bronze_services_outcome(env, dest, registry, archive_store).await
}

async fn fetch_source_with_bronze_services_outcome(
    env: &JobEnv,
    dest: &Path,
    registry: &dyn BronzeRawSourceRegistry,
    archive_store: &dyn BronzeArchiveStore,
) -> Result<PathBuf, WorkerError> {
    let source_kind = normalize_source_kind(&env.source_kind)?;

    let existing = registry
        .lookup(&env.source, &env.package, &env.revision)
        .await?;
    if let Some(row) = existing.as_ref() {
        if let Some(source_path) =
            restore_registered_bronze_source(row, dest, archive_store).await?
        {
            return Ok(source_path);
        }
    }

    validate_source_url_for_fetch(&env.source_url)?;

    let source_url = env.source_url.clone();
    let revision = env.revision.clone();
    let dest = dest.to_path_buf();
    let fetched = tokio::task::spawn_blocking(move || {
        fetch_source_archive(&source_url, source_kind, &revision, &dest)
    })
    .await
    .map_err(|error| WorkerError::Fetch(format!("worker join failed: {error}")))??;

    if let Some(row) = existing.as_ref() {
        if row.fetch_status == "success" && row.content_sha256 != fetched.content_sha256 {
            let _ = fs::remove_file(&fetched.archive_path);
            return Err(WorkerError::Fetch(format!(
                "bronze content drift for {}/{}/{}: existing sha256 {} != fetched sha256 {}",
                env.source, env.package, env.revision, row.content_sha256, fetched.content_sha256
            )));
        }
    }

    let key = bronze_source_key(&env.source, &env.package, &env.revision, fetched.extension);
    let s3_uri = archive_store
        .upload_path(&key, &fetched.content_sha256, &fetched.archive_path)
        .await?;
    let row = bronze_row_from_env(env, &s3_uri, &fetched.content_sha256, fetched.bytes);
    registry.register(&row).await?;
    let _ = fs::remove_file(&fetched.archive_path);

    Ok(fetched.source_path)
}

pub async fn retrieve_bronze_source_by_coordinate(
    source: &str,
    package: &str,
    version: &str,
    dest: &Path,
    registry: &dyn BronzeRawSourceRegistry,
    archive_store: &dyn BronzeArchiveStore,
) -> Result<Option<PathBuf>, WorkerError> {
    let Some(row) = registry.lookup(source, package, version).await? else {
        return Ok(None);
    };
    restore_registered_bronze_source(&row, dest, archive_store).await
}

async fn restore_registered_bronze_source(
    row: &BronzeRawSource,
    dest: &Path,
    archive_store: &dyn BronzeArchiveStore,
) -> Result<Option<PathBuf>, WorkerError> {
    if row.fetch_status != "success" {
        return Ok(None);
    }
    if archive_store.content_sha256(&row.s3_uri).await?.as_deref()
        != Some(row.content_sha256.as_str())
    {
        return Ok(None);
    }

    let extension = archive_extension_for_row(row)?;
    let archive = archive_path_for_restore(dest, extension);
    if let Some(parent) = archive.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            WorkerError::Fetch(format!("failed to create `{}`: {error}", parent.display()))
        })?;
    }
    archive_store
        .download_to_path(&row.s3_uri, &archive)
        .await?;
    let source_path = restore_source_archive(&row.source_kind, &row.version, &archive, dest);
    let _ = fs::remove_file(&archive);
    source_path.map(Some)
}

async fn fetch_source_with_default_bronze(
    env: &JobEnv,
    dest: &Path,
    leases: &dyn CatalogLeaseStore,
) -> Result<PathBuf, WorkerError> {
    let archive_store = S3BronzeArchiveStore::new(bronze_bucket());
    if env.catalog_dsn.starts_with("s3://") {
        return fetch_source_with_s3_catalog_bronze(env, dest, leases, &archive_store).await;
    }

    let registry = DuckLakeBronzeRegistry::new(env.catalog_dsn.clone());
    fetch_source_with_bronze_services(env, dest, &registry, &archive_store).await
}

async fn restore_bronze_source_with_default_services(
    env: &JobEnv,
    dest: &Path,
    _leases: &dyn CatalogLeaseStore,
) -> Result<Option<PathBuf>, WorkerError> {
    let archive_store = S3BronzeArchiveStore::new(bronze_bucket());
    if env.catalog_dsn.starts_with("s3://") {
        let Some(row) = lookup_bronze_row_in_s3_catalog(env).await? else {
            return Ok(None);
        };
        return restore_registered_bronze_source(&row, dest, &archive_store).await;
    }

    let registry = DuckLakeBronzeRegistry::new(env.catalog_dsn.clone());
    retrieve_bronze_source_by_coordinate(
        &env.source,
        &env.package,
        &env.revision,
        dest,
        &registry,
        &archive_store,
    )
    .await
}

async fn lookup_silver_artifact_with_default_services(
    env: &JobEnv,
) -> Result<Option<SilverGraphArtifact>, WorkerError> {
    if env.catalog_dsn.starts_with("s3://") {
        return lookup_silver_row_in_s3_catalog(env).await;
    }

    let registry = DuckLakeSilverRegistry::new(env.catalog_dsn.clone());
    registry
        .lookup(&env.source, &env.package, &env.revision)
        .await
}

async fn fetch_source_with_s3_catalog_bronze(
    env: &JobEnv,
    dest: &Path,
    leases: &dyn CatalogLeaseStore,
    archive_store: &dyn BronzeArchiveStore,
) -> Result<PathBuf, WorkerError> {
    let source_kind = normalize_source_kind(&env.source_kind)?;

    if let Some(row) = lookup_bronze_row_in_s3_catalog(env).await? {
        if let Some(source_path) =
            restore_registered_bronze_source(&row, dest, archive_store).await?
        {
            return Ok(source_path);
        }
    }

    validate_source_url_for_fetch(&env.source_url)?;

    let source_url = env.source_url.clone();
    let revision = env.revision.clone();
    let dest = dest.to_path_buf();
    let fetched = tokio::task::spawn_blocking(move || {
        fetch_source_archive(&source_url, source_kind, &revision, &dest)
    })
    .await
    .map_err(|error| WorkerError::Fetch(format!("worker join failed: {error}")))??;

    register_fetched_bronze_in_s3_catalog(env, leases, archive_store, &fetched).await?;
    let _ = fs::remove_file(&fetched.archive_path);
    Ok(fetched.source_path)
}

async fn lookup_bronze_row_in_s3_catalog(
    env: &JobEnv,
) -> Result<Option<BronzeRawSource>, WorkerError> {
    let catalog_dl = CatalogDownload::fetch(&env.catalog_dsn)
        .await
        .map_err(|error| WorkerError::Fetch(format!("download bronze catalog: {error:#}")))?;
    let Some(catalog_dl) = catalog_dl else {
        return Ok(None);
    };
    let registry =
        DuckLakeBronzeRegistry::new(catalog_dl.local_path().to_string_lossy().to_string());
    registry
        .lookup(&env.source, &env.package, &env.revision)
        .await
}

async fn lookup_silver_row_in_s3_catalog(
    env: &JobEnv,
) -> Result<Option<SilverGraphArtifact>, WorkerError> {
    let catalog_dl = CatalogDownload::fetch(&env.catalog_dsn)
        .await
        .map_err(|error| WorkerError::Build(format!("download silver catalog: {error:#}")))?;
    let Some(catalog_dl) = catalog_dl else {
        return Ok(None);
    };
    let registry =
        DuckLakeSilverRegistry::new(catalog_dl.local_path().to_string_lossy().to_string());
    registry
        .lookup(&env.source, &env.package, &env.revision)
        .await
}

async fn register_fetched_bronze_in_s3_catalog(
    env: &JobEnv,
    leases: &dyn CatalogLeaseStore,
    archive_store: &dyn BronzeArchiveStore,
    fetched: &FetchedSourceArchive,
) -> Result<(), WorkerError> {
    let lease = leases
        .acquire(&env.catalog_dsn, &env.job_id)
        .await
        .map_err(|error| WorkerError::Fetch(format!("acquire bronze catalog lease: {error:#}")))?;

    let result = async {
        let catalog_dl = CatalogDownload::fetch(&env.catalog_dsn)
            .await
            .map_err(|error| WorkerError::Fetch(format!("download bronze catalog: {error:#}")))?;
        let Some(catalog_dl) = catalog_dl else {
            return Ok(());
        };
        let registry =
            DuckLakeBronzeRegistry::new(catalog_dl.local_path().to_string_lossy().to_string());
        if let Some(row) = registry
            .lookup(&env.source, &env.package, &env.revision)
            .await?
        {
            if row.fetch_status == "success" && row.content_sha256 != fetched.content_sha256 {
                return Err(WorkerError::Fetch(format!(
                    "bronze content drift for {}/{}/{}: existing sha256 {} != fetched sha256 {}",
                    env.source,
                    env.package,
                    env.revision,
                    row.content_sha256,
                    fetched.content_sha256
                )));
            }
            if row.fetch_status == "success"
                && row.content_sha256 == fetched.content_sha256
                && archive_store.content_sha256(&row.s3_uri).await?.as_deref()
                    == Some(fetched.content_sha256.as_str())
            {
                return Ok(());
            }
        }

        let key = bronze_source_key(&env.source, &env.package, &env.revision, fetched.extension);
        let s3_uri = archive_store
            .upload_path(&key, &fetched.content_sha256, &fetched.archive_path)
            .await?;
        let row = bronze_row_from_env(env, &s3_uri, &fetched.content_sha256, fetched.bytes);
        registry.register(&row).await?;
        upload_with_owned_catalog_lease(leases, &lease, || catalog_dl.upload())
            .await
            .map_err(|error| WorkerError::Fetch(format!("upload bronze catalog: {error:#}")))?;
        Ok(())
    }
    .await;

    release_bronze_catalog_lease_best_effort(leases, &lease, env).await;
    result
}

async fn release_bronze_catalog_lease_best_effort(
    leases: &dyn CatalogLeaseStore,
    lease: &CatalogLease,
    env: &JobEnv,
) {
    if let Err(error) = leases.release(lease).await {
        eprintln!(
            "[worker] warning: failed to release bronze catalog lease for job `{}`: {error:#}",
            env.job_id
        );
    }
}

async fn persist_silver_graph_artifact_with_default_services(
    env: &JobEnv,
    artifact_dir: &Path,
    leases: &dyn CatalogLeaseStore,
) -> Result<PersistedSilverArtifact, WorkerError> {
    let bronze_content_sha256 = registered_bronze_content_sha256(env).await?;
    let builder_version = silver_builder_version(artifact_dir)?;
    let store = S3SilverArtifactStore::new(silver_bucket());

    if env.catalog_dsn.starts_with("s3://") {
        let lease = leases
            .acquire(&env.catalog_dsn, &env.job_id)
            .await
            .map_err(|error| {
                WorkerError::Build(format!("acquire silver catalog lease: {error:#}"))
            })?;
        let result = async {
            let catalog_dl = CatalogDownload::fetch(&env.catalog_dsn)
                .await
                .map_err(|error| {
                    WorkerError::Build(format!("download silver catalog: {error:#}"))
                })?;
            let Some(catalog_dl) = catalog_dl else {
                return Err(WorkerError::Build(
                    "S3 silver catalog download returned no local catalog".to_owned(),
                ));
            };
            let registry =
                DuckLakeSilverRegistry::new(catalog_dl.local_path().to_string_lossy().to_string());
            let persisted = persist_silver_graph_artifact_with_manifest(
                env,
                artifact_dir,
                &bronze_content_sha256,
                &builder_version,
                &store,
                &registry,
            )
            .await?;
            upload_with_owned_catalog_lease(leases, &lease, || catalog_dl.upload())
                .await
                .map_err(|error| WorkerError::Build(format!("upload silver catalog: {error:#}")))?;
            Ok(persisted)
        }
        .await;
        release_bronze_catalog_lease_best_effort(leases, &lease, env).await;
        return result;
    }

    let registry = DuckLakeSilverRegistry::new(env.catalog_dsn.clone());
    persist_silver_graph_artifact_with_manifest(
        env,
        artifact_dir,
        &bronze_content_sha256,
        &builder_version,
        &store,
        &registry,
    )
    .await
}

async fn registered_bronze_content_sha256(env: &JobEnv) -> Result<String, WorkerError> {
    let row = if env.catalog_dsn.starts_with("s3://") {
        lookup_bronze_row_in_s3_catalog(env).await?
    } else {
        let registry = DuckLakeBronzeRegistry::new(env.catalog_dsn.clone());
        registry
            .lookup(&env.source, &env.package, &env.revision)
            .await?
    };
    row.map(|row| row.content_sha256).ok_or_else(|| {
        WorkerError::Build(format!(
            "bronze raw source missing after fetch for {}/{}/{}",
            env.source, env.package, env.revision
        ))
    })
}

fn silver_builder_version(artifact_dir: &Path) -> Result<String, WorkerError> {
    if let Some(version) = optional_env("SPUR_GRAPH_BUILDER_VERSION") {
        return Ok(version);
    }
    let manifest_path = artifact_dir.join("manifest.json");
    let content = fs::read_to_string(&manifest_path).map_err(|error| {
        WorkerError::Build(format!(
            "failed to read graph artifact manifest `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
        WorkerError::Build(format!(
            "invalid graph artifact manifest `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    let schema = manifest
        .pointer("/schema_version")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown-schema");
    let extractor = manifest
        .pointer("/extractor_version")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown-extractor");
    Ok(format!("{schema}:{extractor}"))
}

pub fn fetch_source(
    source_url: &str,
    source_kind: &str,
    revision: &str,
    dest: &Path,
) -> Result<PathBuf, WorkerError> {
    validate_source_url_for_fetch(source_url)?;

    match normalize_source_kind(source_kind)? {
        "git" => fetch_git(source_url, revision, dest),
        "tarball" => fetch_tarball(source_url, dest),
        _ => unreachable!("normalize_source_kind returned unsupported source kind"),
    }
}

#[derive(Debug, Clone)]
pub struct DuckLakeBronzeRegistry {
    catalog_dsn: String,
}

impl DuckLakeBronzeRegistry {
    pub fn new(catalog_dsn: impl Into<String>) -> Self {
        Self {
            catalog_dsn: catalog_dsn.into(),
        }
    }

    fn connect(&self) -> Result<Connection, WorkerError> {
        let data_path = bronze_ducklake_data_path(&self.catalog_dsn)?;
        let conn = connect_ducklake_with_data_path(&self.catalog_dsn, &data_path)
            .map_err(|error| WorkerError::Fetch(format!("connect bronze catalog: {error:#}")))?;
        conn.execute_batch(CATALOG_TABLES_SQL).map_err(|error| {
            WorkerError::Fetch(format!("ensure bronze raw_sources schema: {error:#}"))
        })?;
        Ok(conn)
    }
}

#[async_trait]
impl BronzeRawSourceRegistry for DuckLakeBronzeRegistry {
    async fn lookup(
        &self,
        source: &str,
        package: &str,
        version: &str,
    ) -> Result<Option<BronzeRawSource>, WorkerError> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT source, package, version, revision_kind,
                       semver_major, semver_minor, semver_patch,
                       source_kind, source_url, s3_uri, content_sha256,
                       bytes, COALESCE(CAST(epoch(fetched_at) AS BIGINT), 0), fetch_status
                FROM bronze.raw_sources
                WHERE source = ? AND package = ? AND version = ?
                ORDER BY fetched_at DESC NULLS LAST
                LIMIT 1
                "#,
            )
            .map_err(|error| WorkerError::Fetch(format!("prepare bronze lookup: {error}")))?;
        let result = stmt.query_row(params![source, package, version], |row| {
            let bytes: i64 = row.get(11)?;
            Ok(BronzeRawSource {
                source: row.get(0)?,
                package: row.get(1)?,
                version: row.get(2)?,
                revision_kind: row.get(3)?,
                semver_major: row.get(4)?,
                semver_minor: row.get(5)?,
                semver_patch: row.get(6)?,
                source_kind: row.get(7)?,
                source_url: row.get(8)?,
                s3_uri: row.get(9)?,
                content_sha256: row.get(10)?,
                bytes: u64::try_from(bytes).unwrap_or(0),
                fetched_at: row.get(12)?,
                fetch_status: row.get(13)?,
            })
        });
        match result {
            Ok(row) => Ok(Some(row)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(WorkerError::Fetch(format!(
                "lookup bronze raw source: {error}"
            ))),
        }
    }

    async fn register(&self, row: &BronzeRawSource) -> Result<(), WorkerError> {
        let bytes = i64::try_from(row.bytes).map_err(|_| {
            WorkerError::Fetch(format!(
                "bronze archive too large to register: {}",
                row.bytes
            ))
        })?;
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO bronze.raw_sources (
                source, package, version, revision_kind,
                semver_major, semver_minor, semver_patch,
                source_kind, source_url, s3_uri, content_sha256,
                bytes, fetched_at, fetch_status
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CAST(to_timestamp(?) AS TIMESTAMP), ?)
            "#,
            params![
                row.source,
                row.package,
                row.version,
                row.revision_kind,
                row.semver_major,
                row.semver_minor,
                row.semver_patch,
                row.source_kind,
                row.source_url,
                row.s3_uri,
                row.content_sha256,
                bytes,
                row.fetched_at,
                row.fetch_status,
            ],
        )
        .map_err(|error| WorkerError::Fetch(format!("register bronze raw source: {error}")))?;
        conn.execute("FORCE CHECKPOINT", [])
            .map_err(|error| WorkerError::Fetch(format!("checkpoint bronze catalog: {error}")))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct S3BronzeArchiveStore {
    bucket: String,
}

impl S3BronzeArchiveStore {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
        }
    }
}

#[async_trait]
impl BronzeArchiveStore for S3BronzeArchiveStore {
    async fn content_sha256(&self, s3_uri: &str) -> Result<Option<String>, WorkerError> {
        let parsed = parse_s3_uri(s3_uri)
            .map_err(|error| WorkerError::Fetch(format!("invalid bronze S3 URI: {error}")))?;
        let resp = s3_client()
            .head_object()
            .bucket(parsed.bucket)
            .key(parsed.key)
            .send()
            .await
            .map_err(|error| WorkerError::Fetch(format!("head bronze archive: {error}")))?;
        Ok(resp
            .metadata()
            .and_then(|metadata| metadata.get("content-sha256"))
            .cloned())
    }

    async fn download_to_path(&self, s3_uri: &str, path: &Path) -> Result<(), WorkerError> {
        let parsed = parse_s3_uri(s3_uri)
            .map_err(|error| WorkerError::Fetch(format!("invalid bronze S3 URI: {error}")))?;
        let resp = s3_client()
            .get_object()
            .bucket(parsed.bucket)
            .key(parsed.key)
            .send()
            .await
            .map_err(|error| WorkerError::Fetch(format!("download bronze archive: {error}")))?;
        let body =
            resp.body.collect().await.map_err(|error| {
                WorkerError::Fetch(format!("read bronze archive body: {error}"))
            })?;
        tokio::fs::write(path, body.into_bytes())
            .await
            .map_err(|error| {
                WorkerError::Fetch(format!("failed to write `{}`: {error}", path.display()))
            })?;
        Ok(())
    }

    async fn upload_path(
        &self,
        key: &str,
        content_sha256: &str,
        path: &Path,
    ) -> Result<String, WorkerError> {
        let data = tokio::fs::read(path).await.map_err(|error| {
            WorkerError::Fetch(format!("failed to read `{}`: {error}", path.display()))
        })?;
        s3_client()
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .metadata("content-sha256", content_sha256)
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|error| WorkerError::Fetch(format!("upload bronze archive: {error}")))?;
        Ok(format!("s3://{}/{key}", self.bucket))
    }
}

#[derive(Debug, Clone)]
pub struct DuckLakeSilverRegistry {
    catalog_dsn: String,
}

impl DuckLakeSilverRegistry {
    pub fn new(catalog_dsn: impl Into<String>) -> Self {
        Self {
            catalog_dsn: catalog_dsn.into(),
        }
    }

    fn connect(&self) -> Result<Connection, WorkerError> {
        let data_path = bronze_ducklake_data_path(&self.catalog_dsn)
            .map_err(|error| WorkerError::Build(format!("silver catalog data path: {error}")))?;
        let conn = connect_ducklake_with_data_path(&self.catalog_dsn, &data_path)
            .map_err(|error| WorkerError::Build(format!("connect silver catalog: {error:#}")))?;
        conn.execute_batch(CATALOG_TABLES_SQL).map_err(|error| {
            WorkerError::Build(format!("ensure silver graph_artifacts schema: {error:#}"))
        })?;
        Ok(conn)
    }
}

#[async_trait]
impl SilverGraphArtifactRegistry for DuckLakeSilverRegistry {
    async fn lookup(
        &self,
        source: &str,
        package: &str,
        version: &str,
    ) -> Result<Option<SilverGraphArtifact>, WorkerError> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT source, package, version, revision_kind,
                       semver_major, semver_minor, semver_patch,
                       bronze_content_sha256, builder_version, graph_content_hash,
                       artifact_s3_prefix, manifest_uri, manifest_schema_hash,
                       node_count, edge_count, file_count, embedding_count,
                       COALESCE(CAST(epoch(built_at) AS BIGINT), 0), build_status
                FROM silver.graph_artifacts
                WHERE source = ? AND package = ? AND version = ? AND build_status = 'success'
                ORDER BY built_at DESC NULLS LAST
                LIMIT 1
                "#,
            )
            .map_err(|error| WorkerError::Build(format!("prepare silver lookup: {error}")))?;
        let result = stmt.query_row(params![source, package, version], |row| {
            let node_count: i64 = row.get(13)?;
            let edge_count: i64 = row.get(14)?;
            let file_count: i64 = row.get(15)?;
            let embedding_count: i64 = row.get(16)?;
            Ok(SilverGraphArtifact {
                source: row.get(0)?,
                package: row.get(1)?,
                version: row.get(2)?,
                revision_kind: row.get(3)?,
                semver_major: row.get(4)?,
                semver_minor: row.get(5)?,
                semver_patch: row.get(6)?,
                bronze_content_sha256: row.get(7)?,
                builder_version: row.get(8)?,
                graph_content_hash: row.get(9)?,
                artifact_s3_prefix: row.get(10)?,
                manifest_uri: row.get(11)?,
                manifest_schema_hash: row.get(12)?,
                node_count: u64::try_from(node_count).unwrap_or(0),
                edge_count: u64::try_from(edge_count).unwrap_or(0),
                file_count: u64::try_from(file_count).unwrap_or(0),
                embedding_count: u64::try_from(embedding_count).unwrap_or(0),
                built_at: row.get(17)?,
                build_status: row.get(18)?,
            })
        });
        match result {
            Ok(row) => Ok(Some(row)),
            Err(duckdb::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(WorkerError::Build(format!(
                "lookup silver graph artifact: {error}"
            ))),
        }
    }

    async fn register(&self, row: &SilverGraphArtifact) -> Result<(), WorkerError> {
        let node_count = i64::try_from(row.node_count)
            .map_err(|_| WorkerError::Build(format!("node count too large: {}", row.node_count)))?;
        let edge_count = i64::try_from(row.edge_count)
            .map_err(|_| WorkerError::Build(format!("edge count too large: {}", row.edge_count)))?;
        let file_count = i64::try_from(row.file_count)
            .map_err(|_| WorkerError::Build(format!("file count too large: {}", row.file_count)))?;
        let embedding_count = i64::try_from(row.embedding_count).map_err(|_| {
            WorkerError::Build(format!(
                "embedding count too large: {}",
                row.embedding_count
            ))
        })?;
        let conn = self.connect()?;
        conn.execute(
            r#"
            INSERT INTO silver.graph_artifacts (
                source, package, version, revision_kind,
                semver_major, semver_minor, semver_patch,
                bronze_content_sha256, builder_version, graph_content_hash,
                artifact_s3_prefix, manifest_uri, manifest_schema_hash,
                node_count, edge_count, file_count, embedding_count,
                built_at, build_status
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CAST(to_timestamp(?) AS TIMESTAMP), ?)
            "#,
            params![
                row.source,
                row.package,
                row.version,
                row.revision_kind,
                row.semver_major,
                row.semver_minor,
                row.semver_patch,
                row.bronze_content_sha256,
                row.builder_version,
                row.graph_content_hash,
                row.artifact_s3_prefix,
                row.manifest_uri,
                row.manifest_schema_hash,
                node_count,
                edge_count,
                file_count,
                embedding_count,
                row.built_at,
                row.build_status,
            ],
        )
        .map_err(|error| WorkerError::Build(format!("register silver graph artifact: {error}")))?;
        conn.execute("FORCE CHECKPOINT", [])
            .map_err(|error| WorkerError::Build(format!("checkpoint silver catalog: {error}")))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct S3SilverArtifactStore {
    bucket: String,
}

impl S3SilverArtifactStore {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
        }
    }
}

#[async_trait]
impl SilverArtifactStore for S3SilverArtifactStore {
    async fn upload_file(&self, key: &str, path: &Path) -> Result<SilverUploadedFile, WorkerError> {
        let data = tokio::fs::read(path).await.map_err(|error| {
            WorkerError::Build(format!(
                "failed to read silver file `{}`: {error}",
                path.display()
            ))
        })?;
        let size_bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let resp = s3_client()
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .if_none_match("*")
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|error| WorkerError::Build(format!("upload silver file `{key}`: {error}")))?;
        let etag = resp
            .e_tag()
            .ok_or_else(|| WorkerError::Build(format!("silver upload `{key}` returned no ETag")))?
            .to_owned();
        Ok(SilverUploadedFile {
            s3_uri: format!("s3://{}/{key}", self.bucket),
            etag,
            size_bytes,
        })
    }

    async fn upload_manifest(
        &self,
        key: &str,
        manifest: &SilverManifest,
    ) -> Result<String, WorkerError> {
        let data = serde_json::to_vec_pretty(manifest)
            .map_err(|error| WorkerError::Build(format!("encode silver manifest: {error}")))?;
        s3_client()
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .if_none_match("*")
            .content_type("application/json")
            .body(ByteStream::from(data))
            .send()
            .await
            .map_err(|error| {
                WorkerError::Build(format!("upload silver manifest `{key}`: {error}"))
            })?;
        Ok(format!("s3://{}/{key}", self.bucket))
    }

    async fn validate_manifest(
        &self,
        manifest_uri: &str,
        manifest: &SilverManifest,
    ) -> Result<(), WorkerError> {
        let parsed = parse_s3_uri(manifest_uri)
            .map_err(|error| WorkerError::Build(format!("invalid silver manifest URI: {error}")))?;
        let uploaded = self.download_manifest(manifest_uri).await?;
        if &uploaded != manifest {
            return Err(WorkerError::Build(
                "uploaded silver manifest does not match local manifest".to_owned(),
            ));
        }

        for file in &manifest.files {
            validate_silver_manifest_path(&file.path)?;
            let key = silver_manifest_file_key(&parsed.key, &file.path)?;
            let head = s3_client()
                .head_object()
                .bucket(&parsed.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|error| {
                    WorkerError::Build(format!("head silver file `{key}`: {error}"))
                })?;
            let content_length = head.content_length().unwrap_or_default();
            let content_length = u64::try_from(content_length).unwrap_or(u64::MAX);
            if content_length != file.size_bytes {
                return Err(WorkerError::Build(format!(
                    "silver manifest size mismatch for `{}`: manifest {} != S3 {}",
                    file.path, file.size_bytes, content_length
                )));
            }
            let etag = head.e_tag().unwrap_or_default();
            if etag != file.etag {
                return Err(WorkerError::Build(format!(
                    "silver manifest ETag mismatch for `{}`: manifest {} != S3 {}",
                    file.path, file.etag, etag
                )));
            }
        }
        Ok(())
    }

    async fn download_manifest(&self, manifest_uri: &str) -> Result<SilverManifest, WorkerError> {
        let parsed = parse_s3_uri(manifest_uri)
            .map_err(|error| WorkerError::Build(format!("invalid silver manifest URI: {error}")))?;
        let resp = s3_client()
            .get_object()
            .bucket(&parsed.bucket)
            .key(&parsed.key)
            .send()
            .await
            .map_err(|error| WorkerError::Build(format!("download silver manifest: {error}")))?;
        let body =
            resp.body.collect().await.map_err(|error| {
                WorkerError::Build(format!("read silver manifest body: {error}"))
            })?;
        serde_json::from_slice(&body.into_bytes())
            .map_err(|error| WorkerError::Build(format!("parse silver manifest: {error}")))
    }

    async fn download_manifest_file(
        &self,
        manifest_uri: &str,
        relative_path: &str,
        dest: &Path,
    ) -> Result<(), WorkerError> {
        validate_silver_manifest_path(relative_path)?;
        let parsed = parse_s3_uri(manifest_uri)
            .map_err(|error| WorkerError::Build(format!("invalid silver manifest URI: {error}")))?;
        let key = silver_manifest_file_key(&parsed.key, relative_path)?;
        let resp = s3_client()
            .get_object()
            .bucket(&parsed.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|error| {
                WorkerError::Build(format!("download silver file `{key}`: {error}"))
            })?;
        let body =
            resp.body.collect().await.map_err(|error| {
                WorkerError::Build(format!("read silver file `{key}`: {error}"))
            })?;
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                WorkerError::Build(format!(
                    "failed to create silver download dir `{}`: {error}",
                    parent.display()
                ))
            })?;
        }
        tokio::fs::write(dest, body.into_bytes())
            .await
            .map_err(|error| {
                WorkerError::Build(format!(
                    "failed to write silver download `{}`: {error}",
                    dest.display()
                ))
            })?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct PersistedSilverArtifact {
    pub row: SilverGraphArtifact,
    pub manifest: SilverManifest,
}

pub async fn persist_silver_graph_artifact(
    env: &JobEnv,
    artifact_dir: &Path,
    bronze_content_sha256: &str,
    builder_version: &str,
    store: &dyn SilverArtifactStore,
    registry: &dyn SilverGraphArtifactRegistry,
) -> Result<SilverGraphArtifact, WorkerError> {
    let persisted = persist_silver_graph_artifact_with_manifest(
        env,
        artifact_dir,
        bronze_content_sha256,
        builder_version,
        store,
        registry,
    )
    .await?;
    Ok(persisted.row)
}

async fn persist_silver_graph_artifact_with_manifest(
    env: &JobEnv,
    artifact_dir: &Path,
    bronze_content_sha256: &str,
    builder_version: &str,
    store: &dyn SilverArtifactStore,
    registry: &dyn SilverGraphArtifactRegistry,
) -> Result<PersistedSilverArtifact, WorkerError> {
    let metadata = read_graph_artifact_metadata(artifact_dir)?;
    let prefix =
        silver_artifact_key_prefix(&env.source, &env.package, &env.revision, builder_version);
    let artifact_s3_prefix = format!("s3://{}/{prefix}", silver_bucket());

    let mut manifest_files = Vec::new();
    for relative_path in collect_silver_artifact_files(artifact_dir)? {
        validate_silver_manifest_path(&relative_path)?;
        let path = artifact_dir.join(path_from_manifest_relative(&relative_path));
        let uploaded = store
            .upload_file(&format!("{prefix}{relative_path}"), &path)
            .await?;
        manifest_files.push(SilverManifestFile {
            path: relative_path,
            size_bytes: uploaded.size_bytes,
            etag: uploaded.etag,
        });
    }
    validate_required_silver_files(&manifest_files)?;

    let manifest = SilverManifest {
        schema_hash: metadata.schema_hash.clone(),
        files: manifest_files,
    };
    let manifest_key = format!("{prefix}manifest.json");
    let manifest_uri = store.upload_manifest(&manifest_key, &manifest).await?;
    store.validate_manifest(&manifest_uri, &manifest).await?;

    let revision = revision_metadata(&env.revision);
    let row = SilverGraphArtifact {
        source: env.source.clone(),
        package: env.package.clone(),
        version: env.revision.clone(),
        revision_kind: revision.kind,
        semver_major: revision.semver_major,
        semver_minor: revision.semver_minor,
        semver_patch: revision.semver_patch,
        bronze_content_sha256: bronze_content_sha256.to_owned(),
        builder_version: builder_version.to_owned(),
        graph_content_hash: metadata.graph_content_hash,
        artifact_s3_prefix,
        manifest_uri,
        manifest_schema_hash: metadata.schema_hash,
        node_count: metadata.node_count,
        edge_count: metadata.edge_count,
        file_count: metadata.file_count,
        embedding_count: metadata.embedding_count,
        built_at: unix_secs_i64(),
        build_status: "success".to_owned(),
    };
    registry.register(&row).await?;
    Ok(PersistedSilverArtifact { row, manifest })
}

pub async fn download_silver_artifact_from_manifest(
    manifest_uri: &str,
    manifest: &SilverManifest,
    dest: &Path,
    store: &dyn SilverArtifactStore,
) -> Result<(), WorkerError> {
    prepare_artifact_dir(dest)?;
    for file in &manifest.files {
        validate_silver_manifest_path(&file.path)?;
        let dest_path = dest.join(path_from_manifest_relative(&file.path));
        store
            .download_manifest_file(manifest_uri, &file.path, &dest_path)
            .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphArtifactMetadata {
    graph_content_hash: String,
    schema_hash: String,
    node_count: u64,
    edge_count: u64,
    file_count: u64,
    embedding_count: u64,
}

fn read_graph_artifact_metadata(artifact_dir: &Path) -> Result<GraphArtifactMetadata, WorkerError> {
    let manifest_path = artifact_dir.join("manifest.json");
    let content = fs::read_to_string(&manifest_path).map_err(|error| {
        WorkerError::Build(format!(
            "failed to read graph artifact manifest `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&content).map_err(|error| {
        WorkerError::Build(format!(
            "invalid graph artifact manifest `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    let graph_content_hash = json_string(&manifest, "/graph_content_hash")?;
    let schema_hash = silver_schema_hash(&manifest);
    let node_count = json_u64(&manifest, "/row_counts/nodes");
    let edge_count = json_u64(&manifest, "/row_counts/edges");
    let file_count = json_u64(&manifest, "/row_counts/files");
    let embedding_count = json_u64(&manifest, "/sidecar_row_counts/code_symbols");
    Ok(GraphArtifactMetadata {
        graph_content_hash,
        schema_hash,
        node_count,
        edge_count,
        file_count,
        embedding_count,
    })
}

fn json_string(manifest: &serde_json::Value, pointer: &str) -> Result<String, WorkerError> {
    manifest
        .pointer(pointer)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
        .ok_or_else(|| WorkerError::Build(format!("graph artifact manifest missing `{pointer}`")))
}

fn json_u64(manifest: &serde_json::Value, pointer: &str) -> u64 {
    manifest
        .pointer(pointer)
        .and_then(|value| value.as_u64())
        .unwrap_or_default()
}

fn silver_schema_hash(manifest: &serde_json::Value) -> String {
    let identity = json!({
        "graph_index_version": manifest.pointer("/graph_index_version").cloned().unwrap_or(serde_json::Value::Null),
        "schema_version": manifest.pointer("/schema_version").cloned().unwrap_or(serde_json::Value::Null),
        "manifest_version": manifest.pointer("/manifest_version").cloned().unwrap_or(serde_json::Value::Null),
        "extractor_version": manifest.pointer("/extractor_version").cloned().unwrap_or(serde_json::Value::Null),
    });
    format!("sha256:{}", sha256_hex(&identity.to_string()))
}

fn collect_silver_artifact_files(artifact_dir: &Path) -> Result<Vec<String>, WorkerError> {
    let mut files = Vec::new();
    collect_silver_artifact_files_inner(artifact_dir, artifact_dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_silver_artifact_files_inner(
    root: &Path,
    dir: &Path,
    files: &mut Vec<String>,
) -> Result<(), WorkerError> {
    let mut entries = fs::read_dir(dir)
        .map_err(|error| {
            WorkerError::Build(format!("failed to read `{}`: {error}", dir.display()))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| {
            WorkerError::Build(format!(
                "failed to read entry in `{}`: {error}",
                dir.display()
            ))
        })?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_silver_artifact_files_inner(root, &path, files)?;
        } else if path.is_file() {
            let relative = relative_manifest_path(root, &path)?;
            if relative == "manifest.json" {
                continue;
            }
            files.push(relative);
        }
    }
    Ok(())
}

fn relative_manifest_path(root: &Path, path: &Path) -> Result<String, WorkerError> {
    let relative = path.strip_prefix(root).map_err(|error| {
        WorkerError::Build(format!(
            "silver artifact path `{}` is not under `{}`: {error}",
            path.display(),
            root.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WorkerError::Build(format!(
                    "invalid silver artifact path `{}`",
                    path.display()
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

fn validate_required_silver_files(files: &[SilverManifestFile]) -> Result<(), WorkerError> {
    for required in [
        "nodes.parquet",
        "edges.parquet",
        "edges_unresolved.parquet",
        "files.parquet",
        "file_manifests.parquet",
    ] {
        if !files.iter().any(|file| file.path == required) {
            return Err(WorkerError::Build(format!(
                "silver artifact missing required file `{required}`"
            )));
        }
    }
    Ok(())
}

fn validate_silver_manifest_path(path: &str) -> Result<(), WorkerError> {
    if path.trim().is_empty() || path.contains('\\') {
        return Err(WorkerError::Build(format!(
            "invalid silver manifest path `{path}`"
        )));
    }
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err(WorkerError::Build(format!(
            "silver manifest path must be relative: `{path}`"
        )));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(WorkerError::Build(format!(
                    "silver manifest path escapes artifact root: `{path}`"
                )));
            }
        }
    }
    Ok(())
}

fn path_from_manifest_relative(relative_path: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for part in relative_path.split('/') {
        path.push(part);
    }
    path
}

fn silver_artifact_key_prefix(
    source: &str,
    package: &str,
    version: &str,
    builder_version: &str,
) -> String {
    format!("{SILVER_PREFIX}/{source}/{package}/{version}/{builder_version}/")
}

fn silver_manifest_file_key(
    manifest_key: &str,
    relative_path: &str,
) -> Result<String, WorkerError> {
    validate_silver_manifest_path(relative_path)?;
    let prefix = manifest_key.strip_suffix("manifest.json").ok_or_else(|| {
        WorkerError::Build(format!(
            "silver manifest key must end with manifest.json: `{manifest_key}`"
        ))
    })?;
    Ok(format!("{prefix}{relative_path}"))
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
    let _embed_model = EnvVarGuard::set(EMBED_MODEL_ENV, EMBEDDING_GEMMA_EMBED_MODEL_NAME);

    let started = Instant::now();
    eprintln!(
        "[worker] running spur graph build root={} output={}",
        source_path.display(),
        artifact_dir.display()
    );
    let mut child = Command::new("spur")
        .env(GRAPH_SKIP_SECTION_EMBEDDINGS_ENV, "1")
        .env(GRAPH_SKIP_CODE_SYMBOL_EMBEDDINGS_ENV, "1")
        .args([
            "graph",
            "build",
            "--root",
            &source_path.to_string_lossy(),
            "--output",
            &artifact_dir.to_string_lossy(),
            "--no-analyst",
        ])
        .spawn()
        .map_err(|error| {
            WorkerError::Build(format!("failed to run `spur graph build`: {error}"))
        })?;
    let status = wait_for_child_with_timeout(
        &mut child,
        max_build_duration(),
        "`spur graph build`",
        started,
    )?;

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

fn wait_for_child_with_timeout(
    child: &mut Child,
    timeout: Duration,
    label: &str,
    started: Instant,
) -> Result<ExitStatus, WorkerError> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| WorkerError::Build(format!("failed to wait for {label}: {error}")))?
        {
            return Ok(status);
        }

        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkerError::Build(format!(
                "{label} timed out after {} (limit {})",
                format_duration(started.elapsed()),
                format_duration_limit(timeout)
            )));
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn max_build_duration() -> Duration {
    Duration::from_secs(env_u64(
        "SPUR_CONTEXT_MAX_BUILD_SECONDS",
        DEFAULT_MAX_BUILD_SECONDS,
    ))
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

fn format_duration_limit(duration: Duration) -> String {
    if duration.subsec_millis() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format_duration(duration)
    }
}

pub fn translate(artifact_dir: &Path, env: &JobEnv) -> Result<TranslateStats, WorkerError> {
    translate_with_source_root(artifact_dir, None, None, None, false, env)
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

fn normalize_source_kind(source_kind: &str) -> Result<&'static str, WorkerError> {
    match source_kind.trim().to_ascii_lowercase().as_str() {
        "git" => Ok("git"),
        "tarball" => Ok("tarball"),
        other => Err(WorkerError::Fetch(format!(
            "unsupported SOURCE_KIND `{other}`"
        ))),
    }
}

fn validate_source_url_for_fetch(source_url: &str) -> Result<(), WorkerError> {
    if matches!(
        optional_env("SPUR_CONTEXT_WORKER_SKIP_ABUSE_REVALIDATE").as_deref(),
        Some("1")
    ) {
        return Ok(());
    }
    let parsed =
        abuse::validate(source_url, &abuse::ValidateOptions::default()).map_err(|error| {
            WorkerError::Fetch(format!("source_url abuse re-validation failed: {error}"))
        })?;
    abuse::resolve_and_check_dns(&parsed)
        .map_err(|error| WorkerError::Fetch(format!("source_url DNS check failed: {error}")))?;
    Ok(())
}

fn fetch_source_archive(
    source_url: &str,
    source_kind: &str,
    revision: &str,
    dest: &Path,
) -> Result<FetchedSourceArchive, WorkerError> {
    match source_kind {
        "git" => fetch_git_with_archive(source_url, revision, dest),
        "tarball" => fetch_tarball_with_archive(source_url, dest),
        _ => unreachable!("source kind is normalized before fetch_source_archive"),
    }
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

fn fetch_git_with_archive(
    source_url: &str,
    revision: &str,
    dest: &Path,
) -> Result<FetchedSourceArchive, WorkerError> {
    let source_path = fetch_git(source_url, revision, dest)?;
    enforce_source_tree_cap(&source_path, "git")?;
    let archive_path = archive_path_for_restore(dest, "gitbundle");
    if archive_path.exists() {
        fs::remove_file(&archive_path).map_err(|error| {
            WorkerError::Fetch(format!(
                "failed to clear `{}`: {error}",
                archive_path.display()
            ))
        })?;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&source_path)
        .args(["bundle", "create"])
        .arg(&archive_path)
        .args(["--all", "HEAD"])
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to run git bundle: {error}")))?;
    if !output.status.success() {
        return Err(WorkerError::Fetch(format!(
            "git bundle failed: {}",
            command_stderr(&output)
        )));
    }
    let (content_sha256, bytes) = archive_metadata(&archive_path)?;
    Ok(FetchedSourceArchive {
        source_path,
        archive_path,
        extension: "gitbundle",
        content_sha256,
        bytes,
    })
}

fn fetch_tarball(source_url: &str, dest: &Path) -> Result<PathBuf, WorkerError> {
    let fetched = fetch_tarball_with_archive(source_url, dest)?;
    fs::remove_file(&fetched.archive_path).map_err(|error| {
        WorkerError::Fetch(format!(
            "failed to remove `{}`: {error}",
            fetched.archive_path.display()
        ))
    })?;
    Ok(fetched.source_path)
}

fn fetch_tarball_with_archive(
    source_url: &str,
    dest: &Path,
) -> Result<FetchedSourceArchive, WorkerError> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|error| {
            WorkerError::Fetch(format!("failed to clear `{}`: {error}", dest.display()))
        })?;
    }
    fs::create_dir_all(dest).map_err(|error| {
        WorkerError::Fetch(format!("failed to create `{}`: {error}", dest.display()))
    })?;

    let extension = archive_extension_for_tarball_url(source_url);
    let archive = archive_path_for_restore(dest, extension);
    if let Some(parent) = archive.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            WorkerError::Fetch(format!("failed to create `{}`: {error}", parent.display()))
        })?;
    }
    if archive.exists() {
        fs::remove_file(&archive).map_err(|error| {
            WorkerError::Fetch(format!("failed to clear `{}`: {error}", archive.display()))
        })?;
    }
    download_tarball(source_url, &archive)?;
    extract_archive(&archive, dest)?;
    let source_path = single_extracted_root(dest).unwrap_or_else(|| dest.to_path_buf());
    enforce_source_tree_cap(&source_path, "tarball")?;

    let (content_sha256, bytes) = archive_metadata(&archive)?;
    Ok(FetchedSourceArchive {
        source_path,
        archive_path: archive,
        extension,
        content_sha256,
        bytes,
    })
}

fn restore_source_archive(
    source_kind: &str,
    revision: &str,
    archive: &Path,
    dest: &Path,
) -> Result<PathBuf, WorkerError> {
    match normalize_source_kind(source_kind)? {
        "git" => restore_git_bundle(archive, revision, dest),
        "tarball" => restore_tarball_archive(archive, dest),
        _ => unreachable!("normalize_source_kind returned unsupported source kind"),
    }
}

fn restore_git_bundle(archive: &Path, revision: &str, dest: &Path) -> Result<PathBuf, WorkerError> {
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
        .arg("clone")
        .arg(archive)
        .arg(dest)
        .output()
        .map_err(|error| WorkerError::Fetch(format!("failed to run git clone: {error}")))?;
    if !clone.status.success() {
        return Err(WorkerError::Fetch(format!(
            "git clone from bronze bundle failed: {}",
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
            "git checkout `{revision}` from bronze bundle failed: {}",
            command_stderr(&checkout)
        )));
    }

    enforce_source_tree_cap(dest, "git")?;
    Ok(dest.to_path_buf())
}

fn restore_tarball_archive(archive: &Path, dest: &Path) -> Result<PathBuf, WorkerError> {
    if dest.exists() {
        fs::remove_dir_all(dest).map_err(|error| {
            WorkerError::Fetch(format!("failed to clear `{}`: {error}", dest.display()))
        })?;
    }
    fs::create_dir_all(dest).map_err(|error| {
        WorkerError::Fetch(format!("failed to create `{}`: {error}", dest.display()))
    })?;
    extract_archive(archive, dest)?;
    let source_path = single_extracted_root(dest).unwrap_or_else(|| dest.to_path_buf());
    enforce_source_tree_cap(&source_path, "tarball")?;
    Ok(source_path)
}

fn archive_extension_for_tarball_url(source_url: &str) -> &'static str {
    if source_url.to_ascii_lowercase().contains(".zip") {
        "zip"
    } else {
        "tar.gz"
    }
}

fn archive_extension_for_row(row: &BronzeRawSource) -> Result<&'static str, WorkerError> {
    match normalize_source_kind(&row.source_kind)? {
        "git" => Ok("gitbundle"),
        "tarball" if row.s3_uri.to_ascii_lowercase().ends_with(".zip") => Ok("zip"),
        "tarball" => Ok("tar.gz"),
        _ => unreachable!("normalize_source_kind returned unsupported source kind"),
    }
}

fn archive_path_for_restore(dest: &Path, extension: &str) -> PathBuf {
    dest.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("__source_archive.{extension}"))
}

fn bronze_source_key(source: &str, package: &str, version: &str, extension: &str) -> String {
    // Infra follow-up: apply an S3 lifecycle policy to bronze/* that moves raw
    // archives to Intelligent-Tiering after 30 days and expires older
    // noncurrent objects beyond the package's latest-N retention window.
    format!("bronze/{source}/{package}/{version}/source.{extension}")
}

fn bronze_row_from_env(
    env: &JobEnv,
    s3_uri: &str,
    content_sha256: &str,
    bytes: u64,
) -> BronzeRawSource {
    let revision = revision_metadata(&env.revision);
    BronzeRawSource {
        source: env.source.clone(),
        package: env.package.clone(),
        version: env.revision.clone(),
        revision_kind: revision.kind,
        semver_major: revision.semver_major,
        semver_minor: revision.semver_minor,
        semver_patch: revision.semver_patch,
        source_kind: env.source_kind.clone(),
        source_url: env.source_url.clone(),
        s3_uri: s3_uri.to_owned(),
        content_sha256: content_sha256.to_owned(),
        bytes,
        fetched_at: unix_secs_i64(),
        fetch_status: "success".to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RevisionMetadata {
    kind: String,
    semver_major: Option<i32>,
    semver_minor: Option<i32>,
    semver_patch: Option<i32>,
}

fn revision_metadata(revision: &str) -> RevisionMetadata {
    if revision.contains('.') {
        let mut parts = revision.split('.');
        return RevisionMetadata {
            kind: "semver".to_owned(),
            semver_major: parts.next().and_then(|part| part.parse::<i32>().ok()),
            semver_minor: parts.next().and_then(|part| part.parse::<i32>().ok()),
            semver_patch: parts.next().and_then(|part| part.parse::<i32>().ok()),
        };
    }
    RevisionMetadata {
        kind: "git_sha".to_owned(),
        semver_major: None,
        semver_minor: None,
        semver_patch: None,
    }
}

fn archive_metadata(path: &Path) -> Result<(String, u64), WorkerError> {
    let content_sha256 = sha256_file(path)?;
    let bytes = fs::metadata(path)
        .map_err(|error| {
            WorkerError::Fetch(format!(
                "failed to stat bronze archive `{}`: {error}",
                path.display()
            ))
        })?
        .len();
    Ok((content_sha256, bytes))
}

fn sha256_file(path: &Path) -> Result<String, WorkerError> {
    let mut file = fs::File::open(path).map_err(|error| {
        WorkerError::Fetch(format!("failed to open `{}`: {error}", path.display()))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            WorkerError::Fetch(format!("failed to read `{}`: {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn bronze_bucket() -> String {
    optional_env("SPUR_CONTEXT_BRONZE_BUCKET").unwrap_or_else(|| DEFAULT_BRONZE_BUCKET.to_owned())
}

fn silver_bucket() -> String {
    optional_env("SPUR_CONTEXT_SILVER_BUCKET").unwrap_or_else(|| DEFAULT_SILVER_BUCKET.to_owned())
}

fn bronze_ducklake_data_path(catalog_dsn: &str) -> Result<String, WorkerError> {
    ducklake_data_path(catalog_dsn).map_err(|error| WorkerError::Fetch(format!("{error:#}")))
}

async fn ensure_catalog_password_env() -> Result<(), WorkerError> {
    if optional_env(CATALOG_PASSWORD_ENV).is_some() {
        return Ok(());
    }

    let Some(secret_arn) = optional_env(CATALOG_PASSWORD_SECRET_ARN_ENV) else {
        return Ok(());
    };

    let output = secretsmanager_client()
        .get_secret_value()
        .secret_id(secret_arn)
        .send()
        .await
        .map_err(|error| WorkerError::Fetch(format!("get catalog password secret: {error}")))?;
    let secret_string = output
        .secret_string()
        .ok_or_else(|| WorkerError::Fetch("catalog password secret has no string".to_owned()))?;
    let password = catalog_password_from_secret_string(secret_string)?;
    std::env::set_var(CATALOG_PASSWORD_ENV, password);
    Ok(())
}

fn catalog_password_from_secret_string(secret: &str) -> Result<String, WorkerError> {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return Err(WorkerError::Fetch(
            "catalog password secret is empty".to_owned(),
        ));
    }

    if trimmed.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|error| WorkerError::Fetch(format!("parse catalog secret JSON: {error}")))?;
        let password = value
            .get("password")
            .and_then(serde_json::Value::as_str)
            .filter(|password| !password.is_empty())
            .ok_or_else(|| WorkerError::Fetch("catalog secret JSON missing password".to_owned()))?;
        return Ok(password.to_owned());
    }

    Ok(secret.to_owned())
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
    let source_path = single_extracted_root(dest).unwrap_or_else(|| dest.to_path_buf());
    enforce_source_tree_cap(&source_path, "tarball")?;
    Ok(())
}

fn enforce_source_tree_cap(source_path: &Path, source_kind: &str) -> Result<(), WorkerError> {
    let cap = source_size_cap_bytes(source_kind);
    let bytes = source_tree_size_bytes(source_path, cap)?;
    if bytes > cap as u64 {
        return Err(WorkerError::Fetch(format!(
            "source tree exceeded size cap: {bytes} > {cap}"
        )));
    }
    Ok(())
}

fn source_tree_size_bytes(path: &Path, cap: usize) -> Result<u64, WorkerError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        WorkerError::Fetch(format!(
            "failed to stat source path `{}`: {error}",
            path.display()
        ))
    })?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0_u64;
    let entries = fs::read_dir(path).map_err(|error| {
        WorkerError::Fetch(format!(
            "failed to read source dir `{}`: {error}",
            path.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            WorkerError::Fetch(format!(
                "failed to read source dir entry in `{}`: {error}",
                path.display()
            ))
        })?;
        total = total.saturating_add(source_tree_size_bytes(&entry.path(), cap)?);
        if total > cap as u64 {
            return Ok(total);
        }
    }
    Ok(total)
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
    artifact_manifest: Option<SilverManifest>,
    lineage: Option<TranslateLineage>,
    allow_missing_embeddings: bool,
    env: &JobEnv,
) -> Result<TranslateStats, WorkerError> {
    let revision_kind = if env.revision.contains('.') {
        "semver"
    } else {
        "git_sha"
    };

    let actual_artifact = if artifact_dir.join("nodes.parquet").is_file() {
        artifact_dir.to_path_buf()
    } else {
        let mut candidates: Vec<_> = fs::read_dir(artifact_dir)
            .map_err(|e| WorkerError::Translate(format!("read artifact dir: {e}")))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir() && p.join("nodes.parquet").is_file())
            .collect();
        candidates.sort();
        candidates
            .into_iter()
            .next()
            .ok_or_else(|| WorkerError::Translate("no nodes.parquet found in artifact".into()))?
    };

    eprintln!("[worker] artifact: {}", actual_artifact.display());

    let opts = TranslateOptions {
        source: env.source.clone(),
        package: env.package.clone(),
        revision: env.revision.clone(),
        revision_kind: revision_kind.to_owned(),
        artifact_dir: actual_artifact,
        artifact_manifest,
        source_root: source_root.map(|p| p.to_path_buf()),
        catalog_dsn: env.catalog_dsn.clone(),
        lineage,
        allow_missing_embeddings,
    };

    eprintln!("[worker] running Rust API translate...");
    translate_artifact_to_ducklake(&opts).map_err(|e| WorkerError::Translate(format!("{e:#}")))
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
            .expression_attribute_values(
                ":owner_job_id",
                AttributeValue::S(owner_job_id.to_owned()),
            )
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
            format!("catalog lease for {catalog_uri} is held by another worker at {now}")
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
            .key(
                "pk",
                AttributeValue::S(catalog_lease_pk(&lease.catalog_uri)),
            )
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
            .key(
                "pk",
                AttributeValue::S(catalog_lease_pk(&lease.catalog_uri)),
            )
            .condition_expression("#owner_job_id = :owner_job_id AND #lease_token = :lease_token")
            .expression_attribute_names("#owner_job_id", "owner_job_id")
            .expression_attribute_names("#lease_token", "lease_token")
            .expression_attribute_values(
                ":owner_job_id",
                AttributeValue::S(lease.owner_job_id.clone()),
            )
            .expression_attribute_values(
                ":lease_token",
                AttributeValue::S(lease.lease_token.clone()),
            )
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
            None => {
                bail!("downloaded S3 catalog had no ETag; refusing to allow unconditional upload")
            }
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

    let mut interval =
        tokio::time::interval(Duration::from_secs(CATALOG_LEASE_RENEW_INTERVAL_SECS));
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

fn secretsmanager_client() -> aws_sdk_secretsmanager::Client {
    let mut builder = aws_sdk_secretsmanager::Config::builder()
        .behavior_version(aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(aws_sdk_secretsmanager::config::Region::new(aws_region()));
    if let Some(endpoint) = aws_endpoint_url("SECRETSMANAGER") {
        builder = builder.endpoint_url(endpoint);
    }
    if let Some(credentials) = aws_credentials_for_secretsmanager() {
        builder = builder.credentials_provider(credentials);
    } else if aws_endpoint_url("SECRETSMANAGER").is_some() {
        builder = builder.credentials_provider(aws_sdk_secretsmanager::config::Credentials::new(
            "test",
            "test",
            None,
            None,
            "LocalEndpoint",
        ));
    }
    aws_sdk_secretsmanager::Client::from_conf(builder.build())
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

fn aws_credentials_for_secretsmanager() -> Option<aws_sdk_secretsmanager::config::Credentials> {
    credential_parts().map(|parts| {
        aws_sdk_secretsmanager::config::Credentials::new(
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
    env_usize(
        "SPUR_CONTEXT_MAX_TARBALL_BYTES",
        DEFAULT_TARBALL_SIZE_CAP_BYTES,
    )
}

fn source_size_cap_bytes(source_kind: &str) -> usize {
    optional_env("SPUR_CONTEXT_MAX_SOURCE_BYTES")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| match source_kind {
            "git" => env_usize("SPUR_CONTEXT_MAX_GIT_BYTES", DEFAULT_GIT_SIZE_CAP_BYTES),
            "tarball" => env_usize(
                "SPUR_CONTEXT_MAX_TARBALL_BYTES",
                DEFAULT_TARBALL_SIZE_CAP_BYTES,
            ),
            _ => DEFAULT_GIT_SIZE_CAP_BYTES,
        })
}

fn env_usize(name: &str, default: usize) -> usize {
    optional_env(name)
        .or_else(|| {
            if name == "SPUR_CONTEXT_MAX_TARBALL_BYTES" {
                optional_env("SPUR_CONTEXT_WORKER_TARBALL_CAP_BYTES")
            } else {
                None
            }
        })
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    optional_env(name)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn failure_error_code(error: &WorkerError) -> String {
    match error {
        WorkerError::Fetch(_) => "fetch".to_owned(),
        WorkerError::Build(_) => "build".to_owned(),
        WorkerError::Translate(_) => "commit".to_owned(),
        WorkerError::SpotInterrupted => "spot_interrupted".to_owned(),
        WorkerError::SfnSend(_) => "sfn_send".to_owned(),
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
pub struct PreparedJob {
    _workspace: TempWorkspace,
    source_path: Option<PathBuf>,
    artifact_dir: PathBuf,
    artifact_manifest: Option<SilverManifest>,
    lineage: Option<TranslateLineage>,
    allow_missing_embeddings: bool,
}

impl PreparedJob {
    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    pub fn artifact_dir(&self) -> &Path {
        &self.artifact_dir
    }

    pub fn artifact_manifest(&self) -> Option<&SilverManifest> {
        self.artifact_manifest.as_ref()
    }

    pub fn lineage(&self) -> Option<&TranslateLineage> {
        self.lineage.as_ref()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().expect("env lock should not be poisoned")
    }

    #[test]
    fn worker_failure_code_is_stable_and_bounded() {
        let checkout_detail = "git checkout failed: ".to_owned() + &"x".repeat(512);

        assert_eq!(
            failure_error_code(&WorkerError::Fetch(checkout_detail)),
            "fetch"
        );
        assert_eq!(
            failure_error_code(&WorkerError::Build("graph build failed".to_owned())),
            "build"
        );
        assert_eq!(
            failure_error_code(&WorkerError::Translate("catalog write failed".to_owned())),
            "commit"
        );
        assert_eq!(
            failure_error_code(&WorkerError::SfnSend("SendTaskFailure failed".to_owned())),
            "sfn_send"
        );
        assert_eq!(
            failure_error_code(&WorkerError::SpotInterrupted),
            "spot_interrupted"
        );
    }

    #[test]
    fn bronze_ducklake_data_path_requires_env_for_postgres_catalog() {
        let _guard = lock_env();
        let previous = std::env::var_os("SPUR_CONTEXT_DUCKLAKE_DATA_PATH");
        std::env::remove_var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH");

        let error =
            bronze_ducklake_data_path("postgres:host=localhost port=5432 dbname=spur_context")
                .expect_err("postgres catalogs must not fall back to a hard-coded S3 data path");

        match previous {
            Some(value) => std::env::set_var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH", value),
            None => std::env::remove_var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH"),
        }

        assert!(
            format!("{error:#}").contains("SPUR_CONTEXT_DUCKLAKE_DATA_PATH"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn catalog_password_from_rds_secret_json_reads_password() {
        assert_eq!(
            catalog_password_from_secret_string(
                r#"{"username":"spur_context","password":"secret-value","engine":"postgres"}"#
            )
            .expect("password should parse"),
            "secret-value"
        );
    }

    #[test]
    fn catalog_password_from_plain_secret_uses_secret_string() {
        assert_eq!(
            catalog_password_from_secret_string("plain-secret").expect("password should parse"),
            "plain-secret"
        );
    }
}

// build-marker: force fresh relink for graviton2 no-ORT worker
