//! Non-VPC source fetch Lambda for SPUR context indexing.
//!
//! Lambda image/zip entrypoint: `spur-context-fetcher-lambda`
//! (`crates/spur-context-fetcher/src/bin/lambda.rs`). The handler accepts the
//! Step Functions payload described by [`FetchRequest`] and returns
//! [`FetchResponse`] on success.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lambda_runtime::{Error as LambdaError, LambdaEvent};
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use spur_context_source::{SourceKind, ValidateOptions};
use thiserror::Error;

pub mod fetch;
pub mod store;

use crate::fetch::{
    download_and_normalize_tarball, fetch_git_archive, normalize_source_kind,
    validate_public_fetch_url, CommandRunner, FetchError, SystemCommandRunner,
};
use crate::store::{
    build_archive_key, build_archive_metadata, idempotency_metadata_matches, ArchiveStore,
    S3ArchiveStore, StoreError,
};

const DEFAULT_FETCH_PREFIX: &str = "fetch";
const DEFAULT_PRESIGN_SECONDS: u64 = 21_600;
const DEFAULT_TARBALL_SIZE_CAP_BYTES: u64 = 500_u64 * 1024 * 1024;
const DEFAULT_GIT_SIZE_CAP_BYTES: u64 = 2_u64 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct FetchRequest {
    pub job_id: String,
    pub package: String,
    pub revision: String,
    pub source: String,
    pub source_url: String,
    pub source_kind: String,
    #[serde(default)]
    pub limits: Option<FetchLimits>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FetchLimits {
    pub max_source_bytes: Option<u64>,
    pub max_build_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FetchResponse {
    pub source_url: String,
    pub source_kind: String,
    pub source_archive_s3_uri: String,
    pub original_source_url: String,
    pub original_source_kind: String,
    pub content_sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub bucket: String,
    pub prefix: String,
    pub presign_seconds: u64,
    pub validate_options: ValidateOptions,
    pub tmp_root: PathBuf,
}

#[derive(Debug, Error)]
pub enum FetcherError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("http client error: {0}")]
    HttpClient(String),
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
}

impl FetchConfig {
    pub fn from_env() -> Result<Self, FetcherError> {
        let bucket = env::var("SPUR_CONTEXT_FETCH_BUCKET").map_err(|error| {
            FetcherError::Config(format!("SPUR_CONTEXT_FETCH_BUCKET must be set: {error}"))
        })?;
        let prefix = env::var("SPUR_CONTEXT_FETCH_PREFIX")
            .unwrap_or_else(|_| DEFAULT_FETCH_PREFIX.to_owned());
        let presign_seconds = env_u64(
            "SPUR_CONTEXT_FETCH_PRESIGN_SECONDS",
            DEFAULT_PRESIGN_SECONDS,
        );
        let validate_options = ValidateOptions {
            tarball_size_cap_bytes: env_u64(
                "SPUR_CONTEXT_MAX_TARBALL_BYTES",
                DEFAULT_TARBALL_SIZE_CAP_BYTES,
            ),
            git_size_cap_bytes: env_u64("SPUR_CONTEXT_MAX_GIT_BYTES", DEFAULT_GIT_SIZE_CAP_BYTES),
            allowed_domains: allowed_source_domains(),
        };
        Ok(Self {
            bucket,
            prefix,
            presign_seconds,
            validate_options,
            tmp_root: PathBuf::from("/tmp"),
        })
    }
}

pub async fn handler(event: LambdaEvent<FetchRequest>) -> Result<FetchResponse, LambdaError> {
    let config = FetchConfig::from_env()?;
    let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3 = aws_sdk_s3::Client::new(&aws_config);
    let store = S3ArchiveStore::new(config.bucket.clone(), s3);
    let client = http_client()?;
    let mut runner = SystemCommandRunner;
    handle_request(event.payload, &config, &store, &client, &mut runner)
        .await
        .map_err(Into::into)
}

