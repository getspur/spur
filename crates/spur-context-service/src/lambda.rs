//! AWS Lambda HTTP entry point for the context-service MCP surface.
//!
//! Serving intentionally reads only the published frozen DuckLake catalog
//! snapshot from S3 plus the S3 gold data files. It must not attach the live
//! ingest catalog backend, including Aurora/Postgres.
//!
//! The PoC measured roughly 15s cold starts from DuckDB import, extension
//! loading, and snapshot download, while warm invokes were fast. For
//! latency-sensitive serving, use provisioned concurrency, keep DuckDB
//! extensions baked into the Lambda package, and trim the package if init time
//! stays high.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Mutex, MutexGuard, OnceLock};

use lambda_runtime::{Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::catalog::{self, CatalogResolver};
use crate::jobs::{DynamoDbJobStore, JobStore};
use crate::mcp::{self, McpHandlerError};

pub static CATALOG_RESOLVER: OnceLock<Mutex<Option<CatalogCacheEntry>>> = OnceLock::new();
static AWS_CLIENTS: OnceLock<AwsClients> = OnceLock::new();

pub struct CatalogCacheEntry {
    catalog_dsn: String,
    catalog_etag: Option<String>,
    resolver: CatalogResolver,
}

struct PreparedCatalog {
    cache_key: String,
    catalog_etag: Option<String>,
    source: PreparedCatalogSource,
}

enum PreparedCatalogSource {
    FrozenSnapshot {
        local_path: PathBuf,
        data_path: String,
    },
    Direct {
        catalog_dsn: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct ApiGatewayRequest {
    pub body: Option<String>,
    #[serde(rename = "isBase64Encoded", default)]
    pub is_base64_encoded: bool,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(rename = "rawPath", default)]
    pub raw_path: Option<String>,
    #[serde(rename = "requestContext", default)]
    pub request_context: Option<ApiGatewayRequestContext>,
}

#[derive(Debug, Deserialize)]
pub struct ApiGatewayRequestContext {
    #[serde(default)]
    pub authorizer: Option<ApiGatewayAuthorizer>,
    #[serde(default)]
    pub http: Option<ApiGatewayHttp>,
    #[serde(default)]
    pub identity: Option<ApiGatewayIdentity>,
}

#[derive(Debug, Deserialize)]
pub struct ApiGatewayAuthorizer {
    #[serde(rename = "principalId", default)]
    pub principal_id: Option<String>,
    #[serde(default)]
    pub iam: Option<IamAuthorizer>,
    #[serde(default)]
    pub jwt: Option<JwtAuthorizer>,
}

#[derive(Debug, Deserialize)]
pub struct IamAuthorizer {
    #[serde(rename = "userArn", default)]
    pub user_arn: Option<String>,
    #[serde(rename = "callerId", default)]
    pub caller_id: Option<String>,
    #[serde(rename = "userId", default)]
    pub user_id: Option<String>,
    #[serde(rename = "accountId", default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JwtAuthorizer {
    #[serde(default)]
    pub claims: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ApiGatewayHttp {
    #[serde(rename = "sourceIp", default)]
    pub source_ip: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiGatewayIdentity {
    #[serde(rename = "userArn", default)]
    pub user_arn: Option<String>,
    #[serde(rename = "sourceIp", default)]
    pub source_ip: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApiGatewayResponse {
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
    #[serde(rename = "isBase64Encoded")]
    pub is_base64_encoded: bool,
}

#[derive(Debug, Deserialize)]
struct ToolRequest {
    tool: String,
    #[serde(default)]
    args: Value,
}

#[allow(
    clippy::await_holding_lock,
    reason = "route_index only consults the catalog before its first await"
)]
pub async fn handler(event: LambdaEvent<ApiGatewayRequest>) -> Result<ApiGatewayResponse, Error> {
    let request = parse_tool_request(&event.payload);
    let request = match request {
        Ok(request) => request,
        Err(error) => return tool_error_response(error),
    };

    let authenticated_caller = match request.tool.as_str() {
        "external_index" | "external_index_status" => {
            Some(match authenticated_caller_id(&event.payload, anonymous_mutations_allowed()) {
                Ok(caller_id) => caller_id,
                Err(error) => return auth_error_response(error),
            })
        }
        _ => None,
    };

    let result = match request.tool.as_str() {
        "external_index_status" => {
            let jobs = job_store();
            let checker = status_checker();
            let caller_id = authenticated_caller
                .as_deref()
                .expect("external_index_status authenticated caller should be available");
            route_index_status_control_plane(&request.args, &jobs, &checker, caller_id).await
        }
        "external_index" => {
            let prepared_catalog = prepare_catalog().await?;
            let jobs = job_store();
            let sfn_client = sfn_client()?;
            let caller_id = authenticated_caller
                .as_deref()
                .expect("external_index authenticated caller should be available");
            if let Some(prepared_catalog) = prepared_catalog {
                let mut catalog_guard = catalog_resolver()?;
                let catalog = initialized_catalog(&mut catalog_guard, &prepared_catalog)?;
                let db = catalog.connection();
                mcp::route_index(&request.args, db, catalog, &jobs, &sfn_client, caller_id).await
            } else {
                mcp::route_index_without_catalog(&request.args, &jobs, &sfn_client, caller_id).await
            }
        }
        _ => {
            let prepared_catalog = prepare_catalog().await?;
            let Some(prepared_catalog) = prepared_catalog else {
                return match mcp::handle_tool_without_catalog(&request.tool, &request.args) {
                    Ok(value) => json_response(200, &value),
                    Err(error) => tool_error_response(error),
                };
            };
            let mut catalog_guard = catalog_resolver()?;
            let catalog = initialized_catalog(&mut catalog_guard, &prepared_catalog)?;
            let db = catalog.connection();
            mcp::handle_tool_sync(&request.tool, &request.args, db, catalog)
        }
    };

    match result {
        Ok(value) => json_response(200, &value),
        Err(McpHandlerError::Internal(message)) => json_response(
            500,
            &json!({
                "error": {
                    "code": McpHandlerError::Internal(message.clone()).json_rpc_code(),
                    "message": message
                }
            }),
        ),
        Err(error) => tool_error_response(error),
    }
}

pub async fn route_index_status_control_plane(
    args: &Value,
    jobs: &dyn JobStore,
    checker: &dyn mcp::ExecutionStatusChecker,
    caller_id: &str,
) -> Result<Value, McpHandlerError> {
    mcp::route_index_status_for_caller(args, jobs, Some(checker), caller_id).await
}

fn parse_tool_request(request: &ApiGatewayRequest) -> Result<ToolRequest, McpHandlerError> {
    if request.is_base64_encoded {
        return Err(McpHandlerError::InvalidParams(
            "base64-encoded API Gateway bodies are not supported".to_owned(),
        ));
    }
    let body = request
        .body
        .as_deref()
        .ok_or_else(|| McpHandlerError::InvalidParams("missing request body".to_owned()))?;
    if let Some(tool) = routed_tool_name(request) {
        let value: Value = serde_json::from_str(body).map_err(|error| {
            McpHandlerError::InvalidParams(format!("failed to parse request JSON body: {error}"))
        })?;
        let args = value.get("args").cloned().unwrap_or(value);
        return Ok(ToolRequest {
            tool: tool.to_owned(),
            args,
        });
    }
    serde_json::from_str(body).map_err(|error| {
        McpHandlerError::InvalidParams(format!("failed to parse request JSON body: {error}"))
    })
}

fn routed_tool_name(request: &ApiGatewayRequest) -> Option<&'static str> {
    let path = request.raw_path.as_deref().or(request.path.as_deref())?;
    match path.trim_end_matches('/').rsplit('/').next() {
        Some("index") => Some("external_index"),
        Some("index_status") => Some("external_index_status"),
        _ => None,
    }
}

fn catalog_resolver() -> Result<MutexGuard<'static, Option<CatalogCacheEntry>>, Error> {
    CATALOG_RESOLVER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|error| lambda_error(format!("catalog resolver cache is poisoned: {error}")))
}

