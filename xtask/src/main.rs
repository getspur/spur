use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let subcommand = args.next().unwrap_or_default();
    let extra: Vec<String> = args.collect();

    match subcommand.as_str() {
        "install" => install(extra),
        "" | "help" | "--help" | "-h" => {
            print_help();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("xtask: unknown subcommand {other:?}");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!("usage: cargo xtask <subcommand>");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  install [--debug] [--remote]   install spur to $CARGO_HOME/bin");
    eprintln!();
    eprintln!("options:");
    eprintln!("  --debug    build/install debug artifacts for local installs");
    eprintln!("  --notebook-channel <auto|green>");
    eprintln!("             auto aliases green after the standalone notebook cutover");
    eprintln!("             green installs spur only and expects standalone getspur/spur-notebook");
    eprintln!(
        "  --remote   build Linux release binaries on the GCP VM via scripts/gcp-build and fetch them back"
    );
    if cfg!(target_os = "linux") {
        eprintln!("             fetched into $CARGO_HOME/bin (native on this host)");
    } else {
        eprintln!(
            "             on a non-Linux host the Linux binaries are staged under target/remote-linux-bin"
        );
        eprintln!(
            "             instead of $CARGO_HOME/bin; they do not run here. macOS does not build Jute.app"
        );
    }
    eprintln!("  --force    with --remote on a non-Linux host, overwrite $CARGO_HOME/bin anyway");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotebookInstallChannel {
    Auto,
    Green,
}

impl NotebookInstallChannel {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "green" => Ok(Self::Green),
            "blue" => Err(
                "blue notebook install source was removed; install getspur/spur-notebook and use green"
                    .to_owned(),
            ),
            _ => Err(format!(
                "invalid --notebook-channel {value:?}; expected green or auto"
            )),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct InstallOptions {
    debug: bool,
    remote: bool,
    force: bool,
    notebook_channel: NotebookInstallChannel,
    cargo_args: Vec<String>,
}

fn parse_install_options(extra: Vec<String>) -> Result<InstallOptions, String> {
    let mut options = InstallOptions {
        debug: false,
        remote: false,
        force: false,
        notebook_channel: NotebookInstallChannel::Green,
        cargo_args: Vec::new(),
    };
    let mut args = extra.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--debug" {
            options.debug = true;
        } else if arg == "--remote" {
            options.remote = true;
        } else if arg == "--force" {
            options.force = true;
        } else if arg == "--notebook-channel" {
            let value = args
                .next()
                .ok_or_else(|| "--notebook-channel requires green or auto".to_owned())?;
            if value.starts_with('-') {
                return Err("--notebook-channel requires green or auto".to_owned());
            }
            options.notebook_channel = NotebookInstallChannel::parse(&value)?;
        } else if let Some(value) = arg.strip_prefix("--notebook-channel=") {
            options.notebook_channel = NotebookInstallChannel::parse(value)?;
        } else {
            options.cargo_args.push(arg);
        }
    }
    Ok(options)
}

fn install(extra: Vec<String>) -> ExitCode {
    let options = match parse_install_options(extra) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("xtask: {err}");
            return ExitCode::FAILURE;
        }
    };
    let workspace_root = workspace_root();

    if options.remote {
        let host_is_linux = cfg!(target_os = "linux");
        let dest = remote_install_dest(&workspace_root, host_is_linux, options.force);
        if let Err(err) =
            install_remote_linux_binaries(&workspace_root, &dest, options.notebook_channel)
        {
            eprintln!("xtask: {err}");
            return ExitCode::FAILURE;
        }
        report_remote_install(&dest, host_is_linux, options.notebook_channel);
        return ExitCode::SUCCESS;
    }

    if cfg!(target_os = "macos") {
        if let Err(err) = install_macos_cli(&workspace_root, options.debug, &options.cargo_args) {
            eprintln!("xtask: {err}");
            return ExitCode::FAILURE;
        }
        print_green_notebook_install_guidance();
        ExitCode::SUCCESS
    } else {
        if let Err(err) = install_linux_binaries(
            &workspace_root,
            options.debug,
            &options.cargo_args,
            options.notebook_channel,
        ) {
            eprintln!("xtask: {err}");
            return ExitCode::FAILURE;
        }
        print_green_notebook_install_guidance();
        ExitCode::SUCCESS
    }
}

