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
    if cfg!(target_os = "macos") {
        eprintln!(
            "  install [--debug]   install spur to $CARGO_HOME/bin and Jute.app to ~/Applications"
        );
    } else {
        eprintln!("  install [--debug]   install spur and spur-notebook to $CARGO_HOME/bin");
    }
}

fn install(extra: Vec<String>) -> ExitCode {
    let debug = extra.iter().any(|a| a == "--debug");
    let workspace_root = workspace_root();

    if let Err(err) = cargo_install(&workspace_root, "crates/spur-cli", debug, &[], &extra) {
        eprintln!("xtask: {err}");
        return ExitCode::FAILURE;
    }

    if cfg!(target_os = "macos") {
        match install_macos_jute_app(&workspace_root) {
            Ok(app_path) => {
                verify_macos_install(&app_path);
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("xtask: {err}");
                ExitCode::FAILURE
            }
        }
    } else {
        let jute_dir = workspace_root.join("crates/spur-notebook/jute-notebook");
        if let Err(err) = ensure_jute_frontend_dist(&jute_dir) {
            eprintln!("xtask: {err}");
            return ExitCode::FAILURE;
        }

        if let Err(err) = cargo_install(
            &workspace_root,
            "crates/spur-notebook",
            debug,
            &["custom-protocol"],
            &extra,
        ) {
            eprintln!("xtask: {err}");
            return ExitCode::FAILURE;
        }
        verify_sibling_install();
        ExitCode::SUCCESS
    }
}

fn cargo_install(
    workspace_root: &Path,
    crate_path: &str,
    debug: bool,
    features: &[&str],
    extra: &[String],
) -> Result<(), String> {
    let manifest_path = workspace_root.join(crate_path);
    eprintln!("==> cargo install --path {}", manifest_path.display());
    let mut cmd = cargo_install_command(workspace_root, crate_path, debug, features, extra);
    run_status(&mut cmd, &format!("cargo install for {crate_path}"))
}

fn cargo_install_command(
    workspace_root: &Path,
    crate_path: &str,
    debug: bool,
    features: &[&str],
    extra: &[String],
) -> Command {
    let manifest_path = workspace_root.join(crate_path);
    let mut cmd = Command::new(cargo());
    cmd.arg("install")
        .arg("--path")
        .arg(&manifest_path)
        .arg("--force")
        // macOS Sequoia/Tahoe stamps every file with com.apple.provenance based on
        // the creating process. When sccache (configured in ~/.cargo/config.toml as
        // a global rustc-wrapper) writes intermediate artifacts, they carry sccache's
        // provenance and subsequent rustc invocations fail to overwrite them with
        // "Operation not permitted". Disable the wrapper inside install so users
        // don't have to chase the cryptic error.
        .env_remove("RUSTC_WRAPPER");
    if debug {
        cmd.arg("--debug");
    }
    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }
    for arg in extra.iter().filter(|a| a.as_str() != "--debug") {
        cmd.arg(arg);
    }
    cmd
}

fn install_macos_jute_app(workspace_root: &Path) -> Result<PathBuf, String> {
    let jute_dir = workspace_root.join("crates/spur-notebook/jute-notebook");
    let tauri_dir = jute_dir.join("src-tauri");

    ensure_jute_frontend_dist(&jute_dir)?;

    if tauri_uv_sidecars_missing(&tauri_dir) {
        let mut download = Command::new("python3");
        download.arg("binaries/download.py").current_dir(&tauri_dir);
        run_status(&mut download, "python3 binaries/download.py")?;
    } else {
        eprintln!("==> python3 binaries/download.py skipped; uv sidecars already exist");
    }

    let mut tauri_build = tauri_build_command(workspace_root);
    run_status(&mut tauri_build, "tauri build --bundles app")?;

    let built_app = workspace_root.join("target/release/bundle/macos/Jute.app");
    if !built_app.exists() {
        return Err(format!("expected built bundle at {}", built_app.display()));
    }

    resign_macos_bundle_ad_hoc(&built_app);

    let applications_dir = user_applications_dir()?;
    fs::create_dir_all(&applications_dir)
        .map_err(|err| format!("failed to create {}: {err}", applications_dir.display()))?;

    let installed_app = applications_dir.join("Jute.app");
    if installed_app.exists() {
        fs::remove_dir_all(&installed_app).map_err(|err| {
            format!(
                "failed to replace existing {}: {err}",
                installed_app.display()
            )
        })?;
    }

    copy_dir_recursive(&built_app, &installed_app).map_err(|err| {
        format!(
            "failed to copy {} to {}: {err}",
            built_app.display(),
            installed_app.display()
        )
    })?;

    eprintln!("installed Jute.app: {}", installed_app.display());
    Ok(installed_app)
}

