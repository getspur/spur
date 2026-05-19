mod cache;
pub mod install_source;
#[allow(dead_code)]
pub(crate) mod registry;

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use semver::Version;
use tracing::debug;

const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);
const NOTIFY_INTERVAL: Duration = Duration::from_secs(3 * 24 * 3600);

#[cfg(test)]
static REGISTRY_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) async fn lock_registry_env() -> tokio::sync::MutexGuard<'static, ()> {
    REGISTRY_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeInfo {
    pub current: semver::Version,
    pub latest: semver::Version,
    pub install_source: InstallSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallSource {
    Volta,
    Asdf,
    Fnm,
    Pnpm,
    Bun,
    Homebrew,
    Npm,
    Cargo,
    Unknown,
}

pub fn upgrade_check_disabled() -> bool {
    env_disables_upgrade_check("SPUR_NO_UPGRADE_CHECK")
        || env_disables_upgrade_check("NO_UPDATE_NOTIFIER")
}

pub fn cache_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| {
        dirs.home_dir()
            .join(".spur")
            .join("cache")
            .join("upgrade-check.json")
    })
}

pub async fn check_for_upgrade(cache_path: &Path) -> Option<UpgradeInfo> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| {
            debug!(
                version = env!("CARGO_PKG_VERSION"),
                %error,
                "failed to parse current package version for upgrade check"
            );
        })
        .ok()?;

    check_for_upgrade_with_current(cache_path, &current).await
}

fn env_disables_upgrade_check(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        let value = value.trim();
        // npm update-notifier treats any set value as an opt-out except explicit false-y overrides.
        !(value == "0" || value.eq_ignore_ascii_case("false"))
    })
}

async fn check_for_upgrade_with_current(
    cache_path: &Path,
    current: &Version,
) -> Option<UpgradeInfo> {
    let now = Utc::now();
    let cache = cache::read(cache_path);
    let mut cache_state = cache.clone().unwrap_or_else(|| cold_cache(current, now));

    let candidate = match cached_candidate_if_fresh(cache.as_ref(), now) {
        Some(candidate) => candidate,
        None => match fetch_candidate(current).await {
            Some(candidate) => {
                cache_state.current = current.to_string();
                cache_state.latest = candidate.to_string();
                cache_state.checked_at = now;
                if let Err(error) = cache::write(cache_path, &cache_state) {
                    debug!(
                        path = %cache_path.display(),
                        %error,
                        "failed to write upgrade check cache after registry fetch"
                    );
                    return None;
                }
                candidate
            }
            None => return None,
        },
    };

    if candidate <= *current {
        if candidate < *current {
            debug!(
                current = %current,
                latest = %candidate,
                "npm registry latest is lower than current package version"
            );
        }
        return None;
    }

    if !notification_due(now, cache_state.last_notified_at) {
        return None;
    }

    cache_state.current = current.to_string();
    cache_state.latest = candidate.to_string();
    cache_state.last_notified_at = now;
    if let Err(error) = cache::write(cache_path, &cache_state) {
        debug!(
            path = %cache_path.display(),
            %error,
            "failed to write upgrade check cache notification timestamp"
        );
        return None;
    }

    Some(UpgradeInfo {
        current: current.clone(),
        latest: candidate,
        install_source: install_source::detect(),
    })
}

fn cached_candidate_if_fresh(
    cache: Option<&cache::CacheV1>,
    now: DateTime<Utc>,
) -> Option<Version> {
    let cache = cache?;
    if !age_less_than(now, cache.checked_at, CHECK_INTERVAL) {
        return None;
    }

    Version::parse(&cache.latest)
        .map_err(|error| {
            debug!(
                latest = %cache.latest,
                %error,
                "failed to parse cached latest version for upgrade check"
            );
        })
        .ok()
}

pub(crate) async fn fetch_candidate(current: &Version) -> Option<Version> {
    let client = reqwest::Client::new();
    let latest = registry::fetch_latest(&client).await?;

    if current.pre.is_empty() {
        return Some(latest);
    }

    let dist_tags = registry::fetch_dist_tags(&client).await?;
    let mut candidate = latest.max(dist_tags.latest);
    if let Some(beta) = dist_tags.beta {
        candidate = candidate.max(beta);
    }
    if let Some(next) = dist_tags.next {
        candidate = candidate.max(next);
    }

    Some(candidate)
}

fn cold_cache(current: &Version, now: DateTime<Utc>) -> cache::CacheV1 {
    cache::CacheV1 {
        version: 1,
        checked_at: now,
        last_notified_at: DateTime::<Utc>::UNIX_EPOCH,
        current: current.to_string(),
        latest: current.to_string(),
    }
}