fn install_macos_cli(workspace_root: &Path, debug: bool, extra: &[String]) -> Result<(), String> {
    let mut build = cargo_build_command(workspace_root, debug, &["spur-cli"], &[], extra);
    run_status(&mut build, "cargo build -p spur-cli")?;
    install_built_binary(workspace_root, debug, "spur")
}

fn install_linux_binaries(
    workspace_root: &Path,
    debug: bool,
    extra: &[String],
    notebook_channel: NotebookInstallChannel,
) -> Result<(), String> {
    let mut build = linux_install_build_command(workspace_root, debug, extra, notebook_channel);
    run_status(&mut build, linux_install_build_label(notebook_channel))?;
    install_built_binary(workspace_root, debug, "spur")?;
    Ok(())
}

fn linux_install_build_command(
    workspace_root: &Path,
    debug: bool,
    extra: &[String],
    _notebook_channel: NotebookInstallChannel,
) -> Command {
    cargo_build_command(workspace_root, debug, &["spur-cli"], &[], extra)
}

fn linux_install_build_label(_notebook_channel: NotebookInstallChannel) -> &'static str {
    "cargo build -p spur-cli"
}

/// Where `install --remote` deposits the fetched Linux release binaries.
struct RemoteInstallDest {
    dir: PathBuf,
    /// True when we steered the foreign Linux binaries into a staging directory
    /// instead of clobbering `$CARGO_HOME/bin` (the default on a non-Linux host).
    staged: bool,
}

/// Decide where the fetched Linux binaries land.
///
/// On a Linux host the binaries are native, so install them into
/// `$CARGO_HOME/bin` as usual. On any other host they are foreign ELF binaries
/// that would shadow, and break, the working native `spur`, so default to a
/// staging dir under `target/`. `--force` opts back into clobbering
/// `$CARGO_HOME/bin` (useful when the host PATH targets a Linux box over a mount).
fn remote_install_dest(
    workspace_root: &Path,
    host_is_linux: bool,
    force: bool,
) -> RemoteInstallDest {
    if host_is_linux || force {
        RemoteInstallDest {
            dir: cargo_home_bin(),
            staged: false,
        }
    } else {
        RemoteInstallDest {
            dir: workspace_root.join("target/remote-linux-bin"),
            staged: true,
        }
    }
}

fn install_remote_linux_binaries(
    workspace_root: &Path,
    dest: &RemoteInstallDest,
    notebook_channel: NotebookInstallChannel,
) -> Result<(), String> {
    let mut build = remote_install_build_command(workspace_root, notebook_channel);
    run_status(
        &mut build,
        "scripts/gcp-build/build.sh remote release build",
    )?;
    let mut fetch = remote_install_fetch_command(workspace_root, &dest.dir, notebook_channel);
    run_status(&mut fetch, remote_install_fetch_label(notebook_channel))
}

fn remote_install_build_command(
    workspace_root: &Path,
    _notebook_channel: NotebookInstallChannel,
) -> Command {
    let mut cmd = Command::new(workspace_root.join("scripts/gcp-build/build.sh"));
    cmd.arg("--auto-spin")
        .arg("--")
        .args(["build", "--release", "-p", "spur-cli"]);
    cmd.arg("--locked").current_dir(workspace_root);
    cmd
}

fn remote_install_fetch_command(
    workspace_root: &Path,
    dest_dir: &Path,
    _notebook_channel: NotebookInstallChannel,
) -> Command {
    let mut cmd = Command::new(workspace_root.join("scripts/gcp-build/fetch.sh"));
    cmd.arg("--to")
        .arg(dest_dir.join("spur"))
        .arg("target/release/spur");
    cmd.current_dir(workspace_root);
    cmd
}