fn initialized_catalog<'a>(
    guard: &'a mut MutexGuard<'static, Option<CatalogCacheEntry>>,
    prepared_catalog: &PreparedCatalog,
) -> Result<&'a CatalogResolver, Error> {
    let cached_dsn = guard.as_ref().map(|entry| entry.catalog_dsn.as_str());
    let cached_etag = guard
        .as_ref()
        .and_then(|entry| entry.catalog_etag.as_deref());
    let catalog_etag = prepared_catalog.catalog_etag.as_deref();

    if should_initialize_catalog(
        cached_dsn,
        cached_etag,
        &prepared_catalog.cache_key,
        catalog_etag,
    ) {
        let conn = match &prepared_catalog.source {
            PreparedCatalogSource::FrozenSnapshot {
                local_path,
                data_path,
            } => catalog::connect_frozen_snapshot(local_path, data_path).map_err(Error::from)?,
            PreparedCatalogSource::Direct { catalog_dsn } => {
                catalog::connect_ducklake(catalog_dsn).map_err(Error::from)?
            }
        };
        **guard = Some(CatalogCacheEntry {
            catalog_dsn: prepared_catalog.cache_key.clone(),
            catalog_etag: prepared_catalog.catalog_etag.clone(),
            resolver: CatalogResolver::from_connection(conn),
        });
    }
    guard
        .as_ref()
        .map(|entry| &entry.resolver)
        .ok_or_else(|| lambda_error("catalog resolver cache did not initialize"))
}