fn notification_due(now: DateTime<Utc>, last_notified_at: DateTime<Utc>) -> bool {
    elapsed_since(now, last_notified_at).is_some_and(|elapsed| elapsed >= NOTIFY_INTERVAL)
}

fn age_less_than(now: DateTime<Utc>, timestamp: DateTime<Utc>, interval: Duration) -> bool {
    elapsed_since(now, timestamp).is_some_and(|elapsed| elapsed < interval)
}

fn elapsed_since(now: DateTime<Utc>, timestamp: DateTime<Utc>) -> Option<Duration> {
    now.signed_duration_since(timestamp).to_std().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use semver::Version;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::tempdir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ENV_LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn clear_upgrade_env() {
        std::env::remove_var("SPUR_NO_UPGRADE_CHECK");
        std::env::remove_var("NO_UPDATE_NOTIFIER");
    }

    fn use_registry(server: &MockServer) {
        std::env::set_var("SPUR_NPM_REGISTRY", server.uri());
    }

    fn clear_registry() {
        std::env::remove_var("SPUR_NPM_REGISTRY");
    }

    fn cache_path_in_tempdir(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("upgrade-check.json")
    }

    fn write_cache(
        path: &Path,
        current: &str,
        latest: &str,
        checked_at: chrono::DateTime<Utc>,
        last_notified_at: chrono::DateTime<Utc>,
    ) {
        cache::write(
            path,
            &cache::CacheV1 {
                version: 1,
                checked_at,
                last_notified_at,
                current: current.to_string(),
                latest: latest.to_string(),
            },
        )
        .expect("write cache");
    }

    fn version(value: &str) -> Version {
        Version::parse(value).expect("test version should parse")
    }

    fn assert_recent(timestamp: chrono::DateTime<Utc>) {
        let age = Utc::now().signed_duration_since(timestamp);
        assert!(
            age >= ChronoDuration::zero() && age < ChronoDuration::seconds(10),
            "timestamp was not recent: {timestamp:?}"
        );
    }

    async fn mount_latest(server: &MockServer, latest: &str) {
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "version": latest
            })))
            .mount(server)
            .await;
    }

    async fn mount_dist_tags(
        server: &MockServer,
        latest: &str,
        beta: Option<&str>,
        next: Option<&str>,
    ) {
        let mut tags = serde_json::Map::new();
        tags.insert("latest".into(), serde_json::json!(latest));
        if let Some(beta) = beta {
            tags.insert("beta".into(), serde_json::json!(beta));
        }
        if let Some(next) = next {
            tags.insert("next".into(), serde_json::json!(next));
        }

        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dist-tags": tags
            })))
            .mount(server)
            .await;
    }

    #[test]
    fn upgrade_check_disabled_is_true_when_spur_env_var_is_truthy() {
        let _guard = lock_env();
        clear_upgrade_env();
        std::env::set_var("SPUR_NO_UPGRADE_CHECK", "1");

        assert!(upgrade_check_disabled());

        clear_upgrade_env();
    }

    #[test]
    fn upgrade_check_disabled_is_true_when_notifier_env_var_is_truthy() {
        let _guard = lock_env();
        clear_upgrade_env();
        std::env::set_var("NO_UPDATE_NOTIFIER", "yes");

        assert!(upgrade_check_disabled());

        clear_upgrade_env();
    }

    #[test]
    fn upgrade_check_disabled_is_false_when_env_vars_are_unset() {
        let _guard = lock_env();
        clear_upgrade_env();

        assert!(!upgrade_check_disabled());
    }

    #[test]
    fn upgrade_check_disabled_is_false_for_explicit_false_values() {
        let _guard = lock_env();
        clear_upgrade_env();
        std::env::set_var("SPUR_NO_UPGRADE_CHECK", "0");
        std::env::set_var("NO_UPDATE_NOTIFIER", "false");

        assert!(!upgrade_check_disabled());

        clear_upgrade_env();
    }

    #[test]
    fn cache_path_uses_spur_upgrade_check_cache_file() {
        let path = cache_path().expect("cache path should resolve");

        assert!(
            path.to_string_lossy()
                .ends_with(".spur/cache/upgrade-check.json"),
            "unexpected cache path: {}",
            path.display()
        );
    }

    #[tokio::test]
    async fn cold_cache_with_stable_upgrade_returns_info_and_populates_cache() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        mount_latest(&server, "1.1.0").await;
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);

        let info = check_for_upgrade_with_current(&path, &version("1.0.0")).await;

        assert_eq!(
            info.as_ref().map(|info| (&info.current, &info.latest)),
            Some((&version("1.0.0"), &version("1.1.0")))
        );
        let cache = cache::read(&path).expect("cache should be written");
        assert_eq!(cache.current, "1.0.0");
        assert_eq!(cache.latest, "1.1.0");
        assert_recent(cache.checked_at);
        assert_recent(cache.last_notified_at);
        clear_registry();
    }

    #[tokio::test]
    async fn warm_cache_with_no_upgrade_skips_network() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);
        let now = Utc::now();
        write_cache(
            &path,
            "1.0.0",
            "1.0.0",
            now - ChronoDuration::hours(1),
            now - ChronoDuration::days(10),
        );

        let info = check_for_upgrade_with_current(&path, &version("1.0.0")).await;

        assert_eq!(info, None);
        assert_eq!(server.received_requests().await.expect("requests").len(), 0);
        clear_registry();
    }

    #[tokio::test]
    async fn warm_cache_with_recent_notification_suppresses_and_preserves_cache() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);
        let now = Utc::now();
        let checked_at = now - ChronoDuration::hours(1);
        let last_notified_at = now - ChronoDuration::days(1);
        write_cache(&path, "1.0.0", "1.1.0", checked_at, last_notified_at);
        let before = cache::read(&path).expect("cache before");

        let info = check_for_upgrade_with_current(&path, &version("1.0.0")).await;

        assert_eq!(info, None);
        assert_eq!(cache::read(&path), Some(before));
        clear_registry();
    }

    #[tokio::test]
    async fn warm_cache_with_old_notification_returns_info_and_bumps_last_notified() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);
        let now = Utc::now();
        let checked_at = now - ChronoDuration::hours(1);
        write_cache(
            &path,
            "1.0.0",
            "1.1.0",
            checked_at,
            now - ChronoDuration::days(4),
        );

        let info = check_for_upgrade_with_current(&path, &version("1.0.0")).await;

        assert_eq!(
            info.as_ref().map(|info| (&info.current, &info.latest)),
            Some((&version("1.0.0"), &version("1.1.0")))
        );
        let cache = cache::read(&path).expect("cache after");
        assert_eq!(cache.checked_at, checked_at);
        assert_recent(cache.last_notified_at);
        clear_registry();
    }

    #[tokio::test]
    async fn prerelease_current_uses_higher_beta_dist_tag() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        mount_latest(&server, "1.2.0").await;
        mount_dist_tags(&server, "1.2.0", Some("1.3.0-beta.2"), None).await;
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);

        let info = check_for_upgrade_with_current(&path, &version("1.3.0-beta.1")).await;

        assert_eq!(info.map(|info| info.latest), Some(version("1.3.0-beta.2")));
        clear_registry();
    }

    #[tokio::test]
    async fn prerelease_current_without_beta_does_not_fallback_to_lower_stable() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        mount_latest(&server, "1.2.0").await;
        mount_dist_tags(&server, "1.2.0", None, None).await;
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);

        let info = check_for_upgrade_with_current(&path, &version("1.3.0-beta.1")).await;

        assert_eq!(info, None);
        clear_registry();
    }

    #[tokio::test]
    async fn prerelease_current_uses_higher_stable_over_beta() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        mount_latest(&server, "1.4.0").await;
        mount_dist_tags(&server, "1.4.0", Some("1.3.0-beta.2"), None).await;
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);

        let info = check_for_upgrade_with_current(&path, &version("1.3.0-beta.1")).await;

        assert_eq!(info.map(|info| info.latest), Some(version("1.4.0")));
        clear_registry();
    }

    #[tokio::test]
    async fn network_failure_on_cold_cache_returns_none_without_cache_write() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        Mock::given(method("GET"))
            .and(path("/@getspur/spur-cli/latest"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);

        let info = check_for_upgrade_with_current(&path, &version("1.0.0")).await;

        assert_eq!(info, None);
        assert!(!path.exists(), "cache should not be written");
        clear_registry();
    }

    #[tokio::test]
    async fn downgrade_scenario_updates_checked_at_but_returns_none() {
        let _guard = lock_registry_env().await;
        let server = MockServer::start().await;
        use_registry(&server);
        mount_latest(&server, "1.0.0").await;
        let dir = tempdir().expect("tempdir");
        let path = cache_path_in_tempdir(&dir);
        let now = Utc::now();
        write_cache(
            &path,
            "2.0.0",
            "1.9.0",
            now - ChronoDuration::days(2),
            now - ChronoDuration::days(10),
        );

        let info = check_for_upgrade_with_current(&path, &version("2.0.0")).await;

        assert_eq!(info, None);
        let cache = cache::read(&path).expect("cache after");
        assert_eq!(cache.latest, "1.0.0");
        assert_recent(cache.checked_at);
        clear_registry();
    }
}
