//! OAuth authorization-code, OIDC validation, and management-token refresh.

use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use oauth2::{
    basic::{BasicClient, BasicTokenResponse},
    AuthType, ClientId, RefreshToken as OAuthRefreshToken, TokenResponse as _, TokenUrl,
};
use openidconnect::{
    core::{CoreAuthenticationFlow, CoreClient, CoreJsonWebKeySet, CoreProviderMetadata},
    AccessTokenHash, AuthUrl, AuthorizationCode, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse as _,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use subtle::ConstantTimeEq as _;
use url::Url;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// Exact loopback callback registered for the public Cognito CLI client.
pub const HUMAN_CALLBACK_URL: &str = "http://127.0.0.1:8765/callback";

/// Bounded OAuth errors that never embed provider responses or credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    /// A configured URL, client identifier, or scope is invalid.
    #[error("invalid OAuth configuration")]
    InvalidConfiguration,
    /// The context-service discovery document is invalid or unavailable.
    #[error("invalid context-service discovery")]
    InvalidDiscovery,
    /// The callback did not exactly match the configured loopback redirect.
    #[error("authorization callback was rejected")]
    CallbackRejected,
    /// The returned state did not match this authorization attempt.
    #[error("authorization state was rejected")]
    StateRejected,
    /// This one-shot authorization attempt has already been consumed.
    #[error("authorization attempt was already used")]
    AuthorizationAlreadyUsed,
    /// OIDC provider discovery failed or disagreed with service discovery.
    #[error("OIDC discovery failed")]
    OidcDiscoveryFailed,
    /// The authorization-code or refresh-token exchange failed.
    #[error("OAuth token request failed")]
    TokenRequestFailed,
    /// The token response was missing required bounded fields.
    #[error("OAuth token response was invalid")]
    TokenResponseInvalid,
    /// The ID token failed issuer, audience, signature, nonce, or hash checks.
    #[error("OIDC token verification failed")]
    OidcVerificationFailed,
}

/// A secrecy-backed value whose formatting is always redacted.
#[derive(Clone)]
pub struct RedactedSecret(SecretString);

impl RedactedSecret {
    /// Creates a non-empty secret.
    pub fn new(value: impl Into<String>) -> Result<Self, OAuthError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(OAuthError::TokenResponseInvalid);
        }
        Ok(Self(SecretString::from(value)))
    }

    /// Borrows the underlying secret for an explicit protocol boundary.
    #[must_use]
    pub const fn secret(&self) -> &SecretString {
        &self.0
    }
}

impl fmt::Debug for RedactedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl PartialEq for RedactedSecret {
    fn eq(&self, other: &Self) -> bool {
        let left = self.0.expose_secret().as_bytes();
        let right = other.0.expose_secret().as_bytes();
        left.len() == right.len() && bool::from(left.ct_eq(right))
    }
}

impl Eq for RedactedSecret {}

/// Validated public context-service discovery.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryDocument {
    schema_version: u8,
    issuer: Url,
    human_client_id: String,
    human_callback_url: Url,
    authorization_endpoint: Url,
    token_endpoint: Url,
    supported_scopes: Vec<String>,
    api_key_auth_enabled: bool,
    api_key_mcp_url: Url,
    api_key_management_url: Url,
}

impl DiscoveryDocument {
    /// Parses and validates a production discovery document for `service_base`.
    pub fn from_json_for_service(json: &str, service_base: &Url) -> Result<Self, OAuthError> {
        let document: Self =
            serde_json::from_str(json).map_err(|_error| OAuthError::InvalidDiscovery)?;
        document.validate(service_base, false)?;
        Ok(document)
    }

    /// Fetches exact discovery with redirect-disabled, proxy-free bounded HTTP.
    pub async fn fetch(service_base: &Url) -> Result<Self, OAuthError> {
        validate_service_base(service_base, false)?;
        let url = service_base
            .join("/.well-known/spur-context-service")
            .map_err(|_error| OAuthError::InvalidDiscovery)?;
        let response = secure_http_client()?
            .get(url)
            .send()
            .await
            .map_err(|_error| OAuthError::InvalidDiscovery)?;
        if !response.status().is_success() {
            return Err(OAuthError::InvalidDiscovery);
        }
        let bytes = read_bounded(response, MAX_RESPONSE_BYTES).await?;
        let json = std::str::from_utf8(&bytes).map_err(|_error| OAuthError::InvalidDiscovery)?;
        Self::from_json_for_service(json, service_base)
    }

