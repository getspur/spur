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

use crate::auth::{self, AuthConfig, AuthDecision, AuthFailure, IamContext, RequestRoute};
use crate::catalog::{self, CatalogResolver};
use crate::drainer;
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
    #[serde(default)]
    pub method: Option<String>,
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
pub async fn handler(event: LambdaEvent<Value>) -> Result<Value, Error> {
    handle_event_with_drainer(event, drain_queued_jobs()).await
}

async fn handle_event_with_drainer<F>(
    event: LambdaEvent<Value>,
    drain_future: F,
) -> Result<Value, Error>
where
    F: Future<Output = Result<drainer::DrainSummary, Error>>,
{
    if is_scheduled_drainer_event(&event.payload) {
        let summary = drain_future.await?;
        return Ok(json!({
            "operation": "drain_queued_jobs",
            "dispatched": summary.dispatched,
            "skipped": summary.skipped,
            "failed": summary.failed
        }));
    }

    let request = serde_json::from_value::<ApiGatewayRequest>(event.payload).map_err(|error| {
        lambda_error(format!(
            "failed to deserialize API Gateway invocation: {error}"
        ))
    })?;
    let response = handle_api_gateway_request(request).await?;
    serde_json::to_value(response).map_err(Error::from)
}

fn is_scheduled_drainer_event(payload: &Value) -> bool {
    payload.get("source").and_then(Value::as_str) == Some("aws.events")
        && payload.get("detail-type").and_then(Value::as_str) == Some("Scheduled Event")
        && payload.pointer("/detail/operation").and_then(Value::as_str) == Some("drain_queued_jobs")
}

