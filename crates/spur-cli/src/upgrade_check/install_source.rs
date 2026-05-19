use super::InstallSource;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub fn detect() -> InstallSource {
    let Ok(exe) = std::env::current_exe() else {
        return InstallSource::Unknown;
    };
    let Ok(canonical) = std::fs::canonicalize(exe) else {
        return InstallSource::Unknown;
    };

    detect_from_path(&canonical, &RealEnv)
}

pub(crate) fn detect_from_path(canonical: &Path, env: &dyn EnvProvider) -> InstallSource {
    detect_from_path_with_npm_prefix(canonical, env, &CommandNpmPrefixProvider)
}

pub(crate) trait EnvProvider {
    fn var(&self, key: &str) -> Option<String>;
}

trait NpmPrefixProvider {
    fn prefix(&self, env: &dyn EnvProvider) -> Option<PathBuf>;
}

struct RealEnv;

impl EnvProvider for RealEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

struct CommandNpmPrefixProvider;

impl NpmPrefixProvider for CommandNpmPrefixProvider {
    fn prefix(&self, env: &dyn EnvProvider) -> Option<PathBuf> {
        npm_on_path(env)
            .then(npm_prefix_global_with_timeout)
            .flatten()
    }
}

fn detect_from_path_with_npm_prefix(
    canonical: &Path,
    env: &dyn EnvProvider,
    npm_prefix: &dyn NpmPrefixProvider,
) -> InstallSource {
    let path = canonical.to_string_lossy();

    if path.contains("/.volta/tools/image/packages/@getspur/") {
        return InstallSource::Volta;
    }

    if path.contains("/installs/nodejs/") && path.contains("/lib/node_modules/@getspur/") {
        return InstallSource::Asdf;
    }

    if path.contains("/.fnm/node-versions/") || path.contains("/fnm_multishells/") {
        return InstallSource::Fnm;
    }

    if path.contains("/pnpm/global/") || env_path_starts_with(canonical, env, "PNPM_HOME") {
        return InstallSource::Pnpm;
    }

    if path.contains("/.bun/install/global/") {
        return InstallSource::Bun;
    }

    if canonical.starts_with("/opt/homebrew/")
        || canonical.starts_with("/usr/local/Cellar/")
        || canonical.starts_with("/home/linuxbrew/")
    {
        return InstallSource::Homebrew;
    }

    if env.var("npm_execpath").is_some() || env.var("NPM_CONFIG_PREFIX").is_some() {
        return InstallSource::Npm;
    }

    if npm_prefix
        .prefix(env)
        .is_some_and(|prefix| canonical.starts_with(prefix))
    {
        return InstallSource::Npm;
    }

    if env_path_starts_with_bin(canonical, env, "CARGO_HOME")
        || canonical_starts_with_home_cargo(canonical, env)
    {
        return InstallSource::Cargo;
    }

    InstallSource::Unknown
}

fn env_path_starts_with(canonical: &Path, env: &dyn EnvProvider, key: &str) -> bool {
    env.var(key)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| canonical.starts_with(value))
}

fn env_path_starts_with_bin(canonical: &Path, env: &dyn EnvProvider, key: &str) -> bool {
    env.var(key)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| canonical.starts_with(Path::new(&value).join("bin")))
}

fn canonical_starts_with_home_cargo(canonical: &Path, env: &dyn EnvProvider) -> bool {
    env.var("HOME")
        .filter(|value| !value.is_empty())
        .is_some_and(|home| canonical.starts_with(Path::new(&home).join(".cargo").join("bin")))
}

fn npm_on_path(env: &dyn EnvProvider) -> bool {
    let Some(path) = env.var("PATH") else {
        return false;
    };

    std::env::split_paths(&path).any(|dir| dir.join("npm").is_file())
}

