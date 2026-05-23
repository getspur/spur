use std::path::PathBuf;

use agent_client_protocol::schema::{McpServer, McpServerHttp, McpServerStdio};

pub fn control_socket_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        // If HOME is unset, keep brain/notebook wiring on the current-directory fallback.
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".spur").join("notebooks").join("control.sock")
}

pub fn notebook_binary_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPUR_NOTEBOOK_BIN") {
        return PathBuf::from(path);
    }

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join("spur-notebook");
            if sibling.exists() && should_use_sibling_notebook_binary(&sibling) {
                return sibling;
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        for candidate in macos_jute_bundle_candidates() {
            if candidate.exists() {
                return candidate;
            }
        }

        if let Some(cargo_home_bin) = cargo_home_bin() {
            let legacy = cargo_home_bin.join("spur-notebook");
            if legacy.exists() {
                return legacy;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let cargo_bin = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
            .map(|root| root.join("bin").join("spur-notebook"));
        if let Some(path) = cargo_bin {
            if path.exists() {
                return path;
            }
        }
    }

    PathBuf::from("spur-notebook")
}

#[cfg(target_os = "macos")]
fn should_use_sibling_notebook_binary(sibling: &std::path::Path) -> bool {
    // A cargo-installed `spur` lives in $CARGO_HOME/bin. Treat that sibling
    // `spur-notebook` as the legacy fallback so old raw installs do not
    // preempt the bundled Jute.app path.
    cargo_home_bin()
        .map(|bin| sibling != bin.join("spur-notebook"))
        .unwrap_or(true)
}

#[cfg(not(target_os = "macos"))]
fn should_use_sibling_notebook_binary(_sibling: &std::path::Path) -> bool {
    true
}

#[cfg(target_os = "macos")]
fn macos_jute_bundle_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join("Applications")
                .join(jute_bundle_binary_relative_path()),
        );
    }
    candidates.push(PathBuf::from("/Applications").join(jute_bundle_binary_relative_path()));
    candidates
}

#[cfg(target_os = "macos")]
fn jute_bundle_binary_relative_path() -> PathBuf {
    PathBuf::from("Jute.app")
        .join("Contents")
        .join("MacOS")
        .join("Jute")
}

#[cfg(target_os = "macos")]
fn cargo_home_bin() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .map(|cargo_home| cargo_home.join("bin"))
}

pub fn notebook_mcp_server() -> McpServer {
    McpServer::Stdio(
        McpServerStdio::new("notebook", notebook_binary_path()).args(vec![
            "--mcp-proxy".to_string(),
            control_socket_path().display().to_string(),
        ]),
    )
}

pub fn brain_mcp_servers(spur_mcp_url: &str) -> Vec<McpServer> {
    vec![
        McpServer::Http(McpServerHttp::new("spur-mcp", spur_mcp_url)),
        notebook_mcp_server(),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        home: Option<std::ffi::OsString>,
        spur_notebook_bin: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set_home_without_notebook_bin(home: &std::path::Path) -> Self {
            let guard = Self {
                home: std::env::var_os("HOME"),
                spur_notebook_bin: std::env::var_os("SPUR_NOTEBOOK_BIN"),
            };
            std::env::set_var("HOME", home);
            std::env::remove_var("SPUR_NOTEBOOK_BIN");
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
            match &self.spur_notebook_bin {
                Some(path) => std::env::set_var("SPUR_NOTEBOOK_BIN", path),
                None => std::env::remove_var("SPUR_NOTEBOOK_BIN"),
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn notebook_binary_path_prefers_user_app_bundle_on_macos() {
        let _lock = ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().expect("temp home");
        let bundle_binary = home
            .path()
            .join("Applications/Jute.app/Contents/MacOS/Jute");
        std::fs::create_dir_all(bundle_binary.parent().unwrap()).expect("app bundle dir");
        std::fs::write(&bundle_binary, "").expect("bundle binary");

        let _env = EnvGuard::set_home_without_notebook_bin(home.path());

        assert_eq!(super::notebook_binary_path(), bundle_binary);
    }
}