async fn prepare_catalog() -> Result<Option<PreparedCatalog>, Error> {
    let catalog_dsn = catalog_dsn()?;
    prepare_catalog_source(catalog_dsn).await
}

async fn prepare_catalog_source(catalog_dsn: String) -> Result<Option<PreparedCatalog>, Error> {
    if let Some(uri) = parse_s3_uri(&catalog_dsn)? {
        if uri.key.ends_with(".json") {
            let Some(pointer) = download_snapshot_pointer(&uri).await? else {
                return Ok(None);
            };
            let Some(snapshot_uri) = parse_s3_uri(&pointer.manifest.snapshot_uri)? else {
                return Err(lambda_error(format!(
                    "snapshot pointer must reference an S3 snapshot URI, got `{}`",
                    pointer.manifest.snapshot_uri
                )));
            };
            let cache_token =
                snapshot_pointer_cache_token(pointer.pointer_etag.as_deref(), &pointer.manifest);
            let local_path =
                local_snapshot_path(&pointer.manifest.snapshot_uri, Some(&cache_token))?;
            if !local_path.is_file() {
                download_catalog_snapshot(&snapshot_uri, &local_path).await?;
            }
            verify_local_snapshot_hash(&local_path, &pointer.manifest.sha256)?;
            return Ok(Some(PreparedCatalog {
                cache_key: snapshot_pointer_cache_key(
                    &catalog_dsn,
                    pointer.pointer_etag.as_deref(),
                    &pointer.manifest,
                ),
                catalog_etag: pointer.pointer_etag,
                source: PreparedCatalogSource::FrozenSnapshot {
                    local_path,
                    data_path: pointer.manifest.data_path,
                },
            }));
        }

        let catalog_etag = catalog_etag(&catalog_dsn).await?;
        let data_path = catalog_data_path(&catalog_dsn)?;
        let local_path = local_snapshot_path(&catalog_dsn, catalog_etag.as_deref())?;
        if !local_path.is_file() {
            download_catalog_snapshot(&uri, &local_path).await?;
        }
        return Ok(Some(PreparedCatalog {
            cache_key: format!("{catalog_dsn}\n{data_path}"),
            catalog_etag,
            source: PreparedCatalogSource::FrozenSnapshot {
                local_path,
                data_path,
            },
        }));
    }

    let catalog_etag = None;
    if is_postgres_catalog_dsn(&catalog_dsn) {
        return Err(lambda_error(
            "serving requires SPUR_CATALOG_S3_URI to point at a frozen DuckLake snapshot; refusing to connect to Postgres",
        ));
    }

    Ok(Some(PreparedCatalog {
        cache_key: catalog_dsn.clone(),
        catalog_etag,
        source: PreparedCatalogSource::Direct { catalog_dsn },
    }))
}

fn should_initialize_catalog(
    cached_dsn: Option<&str>,
    cached_etag: Option<&str>,
    catalog_dsn: &str,
    current_etag: Option<&str>,
) -> bool {
    let Some(cached_dsn) = cached_dsn else {
        return true;
    };
    if cached_dsn != catalog_dsn {
        return true;
    }
    if !catalog::is_remote_catalog(catalog_dsn) {
        return false;
    }
    match (cached_etag, current_etag) {
        (Some(cached), Some(current)) => cached != current,
        _ => true,
    }
}

fn catalog_dsn() -> Result<String, Error> {
    if let Ok(value) = env::var("SPUR_CATALOG_S3_URI") {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }

    let catalog_dsn = env::var("SPUR_CATALOG_DSN").map_err(|error| {
        lambda_error(format!(
            "SPUR_CATALOG_S3_URI environment variable is required for serving: {error}"
        ))
    })?;
    if is_postgres_catalog_dsn(&catalog_dsn) {
        return Err(lambda_error(
            "SPUR_CATALOG_S3_URI must point at the frozen serving snapshot; SPUR_CATALOG_DSN Postgres catalogs are ingest-only",
        ));
    }
    Ok(catalog_dsn)
}

