//! Standalone Cognito OAuth/OIDC proof-of-concept client.
//!
//! This crate is intentionally isolated from the production Lambda. It owns
//! outbound OAuth/OIDC requests only; the Lambda continues to rely on API
//! Gateway JWT verification and adds no OAuth/OIDC dependencies.

use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use oauth2::{
    basic::{BasicClient, BasicTokenResponse},
    AuthType, ClientId, ClientSecret, Scope, TokenResponse, TokenUrl,
};
use openidconnect::{
    core::CoreProviderMetadata,
    core::{CoreAuthenticationFlow, CoreClient, CoreJsonWebKeySet},
    AccessTokenHash, AuthUrl as OidcAuthUrl, AuthorizationCode as OidcAuthorizationCode, CsrfToken,
    IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope as OidcScope,
    TokenResponse as OidcTokenResponse,
};
use rand::{distributions::Alphanumeric, Rng};
use subtle::ConstantTimeEq;
use tokio::sync::{oneshot, Mutex};
use url::Url;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// A value that is safe to carry in configuration but is never rendered.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wrap a secret obtained from an approved source such as an environment
    /// variable or a secret manager.
    pub fn new(value: String) -> Result<Self, ClientError> {
        if value.trim().is_empty() {
            return Err(ClientError::InvalidConfiguration);
        }
        Ok(Self(value))
    }

    /// Generates an ephemeral value suitable only for tests.
    pub fn random() -> Self {
        Self(
            rand::thread_rng()
                .sample_iter(&Alphanumeric)
                .take(48)
                .map(char::from)
                .collect(),
        )
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Bounded errors deliberately omit OAuth library errors and response bodies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("invalid client configuration")]
    InvalidConfiguration,
    #[error("token request failed")]
    TokenRequestFailed,
    #[error("token response was invalid")]
    TokenResponseInvalid,
    #[error("authorization state was rejected")]
    StateRejected,
    #[error("authorization attempt was already used")]
    AuthorizationAlreadyUsed,
    #[error("OIDC discovery failed")]
    OidcDiscoveryFailed,
    #[error("OIDC token verification failed")]
    OidcVerificationFailed,
    #[error("OIDC token response did not contain an ID token")]
    MissingIdToken,
}

/// An access token kept opaque so callers do not accidentally log it through
/// a `Debug` implementation.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// Validated M2M configuration. Scopes are normalized once at the boundary.
#[derive(Clone, Debug)]
pub struct M2mConfig {
    client_id: String,
    client_secret: SecretString,
    token_endpoint: Url,
    scopes: BTreeSet<String>,
}

impl M2mConfig {
    /// Reads the confidential M2M client configuration from the process
    /// environment. Secrets are deliberately not accepted by CLI arguments.
    pub fn from_environment() -> Result<Self, ClientError> {
        Self::from_environment_with(|name| std::env::var(name).ok())
    }

    /// Injectable environment reader for tests and approved secret adapters.
    pub fn from_environment_with<F>(mut read: F) -> Result<Self, ClientError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let client_id = read("SPUR_AUTH_CLIENT_ID").ok_or(ClientError::InvalidConfiguration)?;
        let client_secret = read("SPUR_AUTH_CLIENT_SECRET")
            .ok_or(ClientError::InvalidConfiguration)
            .and_then(SecretString::new)?;
        let token_endpoint =
            read("SPUR_AUTH_TOKEN_ENDPOINT").ok_or(ClientError::InvalidConfiguration)?;
        let scopes = read("SPUR_AUTH_SCOPES")
            .ok_or(ClientError::InvalidConfiguration)?
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        Self::new(client_id, client_secret, token_endpoint, scopes)
    }

    pub fn new<I, S>(
        client_id: impl Into<String>,
        client_secret: SecretString,
        token_endpoint: impl AsRef<str>,
        scopes: I,
    ) -> Result<Self, ClientError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build(client_id, client_secret, token_endpoint, scopes, false)
    }

    /// Permits an HTTP loopback endpoint solely for local mock-server tests.
    pub fn for_test<I, S>(
        client_id: impl Into<String>,
        client_secret: SecretString,
        token_endpoint: impl AsRef<str>,
        scopes: I,
    ) -> Result<Self, ClientError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self::build(client_id, client_secret, token_endpoint, scopes, true)
    }

    fn build<I, S>(
        client_id: impl Into<String>,
        client_secret: SecretString,
        token_endpoint: impl AsRef<str>,
        scopes: I,
        allow_loopback_http: bool,
    ) -> Result<Self, ClientError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let client_id = client_id.into();
        let token_endpoint = Url::parse(token_endpoint.as_ref())?;
        let scopes = normalize_scopes(scopes)?;
        let is_loopback_http = token_endpoint.scheme() == "http"
            && token_endpoint
                .host_str()
                .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]"));

        if client_id.trim().is_empty()
            || scopes.is_empty()
            || !token_endpoint.username().is_empty()
            || token_endpoint.password().is_some()
            || token_endpoint.query().is_some()
            || token_endpoint.fragment().is_some()
            || (token_endpoint.scheme() != "https" && !(allow_loopback_http && is_loopback_http))
        {
            return Err(ClientError::InvalidConfiguration);
        }

        Ok(Self {
            client_id,
            client_secret,
            token_endpoint,
            scopes,
        })
    }
}