pub async fn handle_request<S: ArchiveStore, R: CommandRunner>(
    request: FetchRequest,
    config: &FetchConfig,
    store: &S,
    client: &reqwest::Client,
    runner: &mut R,
) -> Result<FetchResponse, FetcherError> {
    let original_source_kind = normalize_source_kind(&request.source_kind)?;
    validate_public_fetch_url(&request.source_url, &config.validate_options)?;

    let source_kind_label = source_kind_label(original_source_kind);
    let key = build_archive_key(&config.prefix, &request.job_id)?;
    let s3_uri = format!("s3://{}/{key}", config.bucket);

    if let Some(existing) = store.head_archive(&key).await? {
        if idempotency_metadata_matches(
            &existing.metadata,
            &request.source_url,
            &request.revision,
            source_kind_label,
        ) {
            let presigned = store
                .presign_archive(&key, Duration::from_secs(config.presign_seconds))
                .await?;
            return Ok(FetchResponse {
                source_url: presigned,
                source_kind: "tarball".to_owned(),
                source_archive_s3_uri: s3_uri,
                original_source_url: request.source_url,
                original_source_kind: source_kind_label.to_owned(),
                content_sha256: existing.content_sha256,
                bytes: existing.bytes,
            });
        }
    }

    let workspace = config.tmp_root.join(safe_job_dir(&request.job_id));
    if workspace.exists() {
        std::fs::remove_dir_all(&workspace)?;
    }
    std::fs::create_dir_all(&workspace)?;
    let workspace = WorkspaceGuard::new(workspace);
    let archive_path = workspace.path().join("source.tar.gz");
    let cap = source_cap_bytes(original_source_kind, &request, &config.validate_options);

    let archive_metadata = match original_source_kind {
        SourceKind::Git => {
            let repo_dir = workspace.path().join("repo");
            fetch_git_archive(
                runner,
                &request.source_url,
                &request.revision,
                &repo_dir,
                &archive_path,
                cap,
            )?
        }
        SourceKind::Tarball => {
            let download_path = workspace.path().join("downloaded-source");
            download_and_normalize_tarball(
                client,
                &request.source_url,
                &download_path,
                &archive_path,
                cap,
                &config.validate_options,
            )
            .await?
        }
    };

    let metadata = build_archive_metadata(
        &request.source_url,
        &request.revision,
        source_kind_label,
        &archive_metadata.content_sha256,
        archive_metadata.bytes,
    );
    store.put_archive(&key, &archive_path, metadata).await?;
    let presigned = store
        .presign_archive(&key, Duration::from_secs(config.presign_seconds))
        .await?;

    workspace.cleanup()?;

    Ok(FetchResponse {
        source_url: presigned,
        source_kind: "tarball".to_owned(),
        source_archive_s3_uri: s3_uri,
        original_source_url: request.source_url,
        original_source_kind: source_kind_label.to_owned(),
        content_sha256: archive_metadata.content_sha256,
        bytes: archive_metadata.bytes,
    })
}

pub fn http_client() -> Result<reqwest::Client, FetcherError> {
    reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|error| FetcherError::HttpClient(error.to_string()))
}

fn source_kind_label(source_kind: SourceKind) -> &'static str {
    match source_kind {
        SourceKind::Git => "git",
        SourceKind::Tarball => "tarball",
    }
}

fn source_cap_bytes(
    source_kind: SourceKind,
    request: &FetchRequest,
    validate_options: &ValidateOptions,
) -> u64 {
    let env_cap = match source_kind {
        SourceKind::Git => validate_options.git_size_cap_bytes,
        SourceKind::Tarball => validate_options.tarball_size_cap_bytes,
    };
    request
        .limits
        .as_ref()
        .and_then(|limits| limits.max_source_bytes)
        .filter(|value| *value > 0)
        .map_or(env_cap, |limit| limit.min(env_cap))
}

fn safe_job_dir(job_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(job_id.as_bytes());
    format!("spur-context-fetch-{:x}", hasher.finalize())
}

struct WorkspaceGuard {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl WorkspaceGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            cleanup_on_drop: true,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(mut self) -> Result<(), std::io::Error> {
        std::fs::remove_dir_all(&self.path)?;
        self.cleanup_on_drop = false;
        Ok(())
    }
}

impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn allowed_source_domains() -> Vec<String> {
    env::var("SPUR_CONTEXT_ALLOWED_SOURCE_DOMAINS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}
