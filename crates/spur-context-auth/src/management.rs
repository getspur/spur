//! Typed client for personal API-key management routes.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use crate::{
    credentials::ApiKeyCredential,
    oauth::{
        refresh_session, secure_http_client, DiscoveryDocument, ManagementSession, OAuthError,
    },
};

const REFRESH_SKEW_SECONDS: u64 = 60;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Bounded management errors that never contain remote bodies or credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManagementError {
    /// A local request or endpoint is invalid.
    #[error("invalid management request")]
    InvalidRequest,
    /// Management authentication or refresh failed.
    #[error("management authentication failed")]
    Authentication,
    /// The service rejected the request or returned malformed data.
    #[error("context-service management request failed")]
    RemoteFailure,
}

impl From<OAuthError> for ManagementError {
    fn from(_: OAuthError) -> Self {
        Self::Authentication
    }
}

/// Typed request for `POST /auth/api-keys`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CreateApiKeyRequest {
    name: String,
    scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<u64>,
}

impl CreateApiKeyRequest {
    /// Validates a bounded name and the exact v1 personal-key scope set.
    pub fn new<I, S>(
        name: impl Into<String>,
        scopes: I,
        expires_at: Option<u64>,
    ) -> Result<Self, ManagementError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let name = name.into();
        let mut scopes = scopes
            .into_iter()
            .map(|scope| scope.as_ref().to_owned())
            .collect::<Vec<_>>();
        scopes.sort();
        scopes.dedup();
        if name.is_empty()
            || name.len() > 64
            || name.trim() != name
            || name.chars().any(char::is_control)
            || scopes.is_empty()
            || scopes.iter().any(|scope| {
                !matches!(
                    scope.as_str(),
                    "external.read" | "external.index" | "external.status"
                )
            })
        {
            return Err(ManagementError::InvalidRequest);
        }
        Ok(Self {
            name,
            scopes,
            expires_at,
        })
    }
}

/// Personal API-key lifecycle state returned by management routes.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyStatus {
    /// Key may authenticate until expiry.
    Active,
    /// Key has been revoked.
    Revoked,
}

impl ApiKeyStatus {
    /// Wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

/// Secret-free personal key metadata.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyMetadata {
    key_id: String,
    name: String,
    scopes: Vec<String>,
    status: ApiKeyStatus,
    created_at: u64,
    expires_at: u64,
    revoked_at: Option<u64>,
}

impl ApiKeyMetadata {
    /// Public key identifier.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }
    /// Lifecycle state.
    #[must_use]
    pub const fn status(&self) -> ApiKeyStatus {
        self.status
    }
}

/// One-time creation response. Debug output redacts the full key.
pub struct CreatedApiKey {
    credential: ApiKeyCredential,
    key_id: String,
    name: String,
    scopes: Vec<String>,
    created_at: u64,
    expires_at: u64,
}

impl fmt::Debug for CreatedApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreatedApiKey")
            .field("credential", &"[REDACTED]")
            .field("key_id", &self.key_id)
            .field("name", &self.name)
            .field("scopes", &self.scopes)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl CreatedApiKey {
    /// One-time key secret for immediate credential-store persistence.
    #[must_use]
    pub const fn key(&self) -> &SecretString {
        self.credential.secret()
    }
    /// Public key identifier.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Consumes the one-time response into a purpose-safe stored credential.
    #[must_use]
    pub fn into_credential(self) -> ApiKeyCredential {
        self.credential
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateResponse {
    key: String,
    key_id: String,
    name: String,
    scopes: Vec<String>,
    created_at: u64,
    expires_at: u64,
}

/// One page from `GET /auth/api-keys`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiKeyPage {
    keys: Vec<ApiKeyMetadata>,
    next_cursor: Option<String>,
}

impl ApiKeyPage {
    /// Metadata in this page.
    #[must_use]
    pub fn keys(&self) -> &[ApiKeyMetadata] {
        &self.keys
    }
    /// Opaque cursor for the next page.
    #[must_use]
    pub fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }
}

/// Idempotent revoke response.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevokedApiKey {
    key_id: String,
    status: ApiKeyStatus,
}

impl RevokedApiKey {
    /// Revoked status.
    #[must_use]
    pub const fn status(&self) -> ApiKeyStatus {
        self.status
    }
}

/// Refresh-aware client for exact personal-key management endpoints.
pub struct ManagementClient {
    discovery: DiscoveryDocument,
    endpoint: Url,
    http: reqwest::Client,
    session: Mutex<ManagementSession>,
}

