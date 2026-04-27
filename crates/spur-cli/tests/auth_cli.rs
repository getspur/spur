use std::process::Command;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn spur() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_spur"));
    command
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSESEAT_API_KEY")
        .env_remove("SPUR_LICENSESEAT_PRODUCT_SLUG");
    command
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
fn auth_status_reports_community_without_env() {
    let _guard = LOCK.lock().unwrap();
    let output = spur()
        .args(["auth", "status"])
        .output()
        .expect("spawn spur auth status");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("spur Community"));
    assert!(stdout.contains("free tier"));
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
    assert!(
        stderr.contains("not configured")
            || stderr.contains("license provider")
            || stderr.contains("Community tier provider cannot activate license keys directly")
    );
}

#[test]
fn auth_status_json_emits_parseable_object() {
    let _guard = LOCK.lock().unwrap();
    let output = spur()
        .args(["auth", "status", "--format", "json"])
        .output()
        .expect("spawn spur auth status --format json");
    assert!(output.status.success(), "exit status: {:?}", output.status);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("valid JSON on stdout");
    assert!(
        value.get("status").is_some(),
        "missing status field: {stdout}"
    );
    assert!(value.get("plan").is_some(), "missing plan field: {stdout}");
}
