use assert_cmd::Command;
use std::fs;

fn write_config(root: &std::path::Path, contents: &str) {
    let spur_dir = root.join(".spur");
    fs::create_dir_all(&spur_dir).expect("create .spur directory");
    fs::write(spur_dir.join("config.toml"), contents).expect("write config");
}

#[test]
fn agents_command_merges_user_config_with_project_precedence() {
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");

    write_config(
        home.path(),
        r#"
[[agents.entries]]
name = "layered-codex"
command = "codex"
transport = "acp"
role = "both"
"#,
    );
    write_config(
        repo.path(),
        r#"
[[agents.entries]]
name = "layered-codex"
role = "worker"
"#,
    );

    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSE_TEST_STRIP_KEYS")
        .arg("agents")
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("layered-codex"),
        "runtime loader should inherit user agent entry; stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("Worker"),
        "project layer should override the inherited agent role; stdout:\n{stdout}",
    );
}

#[test]
fn agents_command_uses_user_config_without_project_config() {
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");

    write_config(
        home.path(),
        r#"
[[agents.entries]]
name = "user-only-codex"
command = "codex"
transport = "acp"
role = "both"
"#,
    );

    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .env_remove("SPUR_LICENSE_TEST_STRIP_KEYS")
        .arg("agents")
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("user-only-codex"),
        "runtime loader should read user-only config; stdout:\n{stdout}",
    );
}
