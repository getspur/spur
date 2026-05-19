use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use semver::Version;
use serde::{Deserialize, Serialize};
use spur_cli::upgrade_check;
use tempfile::tempdir;
use tokio::sync::{Mutex, MutexGuard};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

static REGISTRY_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    checked_at: DateTime<Utc>,
    last_notified_at: DateTime<Utc>,
    current: String,
    latest: String,
}

struct RegistryEnvGuard {
    _lock: MutexGuard<'static, ()>,
}

impl RegistryEnvGuard {
    async fn set(registry_url: String) -> Self {
        let lock = REGISTRY_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .await;
        std::env::set_var("SPUR_NPM_REGISTRY", registry_url);
        Self { _lock: lock }
    }
}

impl Drop for RegistryEnvGuard {
    fn drop(&mut self) {
        std::env::remove_var("SPUR_NPM_REGISTRY");
    }
}

fn cache_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("upgrade-check.json")
}

fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("current package version should parse")
}

fn current_version_string() -> String {
    current_version().to_string()
}

fn read_cache(path: &Path) -> CacheFile {
    let contents = std::fs::read_to_string(path).expect("cache should be readable");
    serde_json::from_str(&contents).expect("cache should parse")
}

fn write_cache(path: &Path, cache: &CacheFile) {
    let contents = serde_json::to_vec(cache).expect("cache should serialize");
    std::fs::write(path, contents).expect("cache should be written");
}

fn cache(
    current: &str,
    latest: &str,
    checked_at: DateTime<Utc>,
    last_notified_at: DateTime<Utc>,
) -> CacheFile {
    CacheFile {
        version: 1,
        checked_at,
        last_notified_at,
        current: current.to_string(),
        latest: latest.to_string(),
    }
}

fn assert_between(timestamp: DateTime<Utc>, start: DateTime<Utc>, end: DateTime<Utc>) {
    assert!(
        timestamp >= start && timestamp <= end + ChronoDuration::seconds(1),
        "timestamp {timestamp:?} was not between {start:?} and {end:?}"
    );
}

async fn mock_latest(server: &MockServer, version: &str) {
    Mock::given(method("GET"))
        .and(path("/@getspur/spur-cli/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": version
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn cold_cache_with_available_upgrade_returns_info_and_records_notification() {
    let server = MockServer::start().await;
    let _registry_env = RegistryEnvGuard::set(server.uri()).await;
    mock_latest(&server, "999.0.0").await;
    let dir = tempdir().expect("tempdir");
    let path = cache_path(&dir);
    let started_at = Utc::now();

    let info = upgrade_check::check_for_upgrade(&path).await;

    let finished_at = Utc::now();
    let info = info.expect("upgrade should be announced");
    assert_eq!(info.current, current_version());
    assert_eq!(info.latest, Version::new(999, 0, 0));

    assert!(path.exists(), "cache file should exist");
    let cache = read_cache(&path);
    assert_eq!(cache.current, current_version_string());
    assert_eq!(cache.latest, "999.0.0");
    assert_between(cache.checked_at, started_at, finished_at);
    assert_between(cache.last_notified_at, started_at, finished_at);
}

#[tokio::test]
async fn warm_cache_with_recent_notification_suppresses_and_preserves_last_notified() {
    let server = MockServer::start().await;
    let _registry_env = RegistryEnvGuard::set(server.uri()).await;
    mock_latest(&server, "999.0.0").await;
    let dir = tempdir().expect("tempdir");
    let path = cache_path(&dir);
    let now = Utc::now();
    let last_notified_at = now - ChronoDuration::days(1);
    write_cache(
        &path,
        &cache(
            &current_version_string(),
            "999.0.0",
            now - ChronoDuration::hours(1),
            last_notified_at,
        ),
    );

    let info = upgrade_check::check_for_upgrade(&path).await;

    assert_eq!(info, None);
    let cache = read_cache(&path);
    assert_eq!(cache.last_notified_at, last_notified_at);
    assert!(
        Utc::now().signed_duration_since(cache.last_notified_at) >= ChronoDuration::hours(23),
        "last_notified_at should remain near the preseeded one-day-old timestamp"
    );
}

#[tokio::test]
async fn expired_cache_with_unchanged_latest_refreshes_checked_at_without_announcement() {
    let server = MockServer::start().await;
    let _registry_env = RegistryEnvGuard::set(server.uri()).await;
    let current = current_version_string();
    mock_latest(&server, &current).await;
    let dir = tempdir().expect("tempdir");
    let path = cache_path(&dir);
    let now = Utc::now();
    let last_notified_at = now - ChronoDuration::days(10);
    write_cache(
        &path,
        &cache(
            &current,
            &current,
            now - ChronoDuration::days(2),
            last_notified_at,
        ),
    );
    let started_at = Utc::now();

    let info = upgrade_check::check_for_upgrade(&path).await;

    let finished_at = Utc::now();
    assert_eq!(info, None);
    let cache = read_cache(&path);
    assert_eq!(cache.current, current);
    assert_eq!(cache.latest, current);
    assert_between(cache.checked_at, started_at, finished_at);
    assert_eq!(cache.last_notified_at, last_notified_at);
}
