use std::io::{BufRead, Write};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use semver::Version;

use crate::upgrade_check::{self, InstallSource, UpgradeInfo};

pub struct UpgradeArgs {
    pub check: bool,
    pub force: bool,
}

pub trait CommandRunner {
    fn run(&mut self, command: &str) -> Result<i32>;
}

pub struct ShellCommandRunner;

impl CommandRunner for ShellCommandRunner {
    fn run(&mut self, command: &str) -> Result<i32> {
        let mut child = shell_command(command)
            .spawn()
            .with_context(|| format!("spawning upgrade command: {command}"))?;
        let status = child
            .wait()
            .with_context(|| format!("waiting for upgrade command: {command}"))?;
        Ok(status.code().unwrap_or(1))
    }
}

pub async fn run(args: UpgradeArgs) -> Result<i32> {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let mut runner = ShellCommandRunner;
    run_with_io(
        args,
        None,
        &mut stdin,
        &mut stdout,
        &mut stderr,
        &mut runner,
    )
    .await
}

pub async fn run_with_io(
    args: UpgradeArgs,
    upgrade_info: Option<UpgradeInfo>,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
    _stderr: &mut dyn Write,
    runner: &mut dyn CommandRunner,
) -> Result<i32> {
    let info = match upgrade_info {
        Some(info) => info,
        None if args.check => lookup_upgrade_info_for_check().await,
        None => UpgradeInfo {
            current: current_version()?,
            latest: current_version()?,
            install_source: upgrade_check::install_source::detect(),
        },
    };

    if args.check {
        writeln!(stdout, "current: {}", info.current)?;
        writeln!(stdout, "latest: {}", info.latest)?;
        writeln!(
            stdout,
            "install source: {}",
            install_source_name(info.install_source)
        )?;
        writeln!(stdout, "cache: temporary fresh check")?;
        return Ok(0);
    }

    match install_action(info.install_source) {
        InstallAction::Run(command) => {
            writeln!(stdout, "will run: {command}")?;
            if !args.force {
                write!(stdout, "Proceed? y/N ")?;
                stdout.flush()?;
                let mut answer = String::new();
                stdin.read_line(&mut answer)?;
                if !matches!(answer.trim(), "y" | "Y") {
                    writeln!(stdout, "Aborted.")?;
                    return Ok(0);
                }
            }
            runner.run(command)
        }
        InstallAction::Guidance(guidance) => {
            writeln!(stdout, "{guidance}")?;
            Ok(0)
        }
    }
}

async fn lookup_upgrade_info_for_check() -> UpgradeInfo {
    // `spur upgrade --check` is explicit and must not mutate the persistent
    // notification cache, so use a unique temp-file cache to force a fresh
    // registry fetch through the existing Task 6 orchestration.
    let cache_path = fresh_temp_cache_path();
    let current = current_version().unwrap_or_else(|_| Version::new(0, 0, 0));
    let info = if let Some(info) = upgrade_check::check_for_upgrade(&cache_path).await {
        info
    } else if let Some(latest) = upgrade_check::fetch_candidate(&current).await {
        UpgradeInfo {
            current: current.clone(),
            latest,
            install_source: upgrade_check::install_source::detect(),
        }
    } else {
        UpgradeInfo {
            current: current.clone(),
            latest: current,
            install_source: upgrade_check::install_source::detect(),
        }
    };
    let _ = std::fs::remove_file(cache_path);
    info
}

fn current_version() -> Result<Version> {
    Version::parse(env!("CARGO_PKG_VERSION")).with_context(|| {
        format!(
            "parsing current package version {}",
            env!("CARGO_PKG_VERSION")
        )
    })
}

fn fresh_temp_cache_path() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "spur-upgrade-check-{}-{nanos}.json",
        std::process::id()
    ))
}

