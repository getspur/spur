use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredArchive {
    pub content_sha256: String,
    pub bytes: u64,
    pub metadata: BTreeMap<String, String>,
}

#[async_trait]
pub trait ArchiveStore: Send + Sync {
    async fn head_archive(&self, key: &str) -> Result<Option<StoredArchive>, StoreError>;

    async fn put_archive(
        &self,
        key: &str,
        archive_path: &Path,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), StoreError>;

    async fn presign_archive(&self, key: &str, ttl: Duration) -> Result<String, StoreError>;
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("S3 bucket is not configured")]
    MissingBucket,
    #[error("invalid job_id: must match [A-Za-z0-9_-]{{1,128}}")]
    InvalidJobId,
    #[error("S3 operation failed: {0}")]
    S3(String),
    #[error("presign configuration failed: {0}")]
    PresignConfig(String),
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct S3ArchiveStore {
    bucket: String,
    client: aws_sdk_s3::Client,
}

impl S3ArchiveStore {
    pub fn new(bucket: impl Into<String>, client: aws_sdk_s3::Client) -> Self {
        Self {
            bucket: bucket.into(),
            client,
        }
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }
}

#[async_trait]
impl ArchiveStore for S3ArchiveStore {
    async fn head_archive(&self, key: &str) -> Result<Option<StoredArchive>, StoreError> {
        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        let output = match result {
            Ok(output) => output,
            Err(SdkError::ServiceError(error)) if is_not_found(error.err()) => return Ok(None),
            Err(error) => return Err(StoreError::S3(error.to_string())),
        };

        let metadata = output
            .metadata()
            .map(|metadata| {
                metadata
                    .iter()
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let content_sha256 = metadata.get("content-sha256").cloned().unwrap_or_default();
        let bytes = metadata
            .get("bytes")
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| output.content_length().map(|value| value as u64))
            .unwrap_or_default();

        Ok(Some(StoredArchive {
            content_sha256,
            bytes,
            metadata,
        }))
    }

    async fn put_archive(
        &self,
        key: &str,
        archive_path: &Path,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), StoreError> {
        let body = ByteStream::from_path(archive_path)
            .await
            .map_err(|error| StoreError::S3(error.to_string()))?;
        let request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(body);
        let request = request.set_metadata(Some(metadata.into_iter().collect()));
        request
            .send()
            .await
            .map_err(|error| StoreError::S3(error.to_string()))?;
        Ok(())
    }

    async fn presign_archive(&self, key: &str, ttl: Duration) -> Result<String, StoreError> {
        let config = PresigningConfig::expires_in(ttl)
            .map_err(|error| StoreError::PresignConfig(error.to_string()))?;
        let request = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(config)
            .await
            .map_err(|error| StoreError::S3(error.to_string()))?;
        Ok(request.uri().to_owned())
    }
}

pub fn build_archive_key(prefix: &str, job_id: &str) -> Result<String, StoreError> {
    validate_job_id(job_id)?;
    let prefix = prefix.trim_matches('/');
    let key = if prefix.is_empty() {
        format!("{job_id}/source.tar.gz")
    } else {
        format!("{prefix}/{job_id}/source.tar.gz")
    };
    Ok(key)
}

pub fn build_archive_metadata(
    original_source_url: &str,
    revision: &str,
    source_kind: &str,
    content_sha256: &str,
    bytes: u64,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "original-source-url-sha256".to_owned(),
            source_url_hash(original_source_url),
        ),
        ("revision".to_owned(), revision.to_owned()),
        ("source-kind".to_owned(), source_kind.to_owned()),
        ("content-sha256".to_owned(), content_sha256.to_owned()),
        ("bytes".to_owned(), bytes.to_string()),
    ])
}

pub fn idempotency_metadata_matches(
    metadata: &BTreeMap<String, String>,
    original_source_url: &str,
    revision: &str,
    source_kind: &str,
) -> bool {
    metadata
        .get("original-source-url-sha256")
        .map(String::as_str)
        == Some(source_url_hash(original_source_url).as_str())
        && metadata.get("revision").map(String::as_str) == Some(revision)
        && metadata.get("source-kind").map(String::as_str) == Some(source_kind)
}

pub fn source_url_hash(source_url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source_url.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn is_not_found(error: &HeadObjectError) -> bool {
    matches!(error, HeadObjectError::NotFound(_))
}

fn validate_job_id(job_id: &str) -> Result<(), StoreError> {
    let valid = !job_id.is_empty()
        && job_id.len() <= 128
        && job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(StoreError::InvalidJobId)
    }
}
