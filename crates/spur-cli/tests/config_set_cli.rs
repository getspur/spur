//! End-to-end coverage for `spur config set tui.edit_mode <vim|emacs>`.
//! Calls the handler directly (no spawned process) — `commands::config_set::run`
//! is library-callable.

use spur_acp::config::{EditorMode, SpurConfig};
use spur_cli::commands::config_set;
use std::fs;
use tempfile::tempdir;

fn seed_repo_config(repo: &std::path::Path, body: &str) -> std::path::PathBuf {
    let path = repo.join(".spur").join("config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn config_set_tui_edit_mode_vim_writes_file() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    seed_repo_config(
        &repo,
        r#"
peer_mailbox_enabled = false

[brain]
default = "claude-code"
fallback = []
"#,
    );

    config_set::run(&repo, "tui.edit_mode", "vim", false).expect("set must succeed");

    let path = repo.join(".spur").join("config.toml");
    let cfg: SpurConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);
    assert_eq!(cfg.brain.default, "claude-code");
}

#[test]
fn config_set_tui_edit_mode_emacs_round_trips() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    seed_repo_config(
        &repo,
        r#"
[tui]
edit_mode = "vim"
"#,
    );
    config_set::run(&repo, "tui.edit_mode", "emacs", false).unwrap();
    let cfg: SpurConfig =
        toml::from_str(&fs::read_to_string(repo.join(".spur").join("config.toml")).unwrap())
            .unwrap();
    assert_eq!(cfg.tui.edit_mode, EditorMode::Emacs);
}

#[test]
fn config_set_tui_disable_paste_burst_true_writes_file() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    seed_repo_config(&repo, "");

    config_set::run(&repo, "tui.disable_paste_burst", "true", false).unwrap();
    let cfg: SpurConfig =
        toml::from_str(&fs::read_to_string(repo.join(".spur").join("config.toml")).unwrap())
            .unwrap();
    assert!(cfg.tui.disable_paste_burst);
}

#[test]
fn config_set_invalid_value_errors_clearly() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    seed_repo_config(&repo, "");

    let err = config_set::run(&repo, "tui.edit_mode", "wim", false)
        .expect_err("invalid value must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("vim") && msg.contains("emacs"),
        "error message must hint expected values; got: {msg}"
    );
}

#[test]
fn config_set_unknown_key_errors_clearly() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    seed_repo_config(&repo, "");

    let err = config_set::run(&repo, "made.up.key", "anything", false)
        .expect_err("unknown key must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("tui.edit_mode") && msg.contains("tui.disable_paste_burst"),
        "error must list supported keys; got: {msg}"
    );
}

#[test]
fn config_set_missing_local_config_without_global_errors() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    // Note: no .spur/config.toml seeded.

    let err = config_set::run(&repo, "tui.edit_mode", "vim", false)
        .expect_err("missing local config must error without --global");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("spur init") || msg.contains("--global"),
        "error must guide user; got: {msg}"
    );
}

/// Brain-direct downstream review of T4 found a UX bug: `--global`
/// against a non-existent `~/.spur/config.toml` produced a raw
/// "No such file or directory" error, contradicting the local-mode
/// error message that promises `--global` is the recovery path.
/// Fix: `resolve_target_path` auto-creates the global file when
/// `--global` is passed and it doesn't exist. This regression-guards
/// that behavior using a tempdir as a fake home.
#[test]
fn config_set_global_auto_creates_missing_home_config() {
    use std::env;
    let fake_home = tempdir().unwrap();
    // `directories::BaseDirs` reads $HOME on Unix. Override for the test.
    let original = env::var_os("HOME");
    // Safety: tests run sequentially in the same binary by default; if this
    // test ran in parallel with another that also touches HOME, results
    // could race. Acceptable for now — there is no other home-touching test.
    env::set_var("HOME", fake_home.path());

    let repo = tempdir().unwrap();
    // Local config does NOT exist; --global is the path under test.
    let result = config_set::run(repo.path(), "tui.edit_mode", "vim", true);

    // Restore HOME before any assertion so a panic does not leak the override.
    match original {
        Some(v) => env::set_var("HOME", v),
        None => env::remove_var("HOME"),
    }

    result.expect("global set must succeed even when home config is missing");

    let global_path = fake_home.path().join(".spur").join("config.toml");
    assert!(
        global_path.exists(),
        "expected {} to be auto-created",
        global_path.display()
    );
    let cfg: SpurConfig = toml::from_str(&fs::read_to_string(&global_path).unwrap()).unwrap();
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);
}
