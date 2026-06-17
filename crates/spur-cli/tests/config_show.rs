use assert_cmd::Command;

#[test]
fn config_show_defaults_only_exits_zero_and_does_not_write() {
    let home = tempfile::tempdir().expect("home tempdir");
    let repo = tempfile::tempdir().expect("repo tempdir");

    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .current_dir(repo.path())
        .env("HOME", home.path())
        .env_remove("SPUR_TELEGRAM_BOT_TOKEN")
        .args(["config", "show"])
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("# section origins:"),
        "expected section origin header; stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("<- default"),
        "expected default-origin sections; stdout:\n{stdout}",
    );
    assert!(
        stdout.contains("[brain]"),
        "expected merged config TOML; stdout:\n{stdout}",
    );
    assert!(
        !repo.path().join(".spur").exists(),
        "config show must not create repo-local .spur directory",
    );
}