fn tauri_build_command(workspace_root: &Path) -> Command {
    let spur_notebook_dir = workspace_root.join("crates/spur-notebook");
    let jute_dir = spur_notebook_dir.join("jute-notebook");
    let mut cmd = Command::new(jute_dir.join(tauri_cli_bin()));
    cmd.args(["build", "--bundles", "app"])
        .current_dir(spur_notebook_dir)
        .env_remove("TAURI_CONFIG")
        // See cargo_install: avoids the macOS provenance vs sccache collision.
        .env_remove("RUSTC_WRAPPER");
    cmd
}

fn tauri_cli_bin() -> PathBuf {
    let mut path = PathBuf::from("node_modules").join(".bin");
    if cfg!(windows) {
        path.push("tauri.cmd");
    } else {
        path.push("tauri");
    }
    path
}

fn ensure_jute_frontend_dist(jute_dir: &Path) -> Result<(), String> {
    if npm_install_needed(jute_dir) {
        let mut cmd = Command::new("npm");
        cmd.arg("install").current_dir(jute_dir);
        run_status(&mut cmd, "npm install")?;
    } else {
        eprintln!("==> npm install skipped; node_modules and package-lock.json look current");
    }

    let mut build = Command::new("npm");
    build.arg("run").arg("build").current_dir(jute_dir);
    run_status(&mut build, "npm run build")?;
    Ok(())
}

fn resign_macos_bundle_ad_hoc(built_app: &Path) {
    eprintln!(
        "==> codesign --force --deep --sign - {}",
        built_app.display()
    );
    match Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(built_app)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!(
                "warning: ad-hoc codesign failed for {} ({status})",
                built_app.display()
            );
        }
        Err(err) => {
            eprintln!(
                "warning: failed to spawn ad-hoc codesign for {}: {err}",
                built_app.display()
            );
        }
    }
}

fn jute_bundle_executable(app_path: &Path) -> PathBuf {
    app_path.join("Contents/MacOS/Jute")
}

fn npm_install_needed(jute_dir: &Path) -> bool {
    let node_modules = jute_dir.join("node_modules");
    let package_json = jute_dir.join("package.json");
    let package_lock = jute_dir.join("package-lock.json");

    if !node_modules.is_dir() || !package_lock.is_file() {
        return true;
    }

    let Ok(lock_modified) = package_lock.metadata().and_then(|m| m.modified()) else {
        return true;
    };
    let Ok(package_modified) = package_json.metadata().and_then(|m| m.modified()) else {
        return true;
    };

    lock_modified < package_modified
}

fn tauri_uv_sidecars_missing(tauri_dir: &Path) -> bool {
    let binaries = tauri_dir.join("binaries");
    let aarch64 = binaries.join("uv-aarch64-apple-darwin");
    let x86_64 = binaries.join("uv-x86_64-apple-darwin");
    !aarch64.is_file() || !x86_64.is_file()
}

