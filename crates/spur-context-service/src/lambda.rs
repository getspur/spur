//! AWS Lambda HTTP entry point for the context-service MCP surface.

use std::collections::BTreeMap;
use std::env;
use std::sync::{Mutex, MutexGuard, OnceLock};

use duckdb::Connection;
use lambda_runtime::{Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{self, CatalogResolver};
use crate::mcp::{self, McpHandlerError};

static DB_CONNECTION: OnceLock<Mutex<Option<Connection>>> = OnceLock::new();
static CATALOG_RESOLVER: OnceLock<Mutex<Option<CatalogResolver>>> = OnceLock::new();

#[derive(Debug, Deserialize)]
pub struct ApiGatewayRequest {
    pub body: Option<String>,
    #[serde(rename = "isBase64Encoded", default)]
    pub is_base64_encoded: bool,
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

#[expect(
    clippy::unused_async,
    reason = "cargo-lambda handler entry point is required to be async"
)]
pub async fn handler(event: LambdaEvent<ApiGatewayRequest>) -> Result<ApiGatewayResponse, Error> {
    let request = parse_tool_request(&event.payload);
    let request = match request {
        Ok(request) => request,
        Err(error) => return tool_error_response(error),
    };

    let mut db_guard = db_connection()?;
    let mut catalog_guard = catalog_resolver()?;
    let db = initialized_db(&mut db_guard)?;
    let catalog = initialized_catalog(&mut catalog_guard)?;
    match mcp::handle_tool_sync(&request.tool, &request.args, db, catalog) {
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
    serde_json::from_str(body).map_err(|error| {
        McpHandlerError::InvalidParams(format!("failed to parse request JSON body: {error}"))
    })
}

fn db_connection() -> Result<MutexGuard<'static, Option<Connection>>, Error> {
    DB_CONNECTION
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|error| lambda_error(format!("DuckDB connection cache is poisoned: {error}")))
}

fn catalog_resolver() -> Result<MutexGuard<'static, Option<CatalogResolver>>, Error> {
    CATALOG_RESOLVER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|error| lambda_error(format!("catalog resolver cache is poisoned: {error}")))
}

fn initialized_db<'a>(
    guard: &'a mut MutexGuard<'static, Option<Connection>>,
) -> Result<&'a Connection, Error> {
    if guard.is_none() {
        let catalog_dsn = catalog_dsn()?;
        **guard = Some(catalog::connect_ducklake(&catalog_dsn).map_err(Error::from)?);
    }
    guard
        .as_ref()
        .ok_or_else(|| lambda_error("DuckDB connection cache did not initialize"))
}

fn initialized_catalog<'a>(
    guard: &'a mut MutexGuard<'static, Option<CatalogResolver>>,
) -> Result<&'a CatalogResolver, Error> {
    if guard.is_none() {
        let catalog_dsn = catalog_dsn()?;
        **guard = Some(CatalogResolver::new(&catalog_dsn).map_err(Error::from)?);
    }
    guard
        .as_ref()
        .ok_or_else(|| lambda_error("catalog resolver cache did not initialize"))
}

fn catalog_dsn() -> Result<String, Error> {
    env::var("SPUR_CATALOG_DSN").map_err(|error| {
        lambda_error(format!(
            "SPUR_CATALOG_DSN environment variable is required: {error}"
        ))
    })
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
