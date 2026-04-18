use std::process::Command;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn spur() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spur"))
}

#[test]
fn auth_help_lists_subcommands() {
    let _guard = LOCK.lock().unwrap();
    let output = spur()
        .args(["auth", "--help"])
        .output()
        .expect("spawn spur auth --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("login"));
    assert!(stdout.contains("status"));
    assert!(stdout.contains("refresh"));
    assert!(stdout.contains("logout"));
}

#[test]
fn auth_status_reports_config_error_without_env() {
    let _guard = LOCK.lock().unwrap();
    let output = spur()
        .args(["auth", "status"])
        .output()
        .expect("spawn spur auth status");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("License status"));
    assert!(stdout.contains("ConfigError") || stdout.contains("config"));
}

#[test]
fn auth_login_requires_provider_configuration() {
    let _guard = LOCK.lock().unwrap();
    let output = spur()
        .args(["auth", "login", "--key", "test-key"])
        .output()
        .expect("spawn spur auth login");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not configured") || stderr.contains("license provider"));
}
