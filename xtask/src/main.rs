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

    if let Err(err) = cargo_install(&workspace_root, "crates/spur-cli", debug, &extra) {
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
        if let Err(err) = cargo_install(&workspace_root, "crates/spur-notebook", debug, &extra) {
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
    extra: &[String],
) -> Result<(), String> {
    let manifest_path = workspace_root.join(crate_path);
    eprintln!("==> cargo install --path {}", manifest_path.display());
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
    for arg in extra.iter().filter(|a| a.as_str() != "--debug") {
        cmd.arg(arg);
    }
    run_status(&mut cmd, &format!("cargo install for {crate_path}"))
}

fn install_macos_jute_app(workspace_root: &Path) -> Result<PathBuf, String> {
    let jute_dir = workspace_root.join("crates/spur-notebook/jute-notebook");
    let tauri_dir = jute_dir.join("src-tauri");

    if npm_install_needed(&jute_dir) {
        let mut cmd = Command::new("npm");
        cmd.arg("install").current_dir(&jute_dir);
        run_status(&mut cmd, "npm install")?;
    } else {
        eprintln!("==> npm install skipped; node_modules and package-lock.json look current");
    }

    let mut build = Command::new("npm");
    build.arg("run").arg("build").current_dir(&jute_dir);
    run_status(&mut build, "npm run build")?;

    if tauri_uv_sidecars_missing(&tauri_dir) {
        let mut download = Command::new("python3");
        download.arg("binaries/download.py").current_dir(&tauri_dir);
        run_status(&mut download, "python3 binaries/download.py")?;
    } else {
        eprintln!("==> python3 binaries/download.py skipped; uv sidecars already exist");
    }

    let mut tauri_build = Command::new("npm");
    tauri_build
        .args(["run", "tauri", "build", "--", "--bundles", "app"])
        .current_dir(&jute_dir)
        .env_remove("TAURI_CONFIG")
        // See cargo_install: avoids the macOS provenance vs sccache collision.
        .env_remove("RUSTC_WRAPPER");
    run_status(&mut tauri_build, "npm run tauri build -- --bundles app")?;

    let built_app = workspace_root.join("target/release/bundle/macos/Jute.app");
    if !built_app.exists() {
        return Err(format!("expected built bundle at {}", built_app.display()));
    }

    build_outer_spur_notebook_binary(workspace_root)?;
    // Tauri builds the vendored upstream Jute binary from
    // crates/spur-notebook/jute-notebook/src-tauri, but that binary does not know
    // about our MCP proxy, --socket, or lazy-spawn daemon flow. Keep Tauri's .app
    // scaffolding and sidecars, then swap in the outer crates/spur-notebook binary
    // as Contents/MacOS/Jute to match CFBundleExecutable.
    replace_bundle_executable_with_outer_binary(workspace_root, &built_app)?;
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

fn build_outer_spur_notebook_binary(workspace_root: &Path) -> Result<PathBuf, String> {
    let mut cmd = Command::new(cargo());
    cmd.args([
        "build",
        "--release",
        "-p",
        "spur-notebook",
        "--bin",
        "spur-notebook",
    ])
    .current_dir(workspace_root)
    .env_remove("RUSTC_WRAPPER");
    run_status(
        &mut cmd,
        "cargo build --release -p spur-notebook --bin spur-notebook",
    )?;

    let outer_binary = outer_spur_notebook_binary(workspace_root);
    if outer_binary.is_file() {
        Ok(outer_binary)
    } else {
        Err(format!(
            "expected outer spur-notebook binary at {}",
            outer_binary.display()
        ))
    }
}

fn replace_bundle_executable_with_outer_binary(
    workspace_root: &Path,
    built_app: &Path,
) -> Result<(), String> {
    let outer_binary = outer_spur_notebook_binary(workspace_root);
    if !outer_binary.is_file() {
        return Err(format!(
            "expected outer spur-notebook binary at {}",
            outer_binary.display()
        ));
    }

    let bundle_executable = jute_bundle_executable(built_app);
    if !bundle_executable
        .parent()
        .is_some_and(|parent| parent.is_dir())
    {
        return Err(format!(
            "expected bundle executable directory at {}",
            bundle_executable.parent().unwrap_or(built_app).display()
        ));
    }

    fs::copy(&outer_binary, &bundle_executable).map_err(|err| {
        format!(
            "failed to copy {} to {}: {err}",
            outer_binary.display(),
            bundle_executable.display()
        )
    })?;

    let source_permissions = fs::metadata(&outer_binary)
        .map_err(|err| format!("failed to stat {}: {err}", outer_binary.display()))?
        .permissions();
    fs::set_permissions(&bundle_executable, source_permissions).map_err(|err| {
        format!(
            "failed to set permissions on {}: {err}",
            bundle_executable.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&bundle_executable)
            .map_err(|err| format!("failed to stat {}: {err}", bundle_executable.display()))?
            .permissions();
        let mode = permissions.mode();
        if mode & 0o111 == 0 {
            permissions.set_mode(mode | 0o111);
            fs::set_permissions(&bundle_executable, permissions).map_err(|err| {
                format!(
                    "failed to mark {} executable: {err}",
                    bundle_executable.display()
                )
            })?;
        }
    }

    eprintln!(
        "replaced bundled Jute executable with {}",
        outer_binary.display()
    );
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

fn outer_spur_notebook_binary(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target/release/spur-notebook")
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
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn swap_outer_spur_notebook_binary_copies_marker_binary_into_bundle() {
        let root = make_temp_workspace("swap-outer-binary");
        let outer_binary = root.join("target/release/spur-notebook");
        let bundle_binary = root.join("target/release/bundle/macos/Jute.app/Contents/MacOS/Jute");

        fs::create_dir_all(outer_binary.parent().unwrap()).unwrap();
        fs::create_dir_all(bundle_binary.parent().unwrap()).unwrap();
        fs::write(&outer_binary, b"outer spur-notebook --socket --mcp-proxy").unwrap();
        fs::write(&bundle_binary, b"upstream jute").unwrap();

        replace_bundle_executable_with_outer_binary(
            &root,
            &root.join("target/release/bundle/macos/Jute.app"),
        )
        .unwrap();

        let installed_bytes = fs::read(&bundle_binary).unwrap();
        assert_eq!(installed_bytes, b"outer spur-notebook --socket --mcp-proxy");
        assert!(binary_contains(&bundle_binary, b"--socket").unwrap());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&bundle_binary).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "bundle executable should be executable");
        }

        fs::remove_dir_all(root).unwrap();
    }

    fn make_temp_workspace(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("spur-xtask-{name}-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        root
    }
}