fn normalize_scopes<I, S>(scopes: I) -> Result<BTreeSet<String>, ClientError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let normalized = scopes
        .into_iter()
        .map(|scope| scope.as_ref().trim().to_owned())
        .collect::<BTreeSet<_>>();
    if normalized
        .iter()
        .any(|scope| scope.is_empty() || scope.split_whitespace().count() != 1)
    {
        return Err(ClientError::InvalidConfiguration);
    }
    Ok(normalized)
}

/// Reusable redirect-disabled Rustls HTTP client for all OAuth/OIDC calls.
pub fn secure_http_client() -> Result<reqwest::Client, ClientError> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .no_proxy()
        .build()
        .map_err(|_| ClientError::InvalidConfiguration)
}

/// M2M client-credentials client. The next TDD step adds its shared cache.
#[derive(Clone)]
pub struct M2mClient {
    config: Arc<M2mConfig>,
    http: reqwest::Client,
    cache: Arc<TokenCache>,
}

impl M2mClient {
    pub fn new(config: M2mConfig) -> Result<Self, ClientError> {
        Self::new_with_cache(config, Arc::new(TokenCache::default()))
    }

    /// Allows callers that own multiple M2M client configurations to share the
    /// in-memory cache. Entries remain isolated by client ID and scope set.
    pub fn new_with_cache(config: M2mConfig, cache: Arc<TokenCache>) -> Result<Self, ClientError> {
        Ok(Self {
            config: Arc::new(config),
            http: secure_http_client()?,
            cache,
        })
    }

    pub async fn access_token(&self) -> Result<AccessToken, ClientError> {
        let key = CacheKey {
            client_id: self.config.client_id.clone(),
            scopes: self.config.scopes.clone(),
        };

        loop {
            let waiting_for_leader = {
                let mut state = self.cache.state.lock().await;
                let now = Instant::now();
                if let Some(cached) = state.entries.get(&key) {
                    if now < cached.refresh_at && now < cached.expires_at {
                        return Ok(cached.access_token.clone());
                    }
                }

                if let Some(waiters) = state.in_flight.get_mut(&key) {
                    let (sender, receiver) = oneshot::channel();
                    waiters.push(sender);
                    Some(receiver)
                } else {
                    state.in_flight.insert(key.clone(), Vec::new());
                    None
                }
            };

            if let Some(receiver) = waiting_for_leader {
                let _ = receiver.await;
                continue;
            }

            let result = self
                .request_token()
                .await
                .and_then(|(access_token, expires_in)| {
                    let issued_at = Instant::now();
                    let expires_at = issued_at
                        .checked_add(expires_in)
                        .ok_or(ClientError::TokenResponseInvalid)?;
                    let refresh_at = refresh_at(issued_at, expires_in)
                        .ok_or(ClientError::TokenResponseInvalid)?;
                    Ok((access_token, refresh_at, expires_at))
                });
            let mut state = self.cache.state.lock().await;
            if let Ok((access_token, refresh_at, expires_at)) = &result {
                state.entries.insert(
                    key.clone(),
                    CachedToken {
                        access_token: access_token.clone(),
                        expires_at: *expires_at,
                        refresh_at: *refresh_at,
                    },
                );
            }
            let waiters = state.in_flight.remove(&key).unwrap_or_default();
            drop(state);
            for waiter in waiters {
                let _ = waiter.send(());
            }
            return result.map(|(access_token, _, _)| access_token);
        }
    }