    /// Creates loopback-only discovery for offline tests.
    #[doc(hidden)]
    pub fn for_test(
        service_base: impl AsRef<str>,
        issuer: impl AsRef<str>,
        authorization_endpoint: impl AsRef<str>,
        token_endpoint: impl AsRef<str>,
        human_client_id: impl Into<String>,
    ) -> Result<Self, OAuthError> {
        let service_base =
            Url::parse(service_base.as_ref()).map_err(|_error| OAuthError::InvalidDiscovery)?;
        let mut document = Self {
            schema_version: 1,
            issuer: Url::parse(issuer.as_ref()).map_err(|_error| OAuthError::InvalidDiscovery)?,
            human_client_id: human_client_id.into(),
            human_callback_url: Url::parse(HUMAN_CALLBACK_URL)
                .map_err(|_error| OAuthError::InvalidDiscovery)?,
            authorization_endpoint: Url::parse(authorization_endpoint.as_ref())
                .map_err(|_error| OAuthError::InvalidDiscovery)?,
            token_endpoint: Url::parse(token_endpoint.as_ref())
                .map_err(|_error| OAuthError::InvalidDiscovery)?,
            supported_scopes: vec!["urn:spur:context-service/keys.manage".to_owned()],
            api_key_auth_enabled: true,
            api_key_mcp_url: service_base
                .join("/mcp/api-key")
                .map_err(|_error| OAuthError::InvalidDiscovery)?,
            api_key_management_url: service_base
                .join("/auth/api-keys")
                .map_err(|_error| OAuthError::InvalidDiscovery)?,
        };
        document.validate(&service_base, true)?;
        document.api_key_auth_enabled = true;
        Ok(document)
    }

    fn validate(&self, service_base: &Url, allow_loopback_http: bool) -> Result<(), OAuthError> {
        validate_service_base(service_base, allow_loopback_http)?;
        if self.schema_version != 1
            || self.human_client_id.trim().is_empty()
            || self.human_client_id.len() > 256
            || self.human_client_id.chars().any(char::is_control)
            || self.human_callback_url.as_str() != HUMAN_CALLBACK_URL
            || !fixed_endpoint(&self.issuer, allow_loopback_http)
            || !fixed_endpoint(&self.authorization_endpoint, allow_loopback_http)
            || !fixed_endpoint(&self.token_endpoint, allow_loopback_http)
            || !same_origin(service_base, &self.api_key_mcp_url)
            || !same_origin(service_base, &self.api_key_management_url)
            || self.api_key_mcp_url.path() != "/mcp/api-key"
            || self.api_key_management_url.path() != "/auth/api-keys"
            || !fixed_endpoint(&self.api_key_mcp_url, allow_loopback_http)
            || !fixed_endpoint(&self.api_key_management_url, allow_loopback_http)
            || !self
                .supported_scopes
                .iter()
                .any(|scope| scope == "urn:spur:context-service/keys.manage")
            || self
                .supported_scopes
                .iter()
                .any(|scope| !valid_scope(scope))
        {
            return Err(OAuthError::InvalidDiscovery);
        }
        Ok(())
    }

    /// Public OAuth client ID.
    #[must_use]
    pub fn human_client_id(&self) -> &str {
        &self.human_client_id
    }
    /// Exact registered loopback callback for the public CLI client.
    #[must_use]
    pub const fn human_callback_url(&self) -> &Url {
        &self.human_callback_url
    }
    /// Whether personal API-key management and MCP authentication are enabled.
    ///
    /// Callers must check this feature status before issuing management or
    /// API-key MCP requests so a disabled service fails closed.
    #[must_use]
    pub const fn api_key_auth_enabled(&self) -> bool {
        self.api_key_auth_enabled
    }
    /// Validated issuer.
    #[must_use]
    pub const fn issuer(&self) -> &Url {
        &self.issuer
    }
    /// Validated authorization endpoint.
    #[must_use]
    pub const fn authorization_endpoint(&self) -> &Url {
        &self.authorization_endpoint
    }
    /// Validated token endpoint.
    #[must_use]
    pub const fn token_endpoint(&self) -> &Url {
        &self.token_endpoint
    }
    /// Exact management collection URL.
    #[must_use]
    pub const fn management_url(&self) -> &Url {
        &self.api_key_management_url
    }
}

