use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::error::{GatewayError, Result};
use base64::Engine as _;
use rand::RngCore as _;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

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

/// PKCE verifier + its S256 challenge.
#[allow(dead_code)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a PKCE pair: a high-entropy verifier and its base64url(S256) challenge.
#[allow(dead_code)]
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

/// Generate a single-use CSRF `state` nonce (URL-safe, no padding).
#[allow(dead_code)]
pub fn generate_state() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Inputs for the consent URL. `extra` carries provider `authorization_params`
/// (e.g. `access_type=offline`, `prompt=consent`).
#[allow(dead_code)]
pub struct AuthorizeUrlParams<'a> {
    pub authorization_url: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub scope: &'a str,
    pub state: &'a str,
    pub code_challenge: &'a str,
    pub extra: &'a [(&'a str, &'a str)],
}

/// Build the provider consent URL with PKCE + state. Uses `reqwest::Url`
/// so query values are correctly percent-encoded.
#[allow(dead_code)]
pub fn build_authorize_url(p: &AuthorizeUrlParams<'_>) -> Result<String> {
    let mut params = vec![
        ("client_id", p.client_id),
        ("redirect_uri", p.redirect_uri),
        ("response_type", "code"),
        ("scope", p.scope),
        ("state", p.state),
        ("code_challenge", p.code_challenge),
        ("code_challenge_method", "S256"),
    ];
    params.extend_from_slice(p.extra);
    let url = reqwest::Url::parse_with_params(p.authorization_url, &params)
        .map_err(|e| GatewayError::Auth(format!("authorize url build failed: {e}")))?;
    Ok(url.to_string())
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
            .is_some_and(|left| left > REFRESH_SKEW)
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

/// The authorization `code` + `state` captured from the OAuth redirect.
#[allow(dead_code)]
pub struct Callback {
    pub code: String,
    pub state: String,
}

/// Bind an ephemeral loopback listener for the OAuth redirect. Returns the
/// `redirect_uri` to hand the provider plus the bound listener to await on.
#[allow(dead_code)]
pub async fn bind_loopback() -> Result<(String, TcpListener)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| GatewayError::Auth(format!("loopback bind failed: {e}")))?;
    let addr: SocketAddr = listener
        .local_addr()
        .map_err(|e| GatewayError::Auth(format!("loopback local_addr failed: {e}")))?;
    let redirect_uri = format!("http://127.0.0.1:{}/callback", addr.port());
    Ok((redirect_uri, listener))
}

/// Await exactly one redirect request, parse `code`/`state` from the query string,
/// reply with a minimal close-tab page, and return the captured values. The listener
/// is consumed (dropped) after one request.
#[allow(dead_code)]
pub async fn await_callback(listener: TcpListener) -> Result<Callback> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| GatewayError::Auth(format!("loopback accept failed: {e}")))?;

    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| GatewayError::Auth(format!("loopback read failed: {e}")))?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Request line: "GET /callback?code=...&state=... HTTP/1.1"
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| GatewayError::Auth("loopback: malformed request line".to_string()))?;
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");

    let mut code = None;
    let mut state = None;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(percent_decode(v)),
                "state" => state = Some(percent_decode(v)),
                _ => {}
            }
        }
    }

    let body = "<html><body>You can close this tab and return to SPUR.</body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;

    match (code, state) {
        (Some(code), Some(state)) => Ok(Callback { code, state }),
        _ => Err(GatewayError::Auth(
            "loopback callback missing code or state".to_string(),
        )),
    }
}

/// Minimal `application/x-www-form-urlencoded` percent-decoding for query values.
#[allow(dead_code)]
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
    async fn loopback_captures_code_and_state() {
        let (redirect_uri, listener) = bind_loopback().await.expect("bind loopback");
        assert!(redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(redirect_uri.ends_with("/callback"));

        let handle = tokio::spawn(async move { await_callback(listener).await });

        let client = reqwest::Client::new();
        let _ = client
            .get(format!("{redirect_uri}?code=abc%2F123&state=xyz789"))
            .send()
            .await
            .expect("redirect request should reach the listener");

        let cb = handle.await.expect("join").expect("callback parsed");
        assert_eq!(cb.code, "abc/123");
        assert_eq!(cb.state, "xyz789");
    }

    #[test]
    fn percent_decode_handles_escapes_and_plus() {
        assert_eq!(percent_decode("abc%2F123"), "abc/123");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%41%42"), "AB");
    }

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let pkce = generate_pkce();
        assert!(!pkce.verifier.is_empty());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, expected);
        assert!(!pkce.challenge.contains('='));
    }

    #[test]
    fn state_nonce_is_unique_and_urlsafe() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b);
        assert!(!a.is_empty());
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn authorize_url_contains_required_params() {
        let url = build_authorize_url(&AuthorizeUrlParams {
            authorization_url: "https://accounts.google.com/o/oauth2/v2/auth",
            client_id: "cid.apps.googleusercontent.com",
            redirect_uri: "http://127.0.0.1:51847/callback",
            scope: "https://www.googleapis.com/auth/adwords",
            state: "st8",
            code_challenge: "chal",
            extra: &[("access_type", "offline"), ("prompt", "consent")],
        })
        .expect("url");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("code_challenge=chal"));
        assert!(url.contains("state=st8"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fadwords"));
    }

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