async fn catalog_etag(catalog_dsn: &str) -> Result<Option<String>, Error> {
    let Some(uri) = parse_s3_uri(catalog_dsn)? else {
        return Ok(None);
    };
    let output = aws_clients()
        .s3
        .head_object()
        .bucket(uri.bucket)
        .key(uri.key)
        .send()
        .await
        .map_err(|error| lambda_error(format!("failed to read catalog ETag: {error}")))?;
    Ok(output.e_tag().map(str::to_owned))
}

struct SnapshotPointerDownload {
    manifest: catalog::FrozenSnapshotManifest,
    pointer_etag: Option<String>,
}

async fn download_snapshot_pointer(uri: &S3Uri) -> Result<Option<SnapshotPointerDownload>, Error> {
    let output = match aws_clients()
        .s3
        .get_object()
        .bucket(&uri.bucket)
        .key(&uri.key)
        .send()
        .await
    {
        Ok(output) => output,
        Err(error) => {
            let error = anyhow::Error::new(error);
            if catalog::is_s3_not_found_error(&error) {
                return Ok(None);
            }
            return Err(lambda_error(format!(
                "failed to download catalog pointer: {error:#}"
            )));
        }
    };
    let pointer_etag = output.e_tag().map(str::to_owned);
    let bytes = output.body.collect().await.map_err(|error| {
        lambda_error(format!(
            "failed to read catalog pointer download body: {error}"
        ))
    })?;
    let bytes = bytes.into_bytes();
    let manifest =
        catalog::FrozenSnapshotManifest::from_json_slice(bytes.as_ref()).map_err(Error::from)?;
    Ok(Some(SnapshotPointerDownload {
        manifest,
        pointer_etag,
    }))
}

fn snapshot_pointer_cache_key(
    pointer_uri: &str,
    pointer_etag: Option<&str>,
    manifest: &catalog::FrozenSnapshotManifest,
) -> String {
    format!(
        "pointer={pointer_uri}\netag={}\ngeneration={}\nsnapshot={}\nsha256={}",
        pointer_etag.unwrap_or("<missing>"),
        manifest.generation,
        manifest.snapshot_uri,
        manifest.sha256
    )
}

fn snapshot_pointer_cache_token(
    pointer_etag: Option<&str>,
    manifest: &catalog::FrozenSnapshotManifest,
) -> String {
    format!(
        "{}:{}:{}",
        pointer_etag.unwrap_or("<missing>"),
        manifest.generation,
        manifest.sha256
    )
}

fn verify_local_snapshot_hash(local_path: &Path, expected_sha256: &str) -> Result<(), Error> {
    let bytes = fs::read(local_path).map_err(|error| {
        lambda_error(format!(
            "failed to read cached catalog snapshot `{}`: {error}",
            local_path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected_sha256 {
        return Err(lambda_error(format!(
            "cached catalog snapshot `{}` sha256 mismatch: expected {expected_sha256}, got {actual}",
            local_path.display()
        )));
    }
    Ok(())
}

fn catalog_data_path(snapshot_uri: &str) -> Result<String, Error> {
    if let Ok(path) = env::var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH") {
        if !path.trim().is_empty() {
            return Ok(path);
        }
    }

    infer_data_path_from_snapshot_uri(snapshot_uri).ok_or_else(|| {
        lambda_error(
            "SPUR_CONTEXT_DUCKLAKE_DATA_PATH must be set when the frozen snapshot URI does not include /gold/catalog-snapshot/",
        )
    })
}

fn infer_data_path_from_snapshot_uri(snapshot_uri: &str) -> Option<String> {
    let marker = "/gold/catalog-snapshot/";
    let prefix = snapshot_uri.split_once(marker)?.0;
    Some(format!("{prefix}/gold/data/"))
}

fn local_snapshot_path(catalog_dsn: &str, catalog_etag: Option<&str>) -> Result<PathBuf, Error> {
    let mut hasher = Sha256::new();
    hasher.update(catalog_dsn.as_bytes());
    if let Some(etag) = catalog_etag {
        hasher.update(b"\0");
        hasher.update(etag.as_bytes());
    }
    let digest = hasher.finalize();
    let suffix = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let dir = env::temp_dir().join("spur-context-service-catalog");
    fs::create_dir_all(&dir).map_err(|error| {
        lambda_error(format!(
            "failed to create catalog snapshot cache dir `{}`: {error}",
            dir.display()
        ))
    })?;
    Ok(dir.join(format!("catalog-{suffix}.ducklake")))
}

async fn download_catalog_snapshot(uri: &S3Uri, local_path: &Path) -> Result<(), Error> {
    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            lambda_error(format!(
                "failed to create catalog snapshot dir `{}`: {error}",
                parent.display()
            ))
        })?;
    }

    let output = aws_clients()
        .s3
        .get_object()
        .bucket(&uri.bucket)
        .key(&uri.key)
        .send()
        .await
        .map_err(|error| lambda_error(format!("failed to download catalog snapshot: {error}")))?;
    let bytes = output.body.collect().await.map_err(|error| {
        lambda_error(format!(
            "failed to read catalog snapshot download body: {error}"
        ))
    })?;

    let tmp_path = local_path.with_file_name(format!(
        ".{}.{}.tmp",
        local_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("catalog.ducklake"),
        std::process::id()
    ));
    tokio::fs::write(&tmp_path, bytes.into_bytes())
        .await
        .map_err(|error| {
            lambda_error(format!(
                "failed to write catalog snapshot `{}`: {error}",
                tmp_path.display()
            ))
        })?;
    tokio::fs::rename(&tmp_path, local_path)
        .await
        .map_err(|error| {
            lambda_error(format!(
                "failed to install catalog snapshot `{}`: {error}",
                local_path.display()
            ))
        })
}

