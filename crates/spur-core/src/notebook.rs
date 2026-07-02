use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use spur_acp::{McpServer, McpServerHttp, McpServerStdio};

const SPUR_NOTEBOOK_BIN_ENV: &str = "SPUR_NOTEBOOK_BIN";
const SPUR_NOTEBOOK_CHANNEL_ENV: &str = "SPUR_NOTEBOOK_CHANNEL";
const NOTEBOOK_BINARY_NAME: &str = "spur-notebook";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotebookChannel {
    Auto,
    Blue,
    Green,
}

impl NotebookChannel {
    pub fn parse_env_value(value: &str) -> Result<Self, NotebookResolverError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "blue" => Ok(Self::Blue),
            "green" => Ok(Self::Green),
            _ => Err(NotebookResolverError::InvalidChannel {
                value: value.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for NotebookChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Blue => f.write_str("blue"),
            Self::Green => f.write_str("green"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookLaunchSelection {
    pub channel: NotebookChannel,
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotebookResolverError {
    InvalidChannel { value: String },
    GreenUnavailable { searched_paths: Vec<PathBuf> },
}

impl std::fmt::Display for NotebookResolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidChannel { value } => write!(
                f,
                "invalid {SPUR_NOTEBOOK_CHANNEL_ENV}={value:?}; expected auto, blue, or green"
            ),
            Self::GreenUnavailable { searched_paths } => write!(
                f,
                "{SPUR_NOTEBOOK_CHANNEL_ENV}=green but no external notebook install was found; \
                 install getspur/spur-notebook or set {SPUR_NOTEBOOK_BIN_ENV} (searched: {})",
                format_searched_paths(searched_paths)
            ),
        }
    }
}

impl std::error::Error for NotebookResolverError {}

#[derive(Debug, Clone)]
struct NotebookResolverContext {
    spur_notebook_bin: Option<OsString>,
    spur_notebook_channel: Option<OsString>,
    current_exe: Option<PathBuf>,
    home: Option<PathBuf>,
    cargo_home: Option<PathBuf>,
    path: Option<OsString>,
    #[cfg(target_os = "macos")]
    system_applications_dir: PathBuf,
}

impl NotebookResolverContext {
    fn from_process() -> Self {
        Self {
            spur_notebook_bin: std::env::var_os(SPUR_NOTEBOOK_BIN_ENV),
            spur_notebook_channel: std::env::var_os(SPUR_NOTEBOOK_CHANNEL_ENV),
            current_exe: std::env::current_exe().ok(),
            home: std::env::var_os("HOME").map(PathBuf::from),
            cargo_home: std::env::var_os("CARGO_HOME").map(PathBuf::from),
            path: std::env::var_os("PATH"),
            #[cfg(target_os = "macos")]
            system_applications_dir: PathBuf::from("/Applications"),
        }
    }
}

pub fn control_socket_path(socket_nonce: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        // If HOME is unset, keep brain/notebook wiring on the current-directory fallback.
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".spur")
        .join("notebooks")
        .join("sessions")
        .join(format!("{socket_nonce}.sock"))
}

/// Derive a stable, per-workspace notebook daemon socket nonce.
///
/// All `spur` processes launched against the same `repo_root` resolve to the
/// same socket path, so a second process *attaches* to the first's notebook
/// daemon (one Jute window) instead of spawning a rival window that would
/// fork the open `.ipynb`. Distinct workspaces stay isolated. The value is
/// opaque; only stability and per-workspace uniqueness matter.
pub fn stable_notebook_nonce(repo_root: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let canonical = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(24);
    for byte in digest.iter().take(12) {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("ws-{hex}")
}

pub fn notebook_launch_selection() -> Result<NotebookLaunchSelection, NotebookResolverError> {
    notebook_launch_selection_with_context(&NotebookResolverContext::from_process())
}

fn notebook_launch_selection_with_context(
    context: &NotebookResolverContext,
) -> Result<NotebookLaunchSelection, NotebookResolverError> {
    if let Some(path) = &context.spur_notebook_bin {
        return Ok(NotebookLaunchSelection {
            channel: requested_notebook_channel(context).unwrap_or(NotebookChannel::Auto),
            path: PathBuf::from(path),
            reason: format!("{SPUR_NOTEBOOK_BIN_ENV} explicit binary override"),
        });
    }

    let requested_channel = requested_notebook_channel(context)?;
    match requested_channel {
        NotebookChannel::Green => resolve_green_notebook(context),
        NotebookChannel::Auto | NotebookChannel::Blue => Ok(resolve_blue_or_auto_notebook(context)),
    }
}

/// Legacy binary-path helper for MCP server setup.
///
/// This preserves the historical `spur-notebook` PATH fallback on resolver
/// errors. Call `notebook_launch_selection()` when a caller needs checked
/// blue/green channel errors and launch diagnostics.
pub fn notebook_binary_path() -> PathBuf {
    notebook_launch_selection()
        .map(|selection| selection.path)
        .unwrap_or_else(|_| PathBuf::from(NOTEBOOK_BINARY_NAME))
}

fn requested_notebook_channel(
    context: &NotebookResolverContext,
) -> Result<NotebookChannel, NotebookResolverError> {
    context
        .spur_notebook_channel
        .as_ref()
        .map(|value| NotebookChannel::parse_env_value(&value.to_string_lossy()))
        .transpose()
        .map(|channel| channel.unwrap_or(NotebookChannel::Auto))
}

fn resolve_blue_or_auto_notebook(context: &NotebookResolverContext) -> NotebookLaunchSelection {
    if let Some(current_exe) = &context.current_exe {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join(NOTEBOOK_BINARY_NAME);
            if sibling.exists() && should_use_sibling_notebook_binary(&sibling, context) {
                return NotebookLaunchSelection {
                    channel: NotebookChannel::Blue,
                    path: sibling,
                    reason: "blue sibling spur-notebook next to current executable".to_string(),
                };
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        for candidate in macos_jute_bundle_candidates(context) {
            if candidate.exists() {
                return NotebookLaunchSelection {
                    channel: NotebookChannel::Blue,
                    path: candidate,
                    reason: "blue legacy macOS Jute.app bundle".to_string(),
                };
            }
        }
    }

    if let Some(candidate) = cargo_home_notebook_binary(context) {
        if candidate.exists() {
            return NotebookLaunchSelection {
                channel: NotebookChannel::Blue,
                path: candidate,
                reason: "blue legacy cargo-installed spur-notebook".to_string(),
            };
        }
    }

    NotebookLaunchSelection {
        channel: NotebookChannel::Blue,
        path: PathBuf::from(NOTEBOOK_BINARY_NAME),
        reason: "blue legacy fallback to spur-notebook on PATH".to_string(),
    }
}

fn resolve_green_notebook(
    context: &NotebookResolverContext,
) -> Result<NotebookLaunchSelection, NotebookResolverError> {
    let candidates = green_notebook_candidates(context);
    for (path, reason) in &candidates {
        if path.exists() {
            return Ok(NotebookLaunchSelection {
                channel: NotebookChannel::Green,
                path: path.clone(),
                reason: reason.clone(),
            });
        }
    }

    Err(NotebookResolverError::GreenUnavailable {
        searched_paths: candidates.into_iter().map(|(path, _)| path).collect(),
    })
}

fn green_notebook_candidates(context: &NotebookResolverContext) -> Vec<(PathBuf, String)> {
    let mut candidates = Vec::new();

    #[cfg(target_os = "macos")]
    {
        candidates.extend(
            macos_spurlab_bundle_candidates(context)
                .into_iter()
                .map(|path| (path, "external macOS SpurLab.app bundle".to_string())),
        );
    }

    if let Some(path) = cargo_home_notebook_binary(context) {
        candidates.push((path, "external cargo-installed spur-notebook".to_string()));
    }

    candidates.extend(path_notebook_candidates(context).into_iter().map(|path| {
        (
            path,
            "external spur-notebook discovered on PATH".to_string(),
        )
    }));

    candidates
}

fn cargo_home_notebook_binary(context: &NotebookResolverContext) -> Option<PathBuf> {
    context
        .cargo_home
        .clone()
        .or_else(|| context.home.as_ref().map(|home| home.join(".cargo")))
        .map(|cargo_home| cargo_home.join("bin").join(NOTEBOOK_BINARY_NAME))
}

fn path_notebook_candidates(context: &NotebookResolverContext) -> Vec<PathBuf> {
    let Some(path) = &context.path else {
        return Vec::new();
    };
    if path.is_empty() {
        return Vec::new();
    }

    std::env::split_paths(path)
        .map(|dir| dir.join(NOTEBOOK_BINARY_NAME))
        .collect()
}

fn format_searched_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "none".to_string();
    }

    let mut rendered = String::new();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            rendered.push_str(", ");
        }
        rendered.push_str(&path.display().to_string());
    }
    rendered
}

