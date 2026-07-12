//! Fail-closed scheduled cleanup adapter for expired personal API keys.
//!
//! The durable lease, expiry-index pagination, cursor progression, and atomic
//! owner-counter decrement live in the shared API key store. This module
//! validates the Lambda boundary and translates bounded configuration into one
//! sweep request without depending on the serving or `DuckDB` modules.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::api_keys::{
    ApiKeyStore, ApiKeyStoreError, SweepRequest, MAX_PAGE_LIMIT, MAX_SWEEP_BUCKETS,
    MAX_SWEEP_PAGES, MAX_SWEEP_RECORDS,
};

const CLEANUP_OPERATION: &str = "sweep_expired_api_keys";
const EVENT_SOURCE: &str = "aws.events";
const EVENT_DETAIL_TYPE: &str = "Scheduled Event";
const MAX_CATCHUP_HOURS: usize = 8_760;
const LEASE_DURATION_SECONDS: u64 = 330;
const MAX_LEASE_OWNER_LEN: usize = 128;

/// Exact `EventBridge` input accepted by the cleanup Lambda.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyCleanupEvent {
    source: String,
    #[serde(rename = "detail-type")]
    detail_type: String,
    detail: ApiKeyCleanupDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiKeyCleanupDetail {
    operation: String,
}

impl ApiKeyCleanupEvent {
    fn is_scheduled_sweep(&self) -> bool {
        self.source == EVENT_SOURCE
            && self.detail_type == EVENT_DETAIL_TYPE
            && self.detail.operation == CLEANUP_OPERATION
    }
}

/// Validated per-invocation cleanup bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiKeyCleanupConfig {
    max_catchup_hours: usize,
    max_buckets: usize,
    max_pages: usize,
    max_records: usize,
    page_limit: usize,
}

impl ApiKeyCleanupConfig {
    /// Parses the Terraform-provided whole-number limits.
    ///
    /// # Errors
    ///
    /// Returns [`ApiKeyCleanupError::InvalidConfig`] for a non-integer or a
    /// value outside the runtime bounds.
    ///
    /// # Examples
    ///
    /// ```
    /// use spur_context_service::api_key_cleanup::ApiKeyCleanupConfig;
    ///
    /// assert!(ApiKeyCleanupConfig::parse("168", "4", "8", "100", "100").is_ok());
    /// assert!(ApiKeyCleanupConfig::parse("0", "4", "8", "100", "100").is_err());
    /// ```
    pub fn parse(
        max_catchup_hours: &str,
        max_buckets: &str,
        max_pages: &str,
        max_records: &str,
        page_limit: &str,
    ) -> Result<Self, ApiKeyCleanupError> {
        let max_catchup_hours = parse_bounded_limit(max_catchup_hours, MAX_CATCHUP_HOURS)?;
        let max_buckets = parse_bounded_limit(max_buckets, MAX_SWEEP_BUCKETS)?;
        let max_pages = parse_bounded_limit(max_pages, MAX_SWEEP_PAGES)?;
        let max_records = parse_bounded_limit(max_records, MAX_SWEEP_RECORDS)?;
        let page_limit = parse_bounded_limit(page_limit, MAX_PAGE_LIMIT)?;
        if max_pages < max_buckets + 2 {
            return Err(ApiKeyCleanupError::InvalidConfig);
        }
        Ok(Self {
            max_catchup_hours,
            max_buckets,
            max_pages,
            max_records,
            page_limit,
        })
    }

    fn sweep_request(self, now_epoch_seconds: u64, lease_owner: &str) -> SweepRequest {
        let current_hour = now_epoch_seconds / 3_600;
        SweepRequest {
            now_epoch_seconds,
            start_hour: current_hour.saturating_sub(self.max_catchup_hours as u64),
            max_buckets: self.max_buckets,
            max_pages: self.max_pages,
            max_records: self.max_records,
            page_limit: self.page_limit,
            lease_owner: lease_owner.to_owned(),
            lease_duration_seconds: LEASE_DURATION_SECONDS,
        }
    }
}