fn remote_install_fetch_label(_notebook_channel: NotebookInstallChannel) -> &'static str {
    "scripts/gcp-build/fetch.sh target/release/spur"
}

fn report_remote_install(
    dest: &RemoteInstallDest,
    host_is_linux: bool,
    _notebook_channel: NotebookInstallChannel,
) {
    if dest.staged {
        let spur = dest.dir.join("spur");
        eprintln!();
        eprintln!("fetched Linux release binary to a staging dir (not $CARGO_HOME/bin):");
        eprintln!("  {}", spur.display());
        eprintln!();
        eprintln!("this is a Linux ELF binary and will not run on this host. Copy it");
        eprintln!("to a Linux box, or re-run with --force to overwrite $CARGO_HOME/bin.");
        print_green_notebook_install_guidance();
    } else {
        if !host_is_linux {
            eprintln!();
            eprintln!("warning: --force installed Linux binaries into $CARGO_HOME/bin on a");
            eprintln!("warning: non-Linux host; they will not run here. Re-run `cargo xtask");
            eprintln!("warning: install` (no --remote) to restore the native binaries.");
        }
        print_green_notebook_install_guidance();
    }
}

fn cargo_build_command(
    workspace_root: &Path,
    debug: bool,
    packages: &[&str],
    features: &[&str],
    extra: &[String],
) -> Command {
    let mut cmd = Command::new(cargo());
    cmd.arg("build").current_dir(workspace_root);
    if !debug {
        cmd.arg("--release");
    }
    for package in packages {
        cmd.arg("-p").arg(package);
    }
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    for arg in extra.iter().filter(|arg| !is_xtask_install_flag(arg)) {
        cmd.arg(arg);
    }
    apply_macos_rustc_wrapper_workaround(&mut cmd);
    cmd
}

fn is_xtask_install_flag(arg: &str) -> bool {
    matches!(arg, "--debug" | "--remote" | "--force")
        || arg == "--notebook-channel"
        || arg.starts_with("--notebook-channel=")
}

fn green_notebook_install_guidance() -> String {
    [
        "notebook channel green selected; notebook source is owned by getspur/spur-notebook.",
        "Install the standalone notebook from https://github.com/getspur/spur-notebook.",
        "Linux expects $CARGO_HOME/bin/spur-notebook or another spur-notebook on PATH.",
        "macOS expects ~/Applications/Jute.app/Contents/MacOS/Jute.",
        "Set SPUR_NOTEBOOK_CHANNEL=green and, if needed, SPUR_NOTEBOOK_BIN=/path/to/spur-notebook.",
    ]
    .join("\n")
}

fn print_green_notebook_install_guidance() {
    eprintln!();
    for line in green_notebook_install_guidance().lines() {
        eprintln!("{line}");
    }
}

fn install_built_binary(
    workspace_root: &Path,
    debug: bool,
    binary_name: &str,
) -> Result<(), String> {
    let profile = if debug { "debug" } else { "release" };
    let built_binary = workspace_root
        .join("target")
        .join(profile)
        .join(binary_name);
    if !built_binary.is_file() {
        return Err(format!(
            "expected built binary at {}",
            built_binary.display()
        ));
    }

    let bin_dir = cargo_home_bin();
    fs::create_dir_all(&bin_dir)
        .map_err(|err| format!("failed to create {}: {err}", bin_dir.display()))?;
    let installed_binary = bin_dir.join(binary_name);
    remove_existing_path(&installed_binary)?;
    fs::copy(&built_binary, &installed_binary).map_err(|err| {
        format!(
            "failed to copy {} to {}: {err}",
            built_binary.display(),
            installed_binary.display()
        )
    })?;
    eprintln!("installed {binary_name}: {}", installed_binary.display());
    Ok(())
}

fn remove_existing_path(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let result = if metadata.file_type().is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            result.map_err(|err| format!("failed to remove existing {}: {err}", path.display()))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!(
            "failed to inspect existing {}: {err}",
            path.display()
        )),
    }
}

