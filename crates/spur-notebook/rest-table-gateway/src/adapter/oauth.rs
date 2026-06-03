use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{GatewayError, Result};

/// Refresh proactively this long before the token's stated expiry.
const REFRESH_SKEW: Duration = Duration::from_secs(60);
/// Fallback lifetime when the provider omits `expires_in`.
const DEFAULT_TTL: Duration = Duration::from_secs(3600);

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, CachedToken>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedToken>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct RefreshGrant<'a> {
    pub token_url: &'a str,
    pub client_id: &'a str,
    pub client_secret: &'a str,
    pub refresh_token: &'a str,
    pub scope: Option<&'a str>,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn cache_key(grant: &RefreshGrant<'_>) -> String {
    format!("{}|{}", grant.token_url, grant.refresh_token)
}

/// Return a valid access token for `grant`, minting one via the refresh-token
/// grant when the cache is empty or the cached token is within REFRESH_SKEW of expiry.
pub async fn access_token(client: &reqwest::Client, grant: &RefreshGrant<'_>) -> Result<String> {
    let key = cache_key(grant);

    if let Some(tok) = cache().lock().unwrap().get(&key) {
        if tok
            .expires_at
            .checked_duration_since(Instant::now())
            .map_or(false, |left| left > REFRESH_SKEW)
        {
            return Ok(tok.access_token.clone());
        }
    }

    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", grant.refresh_token),
        ("client_id", grant.client_id),
        ("client_secret", grant.client_secret),
    ];
    if let Some(scope) = grant.scope {
        form.push(("scope", scope));
    }

    let resp = client
        .post(grant.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| GatewayError::Auth(format!("token refresh request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(GatewayError::Auth(format!(
            "token refresh returned status {}",
            resp.status()
        )));
    }
    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| GatewayError::Auth(format!("token refresh response parse failed: {e}")))?;

    let ttl = body
        .expires_in
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TTL);
    cache().lock().unwrap().insert(
        key,
        CachedToken {
            access_token: body.access_token.clone(),
            expires_at: Instant::now() + ttl,
        },
    );
    Ok(body.access_token)
}

/// One-time authorization-code exchange — Approach C's *acquire* step.
/// `code_verifier` is supplied by the caller (PKCE generation is a separate,
/// deferred concern); this function only performs the token POST.
#[allow(dead_code)]
pub struct AuthCodeGrant<'a> {
    pub token_url: &'a str,
    pub client_id: &'a str,
    pub client_secret: &'a str,
    pub code: &'a str,
    pub code_verifier: &'a str,
    pub redirect_uri: &'a str,
}

/// Tokens returned by a successful authorization-code exchange.
#[derive(Debug)]
#[allow(dead_code)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
}

/// Exchange an authorization `code` for tokens via `grant_type=authorization_code`.
/// Unlike `access_token`, this is a one-shot setup-time call: it is NOT cached, and it
/// REQUIRES a `refresh_token` in the response (acquiring that token is the point of C).
#[allow(dead_code)]
pub async fn exchange_code(
    client: &reqwest::Client,
    grant: &AuthCodeGrant<'_>,
) -> Result<TokenSet> {
    let form = vec![
        ("grant_type", "authorization_code"),
        ("code", grant.code),
        ("code_verifier", grant.code_verifier),
        ("client_id", grant.client_id),
        ("client_secret", grant.client_secret),
        ("redirect_uri", grant.redirect_uri),
    ];

    let resp = client
        .post(grant.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| GatewayError::Auth(format!("code exchange request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(GatewayError::Auth(format!(
            "code exchange returned status {}",
            resp.status()
        )));
    }
    let body: TokenResponse = resp
        .json()
        .await
        .map_err(|e| GatewayError::Auth(format!("code exchange response parse failed: {e}")))?;
    let refresh_token = body.refresh_token.ok_or_else(|| {
        GatewayError::Auth("authorization_code exchange did not return a refresh_token".to_string())
    })?;
    Ok(TokenSet {
        access_token: body.access_token,
        refresh_token,
    })
}

/// Drop a cached token (e.g. after a 401 from the resource API).
// Reserved for the 401-invalidate-and-retry path (Approach B follow-up); not yet wired.
#[allow(dead_code)]
pub fn invalidate(grant: &RefreshGrant<'_>) {
    cache().lock().unwrap().remove(&cache_key(grant));
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn exchange_code_returns_access_and_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier=verifier-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-1",
                "refresh_token": "rt-1",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/token", server.uri());
        let grant = AuthCodeGrant {
            token_url: &url,
            client_id: "cid",
            client_secret: "csec",
            code: "auth-code-xyz",
            code_verifier: "verifier-123",
            redirect_uri: "http://127.0.0.1:0/callback",
        };
        let set = exchange_code(&reqwest::Client::new(), &grant)
            .await
            .expect("exchange should succeed");
        assert_eq!(set.access_token, "at-1");
        assert_eq!(set.refresh_token, "rt-1");
    }

    #[tokio::test]
    async fn exchange_code_missing_refresh_token_is_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "at-only"
            })))
            .mount(&server)
            .await;

        let url = format!("{}/token", server.uri());
        let grant = AuthCodeGrant {
            token_url: &url,
            client_id: "cid",
            client_secret: "csec",
            code: "c",
            code_verifier: "v",
            redirect_uri: "http://127.0.0.1:0/callback",
        };
        let err = exchange_code(&reqwest::Client::new(), &grant)
            .await
            .expect_err("missing refresh_token should error");
        assert!(matches!(err, GatewayError::Auth(_)));
    }

    #[tokio::test]
    async fn exchange_code_non_2xx_is_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let url = format!("{}/token", server.uri());
        let grant = AuthCodeGrant {
            token_url: &url,
            client_id: "cid",
            client_secret: "csec",
            code: "c",
            code_verifier: "v",
            redirect_uri: "http://127.0.0.1:0/callback",
        };
        let err = exchange_code(&reqwest::Client::new(), &grant)
            .await
            .expect_err("non-2xx should error");
        assert!(matches!(err, GatewayError::Auth(_)));
    }

    #[tokio::test]
    async fn miss_exchanges_then_hit_is_cached() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "tok-abc",
                "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;

        let url = format!("{}/token", server.uri());
        let grant = RefreshGrant {
            token_url: &url,
            client_id: "cid",
            client_secret: "csec",
            refresh_token: "miss_exchanges_then_hit_rt",
            scope: None,
        };
        let client = reqwest::Client::new();

        let t1 = access_token(&client, &grant).await.expect("first mint");
        let t2 = access_token(&client, &grant).await.expect("cache hit");
        assert_eq!(t1, "tok-abc");
        assert_eq!(t2, "tok-abc");
    }

    #[tokio::test]
    async fn non_2xx_is_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let url = format!("{}/token", server.uri());
        let grant = RefreshGrant {
            token_url: &url,
            client_id: "cid",
            client_secret: "csec",
            refresh_token: "non_2xx_rt",
            scope: None,
        };
        let err = access_token(&reqwest::Client::new(), &grant)
            .await
            .expect_err("should be auth error");
        assert!(matches!(err, GatewayError::Auth(_)));
    }
}
