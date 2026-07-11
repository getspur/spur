use assert_cmd::Command;
use spur_acp::config::{ContextServiceAuthMode, ContextServiceConfig};
use std::fs;
use tempfile::tempdir;

fn spur() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("spur"))
}

#[test]
fn context_help_lists_auth_key_and_mcp_workflows() {
    let output = spur()
        .args(["context", "--help"])
        .output()
        .expect("spawn spur context --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for command in ["auth", "key", "mcp"] {
        assert!(stdout.contains(command), "missing {command} in:\n{stdout}");
    }
}

#[test]
fn context_key_help_exposes_safe_lifecycle_commands() {
    let output = spur()
        .args(["context", "key", "--help"])
        .output()
        .expect("spawn spur context key --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for command in ["create", "list", "use", "revoke", "add"] {
        assert!(stdout.contains(command), "missing {command} in:\n{stdout}");
    }
}

#[test]
fn context_key_add_requires_stdin_switch() {
    let output = spur()
        .args(["context", "key", "add"])
        .output()
        .expect("spawn spur context key add");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--stdin"),
        "missing safe input guidance:\n{stderr}"
    );
}

#[test]
fn normal_context_config_serializes_only_non_secret_selection() {
    let config = ContextServiceConfig {
        url: "https://context.example.test".to_owned(),
        auth_mode: ContextServiceAuthMode::ApiKey,
        profile: "workstation".to_owned(),
        public_id_hint: Some("aaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()),
        token: Some("legacy-secret-must-not-serialize".to_owned()),
    };

    let encoded = toml::to_string(&config).expect("serialize context config");
    assert!(encoded.contains("auth_mode = \"api_key\""));
    assert!(encoded.contains("profile = \"workstation\""));
    assert!(encoded.contains("public_id_hint"));
    assert!(!encoded.contains("legacy-secret-must-not-serialize"));
    assert!(!encoded.contains("token"));
}

#[test]
fn key_add_imports_stdin_without_putting_secret_in_normal_config_or_output() {
    let repo = tempdir().expect("temp repo");
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repo.path())
        .status()
        .expect("git init");
    fs::create_dir_all(repo.path().join(".spur")).expect("create .spur");
    fs::write(repo.path().join(".spur/config.toml"), "").expect("seed config");
    let credentials = repo.path().join("credentials.json");
    let secret =
        "spur_test_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let output = spur()
        .current_dir(repo.path())
        .env("SPUR_CONTEXT_CREDENTIALS_FILE", &credentials)
        .args(["context", "key", "add", "--stdin", "--profile", "imported"])
        .write_stdin(format!("{secret}\n"))
        .output()
        .expect("import key from stdin");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("aaaaaaaaaaaaaaaaaaaaaaaaaa"));
    assert!(!stdout.contains(secret));
    let config = fs::read_to_string(repo.path().join(".spur/config.toml")).expect("read config");
    assert!(config.contains("auth_mode = \"api_key\""));
    assert!(config.contains("profile = \"imported\""));
    assert!(!config.contains(secret));
    let stored = fs::read_to_string(credentials).expect("read restricted credentials");
    assert!(stored.contains(secret));
}

#[test]
fn show_secret_is_rejected_when_stdout_is_not_a_terminal() {
    let output = spur()
        .args([
            "context",
            "key",
            "create",
            "--name",
            "workstation",
            "--scope",
            "external.read",
            "--show-secret",
        ])
        .output()
        .expect("spawn non-TTY key create");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("interactive terminal"), "stderr: {stderr}");
}

#[test]
fn key_use_and_logout_are_local_and_logout_preserves_api_key() {
    let repo = tempdir().expect("temp repo");
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repo.path())
        .status()
        .expect("git init");
    fs::create_dir_all(repo.path().join(".spur")).expect("create .spur");
    fs::write(repo.path().join(".spur/config.toml"), "").expect("seed config");
    let credentials = repo.path().join("credentials.json");
    let public_id = "aaaaaaaaaaaaaaaaaaaaaaaaaa";
    let secret =
        format!("spur_test_{public_id}_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    let imported = spur()
        .current_dir(repo.path())
        .env("SPUR_CONTEXT_CREDENTIALS_FILE", &credentials)
        .args(["context", "key", "add", "--stdin", "--profile", public_id])
        .write_stdin(format!("{secret}\n"))
        .output()
        .expect("import key");
    assert!(imported.status.success());
    let before_logout = fs::read_to_string(&credentials).expect("read credentials");

    let selected = spur()
        .current_dir(repo.path())
        .env("SPUR_CONTEXT_CREDENTIALS_FILE", &credentials)
        .env("SPUR_CONTEXT_SERVICE_URL", "http://127.0.0.1:9")
        .args(["context", "key", "use", public_id])
        .output()
        .expect("select local key");
    assert!(
        selected.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&selected.stderr)
    );

    let logged_out = spur()
        .current_dir(repo.path())
        .env("SPUR_CONTEXT_CREDENTIALS_FILE", &credentials)
        .args(["context", "auth", "logout"])
        .output()
        .expect("remove management credentials");
    assert!(logged_out.status.success());
    assert_eq!(
        fs::read_to_string(&credentials).expect("read credentials after logout"),
        before_logout
    );
}

#[test]
fn api_key_cannot_authenticate_management_commands() {
    let repo = tempdir().expect("temp repo");
    std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repo.path())
        .status()
        .expect("git init");
    fs::create_dir_all(repo.path().join(".spur")).expect("create .spur");
    fs::write(repo.path().join(".spur/config.toml"), "").expect("seed config");
    let credentials = repo.path().join("credentials.json");
    let secret =
        "spur_test_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let imported = spur()
        .current_dir(repo.path())
        .env("SPUR_CONTEXT_CREDENTIALS_FILE", &credentials)
        .args(["context", "key", "add", "--stdin", "--profile", "default"])
        .write_stdin(format!("{secret}\n"))
        .output()
        .expect("import API key into default profile");
    assert!(imported.status.success());

    let managed = spur()
        .current_dir(repo.path())
        .env("SPUR_CONTEXT_CREDENTIALS_FILE", &credentials)
        .args(["context", "key", "list"])
        .output()
        .expect("attempt OAuth-only management");
    assert!(!managed.status.success());
    let stderr = String::from_utf8_lossy(&managed.stderr);
    assert!(!stderr.contains(secret), "secret leaked in error: {stderr}");
}
