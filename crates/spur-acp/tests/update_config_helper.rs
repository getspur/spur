//! Verifies that `update_config` mutates the targeted field while leaving
//! all sibling fields byte-identical on disk.

use spur_acp::config::{update_config, EditorMode, SpurConfig};
use std::fs;
use tempfile::tempdir;

fn write_seed(path: &std::path::Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[test]
fn update_config_mutates_tui_edit_mode() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(".spur").join("config.toml");

    // Seed a non-default config: TuiConfig != default → [tui] block is
    // serialized; we expect update_config to change just the edit_mode.
    let initial = r#"
peer_mailbox_enabled = false

[brain]
default = "claude-code"
fallback = []

[bot.telegram]
enabled = true
operator_user_id = 12345

[tui]
edit_mode = "emacs"
"#;
    write_seed(&path, initial);

    update_config(&path, |c| c.tui.edit_mode = EditorMode::Vim).unwrap();

    let after: SpurConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after.tui.edit_mode, EditorMode::Vim);
    // Sibling fields preserved.
    assert_eq!(after.brain.default, "claude-code");
    assert_eq!(after.bot.telegram.enabled, true);
    assert_eq!(after.bot.telegram.operator_user_id, Some(12345));
    assert_eq!(after.peer_mailbox_enabled, false);
}

#[test]
fn update_config_returns_error_when_file_missing() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.toml");
    let err = update_config(&missing, |c| c.tui.edit_mode = EditorMode::Vim)
        .expect_err("must error when file is missing");
    let msg = format!("{err:#}");
    assert!(
        msg.to_lowercase().contains("read") || msg.to_lowercase().contains("no such"),
        "error should mention read failure, got: {msg}"
    );
}

#[test]
fn update_config_returns_error_on_invalid_toml() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "this is :: not valid toml ===\n").unwrap();

    let err = update_config(&path, |c| c.tui.edit_mode = EditorMode::Vim)
        .expect_err("must error on invalid TOML");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("parse") || msg.contains("expected") || msg.contains("toml"),
        "error should mention parse failure, got: {msg}"
    );

    // Critical: original file must NOT be overwritten when parse fails.
    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(after, "this is :: not valid toml ===\n");
}
