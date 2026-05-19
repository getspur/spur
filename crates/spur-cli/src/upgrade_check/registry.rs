use semver::Version;
use serde::Deserialize;
use std::env;
use std::time::Duration;
use tracing::debug;

const PACKAGE_PATH: &str = "/@getspur/spur-cli";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DistTags {
    pub latest: Version,
    pub beta: Option<Version>,
    pub next: Option<Version>,
}

#[derive(Deserialize)]
struct LatestResponse {
    version: String,
}

#[derive(Deserialize)]
struct DistTagsResponse {
    #[serde(rename = "dist-tags")]
    dist_tags: RawDistTags,
}

#[derive(Deserialize)]
struct RawDistTags {
    latest: String,
    beta: Option<String>,
    next: Option<String>,
}

// Test-only override for wiremock-backed registry tests.
pub(crate) fn registry_base() -> String {
    env::var("SPUR_NPM_REGISTRY").unwrap_or_else(|_| "https://registry.npmjs.org".into())
}

pub(crate) async fn fetch_latest(client: &reqwest::Client) -> Option<Version> {
    let url = registry_url("/latest");
    let response = fetch_json::<LatestResponse>(client, &url).await?;
    parse_version("latest.version", &response.version)
}

pub(crate) async fn fetch_dist_tags(client: &reqwest::Client) -> Option<DistTags> {
    let url = registry_url("");
    let response = fetch_json::<DistTagsResponse>(client, &url).await?;
    let raw = response.dist_tags;
    Some(DistTags {
        latest: parse_version("dist-tags.latest", &raw.latest)?,
        beta: parse_optional_version("dist-tags.beta", raw.beta)?,
        next: parse_optional_version("dist-tags.next", raw.next)?,
    })
}

fn registry_url(suffix: &str) -> String {
    format!(
        "{}{}{}",
        registry_base().trim_end_matches('/'),
        PACKAGE_PATH,
        suffix
    )
}

async fn fetch_json<T>(client: &reqwest::Client, url: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    match tokio::time::timeout(REQUEST_TIMEOUT, fetch_json_inner::<T>(client, url)).await {
        Ok(FetchResult::Ok(body)) => Some(body),
        Ok(FetchResult::RequestError(error)) => {
            debug!(%url, %error, "npm registry request failed");
            None
        }
        Ok(FetchResult::StatusError(status)) => {
            debug!(%url, %status, "npm registry returned non-success status");
            None
        }
        Ok(FetchResult::ParseError(error)) => {
            debug!(%url, %error, "npm registry response parsing failed");
            None
        }
        Err(error) => {
            debug!(%url, %error, "npm registry request timed out");
            None
        }
    }
}

async fn fetch_json_inner<T>(client: &reqwest::Client, url: &str) -> FetchResult<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(error) => return FetchResult::RequestError(error),
    };

    if !response.status().is_success() {
        return FetchResult::StatusError(response.status());
    }

    response
        .json::<T>()
        .await
        .map(FetchResult::Ok)
        .unwrap_or_else(FetchResult::ParseError)
}

enum FetchResult<T> {
    Ok(T),
    RequestError(reqwest::Error),
    StatusError(reqwest::StatusCode),
    ParseError(reqwest::Error),
}

fn parse_version(field: &str, value: &str) -> Option<Version> {
    Version::parse(value)
        .map_err(|error| {
            debug!(field, value, %error, "npm registry version parsing failed");
        })
        .ok()
}

fn parse_optional_version(field: &str, value: Option<String>) -> Option<Option<Version>> {
    match value {
        Some(value) => parse_version(field, &value).map(Some),
        None => Some(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;
    use std::sync::OnceLock;
    use std::time::Duration;
    use tokio::sync::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    async fn lock_env() -> tokio::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().await
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("test client should build")
    }

    fn use_registry(server: &MockServer) {
        std::env::set_var("SPUR_NPM_REGISTRY", server.uri());
    }

    fn clear_registry() {
        std::env::remove_var("SPUR_NPM_REGISTRY");
    }

    #[tokio::test]
    async fn fetch_latest_returns_version_from_latest_endpoint() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "version": "1.2.0"
            })))
            .mount(&server)
            .await;

        let version = fetch_latest(&client()).await;

        assert_eq!(version, Some(Version::new(1, 2, 0)));
        clear_registry();
    }

    #[tokio::test]
    async fn fetch_latest_returns_none_for_malformed_json() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("{", "application/json"))
            .mount(&server)
            .await;

        let version = fetch_latest(&client()).await;

        assert_eq!(version, None);
        clear_registry();
    }

    #[tokio::test]
    async fn fetch_latest_returns_none_when_version_is_missing() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let version = fetch_latest(&client()).await;

        assert_eq!(version, None);
        clear_registry();
    }

    #[tokio::test]
    async fn fetch_latest_returns_none_for_server_errors() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli/latest"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let version = fetch_latest(&client()).await;

        assert_eq!(version, None);
        clear_registry();
    }

    #[tokio::test]
    async fn fetch_latest_returns_none_when_request_times_out() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli/latest"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(serde_json::json!({ "version": "1.2.0" })),
            )
            .mount(&server)
            .await;

        let version = fetch_latest(&client()).await;

        assert_eq!(version, None);
        clear_registry();
    }

    #[tokio::test]
    async fn fetch_dist_tags_allows_missing_beta_and_next() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dist-tags": {
                    "latest": "1.2.0"
                }
            })))
            .mount(&server)
            .await;

        let tags = fetch_dist_tags(&client()).await;

        assert_eq!(
            tags,
            Some(DistTags {
                latest: Version::new(1, 2, 0),
                beta: None,
                next: None,
            })
        );
        clear_registry();
    }

    #[tokio::test]
    async fn fetch_dist_tags_returns_latest_beta_and_next() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dist-tags": {
                    "latest": "1.2.0",
                    "beta": "1.3.0-beta.1",
                    "next": "2.0.0-alpha.1"
                }
            })))
            .mount(&server)
            .await;

        let tags = fetch_dist_tags(&client()).await;

        assert_eq!(
            tags,
            Some(DistTags {
                latest: Version::new(1, 2, 0),
                beta: Some(Version::parse("1.3.0-beta.1").unwrap()),
                next: Some(Version::parse("2.0.0-alpha.1").unwrap()),
            })
        );
        clear_registry();
    }

    #[tokio::test]
    async fn fetch_dist_tags_returns_none_for_malformed_semver() {
        let _guard = lock_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dist-tags": {
                    "latest": "not-semver"
                }
            })))
            .mount(&server)
            .await;

        let tags = fetch_dist_tags(&client()).await;

        assert_eq!(tags, None);
        clear_registry();
    }
}