struct S3Uri {
    bucket: String,
    key: String,
}

fn parse_s3_uri(uri: &str) -> Result<Option<S3Uri>, Error> {
    let Some(without_scheme) = uri.strip_prefix("s3://") else {
        return Ok(None);
    };
    let (bucket, key) = without_scheme.split_once('/').ok_or_else(|| {
        lambda_error(format!("S3 catalog URI must include bucket and key: {uri}"))
    })?;
    if bucket.is_empty() || key.is_empty() {
        return Err(lambda_error(format!(
            "S3 catalog URI must include bucket and key: {uri}"
        )));
    }
    Ok(Some(S3Uri {
        bucket: bucket.to_owned(),
        key: key.to_owned(),
    }))
}

fn is_postgres_catalog_dsn(catalog_dsn: &str) -> bool {
    let dsn = catalog_dsn.strip_prefix("ducklake:").unwrap_or(catalog_dsn);
    dsn.starts_with("postgres:")
        || dsn.starts_with("postgresql:")
        || dsn.starts_with("postgresql://")
}

fn job_store() -> DynamoDbJobStore {
    DynamoDbJobStore::new(aws_clients().dynamodb.clone())
}

fn status_checker() -> mcp::SfnExecutionStatusChecker {
    mcp::SfnExecutionStatusChecker::new(aws_clients().sfn.clone())
}

fn sfn_client() -> Result<SfnIndexExecutionStarter, Error> {
    let client = aws_clients().sfn.clone();
    Ok(SfnIndexExecutionStarter {
        client,
        state_machine_arn: env::var("SPUR_INDEX_STATE_MACHINE_ARN").map_err(|error| {
            lambda_error(format!(
                "SPUR_INDEX_STATE_MACHINE_ARN environment variable is required: {error}"
            ))
        })?,
    })
}

#[derive(Debug, Clone)]
struct AwsClients {
    dynamodb: aws_sdk_dynamodb::Client,
    s3: aws_sdk_s3::Client,
    sfn: aws_sdk_sfn::Client,
}

fn aws_clients() -> &'static AwsClients {
    AWS_CLIENTS.get_or_init(|| AwsClients {
        dynamodb: dynamodb_client_from_env(),
        s3: s3_client_from_env(),
        sfn: sfn_client_from_env(),
    })
}

fn sfn_client_from_env() -> aws_sdk_sfn::Client {
    let region = env::var("AWS_REGION")
        .or_else(|_| env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_owned());
    let mut config = aws_sdk_sfn::Config::builder()
        .behavior_version(aws_sdk_sfn::config::BehaviorVersion::latest())
        .region(aws_sdk_sfn::config::Region::new(region));

    if let (Ok(access_key), Ok(secret_key)) = (
        env::var("AWS_ACCESS_KEY_ID"),
        env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        config = config.credentials_provider(aws_sdk_sfn::config::Credentials::new(
            access_key,
            secret_key,
            env::var("AWS_SESSION_TOKEN").ok(),
            None,
            "lambda-env",
        ));
    }

    aws_sdk_sfn::Client::from_conf(config.build())
}

