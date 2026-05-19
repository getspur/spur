mod cache;
mod install_source;
mod registry;

use std::path::{Path, PathBuf};

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
    let _cache = cache::read(cache_path);
    None
}

fn env_disables_upgrade_check(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| {
        let value = value.to_string_lossy();
        let value = value.trim();
        // npm update-notifier treats any set value as an opt-out except explicit false-y overrides.
        !(value == "0" || value.eq_ignore_ascii_case("false"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn clear_upgrade_env() {
        std::env::remove_var("SPUR_NO_UPGRADE_CHECK");
        std::env::remove_var("NO_UPDATE_NOTIFIER");
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
}
