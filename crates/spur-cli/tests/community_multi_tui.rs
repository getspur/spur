//! Community TUI processes must not be serialized by the legacy repo pidfile.

use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const LEGACY_SINGLETON_REJECTION: &str = "Another SPUR TUI is already running on this repo.";

#[test]
fn community_tui_ignores_legacy_singleton_pidfile() {
    let repo = TempDir::new().expect("create isolated repository");
    let spur_dir = repo.path().join(".spur");
    std::fs::create_dir_all(&spur_dir).expect("create .spur directory");

    let lock_path = spur_dir.join(".spur-tui.pid");
    let _legacy_guard =
        spur_pm::pidfile::PidFileGuard::acquire(&lock_path).expect("hold legacy TUI pidfile");

    let home = TempDir::new().expect("create isolated home");
    let mut child = Command::new(env!("CARGO_BIN_EXE_spur"))
        .args(["tui", "--new"])
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSESEAT_API_KEY")
        .env_remove("SPUR_LICENSESEAT_PRODUCT_SLUG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn Community TUI");

    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().expect("poll Community TUI").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop headless TUI after startup");
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let output = child
        .wait_with_output()
        .expect("collect Community TUI output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains(LEGACY_SINGLETON_REJECTION),
        "Community TUI was rejected by the legacy singleton lock:\n{stderr}"
    );
}
