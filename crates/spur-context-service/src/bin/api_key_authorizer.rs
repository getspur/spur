use std::env;
use std::fmt;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use lambda_runtime::{run, service_fn, Error, LambdaEvent};

#[allow(
    dead_code,
    reason = "serving-only context parsing shares this source file"
)]
#[path = "../api_key_authorizer.rs"]
mod api_key_authorizer;
#[allow(
    dead_code,
    clippy::enum_variant_names,
    reason = "the authorizer includes a narrow subset of the shared key domain"
)]
#[path = "../api_keys.rs"]
mod api_keys;

use api_key_authorizer::{
    authorize_api_key_with_environment, ApiKeyAuthorizerError, ApiKeyAuthorizerRequest,
    ApiKeyAuthorizerResponse,
};
use api_keys::{ApiKeyStore, DynamoDbApiKeyStore};

static STORE: OnceLock<DynamoDbApiKeyStore> = OnceLock::new();

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

async fn handler(
    event: LambdaEvent<ApiKeyAuthorizerRequest>,
) -> Result<ApiKeyAuthorizerResponse, Error> {
    let store = store().map_err(boundary_error)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| boundary_error(ApiKeyAuthorizerError::Unavailable))?
        .as_secs();
    let expected_environment = env::var("SPUR_API_KEY_ENVIRONMENT").ok();
    handle_request(&event.payload, store, expected_environment.as_deref(), now).await
}

async fn handle_request(
    request: &ApiKeyAuthorizerRequest,
    store: &dyn ApiKeyStore,
    expected_environment: Option<&str>,
    now_epoch_seconds: u64,
) -> Result<ApiKeyAuthorizerResponse, Error> {
    authorize_api_key_with_environment(request, store, expected_environment, now_epoch_seconds)
        .await
        .map_err(boundary_error)
}

fn store() -> Result<&'static DynamoDbApiKeyStore, ApiKeyAuthorizerError> {
    if !matches!(
        env::var("SPUR_API_KEY_AUTH_ENABLED")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "true" | "TRUE" | "yes")
    ) {
        return Err(ApiKeyAuthorizerError::Unavailable);
    }
    let table_name = env::var("SPUR_CONTEXT_API_KEYS_TABLE")
        .ok()
        .filter(|name| !name.trim().is_empty() && name.len() <= 255)
        .ok_or(ApiKeyAuthorizerError::Unavailable)?;
    Ok(STORE.get_or_init(|| {
        DynamoDbApiKeyStore::with_table_name(dynamodb_client_from_env(), table_name)
    }))
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

#[derive(Debug)]
struct BoundaryError(&'static str);

impl fmt::Display for BoundaryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for BoundaryError {}

fn boundary_error(error: ApiKeyAuthorizerError) -> Error {
    let message = match error {
        ApiKeyAuthorizerError::AuthenticationFailed => "Unauthorized",
        ApiKeyAuthorizerError::Unavailable => "AuthorizerUnavailable",
    };
    Box::new(BoundaryError(message))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use api_keys::FakeApiKeyStore;

    #[tokio::test]
    async fn binary_boundary_serializes_cacheable_denies_and_keeps_config_failures_as_errors() {
        let request = ApiKeyAuthorizerRequest {
            route_key: Some("POST /mcp/api-key".to_owned()),
            identity_source: Some(vec!["not-a-key".to_owned(), "POST /mcp/api-key".to_owned()]),
            headers: BTreeMap::from([("x-spur-api-key".to_owned(), "not-a-key".to_owned())]),
        };
        let store = FakeApiKeyStore::new();

        let denied = handle_request(&request, &store, Some("live"), 1_700_000_000)
            .await
            .expect("credential denial should serialize instead of failing the Lambda");
        assert_eq!(
            serde_json::to_value(denied).expect("deny should serialize"),
            json!({
                "isAuthorized": false,
                "context": {
                    "auth_context_version": 1,
                    "auth_kind": "api_key",
                    "denial_code": "authentication_failed"
                }
            })
        );

        assert_eq!(
            handle_request(&request, &store, Some("staging"), 1_700_000_000)
                .await
                .expect_err("bad configuration should fail the Lambda")
                .to_string(),
            "AuthorizerUnavailable"
        );
    }
}