#[cfg(target_os = "macos")]
fn should_use_sibling_notebook_binary(
    sibling: &std::path::Path,
    context: &NotebookResolverContext,
) -> bool {
    // A cargo-installed `spur` lives in $CARGO_HOME/bin. Treat that sibling
    // `spur-notebook` as the legacy fallback so old raw installs do not
    // preempt the bundled Jute.app path.
    cargo_home_bin(context)
        .map(|bin| sibling != bin.join("spur-notebook"))
        .unwrap_or(true)
}

#[cfg(not(target_os = "macos"))]
fn should_use_sibling_notebook_binary(
    _sibling: &std::path::Path,
    _context: &NotebookResolverContext,
) -> bool {
    true
}

#[cfg(target_os = "macos")]
fn macos_app_bundle_candidates(
    context: &NotebookResolverContext,
    bundle_relative_path: &Path,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = &context.home {
        candidates.push(home.join("Applications").join(bundle_relative_path));
    }
    candidates.push(context.system_applications_dir.join(bundle_relative_path));
    candidates
}

/// Green channel bundle: the standalone, rebranded `SpurLab` desktop app.
#[cfg(target_os = "macos")]
fn macos_spurlab_bundle_candidates(context: &NotebookResolverContext) -> Vec<PathBuf> {
    macos_app_bundle_candidates(context, &spurlab_bundle_binary_relative_path())
}

