use std::env;
use std::fmt;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use lambda_runtime::{run, service_fn, Error, LambdaEvent};

#[path = "../api_key_cleanup.rs"]
mod api_key_cleanup;
#[allow(
    dead_code,
    clippy::enum_variant_names,
    reason = "the cleanup Lambda includes the narrow shared key-store domain"
)]
#[path = "../api_keys.rs"]
mod api_keys;

use api_key_cleanup::{
    run_scheduled_cleanup, ApiKeyCleanupConfig, ApiKeyCleanupError, ApiKeyCleanupEvent,
    ApiKeyCleanupResult,
};
use api_keys::{ApiKeyStore, DynamoDbApiKeyStore};

static STORE: OnceLock<DynamoDbApiKeyStore> = OnceLock::new();

#[tokio::main]
async fn main() -> Result<(), Error> {
    run(service_fn(handler)).await
}

async fn handler(event: LambdaEvent<ApiKeyCleanupEvent>) -> Result<ApiKeyCleanupResult, Error> {
    let store = store().map_err(boundary_error)?;
    let config = config().map_err(boundary_error)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| boundary_error(ApiKeyCleanupError::InvalidInvocation))?
        .as_secs();
    let result = handle_request(
        &event.payload,
        store,
        &config,
        now,
        &event.context.request_id,
    )
    .await?;
    println!("{}", result.emf_document(now.saturating_mul(1_000)));
    Ok(result)
}

async fn handle_request<S: ApiKeyStore + ?Sized>(
    event: &ApiKeyCleanupEvent,
    store: &S,
    config: &ApiKeyCleanupConfig,
    now_epoch_seconds: u64,
    lease_owner: &str,
) -> Result<ApiKeyCleanupResult, Error> {
    run_scheduled_cleanup(event, store, config, now_epoch_seconds, lease_owner)
        .await
        .map_err(boundary_error)
}

fn config() -> Result<ApiKeyCleanupConfig, ApiKeyCleanupError> {
    let max_catchup_hours = env::var("SPUR_API_KEY_CLEANUP_MAX_CATCHUP_HOURS")
        .map_err(|_| ApiKeyCleanupError::InvalidConfig)?;
    let max_buckets = env::var("SPUR_API_KEY_CLEANUP_MAX_BUCKETS")
        .map_err(|_| ApiKeyCleanupError::InvalidConfig)?;
    let max_pages = env::var("SPUR_API_KEY_CLEANUP_MAX_PAGES")
        .map_err(|_| ApiKeyCleanupError::InvalidConfig)?;
    let max_records = env::var("SPUR_API_KEY_CLEANUP_MAX_RECORDS")
        .map_err(|_| ApiKeyCleanupError::InvalidConfig)?;
    let page_limit = env::var("SPUR_API_KEY_CLEANUP_PAGE_LIMIT")
        .map_err(|_| ApiKeyCleanupError::InvalidConfig)?;
    ApiKeyCleanupConfig::parse(
        &max_catchup_hours,
        &max_buckets,
        &max_pages,
        &max_records,
        &page_limit,
    )
}

fn store() -> Result<&'static DynamoDbApiKeyStore, ApiKeyCleanupError> {
    if !matches!(
        env::var("SPUR_API_KEY_AUTH_ENABLED")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "true" | "TRUE" | "yes")
    ) {
        return Err(ApiKeyCleanupError::InvalidConfig);
    }
    let table_name = env::var("SPUR_CONTEXT_API_KEYS_TABLE")
        .ok()
        .filter(|name| !name.trim().is_empty() && name.len() <= 255)
        .ok_or(ApiKeyCleanupError::InvalidConfig)?;
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

fn boundary_error(error: ApiKeyCleanupError) -> Error {
    let message = match error {
        ApiKeyCleanupError::InvalidEvent => "InvalidCleanupEvent",
        ApiKeyCleanupError::InvalidConfig => "CleanupConfigurationUnavailable",
        ApiKeyCleanupError::InvalidInvocation => "InvalidCleanupInvocation",
        ApiKeyCleanupError::Store(_) => "CleanupStoreUnavailable",
    };
    Box::new(BoundaryError(message))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use api_keys::FakeApiKeyStore;

    #[tokio::test]
    async fn binary_boundary_sanitizes_invalid_events() {
        let event = serde_json::from_value(json!({
            "source": "aws.events",
            "detail-type": "Scheduled Event",
            "detail": { "operation": "drain_queued_jobs" }
        }))
        .expect("fixture should deserialize");

        let error = handle_request(
            &event,
            &FakeApiKeyStore::new(),
            &ApiKeyCleanupConfig::parse("24", "4", "8", "100", "100")
                .expect("config should be valid"),
            100 * 3_600 + 120,
            "request-id",
        )
        .await
        .expect_err("wrong operation should fail the Lambda boundary");
        assert_eq!(error.to_string(), "InvalidCleanupEvent");
    }
}