fn parse_bounded_limit(value: &str, maximum: usize) -> Result<usize, ApiKeyCleanupError> {
    let value = value
        .parse::<usize>()
        .map_err(|_| ApiKeyCleanupError::InvalidConfig)?;
    if !(1..=maximum).contains(&value) {
        return Err(ApiKeyCleanupError::InvalidConfig);
    }
    Ok(value)
}

/// Sanitized result returned by the cleanup Lambda and logged as EMF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiKeyCleanupResult {
    /// Number of records newly transitioned from active to revoked.
    pub processed: usize,
    /// Last fully drained UTC-hour bucket.
    pub completed_hour: Option<u64>,
    /// Whether a partial bucket or another closed bucket remains.
    pub has_more: bool,
    /// Distance from the last closed hour to the persisted cursor.
    pub cursor_lag_hours: u64,
}

impl ApiKeyCleanupResult {
    /// Returns a secret-free `CloudWatch` Embedded Metric Format document.
    #[must_use]
    pub fn emf_document(&self, timestamp_millis: u64) -> String {
        json!({
            "_aws": {
                "Timestamp": timestamp_millis,
                "CloudWatchMetrics": [{
                    "Namespace": "SPUR/ContextServiceAuth",
                    "Dimensions": [[]],
                    "Metrics": [
                        { "Name": "ApiKeyCleanupCursorLagHours", "Unit": "Count" },
                        { "Name": "ApiKeyCleanupProcessed", "Unit": "Count" },
                        { "Name": "ApiKeyCleanupHasMore", "Unit": "Count" }
                    ]
                }]
            },
            "ApiKeyCleanupCursorLagHours": self.cursor_lag_hours,
            "ApiKeyCleanupProcessed": self.processed,
            "ApiKeyCleanupHasMore": u8::from(self.has_more)
        })
        .to_string()
    }
}

/// Bounded cleanup boundary failures. Values and provider details are omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyCleanupError {
    /// The `EventBridge` discriminator did not exactly match the cleanup event.
    #[error("invalid cleanup event")]
    InvalidEvent,
    /// A required environment value was missing, malformed, or out of bounds.
    #[error("invalid cleanup configuration")]
    InvalidConfig,
    /// The invocation request ID was not safe to persist as a lease owner.
    #[error("invalid cleanup invocation")]
    InvalidInvocation,
    /// The durable store could not safely complete the sweep.
    #[error("API key cleanup store unavailable")]
    Store(#[from] ApiKeyStoreError),
}

/// Validates one scheduled event and performs one bounded durable sweep.
///
/// # Errors
///
/// Returns a sanitized boundary error for a wrong event, invalid invocation
/// identity, lease conflict, persisted-data failure, or `DynamoDB` failure.
pub async fn run_scheduled_cleanup<S: ApiKeyStore + ?Sized>(
    event: &ApiKeyCleanupEvent,
    store: &S,
    config: &ApiKeyCleanupConfig,
    now_epoch_seconds: u64,
    lease_owner: &str,
) -> Result<ApiKeyCleanupResult, ApiKeyCleanupError> {
    if !event.is_scheduled_sweep() {
        return Err(ApiKeyCleanupError::InvalidEvent);
    }
    if lease_owner.trim().is_empty()
        || lease_owner.len() > MAX_LEASE_OWNER_LEN
        || lease_owner.chars().any(char::is_control)
    {
        return Err(ApiKeyCleanupError::InvalidInvocation);
    }

    let page = store
        .sweep_expired(config.sweep_request(now_epoch_seconds, lease_owner))
        .await?;
    let last_closed_hour = (now_epoch_seconds / 3_600).saturating_sub(1);
    let cursor_lag_hours = page
        .completed_hour
        .map_or(config.max_catchup_hours as u64, |completed| {
            last_closed_hour.saturating_sub(completed)
        });
    Ok(ApiKeyCleanupResult {
        processed: page.processed,
        completed_hour: page.completed_hour,
        has_more: page.has_more,
        cursor_lag_hours,
    })
}