/// Blue channel bundle: the legacy in-tree `Jute` desktop app.
#[cfg(target_os = "macos")]
fn macos_jute_bundle_candidates(context: &NotebookResolverContext) -> Vec<PathBuf> {
    macos_app_bundle_candidates(context, &jute_bundle_binary_relative_path())
}

#[cfg(target_os = "macos")]
fn spurlab_bundle_binary_relative_path() -> PathBuf {
    PathBuf::from("SpurLab.app")
        .join("Contents")
        .join("MacOS")
        .join("SpurLab")
}

#[cfg(target_os = "macos")]
fn jute_bundle_binary_relative_path() -> PathBuf {
    PathBuf::from("Jute.app")
        .join("Contents")
        .join("MacOS")
        .join("Jute")
}

#[cfg(target_os = "macos")]
fn cargo_home_bin(context: &NotebookResolverContext) -> Option<PathBuf> {
    context
        .cargo_home
        .clone()
        .or_else(|| context.home.as_ref().map(|home| home.join(".cargo")))
        .map(|cargo_home| cargo_home.join("bin"))
}

pub fn notebook_mcp_server(socket_nonce: &str) -> Result<McpServer, NotebookResolverError> {
    let selection = notebook_launch_selection()?;
    Ok(McpServer::Stdio(
        McpServerStdio::new("notebook", selection.path).args(vec![
            "--mcp-proxy".to_string(),
            control_socket_path(socket_nonce).display().to_string(),
        ]),
    ))
}