fn validate_service_base(url: &Url, allow_loopback_http: bool) -> Result<(), OAuthError> {
    if !fixed_endpoint(url, allow_loopback_http) || !matches!(url.path(), "" | "/") {
        return Err(OAuthError::InvalidDiscovery);
    }
    Ok(())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.origin() == right.origin()
}

fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 256
        && !scope.chars().any(char::is_control)
        && scope.split_whitespace().count() == 1
}

fn fixed_endpoint(url: &Url, allow_loopback_http: bool) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && (url.scheme() == "https" || (allow_loopback_http && loopback_http(url)))
}

fn loopback_http(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "::1"))
}

/// Fixed configuration for a public native OIDC client.
#[derive(Clone, Debug)]
pub struct HumanConfig {
    issuer: Url,
    authorization_endpoint: Url,
    token_endpoint: Url,
    client_id: String,
    redirect_uri: Url,
}

impl HumanConfig {
    /// Builds production configuration from validated service discovery.
    pub fn from_discovery(discovery: &DiscoveryDocument) -> Result<Self, OAuthError> {
        Self::build(
            discovery.issuer.clone(),
            discovery.authorization_endpoint.clone(),
            discovery.token_endpoint.clone(),
            discovery.human_client_id.clone(),
            discovery.human_callback_url.clone(),
            false,
        )
    }

    /// Builds loopback-only configuration for offline mock tests.
    #[doc(hidden)]
    pub fn for_test(
        issuer: impl AsRef<str>,
        authorization_endpoint: impl AsRef<str>,
        token_endpoint: impl AsRef<str>,
        client_id: impl Into<String>,
        redirect_uri: impl AsRef<str>,
    ) -> Result<Self, OAuthError> {
        Self::build(
            Url::parse(issuer.as_ref()).map_err(|_error| OAuthError::InvalidConfiguration)?,
            Url::parse(authorization_endpoint.as_ref())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
            Url::parse(token_endpoint.as_ref())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
            client_id.into(),
            Url::parse(redirect_uri.as_ref()).map_err(|_error| OAuthError::InvalidConfiguration)?,
            true,
        )
    }

    fn build(
        issuer: Url,
        authorization_endpoint: Url,
        token_endpoint: Url,
        client_id: String,
        redirect_uri: Url,
        allow_loopback_http: bool,
    ) -> Result<Self, OAuthError> {
        if client_id.trim().is_empty()
            || !fixed_endpoint(&issuer, allow_loopback_http)
            || !fixed_endpoint(&authorization_endpoint, allow_loopback_http)
            || !fixed_endpoint(&token_endpoint, allow_loopback_http)
            || !exact_loopback_redirect(&redirect_uri)
        {
            return Err(OAuthError::InvalidConfiguration);
        }
        Ok(Self {
            issuer,
            authorization_endpoint,
            token_endpoint,
            client_id,
            redirect_uri,
        })
    }
}

