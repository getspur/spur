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
    pub jwt: Option<JwtAuthorizer>,
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

    let result = match request.tool.as_str() {
        "external_index_status" => {
            let jobs = job_store();
            let checker = status_checker();
            route_index_status_control_plane(&request.args, &jobs, &checker).await
        }
        "external_index" => {
            let prepared_catalog = prepare_catalog().await?;
            let mut catalog_guard = catalog_resolver()?;
            let catalog = initialized_catalog(&mut catalog_guard, &prepared_catalog)?;
            let db = catalog.connection();
            let jobs = job_store();
            let sfn_client = sfn_client()?;
            let caller_id = caller_id(&event.payload);
            mcp::route_index(&request.args, db, catalog, &jobs, &sfn_client, &caller_id).await
        }
        _ => {
            let prepared_catalog = prepare_catalog().await?;
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
) -> Result<Value, McpHandlerError> {
    mcp::route_index_status(args, jobs, Some(checker)).await
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

async fn prepare_catalog() -> Result<PreparedCatalog, Error> {
    let catalog_dsn = catalog_dsn()?;
    let catalog_etag = catalog_etag(&catalog_dsn).await?;
    prepare_catalog_source(catalog_dsn, catalog_etag).await
}

async fn prepare_catalog_source(
    catalog_dsn: String,
    catalog_etag: Option<String>,
) -> Result<PreparedCatalog, Error> {
    if let Some(uri) = parse_s3_uri(&catalog_dsn)? {
        let data_path = catalog_data_path(&catalog_dsn);
        let local_path = local_snapshot_path(&catalog_dsn, catalog_etag.as_deref())?;
        if !local_path.is_file() {
            download_catalog_snapshot(&uri, &local_path).await?;
        }
        return Ok(PreparedCatalog {
            cache_key: format!("{catalog_dsn}\n{data_path}"),
            catalog_etag,
            source: PreparedCatalogSource::FrozenSnapshot {
                local_path,
                data_path,
            },
        });
    }

    if is_postgres_catalog_dsn(&catalog_dsn) {
        return Err(lambda_error(
            "serving requires SPUR_CATALOG_S3_URI to point at a frozen DuckLake snapshot; refusing to connect to Postgres",
        ));
    }

    Ok(PreparedCatalog {
        cache_key: catalog_dsn.clone(),
        catalog_etag,
        source: PreparedCatalogSource::Direct { catalog_dsn },
    })
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

fn catalog_data_path(snapshot_uri: &str) -> String {
    if let Ok(path) = env::var("SPUR_CONTEXT_DUCKLAKE_DATA_PATH") {
        if !path.trim().is_empty() {
            return path;
        }
    }

    infer_data_path_from_snapshot_uri(snapshot_uri)
        .unwrap_or_else(|| catalog::DEFAULT_DATA_PATH.to_owned())
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

fn caller_id(request: &ApiGatewayRequest) -> String {
    request
        .request_context
        .as_ref()
        .and_then(|context| {
            context
                .authorizer
                .as_ref()
                .and_then(jwt_caller_id)
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

fn jwt_caller_id(authorizer: &ApiGatewayAuthorizer) -> Option<&str> {
    let claims = authorizer.jwt.as_ref()?.claims.as_ref()?;
    claim_str(claims, "sub").or_else(|| claim_str(claims, "principal_id"))
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
    fn remote_catalog_dsn_reinitializes_when_etag_changes() {
        let s3_dsn = "s3://spur-context/catalog/catalog.ducklake";

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
}