impl fmt::Debug for ManagementClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagementClient")
            .field("discovery", &self.discovery)
            .field("endpoint", &self.endpoint)
            .field("http", &self.http)
            .field("session", &"[REDACTED]")
            .finish()
    }
}

impl ManagementClient {
    /// Creates a client from validated discovery and stored management credentials.
    pub fn new(
        discovery: DiscoveryDocument,
        session: ManagementSession,
    ) -> Result<Self, ManagementError> {
        if session.issuer() != discovery.issuer()
            || session.client_id() != discovery.human_client_id()
        {
            return Err(ManagementError::Authentication);
        }
        let endpoint = discovery.management_url().clone();
        Ok(Self {
            discovery,
            endpoint,
            http: secure_http_client()?,
            session: Mutex::new(session),
        })
    }

    /// Returns a redacted snapshot suitable for credential-store persistence.
    pub async fn session(&self) -> ManagementSession {
        self.session.lock().await.clone()
    }

    /// Creates a personal key and returns its plaintext exactly once.
    pub async fn create_key(
        &self,
        request: CreateApiKeyRequest,
    ) -> Result<CreatedApiKey, ManagementError> {
        let token = self.fresh_access_token().await?;
        let response = self
            .http
            .post(self.endpoint.clone())
            .bearer_auth(token.expose_secret())
            .json(&request)
            .send()
            .await
            .map_err(|_error| ManagementError::RemoteFailure)?;
        let wire: CreateResponse = decode_success(response, reqwest::StatusCode::CREATED).await?;
        let parsed = ApiKeyCredential::parse_stdin(&wire.key)
            .map_err(|_error| ManagementError::RemoteFailure)?;
        if parsed.public_id() != wire.key_id {
            return Err(ManagementError::RemoteFailure);
        }
        Ok(CreatedApiKey {
            credential: parsed,
            key_id: wire.key_id,
            name: wire.name,
            scopes: wire.scopes,
            created_at: wire.created_at,
            expires_at: wire.expires_at,
        })
    }

    /// Lists secret-free key metadata with bounded optional pagination.
    pub async fn list_keys(
        &self,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<ApiKeyPage, ManagementError> {
        if cursor.is_some_and(|value| {
            value.is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
        }) || limit.is_some_and(|value| !(1..=100).contains(&value))
        {
            return Err(ManagementError::InvalidRequest);
        }
        let token = self.fresh_access_token().await?;
        let mut request = self
            .http
            .get(self.endpoint.clone())
            .bearer_auth(token.expose_secret());
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        if let Some(limit) = limit {
            request = request.query(&[("limit", limit)]);
        }
        decode_success(
            request
                .send()
                .await
                .map_err(|_error| ManagementError::RemoteFailure)?,
            reqwest::StatusCode::OK,
        )
        .await
    }

    /// Idempotently revokes one public key ID.
    pub async fn revoke_key(&self, key_id: &str) -> Result<RevokedApiKey, ManagementError> {
        if !valid_public_id(key_id) {
            return Err(ManagementError::InvalidRequest);
        }
        let token = self.fresh_access_token().await?;
        let endpoint = self.endpoint.join(&format!("api-keys/{key_id}"));
        let endpoint = endpoint.map_err(|_error| ManagementError::InvalidRequest)?;
        let revoked: RevokedApiKey = decode_success(
            self.http
                .delete(endpoint)
                .bearer_auth(token.expose_secret())
                .send()
                .await
                .map_err(|_error| ManagementError::RemoteFailure)?,
            reqwest::StatusCode::OK,
        )
        .await?;
        if revoked.key_id != key_id || revoked.status != ApiKeyStatus::Revoked {
            return Err(ManagementError::RemoteFailure);
        }
        Ok(revoked)
    }

    async fn fresh_access_token(&self) -> Result<SecretString, ManagementError> {
        let mut session = self.session.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_error| ManagementError::Authentication)?
            .as_secs();
        if session.needs_refresh(now, REFRESH_SKEW_SECONDS) {
            *session = refresh_session(&self.discovery, &self.http, &session).await?;
        }
        Ok(session.access_token().clone())
    }
}

fn valid_public_id(value: &str) -> bool {
    value.len() == 26
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
}

async fn decode_success<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    expected: reqwest::StatusCode,
) -> Result<T, ManagementError> {
    if response.status() != expected {
        return Err(ManagementError::RemoteFailure);
    }
    let body = read_bounded(response).await?;
    serde_json::from_slice(&body).map_err(|_error| ManagementError::RemoteFailure)
}

async fn read_bounded(mut response: reqwest::Response) -> Result<Vec<u8>, ManagementError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_error| ManagementError::RemoteFailure)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(ManagementError::RemoteFailure);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