fn exact_loopback_redirect(url: &Url) -> bool {
    loopback_http(url)
        && url.port().is_some()
        && url.path() == "/callback"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

/// OIDC client that creates one-shot S256 authorization attempts.
#[derive(Clone)]
pub struct HumanClient {
    config: HumanConfig,
    http: reqwest::Client,
    provider_metadata: Option<CoreProviderMetadata>,
}

impl HumanClient {
    /// Creates a production OIDC client.
    pub fn new(config: HumanConfig) -> Result<Self, OAuthError> {
        Ok(Self {
            config,
            http: secure_http_client()?,
            provider_metadata: None,
        })
    }

    /// Injects locally generated provider metadata for signature tests.
    #[doc(hidden)]
    pub fn with_provider_metadata_for_test(
        config: HumanConfig,
        provider_metadata: CoreProviderMetadata,
    ) -> Result<Self, OAuthError> {
        Ok(Self {
            config,
            http: secure_http_client()?,
            provider_metadata: Some(provider_metadata),
        })
    }

    /// Starts a fresh authorization-code flow with S256 PKCE, state, and nonce.
    pub fn begin_authorization<I, S>(&self, scopes: I) -> Result<PendingAuthorization, OAuthError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let scopes = normalize_scopes(scopes)?;
        let client = CoreClient::new(
            ClientId::new(self.config.client_id.clone()),
            IssuerUrl::new(self.config.issuer.to_string())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
            CoreJsonWebKeySet::new(Vec::new()),
        )
        .set_auth_uri(
            AuthUrl::new(self.config.authorization_endpoint.to_string())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
        )
        .set_token_uri(
            TokenUrl::new(self.config.token_endpoint.to_string())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
        )
        .set_redirect_uri(
            RedirectUrl::new(self.config.redirect_uri.to_string())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
        );
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (authorization_url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scopes(scopes.into_iter().map(Scope::new))
            .set_pkce_challenge(challenge)
            .url();
        Ok(PendingAuthorization {
            authorization_url,
            redirect_uri: self.config.redirect_uri.clone(),
            state,
            nonce,
            verifier: Some(verifier),
            used: false,
        })
    }

    /// Exchanges a callback and strictly verifies the returned ID token.
    pub async fn finish_authorization(
        &self,
        pending: &mut PendingAuthorization,
        callback: AuthorizationCallback,
    ) -> Result<ManagementSession, OAuthError> {
        if !pending.used {
            return Err(OAuthError::CallbackRejected);
        }
        let verifier = pending
            .verifier
            .take()
            .ok_or(OAuthError::AuthorizationAlreadyUsed)?;
        let metadata = match &self.provider_metadata {
            Some(metadata) => metadata.clone(),
            None => CoreProviderMetadata::discover_async(
                IssuerUrl::new(self.config.issuer.to_string())
                    .map_err(|_error| OAuthError::InvalidConfiguration)?,
                &self.http,
            )
            .await
            .map_err(|_error| OAuthError::OidcDiscoveryFailed)?,
        };
        if !same_url(&self.config.issuer, metadata.issuer().as_str())
            || !same_url(
                &self.config.authorization_endpoint,
                metadata.authorization_endpoint().as_str(),
            )
            || metadata
                .token_endpoint()
                .is_none_or(|url| !same_url(&self.config.token_endpoint, url.as_str()))
        {
            return Err(OAuthError::OidcDiscoveryFailed);
        }
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.config.client_id.clone()),
            None,
        )
        .set_redirect_uri(
            RedirectUrl::new(self.config.redirect_uri.to_string())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
        );
        let response = client
            .exchange_code(AuthorizationCode::new(
                callback.code.0.expose_secret().to_owned(),
            ))
            .map_err(|_error| OAuthError::InvalidConfiguration)?
            .set_pkce_verifier(verifier)
            .request_async(&self.http)
            .await
            .map_err(|_error| OAuthError::TokenRequestFailed)?;
        let id_token = response
            .id_token()
            .ok_or(OAuthError::OidcVerificationFailed)?;
        let verifier = client.id_token_verifier();
        let claims = id_token
            .claims(&verifier, &pending.nonce)
            .map_err(|_error| OAuthError::OidcVerificationFailed)?;
        let expected = claims
            .access_token_hash()
            .ok_or(OAuthError::OidcVerificationFailed)?;
        let actual = AccessTokenHash::from_token(
            response.access_token(),
            id_token
                .signing_alg()
                .map_err(|_error| OAuthError::OidcVerificationFailed)?,
            id_token
                .signing_key(&verifier)
                .map_err(|_error| OAuthError::OidcVerificationFailed)?,
        )
        .map_err(|_error| OAuthError::OidcVerificationFailed)?;
        if actual != *expected {
            return Err(OAuthError::OidcVerificationFailed);
        }
        let lifetime = response
            .expires_in()
            .ok_or(OAuthError::TokenResponseInvalid)?;
        let refresh = response
            .refresh_token()
            .ok_or(OAuthError::TokenResponseInvalid)?;
        ManagementSession::from_lifetime(
            response.access_token().secret(),
            refresh.secret(),
            lifetime,
            &self.config.issuer,
            &self.config.client_id,
        )
    }
}