fn apply_macos_rustc_wrapper_workaround(cmd: &mut Command) {
    if should_strip_rustc_wrapper(cfg!(target_os = "macos")) {
        // macOS Sequoia/Tahoe stamps files with com.apple.provenance based on the
        // creating process. sccache-written intermediates can then fail later rustc
        // overwrites with "Operation not permitted"; Linux keeps its wrapper.
        cmd.env_remove("RUSTC_WRAPPER");
    }
}

fn should_strip_rustc_wrapper(is_macos: bool) -> bool {
    is_macos
}

fn run_status(cmd: &mut Command, label: &str) -> Result<(), String> {
    eprintln!("==> {label}");
    let status = cmd
        .status()
        .map_err(|err| format!("failed to spawn {label}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{label} failed (status {status})"))
    }
}

fn cargo() -> PathBuf {
    env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn cargo_home_bin() -> PathBuf {
    env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .unwrap_or_else(|| PathBuf::from(".cargo"))
        .join("bin")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_build_command_includes_requested_features() {
        let root = PathBuf::from("/workspace");
        let extra = vec![
            "--locked".to_owned(),
            "--debug".to_owned(),
            "--remote".to_owned(),
        ];

        let command = cargo_build_command(
            &root,
            true,
            &["spur-cli"],
            &["spur-cli/example-feature"],
            &extra,
        );

        assert_eq!(
            command_args(&command),
            vec![
                "build".to_owned(),
                "-p".to_owned(),
                "spur-cli".to_owned(),
                "--features".to_owned(),
                "spur-cli/example-feature".to_owned(),
                "--locked".to_owned(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn remote_install_build_command_defaults_to_green_cli_only_build() {
        let root = PathBuf::from("/workspace");

        let command = remote_install_build_command(&root, NotebookInstallChannel::Auto);

        assert_eq!(
            command.get_program(),
            root.join("scripts/gcp-build/build.sh").as_os_str()
        );
        assert_eq!(
            command_args(&command),
            vec![
                "--auto-spin".to_owned(),
                "--".to_owned(),
                "build".to_owned(),
                "--release".to_owned(),
                "-p".to_owned(),
                "spur-cli".to_owned(),
                "--locked".to_owned(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn remote_install_build_command_green_builds_cli_only() {
        let root = PathBuf::from("/workspace");

        let command = remote_install_build_command(&root, NotebookInstallChannel::Green);

        assert_eq!(
            command_args(&command),
            vec![
                "--auto-spin".to_owned(),
                "--".to_owned(),
                "build".to_owned(),
                "--release".to_owned(),
                "-p".to_owned(),
                "spur-cli".to_owned(),
                "--locked".to_owned(),
            ]
        );
    }

    #[test]
    fn remote_install_fetch_command_defaults_to_green_spur_only() {
        let root = PathBuf::from("/workspace");
        let dest = PathBuf::from("/workspace/target/remote-linux-bin");

        let command = remote_install_fetch_command(&root, &dest, NotebookInstallChannel::Auto);

        assert_eq!(
            command.get_program(),
            root.join("scripts/gcp-build/fetch.sh").as_os_str()
        );
        assert_eq!(
            command_args(&command),
            vec![
                "--to".to_owned(),
                dest.join("spur").to_string_lossy().into_owned(),
                "target/release/spur".to_owned(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn remote_install_fetch_command_green_fetches_spur_only() {
        let root = PathBuf::from("/workspace");
        let dest = PathBuf::from("/workspace/target/remote-linux-bin");

        let command = remote_install_fetch_command(&root, &dest, NotebookInstallChannel::Green);
        let args = command_args(&command);

        assert_eq!(
            args,
            vec![
                "--to".to_owned(),
                dest.join("spur").to_string_lossy().into_owned(),
                "target/release/spur".to_owned(),
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--bins"));
        assert!(!args.iter().any(|arg| arg.contains("spur-notebook")));
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn remote_install_dest_uses_cargo_bin_on_linux_host() {
        let root = PathBuf::from("/workspace");

        let dest = remote_install_dest(&root, true, false);

        assert!(!dest.staged);
        assert_eq!(dest.dir, cargo_home_bin());
    }

    #[test]
    fn remote_install_dest_stages_foreign_binaries_off_linux() {
        let root = PathBuf::from("/workspace");

        let dest = remote_install_dest(&root, false, false);

        assert!(dest.staged);
        assert_eq!(dest.dir, root.join("target/remote-linux-bin"));
    }

    #[test]
    fn remote_install_dest_force_clobbers_cargo_bin_off_linux() {
        let root = PathBuf::from("/workspace");

        let dest = remote_install_dest(&root, false, true);

        assert!(!dest.staged);
        assert_eq!(dest.dir, cargo_home_bin());
    }

    #[test]
    fn force_flag_is_not_forwarded_to_cargo() {
        assert!(is_xtask_install_flag("--force"));
    }

    #[test]
    fn notebook_channel_parser_accepts_flag_and_equals_forms() {
        let parsed = parse_install_options(vec![
            "--notebook-channel".to_owned(),
            "green".to_owned(),
            "--locked".to_owned(),
        ])
        .expect("green channel should parse");

        assert_eq!(parsed.notebook_channel, NotebookInstallChannel::Green);
        assert_eq!(parsed.cargo_args, vec!["--locked".to_owned()]);

        let parsed = parse_install_options(vec!["--notebook-channel=auto".to_owned()])
            .expect("auto channel should parse");

        assert_eq!(parsed.notebook_channel, NotebookInstallChannel::Auto);
        assert!(parsed.cargo_args.is_empty());
    }

    #[test]
    fn notebook_channel_parser_defaults_to_green_and_rejects_invalid_values() {
        let parsed = parse_install_options(vec![]).expect("default options should parse");
        assert_eq!(parsed.notebook_channel, NotebookInstallChannel::Green);

        let error =
            parse_install_options(vec!["--notebook-channel".to_owned(), "purple".to_owned()])
                .expect_err("invalid channel should fail");

        assert!(error.contains("--notebook-channel"));
        assert!(error.contains("green"));
        assert!(error.contains("auto"));
    }

    #[test]
    fn notebook_channel_parser_rejects_removed_blue_source_install() {
        let error = parse_install_options(vec!["--notebook-channel=blue".to_owned()])
            .expect_err("blue channel source install should be removed");

        assert!(error.contains("blue notebook install source was removed"));
        assert!(error.contains("getspur/spur-notebook"));
    }

    #[test]
    fn linux_install_build_command_defaults_to_green_cli_only_build() {
        let root = PathBuf::from("/workspace");
        let extra = vec!["--locked".to_owned()];

        let command =
            linux_install_build_command(&root, false, &extra, NotebookInstallChannel::Auto);

        assert_eq!(
            command_args(&command),
            vec![
                "build".to_owned(),
                "--release".to_owned(),
                "-p".to_owned(),
                "spur-cli".to_owned(),
                "--locked".to_owned(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn linux_install_build_command_green_builds_cli_only() {
        let root = PathBuf::from("/workspace");
        let extra = vec!["--locked".to_owned()];

        let command =
            linux_install_build_command(&root, false, &extra, NotebookInstallChannel::Green);

        assert_eq!(
            command_args(&command),
            vec![
                "build".to_owned(),
                "--release".to_owned(),
                "-p".to_owned(),
                "spur-cli".to_owned(),
                "--locked".to_owned(),
            ]
        );
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
    }

    #[test]
    fn green_channel_reports_standalone_notebook_guidance() {
        let guidance = green_notebook_install_guidance();

        assert!(guidance.contains("SPUR_NOTEBOOK_CHANNEL=green"));
        assert!(guidance.contains("SPUR_NOTEBOOK_BIN"));
        assert!(guidance.contains("getspur/spur-notebook"));
        assert!(guidance.contains("Jute.app"));
        assert!(guidance.contains("$CARGO_HOME/bin/spur-notebook"));
    }

    #[test]
    fn rustc_wrapper_strip_is_macos_gated() {
        assert!(should_strip_rustc_wrapper(true));
        assert!(!should_strip_rustc_wrapper(false));
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }
}