    async fn request_token(&self) -> Result<(AccessToken, Duration), ClientError> {
        let oauth_client = BasicClient::new(ClientId::new(self.config.client_id.clone()))
            .set_client_secret(ClientSecret::new(
                self.config.client_secret.as_str().to_owned(),
            ))
            .set_token_uri(TokenUrl::new(self.config.token_endpoint.to_string())?)
            .set_auth_type(AuthType::BasicAuth);
        let request = oauth_client
            .exchange_client_credentials()
            .add_scopes(self.config.scopes.iter().cloned().map(Scope::new));
        let response: BasicTokenResponse = request
            .request_async(&self.http)
            .await
            .map_err(|_| ClientError::TokenRequestFailed)?;
        let expires_in = response
            .expires_in()
            .ok_or(ClientError::TokenResponseInvalid)?;
        if expires_in.is_zero()
            || expires_in > MAX_TOKEN_LIFETIME
            || response.access_token().secret().is_empty()
        {
            return Err(ClientError::TokenResponseInvalid);
        }
        Ok((
            AccessToken(response.access_token().secret().to_owned()),
            expires_in,
        ))
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    client_id: String,
    scopes: BTreeSet<String>,
}

#[derive(Clone)]
struct CachedToken {
    access_token: AccessToken,
    refresh_at: Instant,
    expires_at: Instant,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<CacheKey, CachedToken>,
    in_flight: HashMap<CacheKey, Vec<oneshot::Sender<()>>>,
}

/// A process-local M2M token cache. It does not persist bearer values.
#[derive(Default)]
pub struct TokenCache {
    state: Mutex<CacheState>,
}

fn refresh_at(issued_at: Instant, lifetime: Duration) -> Option<Instant> {
    let jitter_cap = lifetime.mul_f64(0.05);
    let jitter_millis = u64::try_from(jitter_cap.as_millis()).unwrap_or(u64::MAX);
    let jitter = if jitter_millis == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(rand::thread_rng().gen_range(0..=jitter_millis))
    };
    refresh_at_with_jitter(issued_at, lifetime, jitter)
}

fn refresh_at_with_jitter(
    issued_at: Instant,
    lifetime: Duration,
    jitter: Duration,
) -> Option<Instant> {
    let refresh_after = lifetime.mul_f64(0.80);
    let jitter = jitter.min(lifetime.mul_f64(0.05));
    let expires_at = issued_at.checked_add(lifetime)?;
    let refresh_at = issued_at.checked_add(refresh_after)?.checked_add(jitter)?;
    Some(refresh_at.min(expires_at))
}

impl From<url::ParseError> for ClientError {
    fn from(_: url::ParseError) -> Self {
        Self::InvalidConfiguration
    }
}

impl From<oauth2::ConfigurationError> for ClientError {
    fn from(_: oauth2::ConfigurationError) -> Self {
        Self::InvalidConfiguration
    }
}

/// Fixed, validated configuration for a public OIDC authorization-code client.
#[derive(Clone, Debug)]
pub struct HumanConfig {
    issuer: Url,
    authorization_endpoint: Url,
    token_endpoint: Url,
    client_id: String,
    redirect_uri: Url,
}

impl HumanConfig {
    /// Reads public OIDC discovery and endpoint configuration from the process
    /// environment. No secret is part of the public PKCE client contract.
    pub fn from_environment() -> Result<Self, ClientError> {
        let required = |name| std::env::var(name).map_err(|_| ClientError::InvalidConfiguration);
        Self::new(
            required("SPUR_AUTH_ISSUER")?,
            required("SPUR_AUTH_AUTHORIZATION_ENDPOINT")?,
            required("SPUR_AUTH_TOKEN_ENDPOINT")?,
            required("SPUR_AUTH_HUMAN_CLIENT_ID")?,
            required("SPUR_AUTH_REDIRECT_URI")?,
        )
    }

    pub fn new(
        issuer: impl AsRef<str>,
        authorization_endpoint: impl AsRef<str>,
        token_endpoint: impl AsRef<str>,
        client_id: impl Into<String>,
        redirect_uri: impl AsRef<str>,
    ) -> Result<Self, ClientError> {
        Self::build(
            issuer,
            authorization_endpoint,
            token_endpoint,
            client_id,
            redirect_uri,
            false,
        )
    }

