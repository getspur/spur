use std::process::Command;
use std::sync::{Mutex, OnceLock};

static LOCK: Mutex<()> = Mutex::new(());
static TEST_HOME: OnceLock<std::path::PathBuf> = OnceLock::new();

fn test_home() -> &'static std::path::Path {
    TEST_HOME
        .get_or_init(|| {
            let path =
                std::env::temp_dir().join(format!("spur-auth-cli-test-{}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create isolated auth test home");
            path
        })
        .as_path()
}

fn spur() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_spur"));
    command
        .env("HOME", test_home())
        .env("XDG_CACHE_HOME", test_home().join(".cache"))
        .env("XDG_CONFIG_HOME", test_home().join(".config"))
        .env("XDG_DATA_HOME", test_home().join(".local/share"))
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
        !stderr.trim().is_empty(),
        "expected auth login failure to explain itself"
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