fn dynamodb_client_from_env() -> aws_sdk_dynamodb::Client {
    let region = env::var("AWS_REGION")
        .or_else(|_| env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_owned());
    let mut config = aws_sdk_dynamodb::Config::builder()
        .behavior_version(aws_sdk_dynamodb::config::BehaviorVersion::latest())
        .region(aws_sdk_dynamodb::config::Region::new(region));

    if let (Ok(access_key), Ok(secret_key)) = (
        env::var("AWS_ACCESS_KEY_ID"),
        env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        config = config.credentials_provider(aws_sdk_dynamodb::config::Credentials::new(
            access_key,
            secret_key,
            env::var("AWS_SESSION_TOKEN").ok(),
            None,
            "lambda-env",
        ));
    }

    aws_sdk_dynamodb::Client::from_conf(config.build())
}

fn s3_client_from_env() -> aws_sdk_s3::Client {
    let region = env::var("AWS_REGION")
        .or_else(|_| env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_owned());
    let mut config = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(region));

    if let (Ok(access_key), Ok(secret_key)) = (
        env::var("AWS_ACCESS_KEY_ID"),
        env::var("AWS_SECRET_ACCESS_KEY"),
    ) {
        config = config.credentials_provider(aws_sdk_s3::config::Credentials::new(
            access_key,
            secret_key,
            env::var("AWS_SESSION_TOKEN").ok(),
            None,
            "lambda-env",
        ));
    }

    aws_sdk_s3::Client::from_conf(config.build())
}

#[cfg(test)]
fn caller_id(request: &ApiGatewayRequest) -> String {
    request
        .request_context
        .as_ref()
        .and_then(|context| {
            context
                .authorizer
                .as_ref()
                .and_then(jwt_caller_id)
                .or_else(|| context.authorizer.as_ref().and_then(iam_caller_id))
                .or_else(|| {
                    context
                        .authorizer
                        .as_ref()
                        .and_then(|authorizer| non_blank(authorizer.principal_id.as_deref()))
                })
                .or_else(|| {
                    context
                        .http
                        .as_ref()
                        .and_then(|http| non_blank(http.source_ip.as_deref()))
                })
                .or_else(|| {
                    context
                        .identity
                        .as_ref()
                        .and_then(|identity| non_blank(identity.user_arn.as_deref()))
                })
                .or_else(|| {
                    context
                        .identity
                        .as_ref()
                        .and_then(|identity| non_blank(identity.source_ip.as_deref()))
                })
        })
        .unwrap_or("anonymous")
        .to_owned()
}

/// When set truthy, mutating tools (`external_index`/`external_index_status`)
/// no longer require an authenticated caller and fall back to a shared anonymous
/// identity. Intended for internal-team / trusted-network deployments where the
/// HTTP API route is `NONE` (no authorizer injects a caller). Secure-by-default:
/// off unless explicitly enabled.
const ALLOW_ANONYMOUS_MUTATIONS_ENV: &str = "SPUR_CONTEXT_ALLOW_ANONYMOUS_MUTATIONS";
/// Shared caller id used for anonymous mutations. All anonymous callers share
/// this bucket, so the existing per-caller rate limit / active-job cap still
/// apply (collectively) rather than being bypassed entirely.
const ANONYMOUS_CALLER_ID: &str = "anonymous-internal";

fn anonymous_mutations_allowed() -> bool {
    matches!(
        env::var(ALLOW_ANONYMOUS_MUTATIONS_ENV)
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes")
    )
}

fn authenticated_caller_id(
    request: &ApiGatewayRequest,
    allow_anonymous: bool,
) -> Result<String, McpHandlerError> {
    request
        .request_context
        .as_ref()
        .and_then(|context| {
            context
                .authorizer
                .as_ref()
                .and_then(jwt_caller_id)
                .or_else(|| context.authorizer.as_ref().and_then(iam_caller_id))
                .or_else(|| {
                    context
                        .authorizer
                        .as_ref()
                        .and_then(|authorizer| non_blank(authorizer.principal_id.as_deref()))
                })
                .or_else(|| {
                    context
                        .identity
                        .as_ref()
                        .and_then(|identity| non_blank(identity.user_arn.as_deref()))
                })
        })
        .map(str::to_owned)
        .or_else(|| allow_anonymous.then(|| ANONYMOUS_CALLER_ID.to_owned()))
        .ok_or_else(|| {
            McpHandlerError::InvalidParams(
                "authenticated caller is required for mutating context-service tools".to_owned(),
            )
        })
}

fn jwt_caller_id(authorizer: &ApiGatewayAuthorizer) -> Option<&str> {
    let claims = authorizer.jwt.as_ref()?.claims.as_ref()?;
    claim_str(claims, "sub").or_else(|| claim_str(claims, "principal_id"))
}

