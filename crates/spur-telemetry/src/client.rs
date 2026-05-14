use crate::error::{Result, TelemetryError};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

const DEFAULT_POSTHOG_ENDPOINT: &str = "https://us.i.posthog.com";
const POSTHOG_ENDPOINT_ENV: &str = "SPUR_POSTHOG_ENDPOINT";
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(not(telemetry_disabled))]
pub(crate) const POSTHOG_KEY: &str = env!("SPUR_POSTHOG_KEY");
#[cfg(telemetry_disabled)]
pub(crate) const POSTHOG_KEY: &str = "";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PosthogEvent {
    pub(crate) event: String,
    pub(crate) distinct_id: String,
    pub(crate) properties: Value,
    pub(crate) timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(crate) struct PosthogClient {
    endpoint: String,
    http: reqwest::Client,
}

impl Default for PosthogClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PosthogClient {
    pub(crate) fn new() -> Self {
        let endpoint = std::env::var(POSTHOG_ENDPOINT_ENV)
            .unwrap_or_else(|_| DEFAULT_POSTHOG_ENDPOINT.to_string());
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("failed to build telemetry HTTP client");
        Self { endpoint, http }
    }

    #[cfg(not(telemetry_disabled))]
    pub(crate) async fn send_batch(&self, events: &[PosthogEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let response = self
            .http
            .post(self.batch_url())
            .json(&serde_json::json!({
                "api_key": POSTHOG_KEY,
                "batch": events,
            }))
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        Err(TelemetryError::HttpStatus(response.status()))
    }

    #[cfg(telemetry_disabled)]
    pub(crate) async fn send_batch(&self, _events: &[PosthogEvent]) -> Result<()> {
        Ok(())
    }

    fn batch_url(&self) -> String {
        format!("{}/batch/", self.endpoint.trim_end_matches('/'))
    }
}

#[cfg(all(test, not(telemetry_disabled)))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_event() -> PosthogEvent {
        PosthogEvent {
            event: "spur_test_event".to_string(),
            distinct_id: "user-123".to_string(),
            properties: json!({"source":"test"}),
            timestamp: Utc::now(),
        }
    }

    #[tokio::test]
    async fn send_batch_posts_to_batch_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/batch/"))
            .and(body_partial_json(json!({
                "api_key": POSTHOG_KEY,
                "batch": [{
                    "event": "spur_test_event",
                    "distinct_id": "user-123",
                    "properties": {"source":"test"}
                }]
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = PosthogClient {
            endpoint: server.uri(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
        };

        client.send_batch(&[sample_event()]).await.unwrap();
    }

    #[tokio::test]
    async fn send_batch_times_out_after_two_seconds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/batch/"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;

        let client = PosthogClient {
            endpoint: server.uri(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
        };

        let start = std::time::Instant::now();
        let err = client.send_batch(&[sample_event()]).await.unwrap_err();
        assert!(matches!(err, TelemetryError::Http(_)));
        assert!(start.elapsed() < Duration::from_secs(4));
    }
}
