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

/// Drop a cached token (e.g. after a 401 from the resource API).
pub fn invalidate(grant: &RefreshGrant<'_>) {
    cache().lock().unwrap().remove(&cache_key(grant));
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