    /// Allows HTTP loopback endpoints solely for local mock-server tests.
    pub fn for_test(
        issuer: impl AsRef<str>,
        authorization_endpoint: impl AsRef<str>,
        token_endpoint: impl AsRef<str>,
        client_id: impl Into<String>,
        redirect_uri: impl AsRef<str>,
    ) -> Result<Self, ClientError> {
        Self::build(
            issuer,
            authorization_endpoint,
            token_endpoint,
            client_id,
            redirect_uri,
            true,
        )
    }

    fn build(
        issuer: impl AsRef<str>,
        authorization_endpoint: impl AsRef<str>,
        token_endpoint: impl AsRef<str>,
        client_id: impl Into<String>,
        redirect_uri: impl AsRef<str>,
        allow_loopback_http: bool,
    ) -> Result<Self, ClientError> {
        let issuer = Url::parse(issuer.as_ref())?;
        let authorization_endpoint = Url::parse(authorization_endpoint.as_ref())?;
        let token_endpoint = Url::parse(token_endpoint.as_ref())?;
        let redirect_uri = Url::parse(redirect_uri.as_ref())?;
        let client_id = client_id.into();

        if client_id.trim().is_empty()
            || !is_fixed_endpoint(&issuer, allow_loopback_http)
            || !is_fixed_endpoint(&authorization_endpoint, allow_loopback_http)
            || !is_fixed_endpoint(&token_endpoint, allow_loopback_http)
            || !is_fixed_endpoint(&redirect_uri, true)
        {
            return Err(ClientError::InvalidConfiguration);
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

fn is_https_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn is_fixed_endpoint(url: &Url, allow_loopback_http: bool) -> bool {
    is_https_url(url)
        || (allow_loopback_http
            && url.scheme() == "http"
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .host_str()
                .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]")))
}

/// Creates PKCE-bound, one-time authorization attempts for a human client.
#[derive(Clone)]
pub struct HumanClient {
    config: Arc<HumanConfig>,
    http: reqwest::Client,
    provider_metadata: Option<CoreProviderMetadata>,
}

impl HumanClient {
    pub fn new(config: HumanConfig) -> Result<Self, ClientError> {
        Ok(Self {
            config: Arc::new(config),
            http: secure_http_client()?,
            provider_metadata: None,
        })
    }

    /// Test-only constructor for a locally generated, already-verified
    /// provider document. Production callers must use [`HumanClient::new`],
    /// which discovers metadata and its JWKS over the configured HTTPS issuer.
    #[doc(hidden)]
    pub fn with_provider_metadata_for_test(
        config: HumanConfig,
        provider_metadata: CoreProviderMetadata,
    ) -> Result<Self, ClientError> {
        Ok(Self {
            config: Arc::new(config),
            http: secure_http_client()?,
            provider_metadata: Some(provider_metadata),
        })
    }

    pub fn begin_authorization<I, S>(
        &self,
        scopes: I,
    ) -> Result<PendingHumanAuthorization, ClientError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let scopes = normalize_scopes(scopes)?;
        let client = CoreClient::new(
            ClientId::new(self.config.client_id.clone()),
            IssuerUrl::new(self.config.issuer.to_string())?,
            CoreJsonWebKeySet::new(Vec::new()),
        )
        .set_auth_uri(OidcAuthUrl::new(
            self.config.authorization_endpoint.to_string(),
        )?)
        .set_token_uri(TokenUrl::new(self.config.token_endpoint.to_string())?)
        .set_redirect_uri(RedirectUrl::new(self.config.redirect_uri.to_string())?);
        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
        let (authorization_url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scopes(scopes.into_iter().map(OidcScope::new))
            .set_pkce_challenge(pkce_challenge)
            .url();

        Ok(PendingHumanAuthorization {
            authorization_url,
            state,
            nonce,
            pkce_verifier: Some(pkce_verifier),
            used: false,
        })
    }

    /// Exchanges a single authorization code and validates the returned ID
    /// token's issuer, audience, signature, nonce, and supplied access-token
    /// hash. Neither the code nor any token is included in an error.
    pub async fn finish_authorization(
        &self,
        pending: &mut PendingHumanAuthorization,
        authorization_code: impl Into<String>,
        returned_state: &str,
    ) -> Result<HumanToken, ClientError> {
        pending.validate_callback_state(returned_state)?;
        let verifier = pending.take_verifier()?;
        let issuer = IssuerUrl::new(self.config.issuer.to_string())?;
        let metadata = match &self.provider_metadata {
            Some(metadata) => metadata.clone(),
            None => CoreProviderMetadata::discover_async(issuer, &self.http)
                .await
                .map_err(|_| ClientError::OidcDiscoveryFailed)?,
        };
        if !same_configured_url(&self.config.issuer, metadata.issuer().as_str())
            || !same_configured_url(
                &self.config.authorization_endpoint,
                metadata.authorization_endpoint().as_str(),
            )
            || !metadata.token_endpoint().is_some_and(|endpoint| {
                same_configured_url(&self.config.token_endpoint, endpoint.as_str())
            })
        {
            return Err(ClientError::OidcDiscoveryFailed);
        }
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.config.client_id.clone()),
            None,
        )
        .set_redirect_uri(RedirectUrl::new(self.config.redirect_uri.to_string())?);
        let response = client
            .exchange_code(OidcAuthorizationCode::new(authorization_code.into()))?
            .set_pkce_verifier(verifier)
            .request_async(&self.http)
            .await
            .map_err(|_| ClientError::TokenRequestFailed)?;
        let id_token = response.id_token().ok_or(ClientError::MissingIdToken)?;
        let id_token_verifier = client.id_token_verifier();
        let claims = id_token
            .claims(&id_token_verifier, pending.nonce())
            .map_err(|_| ClientError::OidcVerificationFailed)?;
        if let Some(expected_access_token_hash) = claims.access_token_hash() {
            let actual_access_token_hash = AccessTokenHash::from_token(
                response.access_token(),
                id_token
                    .signing_alg()
                    .map_err(|_| ClientError::OidcVerificationFailed)?,
                id_token
                    .signing_key(&id_token_verifier)
                    .map_err(|_| ClientError::OidcVerificationFailed)?,
            )
            .map_err(|_| ClientError::OidcVerificationFailed)?;
            if actual_access_token_hash != *expected_access_token_hash {
                return Err(ClientError::OidcVerificationFailed);
            }
        }

        Ok(HumanToken {
            access_token: AccessToken(response.access_token().secret().to_owned()),
        })
    }
}

fn same_configured_url(configured: &Url, received: &str) -> bool {
    Url::parse(received).is_ok_and(|received| received == *configured)
}

/// A validated human access token. The token stays opaque and redacted in
/// debug output just like M2M tokens.
#[derive(Clone, Debug)]
pub struct HumanToken {
    access_token: AccessToken,
}

impl HumanToken {
    pub fn access_token(&self) -> &AccessToken {
        &self.access_token
    }
}

/// A non-cloneable authorization attempt. Its verifier, state, and nonce are
/// retained only until one code exchange.
pub struct PendingHumanAuthorization {
    authorization_url: Url,
    state: CsrfToken,
    nonce: Nonce,
    pkce_verifier: Option<PkceCodeVerifier>,
    used: bool,
}

impl PendingHumanAuthorization {
    pub fn authorization_url(&self) -> &Url {
        &self.authorization_url
    }

