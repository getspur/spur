//! AWS Lambda HTTP entry point for the context-service MCP surface.

use std::collections::BTreeMap;
use std::env;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, MutexGuard, OnceLock};

use lambda_runtime::{Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::catalog::{self, CatalogResolver};
use crate::mcp::{self, McpHandlerError};

pub static CATALOG_RESOLVER: OnceLock<Mutex<Option<CatalogResolver>>> = OnceLock::new();

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

    let mut catalog_guard = catalog_resolver()?;
    let catalog = initialized_catalog(&mut catalog_guard)?;
    let db = catalog.connection();

    let result = match request.tool.as_str() {
        "external_index" => {
            let sfn_client = sfn_client()?;
            let caller_id = caller_id(&event.payload);
            mcp::route_index(&request.args, db, catalog, &sfn_client, &caller_id).await
        }
        "external_index_status" => mcp::route_index_status(&request.args, db),
        _ => mcp::handle_tool_sync(&request.tool, &request.args, db, catalog),
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

fn catalog_resolver() -> Result<MutexGuard<'static, Option<CatalogResolver>>, Error> {
    CATALOG_RESOLVER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|error| lambda_error(format!("catalog resolver cache is poisoned: {error}")))
}

fn initialized_catalog<'a>(
    guard: &'a mut MutexGuard<'static, Option<CatalogResolver>>,
) -> Result<&'a CatalogResolver, Error> {
    if guard.is_none() {
        let catalog_dsn = catalog_dsn()?;
        let conn = catalog::connect_ducklake(&catalog_dsn).map_err(Error::from)?;
        **guard = Some(CatalogResolver::from_connection(conn));
    }
    guard
        .as_ref()
        .ok_or_else(|| lambda_error("catalog resolver cache did not initialize"))
}

fn catalog_dsn() -> Result<String, Error> {
    env::var("SPUR_CATALOG_S3_URI")
        .or_else(|_| env::var("SPUR_CATALOG_DSN"))
        .map_err(|error| {
            lambda_error(format!(
                "SPUR_CATALOG_S3_URI (or SPUR_CATALOG_DSN) environment variable is required: {error}"
            ))
        })
}

fn sfn_client() -> Result<SfnIndexExecutionStarter, Error> {
    let region = env::var("AWS_REGION")
        .or_else(|_| env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".to_owned());
    let mut config =
        aws_sdk_sfn::Config::builder().region(aws_sdk_sfn::config::Region::new(region));

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

    Ok(SfnIndexExecutionStarter {
        client: aws_sdk_sfn::Client::from_conf(config.build()),
        state_machine_arn: env::var("SPUR_INDEX_STATE_MACHINE_ARN").map_err(|error| {
            lambda_error(format!(
                "SPUR_INDEX_STATE_MACHINE_ARN environment variable is required: {error}"
            ))
        })?,
    })
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
}