enum InstallAction {
    Run(&'static str),
    Guidance(&'static str),
}

fn install_action(source: InstallSource) -> InstallAction {
    match source {
        InstallSource::Volta => InstallAction::Run("volta install @getspur/spur-cli@latest"),
        InstallSource::Asdf => {
            InstallAction::Run("npm install -g @getspur/spur-cli@latest && asdf reshim nodejs")
        }
        InstallSource::Fnm | InstallSource::Homebrew | InstallSource::Npm => {
            InstallAction::Run("npm install -g @getspur/spur-cli@latest")
        }
        InstallSource::Pnpm => InstallAction::Run("pnpm add -g @getspur/spur-cli@latest"),
        InstallSource::Bun => InstallAction::Run("bun add -g @getspur/spur-cli@latest"),
        InstallSource::Cargo => InstallAction::Guidance(
            "Detected cargo install; rebuild with: cargo install --path crates/spur-cli",
        ),
        InstallSource::Unknown => InstallAction::Guidance(
            "Could not detect install source. Reinstall using your original method, or: npm install -g @getspur/spur-cli@latest",
        ),
    }
}

fn install_source_name(source: InstallSource) -> &'static str {
    match source {
        InstallSource::Volta => "volta",
        InstallSource::Asdf => "asdf",
        InstallSource::Fnm => "fnm",
        InstallSource::Pnpm => "pnpm",
        InstallSource::Bun => "bun",
        InstallSource::Homebrew => "homebrew",
        InstallSource::Npm => "npm",
        InstallSource::Cargo => "cargo",
        InstallSource::Unknown => "unknown",
    }
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("cmd");
    cmd.arg("/C").arg(command);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upgrade_check::{InstallSource, UpgradeInfo};
    use semver::Version;

    #[derive(Default)]
    struct StubRunner {
        commands: Vec<String>,
    }

    impl CommandRunner for StubRunner {
        fn run(&mut self, command: &str) -> anyhow::Result<i32> {
            self.commands.push(command.to_string());
            Ok(0)
        }
    }

    fn info(source: InstallSource) -> UpgradeInfo {
        UpgradeInfo {
            current: Version::parse("1.0.0").unwrap(),
            latest: Version::parse("1.1.0").unwrap(),
            install_source: source,
        }
    }

    #[tokio::test]
    async fn check_prints_versions_and_source_from_stubbed_upgrade_info() {
        let mut runner = StubRunner::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = b"".as_slice();

        let exit = run_with_io(
            UpgradeArgs {
                check: true,
                force: false,
            },
            Some(info(InstallSource::Npm)),
            &mut stdin,
            &mut stdout,
            &mut stderr,
            &mut runner,
        )
        .await
        .unwrap();

        assert_eq!(exit, 0);
        let stdout = String::from_utf8(stdout).unwrap();
        assert!(stdout.contains("current: 1.0.0"));
        assert!(stdout.contains("latest: 1.1.0"));
        assert!(stdout.contains("install source: npm"));
    }

    #[tokio::test]
    async fn cargo_source_prints_guidance_and_does_not_spawn() {
        let mut runner = StubRunner::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = b"y\n".as_slice();

        let exit = run_with_io(
            UpgradeArgs {
                check: false,
                force: false,
            },
            Some(info(InstallSource::Cargo)),
            &mut stdin,
            &mut stdout,
            &mut stderr,
            &mut runner,
        )
        .await
        .unwrap();

        assert_eq!(exit, 0);
        assert!(runner.commands.is_empty());
        let stdout = String::from_utf8(stdout).unwrap();
        assert!(stdout.contains(
            "Detected cargo install; rebuild with: cargo install --path crates/spur-cli"
        ));
    }

    #[tokio::test]
    async fn force_with_npm_skips_prompt_and_runs_expected_command() {
        let mut runner = StubRunner::default();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdin = b"".as_slice();

        let exit = run_with_io(
            UpgradeArgs {
                check: false,
                force: true,
            },
            Some(info(InstallSource::Npm)),
            &mut stdin,
            &mut stdout,
            &mut stderr,
            &mut runner,
        )
        .await
        .unwrap();

        assert_eq!(exit, 0);
        assert_eq!(
            runner.commands,
            vec!["npm install -g @getspur/spur-cli@latest"]
        );
        let stdout = String::from_utf8(stdout).unwrap();
        assert!(stdout.contains("will run: npm install -g @getspur/spur-cli@latest"));
        assert!(!stdout.contains("Proceed?"));
    }
}