fn same_url(configured: &Url, received: &str) -> bool {
    Url::parse(received).is_ok_and(|received| received == *configured)
}

fn normalize_scopes<I, S>(scopes: I) -> Result<Vec<String>, OAuthError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut scopes = scopes
        .into_iter()
        .map(|value| value.as_ref().trim().to_owned())
        .collect::<Vec<_>>();
    scopes.sort();
    scopes.dedup();
    if scopes.is_empty() || scopes.iter().any(|scope| !valid_scope(scope)) {
        return Err(OAuthError::InvalidConfiguration);
    }
    Ok(scopes)
}

/// A parsed callback code bound to a successfully checked state value.
pub struct AuthorizationCallback {
    code: RedactedSecret,
}

impl fmt::Debug for AuthorizationCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthorizationCallback([REDACTED])")
    }
}

/// Non-cloneable state for one authorization attempt.
pub struct PendingAuthorization {
    authorization_url: Url,
    redirect_uri: Url,
    state: CsrfToken,
    nonce: Nonce,
    verifier: Option<PkceCodeVerifier>,
    used: bool,
}

impl PendingAuthorization {
    /// Browser URL for this attempt.
    #[must_use]
    pub const fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    /// Parses only the exact configured loopback callback and consumes state once.
    pub fn parse_callback(&mut self, callback: &Url) -> Result<AuthorizationCallback, OAuthError> {
        if self.used {
            return Err(OAuthError::AuthorizationAlreadyUsed);
        }
        if callback.scheme() != self.redirect_uri.scheme()
            || callback.host_str() != self.redirect_uri.host_str()
            || callback.port() != self.redirect_uri.port()
            || callback.path() != self.redirect_uri.path()
            || callback.fragment().is_some()
            || !callback.username().is_empty()
            || callback.password().is_some()
        {
            return Err(OAuthError::CallbackRejected);
        }
        let mut code = None;
        let mut state = None;
        for (name, value) in callback.query_pairs() {
            match name.as_ref() {
                "code" if code.is_none() => code = Some(value.into_owned()),
                "state" if state.is_none() => state = Some(value.into_owned()),
                _ => return Err(OAuthError::CallbackRejected),
            }
        }
        let code = code
            .filter(|value| !value.is_empty())
            .ok_or(OAuthError::CallbackRejected)?;
        let state = state.ok_or(OAuthError::CallbackRejected)?;
        if self.state.secret().len() != state.len()
            || !bool::from(self.state.secret().as_bytes().ct_eq(state.as_bytes()))
        {
            return Err(OAuthError::StateRejected);
        }
        self.used = true;
        Ok(AuthorizationCallback {
            code: RedactedSecret::new(code)?,
        })
    }
}

/// OAuth credentials used only for context-service management calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagementSession {
    access_token: RedactedSecret,
    refresh_token: RedactedSecret,
    expires_at: u64,
    issuer: Url,
    client_id: String,
}