fn iam_caller_id(authorizer: &ApiGatewayAuthorizer) -> Option<&str> {
    let iam = authorizer.iam.as_ref()?;
    non_blank(iam.user_arn.as_deref())
        .or_else(|| non_blank(iam.caller_id.as_deref()))
        .or_else(|| non_blank(iam.user_id.as_deref()))
        .or_else(|| non_blank(iam.account_id.as_deref()))
}

fn claim_str<'a>(claims: &'a Value, key: &str) -> Option<&'a str> {
    non_blank(claims.get(key).and_then(Value::as_str))
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

struct SfnIndexExecutionStarter {
    client: aws_sdk_sfn::Client,
    state_machine_arn: String,
}

impl mcp::IndexExecutionStarter for SfnIndexExecutionStarter {
    fn start_execution<'a>(
        &'a self,
        request: mcp::IndexExecutionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<String, McpHandlerError>> + Send + 'a>> {
        Box::pin(async move {
            let input = serde_json::to_string(&request.input).map_err(|error| {
                McpHandlerError::Internal(format!(
                    "external_index StartExecution input serialization failed: {error}"
                ))
            })?;
            let output = self
                .client
                .start_execution()
                .state_machine_arn(self.state_machine_arn.clone())
                .name(request.name)
                .input(input)
                .send()
                .await
                .map_err(|error| {
                    McpHandlerError::Internal(format!(
                        "external_index StartExecution failed: {error}"
                    ))
                })?;
            Ok(output.execution_arn().to_owned())
        })
    }
}

fn tool_error_response(error: McpHandlerError) -> Result<ApiGatewayResponse, Error> {
    json_response(
        200,
        &json!({
            "error": {
                "code": error.json_rpc_code(),
                "message": error.to_string()
            }
        }),
    )
}

fn auth_error_response(error: McpHandlerError) -> Result<ApiGatewayResponse, Error> {
    json_response(
        401,
        &json!({
            "error": {
                "code": error.json_rpc_code(),
                "message": error.to_string()
            }
        }),
    )
}

fn json_response(status_code: u16, value: &Value) -> Result<ApiGatewayResponse, Error> {
    Ok(ApiGatewayResponse {
        status_code,
        headers: json_headers(),
        body: serde_json::to_string(value).map_err(Error::from)?,
        is_base64_encoded: false,
    })
}

fn json_headers() -> BTreeMap<String, String> {
    BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())])
}