fn npm_prefix_global_with_timeout() -> Option<PathBuf> {
    let mut child = Command::new("npm")
        .args(["prefix", "-g"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);

    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => {
                let output = child.wait_with_output().ok()?;
                let stdout = String::from_utf8(output.stdout).ok()?;
                let prefix = stdout.trim();
                return (!prefix.is_empty()).then(|| PathBuf::from(prefix));
            }
            Some(_) => return None,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[derive(Default)]
    struct MockEnv {
        vars: HashMap<&'static str, String>,
    }

    impl MockEnv {
        fn with(mut self, key: &'static str, value: impl Into<String>) -> Self {
            self.vars.insert(key, value.into());
            self
        }
    }

    impl EnvProvider for MockEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
    }

    struct MockNpmPrefix(Option<PathBuf>);

    impl NpmPrefixProvider for MockNpmPrefix {
        fn prefix(&self, _env: &dyn EnvProvider) -> Option<PathBuf> {
            self.0.clone()
        }
    }

    fn detect(path: &str, env: &MockEnv) -> InstallSource {
        detect_from_path(&PathBuf::from(path), env)
    }

    fn detect_with_npm_prefix(
        path: &str,
        env: &MockEnv,
        npm_prefix: Option<&str>,
    ) -> InstallSource {
        detect_from_path_with_npm_prefix(
            &PathBuf::from(path),
            env,
            &MockNpmPrefix(npm_prefix.map(PathBuf::from)),
        )
    }

    #[test]
    fn detects_volta_package_image() {
        let env = MockEnv::default();

        assert_eq!(
            detect(
                "/Users/dev/.volta/tools/image/packages/@getspur/spur-cli/bin/spur",
                &env
            ),
            InstallSource::Volta
        );
    }

    #[test]
    fn detects_asdf_custom_data_dir_layout_without_asdf_segment() {
        let env = MockEnv::default().with("ASDF_DATA_DIR", "/opt/asdf-custom");

        assert_eq!(
            detect(
                "/opt/asdf-custom/installs/nodejs/22.11.0/lib/node_modules/@getspur/spur-cli/bin/spur",
                &env,
            ),
            InstallSource::Asdf
        );
    }

    #[test]
    fn detects_fnm_node_versions_and_multishell_layouts() {
        let env = MockEnv::default();

        assert_eq!(
            detect(
                "/Users/dev/.fnm/node-versions/v22.11.0/installation/bin/spur",
                &env,
            ),
            InstallSource::Fnm
        );
        assert_eq!(
            detect("/var/folders/xx/fnm_multishells/12345/bin/spur", &env),
            InstallSource::Fnm
        );
    }

    #[test]
    fn detects_pnpm_global_and_pnpm_home_layouts() {
        let env = MockEnv::default().with("PNPM_HOME", "/Users/dev/Library/pnpm");

        assert_eq!(
            detect(
                "/Users/dev/.local/share/pnpm/global/5/node_modules/.bin/spur",
                &env
            ),
            InstallSource::Pnpm
        );
        assert_eq!(
            detect("/Users/dev/Library/pnpm/spur", &env),
            InstallSource::Pnpm
        );
    }

    #[test]
    fn detects_bun_global_layout() {
        let env = MockEnv::default();

        assert_eq!(
            detect(
                "/Users/dev/.bun/install/global/node_modules/.bin/spur",
                &env
            ),
            InstallSource::Bun
        );
    }

    #[test]
    fn detects_homebrew_prefixes() {
        let env = MockEnv::default();

        assert_eq!(
            detect("/opt/homebrew/bin/spur", &env),
            InstallSource::Homebrew
        );
        assert_eq!(
            detect("/usr/local/Cellar/spur/1.2.3/bin/spur", &env),
            InstallSource::Homebrew
        );
        assert_eq!(
            detect("/home/linuxbrew/.linuxbrew/bin/spur", &env),
            InstallSource::Homebrew
        );
    }

    #[test]
    fn detects_npm_when_npm_execpath_is_set() {
        let env = MockEnv::default().with(
            "npm_execpath",
            "/usr/local/lib/node_modules/npm/bin/npm-cli.js",
        );

        assert_eq!(detect("/custom/bin/spur", &env), InstallSource::Npm);
    }

    #[test]
    fn volta_takes_precedence_over_generic_npm_signals() {
        let env = MockEnv::default().with(
            "npm_execpath",
            "/usr/local/lib/node_modules/npm/bin/npm-cli.js",
        );

        assert_eq!(
            detect(
                "/Users/dev/.volta/tools/image/packages/@getspur/spur-cli/bin/spur",
                &env,
            ),
            InstallSource::Volta
        );
    }

    #[test]
    fn detects_npm_when_path_is_under_global_prefix() {
        let env = MockEnv::default();

        assert_eq!(
            detect_with_npm_prefix(
                "/usr/local/lib/node_modules/@getspur/spur-cli/bin/spur",
                &env,
                Some("/usr/local"),
            ),
            InstallSource::Npm
        );
    }

    #[test]
    fn detects_cargo_home_bin_and_default_cargo_bin() {
        let env = MockEnv::default()
            .with("CARGO_HOME", "/opt/cargo")
            .with("HOME", "/Users/dev");

        assert_eq!(detect("/opt/cargo/bin/spur", &env), InstallSource::Cargo);
        assert_eq!(
            detect("/Users/dev/.cargo/bin/spur", &env),
            InstallSource::Cargo
        );
    }

    #[test]
    fn unmatched_path_is_unknown() {
        let env = MockEnv::default();

        assert_eq!(
            detect("/custom/toolchains/spur", &env),
            InstallSource::Unknown
        );
    }
}