impl ManagementSession {
    /// Creates a session loaded from a credential store.
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_at: u64,
        issuer: impl AsRef<str>,
        client_id: impl Into<String>,
    ) -> Result<Self, OAuthError> {
        Self::build(
            access_token,
            refresh_token,
            expires_at,
            issuer,
            client_id,
            false,
        )
    }

    /// Creates a loopback-bound session for offline tests.
    #[doc(hidden)]
    pub fn for_test(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_at: u64,
        issuer: impl AsRef<str>,
        client_id: impl Into<String>,
    ) -> Result<Self, OAuthError> {
        Self::build(
            access_token,
            refresh_token,
            expires_at,
            issuer,
            client_id,
            true,
        )
    }

    fn build(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_at: u64,
        issuer: impl AsRef<str>,
        client_id: impl Into<String>,
        allow_loopback_http: bool,
    ) -> Result<Self, OAuthError> {
        let issuer =
            Url::parse(issuer.as_ref()).map_err(|_error| OAuthError::InvalidConfiguration)?;
        let client_id = client_id.into();
        if !fixed_endpoint(&issuer, allow_loopback_http)
            || client_id.trim().is_empty()
            || client_id.len() > 256
            || client_id.chars().any(char::is_control)
        {
            return Err(OAuthError::InvalidConfiguration);
        }
        Ok(Self {
            access_token: RedactedSecret::new(access_token)?,
            refresh_token: RedactedSecret::new(refresh_token)?,
            expires_at,
            issuer,
            client_id,
        })
    }

    fn from_lifetime(
        access_token: &str,
        refresh_token: &str,
        lifetime: Duration,
        issuer: &Url,
        client_id: &str,
    ) -> Result<Self, OAuthError> {
        if lifetime.is_zero() || lifetime > MAX_TOKEN_LIFETIME {
            return Err(OAuthError::TokenResponseInvalid);
        }
        let expires_at = unix_now()?
            .checked_add(lifetime.as_secs())
            .ok_or(OAuthError::TokenResponseInvalid)?;
        Ok(Self {
            access_token: RedactedSecret::new(access_token)?,
            refresh_token: RedactedSecret::new(refresh_token)?,
            expires_at,
            issuer: issuer.clone(),
            client_id: client_id.to_owned(),
        })
    }

    /// Redacted access token.
    #[must_use]
    pub const fn access_token(&self) -> &SecretString {
        self.access_token.secret()
    }
    /// Redacted refresh token.
    #[must_use]
    pub const fn refresh_token(&self) -> &SecretString {
        self.refresh_token.secret()
    }
    /// Unix expiry for the access token.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Issuer to which the refresh token is cryptographically bound.
    #[must_use]
    pub const fn issuer(&self) -> &Url {
        &self.issuer
    }

    /// Public client ID to which the refresh token is bound.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub(crate) fn needs_refresh(&self, now: u64, skew: u64) -> bool {
        self.expires_at <= now.saturating_add(skew)
    }
}

pub(crate) async fn refresh_session(
    discovery: &DiscoveryDocument,
    http: &reqwest::Client,
    session: &ManagementSession,
) -> Result<ManagementSession, OAuthError> {
    if session.issuer() != discovery.issuer() || session.client_id() != discovery.human_client_id()
    {
        return Err(OAuthError::InvalidConfiguration);
    }
    let client = BasicClient::new(ClientId::new(discovery.human_client_id.clone()))
        .set_token_uri(
            TokenUrl::new(discovery.token_endpoint.to_string())
                .map_err(|_error| OAuthError::InvalidConfiguration)?,
        )
        .set_auth_type(AuthType::RequestBody);
    let token = OAuthRefreshToken::new(session.refresh_token().expose_secret().to_owned());
    let response: BasicTokenResponse = client
        .exchange_refresh_token(&token)
        .request_async(http)
        .await
        .map_err(|_error| OAuthError::TokenRequestFailed)?;
    let lifetime = response
        .expires_in()
        .ok_or(OAuthError::TokenResponseInvalid)?;
    let refresh = response.refresh_token().map_or_else(
        || session.refresh_token().expose_secret(),
        |value| value.secret(),
    );
    ManagementSession::from_lifetime(
        response.access_token().secret(),
        refresh,
        lifetime,
        discovery.issuer(),
        discovery.human_client_id(),
    )
}

fn unix_now() -> Result<u64, OAuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_error| OAuthError::TokenResponseInvalid)
}

/// Builds a reusable Rustls client with redirects and environment proxies disabled.
pub fn secure_http_client() -> Result<reqwest::Client, OAuthError> {
    build_http_client(REQUEST_TIMEOUT)
}

/// Test helper for proving request timeout behavior without waiting ten seconds.
#[doc(hidden)]
pub fn secure_http_client_for_test(timeout: Duration) -> Result<reqwest::Client, OAuthError> {
    build_http_client(timeout)
}

fn build_http_client(timeout: Duration) -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .timeout(timeout)
        .no_proxy()
        .build()
        .map_err(|_error| OAuthError::InvalidConfiguration)
}

async fn read_bounded(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, OAuthError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_error| OAuthError::InvalidDiscovery)?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(OAuthError::InvalidDiscovery);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