#[allow(
    clippy::await_holding_lock,
    reason = "route_index only consults the catalog before its first await"
)]
async fn handle_api_gateway_request(
    api_gateway_request: ApiGatewayRequest,
) -> Result<ApiGatewayResponse, Error> {
    if let Err(error) = reject_jwt_auth_on_wrong_route(&api_gateway_request) {
        return authorization_error_response(error);
    }

    let request = parse_tool_request(&api_gateway_request);
    let request = match request {
        Ok(request) => request,
        Err(error) => return tool_error_response(error),
    };

    let is_oauth_request = is_oauth_route(&api_gateway_request);
    let authenticated_caller = if is_oauth_request {
        let config = match AuthConfig::from_environment() {
            Ok(Some(config)) => config,
            Ok(None) => return authorization_error_response(AuthFailure::AuthDisabled),
            Err(error) => return authorization_error_response(error),
        };
        match authorize_oauth_request_now(&api_gateway_request, &request.tool, &config) {
            Ok(decision) => Some(decision.identity.caller_id().to_owned()),
            Err(error) => return authorization_error_response(error),
        }
    } else {
        match request.tool.as_str() {
            "external_index" | "external_index_status" => Some(
                match authenticated_caller_id(&api_gateway_request, anonymous_mutations_allowed()) {
                    Ok(caller_id) => caller_id,
                    Err(error) => return auth_error_response(error),
                },
            ),
            _ => None,
        }
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
            let result = if let Some(prepared_catalog) = prepared_catalog {
                let mut catalog_guard = catalog_resolver()?;
                let catalog = initialized_catalog(&mut catalog_guard, &prepared_catalog)?;
                let db = catalog.connection();
                mcp::route_index(&request.args, db, catalog, &jobs, &sfn_client, caller_id).await
            } else {
                mcp::route_index_without_catalog(&request.args, &jobs, &sfn_client, caller_id).await
            };
            // Best-effort drainer kick: if the job was accepted into the queue,
            // try to dispatch queued work immediately for lower latency. The
            // scheduled EventBridge drainer remains the correctness fallback;
            // failure to kick must not affect the admission response.
            if let Ok(value) = &result {
                if is_queued_job_response(value) {
                    kick_drainer().await;
                }
            }
            result
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

fn is_oauth_route(request: &ApiGatewayRequest) -> bool {
    matches!(
        auth::classify_route(
            request.raw_path.as_deref().or(request.path.as_deref()),
            request
                .request_context
                .as_ref()
                .and_then(|context| context.http.as_ref())
                .and_then(|http| http.method.as_deref()),
        ),
        RequestRoute::OAuth
    )
}

fn reject_jwt_auth_on_wrong_route(request: &ApiGatewayRequest) -> Result<(), AuthFailure> {
    let has_jwt_context = request
        .request_context
        .as_ref()
        .and_then(|context| context.authorizer.as_ref())
        .and_then(|authorizer| authorizer.jwt.as_ref())
        .is_some();

    if has_jwt_context && !is_oauth_route(request) {
        Err(AuthFailure::WrongRoute)
    } else {
        Ok(())
    }
}

fn authorize_oauth_request_now(
    request: &ApiGatewayRequest,
    tool: &str,
    config: &AuthConfig,
) -> Result<AuthDecision, AuthFailure> {
    let claims = request
        .request_context
        .as_ref()
        .and_then(|context| context.authorizer.as_ref())
        .and_then(|authorizer| authorizer.jwt.as_ref())
        .and_then(|jwt| jwt.claims.as_ref());
    auth::authorize_oauth_tool_now(config, tool, claims)
}

#[cfg(test)]
fn authorize_oauth_request(
    request: &ApiGatewayRequest,
    tool: &str,
    config: &AuthConfig,
    now_epoch_seconds: u64,
) -> Result<AuthDecision, AuthFailure> {
    let claims = request
        .request_context
        .as_ref()
        .and_then(|context| context.authorizer.as_ref())
        .and_then(|authorizer| authorizer.jwt.as_ref())
        .and_then(|jwt| jwt.claims.as_ref());
    auth::authorize_oauth_tool(config, tool, claims, now_epoch_seconds)
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

/// Run one bounded drainer invocation using the production DynamoDB job store
/// and Step Functions starter. This is the correctness path that dispatches
/// queued index jobs under configured running caps.
///
/// It is called by the EventBridge-scheduled correctness trigger and as a
/// best-effort kick after a successful enqueue (see [`handler`]). Kick errors
/// are logged and do not affect the admission response.
pub async fn drain_queued_jobs() -> Result<drainer::DrainSummary, Error> {
    let jobs = job_store();
    let starter = sfn_client()?;
    let config = mcp::index_queue_config();
    let limits = mcp::index_drainer_limits();
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    Ok(drainer::Drainer::new(&jobs, &starter, config)
        .with_limits(limits.max_dispatches_per_run, limits.scan_limit_per_shard)
        .with_rotation_interval_secs(limits.rotation_interval_secs)
        .drain(now_secs)
        .await)
}

/// Whether an `external_index` response represents a job accepted into the
/// queue (status = "queued"). Used to decide whether to kick the drainer.
fn is_queued_job_response(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("queued")
}

/// Best-effort drainer kick. Logs failures but never propagates them — the
/// scheduled EventBridge drainer is the correctness fallback.
async fn kick_drainer() {
    if let Err(error) = drain_queued_jobs().await {
        eprintln!("[lambda] best-effort drainer kick failed: {error}");
    }
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
#[cfg(test)]
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
    let caller = request
        .request_context
        .as_ref()
        .and_then(|context| {
            let authorizer = context.authorizer.as_ref();
            let strict_iam_identity = authorizer.and_then(|authorizer| {
                authorizer.iam.as_ref().and_then(|iam| {
                    IamContext {
                        account_id: iam.account_id.as_deref(),
                        user_id: iam.user_id.as_deref(),
                        user_arn: iam.user_arn.as_deref(),
                    }
                    .authenticate()
                    .ok()
                    .map(|identity| identity.caller_id().to_owned())
                })
            });

            authorizer
                .and_then(jwt_caller_id)
                .map(str::to_owned)
                .or(strict_iam_identity)
                .or_else(|| authorizer.and_then(iam_caller_id).map(str::to_owned))
                .or_else(|| {
                    authorizer
                        .and_then(|authorizer| non_blank(authorizer.principal_id.as_deref()))
                        .map(str::to_owned)
                })
                .or_else(|| {
                    context
                        .identity
                        .as_ref()
                        .and_then(|identity| non_blank(identity.user_arn.as_deref()))
                        .map(str::to_owned)
                })
        })
        .or_else(|| {
            allow_anonymous.then(|| auth::legacy_anonymous_identity().caller_id().to_owned())
        });

    caller.ok_or_else(|| {
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

fn authorization_error_response(error: AuthFailure) -> Result<ApiGatewayResponse, Error> {
    let code = if error.status_code() == 401 {
        "authentication_failed"
    } else {
        "authorization_failed"
    };
    json_response(
        error.status_code(),
        &json!({
            "error": {
                "code": code,
                "reason": error.reason(),
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

    fn hybrid_auth_fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/hybrid-auth-contract.json"))
            .expect("hybrid auth contract fixture should be valid JSON")
    }

    fn poc_external_index_fixture() -> &'static str {
        include_str!(
            "../../../infra/spur-context-service/poc/fixtures/external-index-validation-only.json"
        )
    }

    fn fixture_cases<'a>(fixture: &'a Value, key: &str) -> &'a [Value] {
        fixture[key]
            .as_array()
            .unwrap_or_else(|| panic!("fixture field {key:?} should be an array"))
    }

    fn fixture_str<'a>(value: &'a Value, key: &str) -> &'a str {
        value[key]
            .as_str()
            .unwrap_or_else(|| panic!("fixture field {key:?} should be a string"))
    }

    #[test]
    fn poc_external_index_fixture_parses_through_oauth_request_contract() {
        let request = ApiGatewayRequest {
            body: Some(poc_external_index_fixture().to_owned()),
            is_base64_encoded: false,
            path: None,
            raw_path: Some("/mcp/oauth".to_owned()),
            request_context: Some(ApiGatewayRequestContext {
                authorizer: None,
                http: Some(ApiGatewayHttp {
                    method: Some("POST".to_owned()),
                    source_ip: None,
                }),
                identity: None,
            }),
        };

        assert!(is_oauth_route(&request));
        let parsed = parse_tool_request(&request)
            .expect("the exact committed POC body should satisfy the OAuth request contract");

        assert_eq!(parsed.tool, "external_index");
        assert_eq!(
            parsed.args,
            json!({
                "package": "validation-only-fixture",
                "revision": "offline",
                "source_url": "https://validation-only.invalid/spur-context-poc.tar.gz",
                "source_kind": "tarball",
                "force": false,
            })
        );
    }

    struct EnvVarRestore {
        name: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarRestore {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn eventbridge_schedule_routes_to_queue_drainer() {
        let event = json!({
            "source": "aws.events",
            "detail-type": "Scheduled Event",
            "detail": {
                "operation": "drain_queued_jobs"
            }
        });

        assert!(is_scheduled_drainer_event(&event));
    }

    #[test]
    fn hybrid_auth_fixture_covers_scope_identity_denial_and_route_contracts() {
        let fixture = hybrid_auth_fixture();
        let config = crate::auth::AuthConfig::new(
            "https://issuer.example/pool",
            "human-client",
            ["m2m-client", "rotating-m2m-client"],
            ["blocked-client"],
            "urn:spur:context-service",
        );

        let fixture_policy = fixture_cases(&fixture, "scope_cases")
            .iter()
            .map(|case| {
                (
                    fixture_str(case, "tool"),
                    fixture_str(case, "required_scope"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(fixture_policy, crate::auth::external_tool_scopes());
        let all_scopes = fixture_policy
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();

        for (tool, required_scope) in fixture_policy {
            for candidate_scope in &all_scopes {
                let claims = json!({
                    "iss": "https://issuer.example/pool",
                    "token_use": "access",
                    "client_id": "human-client",
                    "sub": "fixture-human",
                    "exp": 2_000_000_000_u64,
                    "scope": candidate_scope,
                });
                let result =
                    crate::auth::authorize_oauth_tool(&config, tool, Some(&claims), 1_700_000_000);

                if *candidate_scope == required_scope {
                    let decision = result.unwrap_or_else(|error| {
                        panic!("{tool} should authorize {candidate_scope}: {error:?}")
                    });
                    assert_eq!(decision.identity.caller_id(), "cognito:user:fixture-human");
                } else {
                    assert_eq!(
                        result,
                        Err(crate::auth::AuthFailure::MissingScope),
                        "{tool} must reject nonmatching scope {candidate_scope}"
                    );
                }
            }
        }

        for case in fixture_cases(&fixture, "identity_cases") {
            let request = serde_json::from_value::<ApiGatewayRequest>(case["event"].clone())
                .unwrap_or_else(|error| {
                    panic!("{} should deserialize: {error}", fixture_str(case, "name"))
                });
            let caller_id = match fixture_str(case, "scheme") {
                "oauth" => authorize_oauth_request(
                    &request,
                    fixture_str(case, "tool"),
                    &config,
                    1_700_000_000,
                )
                .unwrap_or_else(|error| {
                    panic!("{} should authorize: {error:?}", fixture_str(case, "name"))
                })
                .identity
                .caller_id()
                .to_owned(),
                "legacy" => authenticated_caller_id(
                    &request,
                    case["allow_anonymous"].as_bool().unwrap_or(false),
                )
                .unwrap_or_else(|error| {
                    panic!("{} should authenticate: {error}", fixture_str(case, "name"))
                }),
                scheme => panic!("unsupported fixture auth scheme {scheme:?}"),
            };
            assert_eq!(caller_id, fixture_str(case, "expected_caller"));
        }

        for case in fixture_cases(&fixture, "denial_cases") {
            let request = serde_json::from_value::<ApiGatewayRequest>(case["event"].clone())
                .unwrap_or_else(|error| {
                    panic!("{} should deserialize: {error}", fixture_str(case, "name"))
                });
            assert!(is_oauth_route(&request));
            let failure = authorize_oauth_request(
                &request,
                fixture_str(case, "tool"),
                &config,
                1_700_000_000,
            )
            .expect_err("denial fixture should fail closed");
            let reason = failure.reason();
            let response = authorization_error_response(failure)
                .expect("bounded authorization response should serialize");
            let expected_status = u16::try_from(
                case["expected_status"]
                    .as_u64()
                    .expect("expected_status should be numeric"),
            )
            .expect("expected_status should fit in an HTTP status code");

            assert_eq!(
                response.status_code,
                expected_status,
                "{}",
                fixture_str(case, "name")
            );
            assert_eq!(reason, fixture_str(case, "expected_reason"));
            assert!(response.body.contains(reason));
            assert!(!response.body.contains("fallback-principal"));
            assert!(!response.body.contains("AROAFALLBACK"));
            assert!(!response.body.contains("203.0.113.24"));
        }

        for case in fixture_cases(&fixture, "route_cases") {
            let request = serde_json::from_value::<ApiGatewayRequest>(case["event"].clone())
                .unwrap_or_else(|error| {
                    panic!("{} should deserialize: {error}", fixture_str(case, "name"))
                });
            assert_eq!(
                is_oauth_route(&request),
                case["is_oauth"].as_bool().expect("is_oauth should be bool"),
                "{}",
                fixture_str(case, "name")
            );
        }

        assert!(is_scheduled_drainer_event(
            &fixture["scheduled_drainer_event"]
        ));
        assert!(!is_scheduled_drainer_event(&fixture["oauth_http_event"]));
    }

    #[tokio::test]
    async fn scheduled_drainer_fixture_bypasses_http_deserialization_and_auth() {
        let event = LambdaEvent::new(
            hybrid_auth_fixture()["scheduled_drainer_event"].clone(),
            lambda_runtime::Context::default(),
        );
        let response = handle_event_with_drainer(event, async {
            Ok(drainer::DrainSummary {
                dispatched: 2,
                skipped: 1,
                failed: 0,
            })
        })
        .await
        .expect("scheduled event should run the injected drainer before HTTP parsing");

        assert_eq!(
            response,
            json!({
                "operation": "drain_queued_jobs",
                "dispatched": 2,
                "skipped": 1,
                "failed": 0,
            })
        );
    }

    #[test]
    fn jwt_fixture_on_wrong_route_is_rejected_without_identity_downgrade() {
        let request = serde_json::from_value::<ApiGatewayRequest>(
            hybrid_auth_fixture()["wrong_route_jwt_event"].clone(),
        )
        .expect("wrong-route JWT fixture should deserialize");

        let failure = reject_jwt_auth_on_wrong_route(&request)
            .expect_err("JWT context on the legacy route must fail closed");
        let reason = failure.reason();
        let response = authorization_error_response(failure)
            .expect("bounded wrong-route response should serialize");

        assert_eq!(response.status_code, 401);
        assert_eq!(reason, "wrong_route");
        assert!(!response.body.contains("fallback-principal"));
        assert!(!response.body.contains("AROAFALLBACK"));
        assert!(!response.body.contains("203.0.113.24"));
        assert!(!response.body.contains("anonymous-internal"));
    }

    #[tokio::test]
    async fn api_gateway_event_keeps_proxy_response_shape() {
        let event = LambdaEvent::new(
            json!({
                "body": null,
                "isBase64Encoded": false
            }),
            lambda_runtime::Context::default(),
        );

        let response = handler(event)
            .await
            .expect("invalid tool input should return an API Gateway error response");

        assert_eq!(response["statusCode"], 200);
        assert!(response["body"].is_string());
        assert_eq!(response["isBase64Encoded"], false);
    }

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
    fn authenticated_caller_id_uses_stable_iam_principal_without_session_name() {
        let request = request_from_context(json!({
            "authorizer": {
                "iam": {
                    "accountId": "123456789012",
                    "userId": "AROASTABLE:untrusted-session-name",
                    "userArn": "arn:aws:sts::123456789012:assumed-role/context-indexer/untrusted-session-name"
                }
            }
        }));

        assert_eq!(
            authenticated_caller_id(&request, false).expect("IAM caller should authenticate"),
            "iam:123456789012:AROASTABLE"
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
    fn oauth_route_requires_the_exact_post_path() {
        let oauth = serde_json::from_value::<ApiGatewayRequest>(json!({
            "rawPath": "/mcp/oauth",
            "requestContext": { "http": { "method": "POST" } }
        }))
        .expect("OAuth request should deserialize");
        let wrong_method = serde_json::from_value::<ApiGatewayRequest>(json!({
            "rawPath": "/mcp/oauth",
            "requestContext": { "http": { "method": "GET" } }
        }))
        .expect("non-POST request should deserialize");

        assert!(is_oauth_route(&oauth));
        assert!(!is_oauth_route(&wrong_method));
    }

    #[test]
    fn oauth_route_cannot_be_moved_by_an_environment_override() {
        let _path = EnvVarRestore::set("SPUR_COGNITO_OAUTH_PATH", "/different-path");
        let oauth = serde_json::from_value::<ApiGatewayRequest>(json!({
            "rawPath": "/mcp/oauth",
            "requestContext": { "http": { "method": "POST" } }
        }))
        .expect("OAuth request should deserialize");

        assert!(is_oauth_route(&oauth));
    }

    #[test]
    fn oauth_api_gateway_string_claims_authorize_human_identity() {
        let request = serde_json::from_value::<ApiGatewayRequest>(json!({
            "rawPath": "/mcp/oauth",
            "requestContext": {
                "authorizer": {
                    "jwt": {
                        "claims": {
                            "iss": "https://issuer.example/pool",
                            "token_use": "access",
                            "client_id": "human-client",
                            "sub": "human-subject",
                            "exp": "2000000000",
                            "scope": "urn:spur:context-service/external.read"
                        }
                    }
                },
                "http": { "method": "POST" }
            }
        }))
        .expect("API Gateway JWT claims should deserialize as strings");
        let config = crate::auth::AuthConfig::new(
            "https://issuer.example/pool",
            "human-client",
            ["m2m-client"],
            std::iter::empty::<&str>(),
            "urn:spur:context-service",
        );

        let decision =
            authorize_oauth_request(&request, "external_catalog", &config, 1_700_000_000)
                .expect("a valid API Gateway JWT claim map should authorize");

        assert_eq!(decision.identity.caller_id(), "cognito:user:human-subject");
    }

    #[test]
    fn malformed_oauth_jwt_never_falls_back_to_iam_or_principal_id() {
        let request = serde_json::from_value::<ApiGatewayRequest>(json!({
            "rawPath": "/mcp/oauth",
            "requestContext": {
                "authorizer": {
                    "principalId": "legacy-principal",
                    "iam": {
                        "accountId": "123456789012",
                        "userId": "AROATEST:session"
                    },
                    "jwt": {
                        "claims": {
                            "iss": "unexpected-issuer",
                            "token_use": "access",
                            "client_id": "human-client",
                            "sub": "human-subject",
                            "exp": 2000000000,
                            "scope": "urn:spur:context-service/external.read"
                        }
                    }
                },
                "http": { "method": "POST", "sourceIp": "203.0.113.24" }
            }
        }))
        .expect("OAuth request should deserialize");
        let config = crate::auth::AuthConfig::new(
            "https://issuer.example/pool",
            "human-client",
            ["m2m-client"],
            std::iter::empty::<&str>(),
            "urn:spur:context-service",
        );

        assert_eq!(
            authorize_oauth_request(&request, "external_catalog", &config, 1_700_000_000),
            Err(crate::auth::AuthFailure::WrongIssuer)
        );
    }

    #[test]
    fn oauth_errors_return_bounded_401_or_403_bodies() {
        let unauthorized = authorization_error_response(crate::auth::AuthFailure::WrongIssuer)
            .expect("authorization response should serialize");
        let forbidden = authorization_error_response(crate::auth::AuthFailure::MissingScope)
            .expect("authorization response should serialize");

        assert_eq!(unauthorized.status_code, 401);
        assert_eq!(forbidden.status_code, 403);
        assert!(unauthorized.body.contains("wrong_issuer"));
        assert!(!unauthorized.body.contains("token"));
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
