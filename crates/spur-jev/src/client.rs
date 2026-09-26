//! Pluggable transports for the Jev System One API.

use std::{env, fmt, time::Duration};

use async_trait::async_trait;

use crate::wire::{JevRequest, JevResponse};

const JEV_API_URL: &str = "https://api.typesafe.ai/v1/systemone";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const ERROR_BODY_PREFIX_CHARS: usize = 512;

/// Failure while preparing or executing a Jev request.
#[derive(Debug, thiserror::Error)]
pub enum JevError {
    /// The required API key was absent or empty.
    #[error("JEV_API_KEY is not set")]
    MissingApiKey,
    /// The HTTP client could not be built or the request could not complete.
    #[error("Jev transport error: {0}")]
    Transport(#[source] reqwest::Error),
    /// The service returned a non-success status.
    #[error("Jev returned HTTP {status}: {body_prefix}")]
    HttpStatus {
        /// Numeric HTTP status code.
        status: u16,
        /// Bounded, API-key-redacted prefix of the response body.
        body_prefix: String,
    },
    /// A successful response did not match the wire contract.
    #[error("failed to decode Jev response: {0}")]
    Decode(#[source] serde_json::Error),
}

/// Boundary used to evaluate one typed Jev request.
#[async_trait]
pub trait JevTransport: Send + Sync {
    /// Send `request` and return the typed service response.
    async fn send(&self, request: &JevRequest) -> Result<JevResponse, JevError>;
}

/// Offline transport that always returns one canned response.
#[derive(Clone, Debug)]
pub struct MockTransport {
    response: JevResponse,
}

impl MockTransport {
    /// Create a mock backed by `response`.
    #[must_use]
    pub fn new(response: JevResponse) -> Self {
        Self { response }
    }
}

#[async_trait]
impl JevTransport for MockTransport {
    async fn send(&self, _request: &JevRequest) -> Result<JevResponse, JevError> {
        Ok(self.response.clone())
    }
}

/// HTTPS transport for `TypeSafe`'s hosted Jev endpoint.
pub struct HttpJevTransport {
    client: reqwest::Client,
    api_key: Box<str>,
}

impl HttpJevTransport {
    /// Build a transport using the `JEV_API_KEY` environment variable.
    pub fn from_env() -> Result<Self, JevError> {
        let api_key = env::var("JEV_API_KEY").map_err(|_error| JevError::MissingApiKey)?;
        if api_key.is_empty() {
            return Err(JevError::MissingApiKey);
        }
        Self::with_api_key(api_key)
    }

    fn with_api_key(api_key: String) -> Result<Self, JevError> {
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(JevError::Transport)?;
        Ok(Self {
            client,
            api_key: api_key.into_boxed_str(),
        })
    }
}

impl fmt::Debug for HttpJevTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpJevTransport")
            .field("endpoint", &JEV_API_URL)
            .field("timeout", &HTTP_TIMEOUT)
            .field("api_key", &"[redacted]")
            .finish()
    }
}

#[async_trait]
impl JevTransport for HttpJevTransport {
    async fn send(&self, request: &JevRequest) -> Result<JevResponse, JevError> {
        let response = self
            .client
            .post(JEV_API_URL)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .map_err(JevError::Transport)?;
        let status = response.status();
        let body = response.text().await.map_err(JevError::Transport)?;

        if !status.is_success() {
            return Err(JevError::HttpStatus {
                status: status.as_u16(),
                body_prefix: redacted_body_prefix(&body, &self.api_key),
            });
        }

        serde_json::from_str(&body).map_err(JevError::Decode)
    }
}

fn redacted_body_prefix(body: &str, api_key: &str) -> String {
    body.chars()
        .take(ERROR_BODY_PREFIX_CHARS)
        .collect::<String>()
        .replace(api_key, "[redacted]")
}

#[cfg(test)]
mod tests {
    use super::{redacted_body_prefix, HttpJevTransport};

    #[test]
    fn http_transport_debug_and_error_prefix_redact_api_key() {
        let secret = "test-secret-key";
        let transport = HttpJevTransport::with_api_key(secret.to_owned()).expect("client");

        assert!(!format!("{transport:?}").contains(secret));
        assert_eq!(
            redacted_body_prefix("request used test-secret-key", secret),
            "request used [redacted]"
        );
    }
}