pub fn brain_mcp_servers(
    spur_mcp_url: &str,
    socket_nonce: &str,
) -> Result<Vec<McpServer>, NotebookResolverError> {
    Ok(vec![
        McpServer::Http(McpServerHttp::new("spur-mcp", spur_mcp_url)),
        notebook_mcp_server(socket_nonce)?,
    ])
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
    };

    use super::*;

    fn test_context() -> NotebookResolverContext {
        NotebookResolverContext {
            spur_notebook_bin: None,
            spur_notebook_channel: None,
            current_exe: None,
            home: None,
            cargo_home: None,
            path: Some(OsString::new()),
            #[cfg(target_os = "macos")]
            system_applications_dir: PathBuf::from("/definitely/missing/applications"),
        }
    }

    #[test]
    fn notebook_binary_path_accepts_channel_values() {
        assert_eq!(
            NotebookChannel::parse_env_value("auto").expect("auto channel"),
            NotebookChannel::Auto
        );
        assert_eq!(
            NotebookChannel::parse_env_value("blue").expect("blue channel"),
            NotebookChannel::Blue
        );
        assert_eq!(
            NotebookChannel::parse_env_value("green").expect("green channel"),
            NotebookChannel::Green
        );
    }

    #[test]
    fn notebook_binary_path_rejects_invalid_channel_value() {
        let error = NotebookChannel::parse_env_value("purple").expect_err("invalid channel");

        assert!(error.to_string().contains("SPUR_NOTEBOOK_CHANNEL"));
        assert!(error.to_string().contains("auto"));
        assert!(error.to_string().contains("blue"));
        assert!(error.to_string().contains("green"));
    }

    #[test]
    fn notebook_binary_path_prefers_spur_notebook_bin_before_green_lookup() {
        let mut context = test_context();
        let override_path = PathBuf::from("/tmp/pinned-spur-notebook");
        context.spur_notebook_bin = Some(override_path.clone().into_os_string());
        context.spur_notebook_channel = Some(OsString::from("green"));

        let selection =
            notebook_launch_selection_with_context(&context).expect("bin override selection");

        assert_eq!(selection.channel, NotebookChannel::Green);
        assert_eq!(selection.path, override_path);
        assert!(selection.reason.contains("SPUR_NOTEBOOK_BIN"));
    }

    #[test]
    fn notebook_binary_path_prefers_spur_notebook_bin_before_invalid_channel() {
        let mut context = test_context();
        let override_path = PathBuf::from("/tmp/pinned-spur-notebook");
        context.spur_notebook_bin = Some(override_path.clone().into_os_string());
        context.spur_notebook_channel = Some(OsString::from("purple"));

        let selection =
            notebook_launch_selection_with_context(&context).expect("bin override selection");

        assert_eq!(selection.channel, NotebookChannel::Auto);
        assert_eq!(selection.path, override_path);
        assert!(selection.reason.contains("SPUR_NOTEBOOK_BIN"));
    }

    #[test]
    fn notebook_binary_path_green_resolves_external_cargo_install() {
        let install_root = tempfile::tempdir().expect("install root");
        let installed = install_root.path().join("bin").join("spur-notebook");
        std::fs::create_dir_all(installed.parent().unwrap()).expect("install bin dir");
        std::fs::write(&installed, "").expect("installed notebook binary");

        let mut context = test_context();
        context.spur_notebook_channel = Some(OsString::from("green"));
        context.cargo_home = Some(install_root.path().to_path_buf());

        let selection =
            notebook_launch_selection_with_context(&context).expect("green cargo selection");

        assert_eq!(selection.channel, NotebookChannel::Green);
        assert_eq!(selection.path, installed);
        assert!(selection.reason.contains("external"));
    }

    #[test]
    fn notebook_binary_path_green_resolves_external_path_install_without_source_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path_dir = temp.path().join("green-bin");
        let installed = path_dir.join("spur-notebook");
        let source_debug_dir = temp.path().join("crates/spur-notebook/target/debug");
        let current_exe = source_debug_dir.join("spur");
        let blue_sibling = source_debug_dir.join("spur-notebook");
        std::fs::create_dir_all(&path_dir).expect("path dir");
        std::fs::create_dir_all(&source_debug_dir).expect("blue source dir");
        std::fs::write(&installed, "").expect("installed notebook binary");
        std::fs::write(&current_exe, "").expect("spur binary");
        std::fs::write(&blue_sibling, "").expect("blue source notebook binary");

        let mut context = test_context();
        context.spur_notebook_channel = Some(OsString::from("green"));
        context.current_exe = Some(current_exe);
        context.path = Some(std::env::join_paths([path_dir]).expect("join PATH"));

        let selection =
            notebook_launch_selection_with_context(&context).expect("green PATH selection");

        assert_eq!(selection.channel, NotebookChannel::Green);
        assert_eq!(selection.path, installed);
        assert!(selection.reason.contains("PATH"));
        assert!(!selection
            .path
            .display()
            .to_string()
            .contains("crates/spur-notebook"));
    }

    #[test]
    fn notebook_binary_path_auto_reports_cargo_home_install_as_blue() {
        let install_root = tempfile::tempdir().expect("install root");
        let installed = install_root.path().join("bin").join("spur-notebook");
        std::fs::create_dir_all(installed.parent().unwrap()).expect("install bin dir");
        std::fs::write(&installed, "").expect("installed notebook binary");

        let mut context = test_context();
        context.cargo_home = Some(install_root.path().to_path_buf());

        let selection =
            notebook_launch_selection_with_context(&context).expect("auto cargo selection");

        assert_eq!(selection.channel, NotebookChannel::Blue);
        assert_eq!(selection.path, installed);
        assert!(selection.reason.contains("cargo-installed"));
    }

    #[test]
    fn notebook_binary_path_blue_reports_cargo_home_install_as_blue() {
        let install_root = tempfile::tempdir().expect("install root");
        let installed = install_root.path().join("bin").join("spur-notebook");
        std::fs::create_dir_all(installed.parent().unwrap()).expect("install bin dir");
        std::fs::write(&installed, "").expect("installed notebook binary");

        let mut context = test_context();
        context.spur_notebook_channel = Some(OsString::from("blue"));
        context.cargo_home = Some(install_root.path().to_path_buf());

        let selection =
            notebook_launch_selection_with_context(&context).expect("blue cargo selection");

        assert_eq!(selection.channel, NotebookChannel::Blue);
        assert_eq!(selection.path, installed);
        assert!(selection.reason.contains("cargo-installed"));
    }

    #[test]
    fn notebook_binary_path_green_missing_returns_install_error() {
        let mut context = test_context();
        context.spur_notebook_channel = Some(OsString::from("green"));

        let error =
            notebook_launch_selection_with_context(&context).expect_err("green should be missing");

        assert!(matches!(
            error,
            NotebookResolverError::GreenUnavailable { .. }
        ));
        assert!(error.to_string().contains("SPUR_NOTEBOOK_CHANNEL=green"));
        assert!(error.to_string().contains("SPUR_NOTEBOOK_BIN"));
    }

    #[test]
    fn notebook_binary_path_green_missing_error_lists_attempted_external_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cargo_home = temp.path().join("missing-cargo-home");
        let path_dir = temp.path().join("missing-path-bin");
        let cargo_candidate = cargo_home.join("bin").join("spur-notebook");
        let path_candidate = path_dir.join("spur-notebook");

        let mut context = test_context();
        context.spur_notebook_channel = Some(OsString::from("green"));
        context.cargo_home = Some(cargo_home);
        context.path = Some(std::env::join_paths([path_dir]).expect("join PATH"));

        let error =
            notebook_launch_selection_with_context(&context).expect_err("green should be missing");
        let message = error.to_string();

        assert!(message.contains("SPUR_NOTEBOOK_CHANNEL=green"));
        assert!(message.contains("install getspur/spur-notebook"));
        assert!(message.contains(&cargo_candidate.display().to_string()));
        assert!(message.contains(&path_candidate.display().to_string()));
    }

    #[test]
    fn notebook_binary_path_auto_keeps_sibling_blue_before_external_green() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current_exe = temp.path().join("bin").join("spur");
        let sibling = temp.path().join("bin").join("spur-notebook");
        let cargo_home = temp.path().join("cargo-home");
        let external = cargo_home.join("bin").join("spur-notebook");
        std::fs::create_dir_all(current_exe.parent().unwrap()).expect("bin dir");
        std::fs::create_dir_all(external.parent().unwrap()).expect("cargo bin dir");
        std::fs::write(&current_exe, "").expect("spur binary");
        std::fs::write(&sibling, "").expect("sibling notebook binary");
        std::fs::write(&external, "").expect("external notebook binary");

        let mut context = test_context();
        context.current_exe = Some(current_exe);
        context.cargo_home = Some(cargo_home);

        let selection =
            notebook_launch_selection_with_context(&context).expect("auto sibling selection");

        assert_eq!(selection.channel, NotebookChannel::Blue);
        assert_eq!(selection.path, sibling);
        assert!(selection.reason.contains("sibling"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notebook_binary_path_green_resolves_user_app_bundle_on_macos() {
        let home = tempfile::tempdir().expect("temp home");
        let bundle_binary = home
            .path()
            .join("Applications/SpurLab.app/Contents/MacOS/SpurLab");
        std::fs::create_dir_all(bundle_binary.parent().unwrap()).expect("app bundle dir");
        std::fs::write(&bundle_binary, "").expect("bundle binary");

        let mut context = test_context();
        context.spur_notebook_channel = Some(OsString::from("green"));
        context.home = Some(home.path().to_path_buf());

        let selection =
            notebook_launch_selection_with_context(&context).expect("user app bundle selection");

        assert_eq!(selection.channel, NotebookChannel::Green);
        assert_eq!(selection.path, bundle_binary);
        assert!(selection.reason.contains("app bundle"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notebook_binary_path_green_ignores_legacy_jute_app_bundle_on_macos() {
        // The green channel is the rebranded SpurLab app; a leftover legacy
        // Jute.app must NOT satisfy a green install (it would launch the old app).
        let home = tempfile::tempdir().expect("temp home");
        let legacy_bundle = home
            .path()
            .join("Applications/Jute.app/Contents/MacOS/Jute");
        std::fs::create_dir_all(legacy_bundle.parent().unwrap()).expect("app bundle dir");
        std::fs::write(&legacy_bundle, "").expect("bundle binary");

        let mut context = test_context();
        context.spur_notebook_channel = Some(OsString::from("green"));
        context.home = Some(home.path().to_path_buf());

        let error = notebook_launch_selection_with_context(&context)
            .expect_err("legacy Jute.app must not satisfy green");
        assert!(matches!(
            error,
            NotebookResolverError::GreenUnavailable { .. }
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notebook_binary_path_auto_reports_user_app_bundle_as_blue_on_macos() {
        let home = tempfile::tempdir().expect("temp home");
        let bundle_binary = home
            .path()
            .join("Applications/Jute.app/Contents/MacOS/Jute");
        std::fs::create_dir_all(bundle_binary.parent().unwrap()).expect("app bundle dir");
        std::fs::write(&bundle_binary, "").expect("bundle binary");

        let mut context = test_context();
        context.home = Some(home.path().to_path_buf());

        let selection =
            notebook_launch_selection_with_context(&context).expect("user app bundle selection");

        assert_eq!(selection.channel, NotebookChannel::Blue);
        assert_eq!(selection.path, bundle_binary);
        assert!(selection.reason.contains("app bundle"));
    }

    #[test]
    fn notebook_binary_path_legacy_path_helper_returns_selected_path() {
        let override_path = PathBuf::from("/tmp/pinned-spur-notebook");
        let _env = EnvGuard::set_notebook_bin_and_channel(&override_path, "green");

        assert_eq!(notebook_binary_path(), override_path);
    }

    #[test]
    fn notebook_mcp_server_uses_green_resolver_selected_binary() {
        let install_root = tempfile::tempdir().expect("install root");
        let installed = install_root.path().join("bin").join("spur-notebook");
        std::fs::create_dir_all(installed.parent().unwrap()).expect("install bin dir");
        std::fs::write(&installed, "").expect("installed notebook binary");
        let _env = EnvGuard::set_green_cargo_home(install_root.path());

        let server = notebook_mcp_server("green-mcp-nonce").expect("notebook MCP server");
        let McpServer::Stdio(stdio) = server else {
            panic!("notebook MCP server should use stdio");
        };

        assert_eq!(stdio.command, installed);
        assert_eq!(stdio.args[0], "--mcp-proxy");
        assert!(stdio.args[1].ends_with("/green-mcp-nonce.sock"));
    }

    #[test]
    fn notebook_mcp_server_returns_missing_green_resolver_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cargo_home = temp.path().join("missing-cargo-home");
        let path_dir = temp.path().join("missing-path-bin");
        let cargo_candidate = cargo_home.join("bin").join("spur-notebook");
        let path_candidate = path_dir.join("spur-notebook");
        let _env = EnvGuard::set_green_missing_paths(&cargo_home, &path_dir);

        let error = notebook_mcp_server("green-mcp-nonce").expect_err("green should be missing");
        let message = error.to_string();

        assert!(message.contains("SPUR_NOTEBOOK_CHANNEL=green"));
        assert!(message.contains(&cargo_candidate.display().to_string()));
        assert!(message.contains(&path_candidate.display().to_string()));
    }

    struct EnvGuard {
        previous_bin: Option<OsString>,
        previous_channel: Option<OsString>,
        previous_home: Option<OsString>,
        previous_cargo_home: Option<OsString>,
        previous_path: Option<OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set_notebook_bin_and_channel(path: &Path, channel: &str) -> Self {
            let lock = ENV_LOCK.lock().expect("env lock");
            let guard = Self {
                previous_bin: std::env::var_os("SPUR_NOTEBOOK_BIN"),
                previous_channel: std::env::var_os("SPUR_NOTEBOOK_CHANNEL"),
                previous_home: std::env::var_os("HOME"),
                previous_cargo_home: std::env::var_os("CARGO_HOME"),
                previous_path: std::env::var_os("PATH"),
                _lock: lock,
            };
            std::env::set_var("SPUR_NOTEBOOK_BIN", path);
            std::env::set_var("SPUR_NOTEBOOK_CHANNEL", channel);
            guard
        }

        fn set_green_cargo_home(cargo_home: &Path) -> Self {
            let guard = Self::capture();
            std::env::remove_var("SPUR_NOTEBOOK_BIN");
            std::env::set_var("SPUR_NOTEBOOK_CHANNEL", "green");
            std::env::set_var("HOME", cargo_home);
            std::env::set_var("CARGO_HOME", cargo_home);
            std::env::set_var("PATH", "");
            guard
        }

        fn set_green_missing_paths(cargo_home: &Path, path_dir: &Path) -> Self {
            let guard = Self::capture();
            std::env::remove_var("SPUR_NOTEBOOK_BIN");
            std::env::set_var("SPUR_NOTEBOOK_CHANNEL", "green");
            std::env::set_var("HOME", cargo_home);
            std::env::set_var("CARGO_HOME", cargo_home);
            std::env::set_var("PATH", std::env::join_paths([path_dir]).expect("join PATH"));
            guard
        }

        fn capture() -> Self {
            let lock = ENV_LOCK.lock().expect("env lock");
            Self {
                previous_bin: std::env::var_os("SPUR_NOTEBOOK_BIN"),
                previous_channel: std::env::var_os("SPUR_NOTEBOOK_CHANNEL"),
                previous_home: std::env::var_os("HOME"),
                previous_cargo_home: std::env::var_os("CARGO_HOME"),
                previous_path: std::env::var_os("PATH"),
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous_bin {
                Some(value) => std::env::set_var("SPUR_NOTEBOOK_BIN", value),
                None => std::env::remove_var("SPUR_NOTEBOOK_BIN"),
            }
            match &self.previous_channel {
                Some(value) => std::env::set_var("SPUR_NOTEBOOK_CHANNEL", value),
                None => std::env::remove_var("SPUR_NOTEBOOK_CHANNEL"),
            }
            match &self.previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match &self.previous_cargo_home {
                Some(value) => std::env::set_var("CARGO_HOME", value),
                None => std::env::remove_var("CARGO_HOME"),
            }
            match &self.previous_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