    pub fn validate_callback_state(&mut self, returned_state: &str) -> Result<(), ClientError> {
        if self.used {
            return Err(ClientError::AuthorizationAlreadyUsed);
        }
        if !bool::from(
            self.state
                .secret()
                .as_bytes()
                .ct_eq(returned_state.as_bytes()),
        ) {
            return Err(ClientError::StateRejected);
        }
        self.used = true;
        Ok(())
    }

    fn take_verifier(&mut self) -> Result<PkceCodeVerifier, ClientError> {
        self.pkce_verifier
            .take()
            .ok_or(ClientError::AuthorizationAlreadyUsed)
    }

    fn nonce(&self) -> &Nonce {
        &self.nonce
    }
}

#[cfg(test)]
mod cache_schedule_tests {
    use super::{refresh_at_with_jitter, Duration, Instant};

    #[test]
    fn refresh_jitter_stays_after_the_threshold_and_before_expiry() {
        let issued_at = Instant::now();
        let lifetime = Duration::from_secs(100);
        let deadline = refresh_at_with_jitter(issued_at, lifetime, Duration::from_secs(100))
            .expect("bounded test deadline is representable");

        assert!(deadline >= issued_at + Duration::from_secs(80));
        assert!(deadline <= issued_at + Duration::from_secs(85));
        assert!(deadline <= issued_at + lifetime);
    }
}