fn lambda_error(message: impl Into<String>) -> Error {
    Box::new(std::io::Error::other(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_from_context(request_context: Value) -> ApiGatewayRequest {
        serde_json::from_value(json!({
            "body": "{}",
            "requestContext": request_context
        }))
        .expect("API Gateway request should deserialize")
    }

    #[test]
    fn caller_id_prefers_http_api_v2_jwt_subject() {
        let request = request_from_context(json!({
            "authorizer": {
                "principalId": "rest-principal",
                "jwt": {
                    "claims": {
                        "sub": "jwt-subject"
                    }
                }
            },
            "http": {
                "sourceIp": "203.0.113.24"
            },
            "identity": {
                "userArn": "arn:aws:iam::123456789012:user/rest",
                "sourceIp": "198.51.100.10"
            }
        }));

        assert_eq!(caller_id(&request), "jwt-subject");
    }

    #[test]
    fn caller_id_uses_http_api_v2_source_ip_before_rest_identity() {
        let request = request_from_context(json!({
            "http": {
                "sourceIp": "203.0.113.24"
            },
            "identity": {
                "userArn": "arn:aws:iam::123456789012:user/rest",
                "sourceIp": "198.51.100.10"
            }
        }));

        assert_eq!(caller_id(&request), "203.0.113.24");
    }

    #[test]
    fn caller_id_keeps_rest_api_v1_principal_fallback() {
        let request = request_from_context(json!({
            "authorizer": {
                "principalId": "rest-principal"
            },
            "identity": {
                "userArn": "arn:aws:iam::123456789012:user/rest",
                "sourceIp": "198.51.100.10"
            }
        }));

        assert_eq!(caller_id(&request), "rest-principal");
    }

    #[test]
    fn authenticated_caller_id_accepts_http_api_iam_user_arn() {
        let request = request_from_context(json!({
            "authorizer": {
                "iam": {
                    "userArn": "arn:aws:iam::123456789012:role/context-indexer",
                    "callerId": "AROATEST:session"
                }
            },
            "http": {
                "sourceIp": "203.0.113.24"
            }
        }));

        assert_eq!(
            authenticated_caller_id(&request, false).expect("IAM caller should authenticate"),
            "arn:aws:iam::123456789012:role/context-indexer"
        );
    }

    #[test]
    fn authenticated_caller_id_rejects_source_ip_only_request() {
        let request = request_from_context(json!({
            "http": {
                "sourceIp": "203.0.113.24"
            }
        }));

        let error = authenticated_caller_id(&request, false).unwrap_err();

        assert!(error.to_string().contains("authenticated caller"));
    }

    #[test]
    fn authenticated_caller_id_falls_back_to_anonymous_when_allowed() {
        // Public (NONE auth) request: no authorizer/identity caller present.
        let request = request_from_context(json!({
            "http": {
                "sourceIp": "203.0.113.24"
            }
        }));

        assert_eq!(
            authenticated_caller_id(&request, true)
                .expect("anonymous fallback should authenticate when allowed"),
            ANONYMOUS_CALLER_ID
        );
    }

    #[test]
    fn authenticated_caller_id_prefers_real_caller_over_anonymous_fallback() {
        // Even with anonymous allowed, a real authenticated caller wins.
        let request = request_from_context(json!({
            "identity": {
                "userArn": "arn:aws:iam::123456789012:user/real"
            }
        }));

        assert_eq!(
            authenticated_caller_id(&request, true).expect("real caller should authenticate"),
            "arn:aws:iam::123456789012:user/real"
        );
    }

    #[test]
    fn remote_catalog_dsn_reinitializes_when_etag_changes() {
        let s3_dsn = "s3://example-context/catalog/catalog.ducklake";

        assert!(should_initialize_catalog(
            None,
            None,
            s3_dsn,
            Some("etag-a")
        ));
        assert!(!should_initialize_catalog(
            Some(s3_dsn),
            Some("etag-a"),
            s3_dsn,
            Some("etag-a")
        ));
        assert!(should_initialize_catalog(
            Some(s3_dsn),
            Some("etag-a"),
            s3_dsn,
            Some("etag-b")
        ));
        assert!(should_initialize_catalog(
            Some(s3_dsn),
            Some("etag-a"),
            s3_dsn,
            None
        ));

        assert!(should_initialize_catalog(
            None,
            None,
            "sqlite:/tmp/catalog.sqlite",
            None
        ));
        assert!(!should_initialize_catalog(
            Some("sqlite:/tmp/catalog.sqlite"),
            None,
            "sqlite:/tmp/catalog.sqlite",
            None
        ));
    }

    #[test]
    fn pointer_cache_key_changes_when_live_pointer_switches_generation() {
        let pointer_uri = "s3://example-context/gold/catalog-snapshot/current.json";
        let first = catalog::FrozenSnapshotManifest::published(
            10,
            "s3://example-context/gold/catalog-snapshot/generations/00000000000000000010/spur_context.ducklake".to_owned(),
            "s3://example-context/gold/data/".to_owned(),
            "sha10".to_owned(),
            10,
        );
        let second = catalog::FrozenSnapshotManifest::published(
            11,
            "s3://example-context/gold/catalog-snapshot/generations/00000000000000000011/spur_context.ducklake".to_owned(),
            "s3://example-context/gold/data/".to_owned(),
            "sha11".to_owned(),
            11,
        );

        let first_key = snapshot_pointer_cache_key(pointer_uri, Some("etag-a"), &first);
        let second_key = snapshot_pointer_cache_key(pointer_uri, Some("etag-b"), &second);

        assert_ne!(first_key, second_key);
        assert!(should_initialize_catalog(
            Some(&first_key),
            Some("etag-a"),
            &second_key,
            Some("etag-b")
        ));
    }

    #[test]
    fn pointer_cache_key_supports_rollback_to_previous_generation() {
        let pointer_uri = "s3://example-context/gold/catalog-snapshot/current.json";
        let current = catalog::FrozenSnapshotManifest::published(
            11,
            "s3://example-context/gold/catalog-snapshot/generations/00000000000000000011/spur_context.ducklake".to_owned(),
            "s3://example-context/gold/data/".to_owned(),
            "sha11".to_owned(),
            11,
        );
        let rollback = catalog::FrozenSnapshotManifest::published(
            10,
            "s3://example-context/gold/catalog-snapshot/generations/00000000000000000010/spur_context.ducklake".to_owned(),
            "s3://example-context/gold/data/".to_owned(),
            "sha10".to_owned(),
            10,
        );

        let current_key = snapshot_pointer_cache_key(pointer_uri, Some("etag-current"), &current);
        let rollback_key =
            snapshot_pointer_cache_key(pointer_uri, Some("etag-rollback"), &rollback);

        assert_ne!(current_key, rollback_key);
        assert!(rollback_key.contains("generation=10"));
        assert!(rollback_key.contains(&rollback.snapshot_uri));
    }
}