fn copy_dir_recursive(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source, &target)?;
        } else if file_type.is_symlink() {
            copy_symlink(&source, &target)?;
        } else {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path) -> io::Result<()> {
    let link_target = fs::read_link(source)?;
    std::os::unix::fs::symlink(link_target, target)
}

#[cfg(not(unix))]
fn copy_symlink(source: &Path, target: &Path) -> io::Result<()> {
    fs::copy(source, target).map(|_| ())
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

fn verify_sibling_install() {
    let bin_dir = cargo_home_bin();
    let spur = bin_dir.join("spur");
    let notebook = bin_dir.join("spur-notebook");
    let ok = spur.exists() && notebook.exists();
    if ok {
        eprintln!();
        eprintln!("installed:");
        eprintln!("  {}", spur.display());
        eprintln!("  {}", notebook.display());
        eprintln!();
        eprintln!("sibling lookup will resolve spur-notebook automatically.");
    } else {
        eprintln!();
        eprintln!(
            "warning: expected siblings not both present in {}",
            bin_dir.display()
        );
        eprintln!("  spur:           exists={}", spur.exists());
        eprintln!("  spur-notebook:  exists={}", notebook.exists());
    }
}

fn verify_macos_install(app_path: &Path) {
    let bin_dir = cargo_home_bin();
    let spur = bin_dir.join("spur");
    let jute = jute_bundle_executable(app_path);

    eprintln!();
    eprintln!("installed:");
    eprintln!("  {}", spur.display());
    eprintln!("  {}", app_path.display());
    eprintln!();

    if spur.exists() && jute.exists() {
        eprintln!(
            "notebook lookup will resolve bundled binary: {}",
            jute.display()
        );
        match binary_contains(&jute, b"--socket") {
            Ok(true) => {
                eprintln!("verified bundled Jute executable contains --socket marker");
            }
            Ok(false) => {
                eprintln!();
                eprintln!("WARNING: bundled Jute executable does not contain --socket");
                eprintln!("WARNING: this likely means the upstream jute binary was installed");
                eprintln!("WARNING: MCP proxy lazy-spawn will not work until the bundle is fixed");
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to scan {} for --socket marker: {err}",
                    jute.display()
                );
            }
        }
    } else {
        eprintln!("warning: expected install artifacts were not all present");
        eprintln!("  spur:  exists={}", spur.exists());
        eprintln!("  Jute:  exists={}", jute.exists());
    }
}

fn binary_contains(path: &Path, needle: &[u8]) -> io::Result<bool> {
    if needle.is_empty() {
        return Ok(true);
    }

    let bytes = fs::read(path)?;
    Ok(bytes.windows(needle.len()).any(|window| window == needle))
}

fn user_applications_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Applications"))
        .ok_or_else(|| "HOME is not set; cannot install Jute.app to ~/Applications".to_string())
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
    fn cargo_install_command_includes_requested_features() {
        let root = PathBuf::from("/workspace");
        let extra = vec!["--locked".to_string(), "--debug".to_string()];

        let command = cargo_install_command(
            &root,
            "crates/spur-notebook",
            true,
            &["custom-protocol"],
            &extra,
        );

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "install".to_string(),
                "--path".to_string(),
                root.join("crates/spur-notebook")
                    .to_string_lossy()
                    .into_owned(),
                "--force".to_string(),
                "--debug".to_string(),
                "--features".to_string(),
                "custom-protocol".to_string(),
                "--locked".to_string(),
            ]
        );
    }

    #[test]
    fn tauri_build_command_runs_outer_spur_notebook_crate() {
        let root = PathBuf::from("/workspace");

        let command = tauri_build_command(&root);

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            command.get_program(),
            root.join("crates/spur-notebook/jute-notebook")
                .join(tauri_cli_bin())
                .as_os_str()
        );
        assert_eq!(
            args,
            vec![
                "build".to_string(),
                "--bundles".to_string(),
                "app".to_string()
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(root.join("crates/spur-notebook").as_path())
        );
    }
}
