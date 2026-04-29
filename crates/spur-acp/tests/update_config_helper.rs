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
    assert!(after.bot.telegram.enabled);
    assert_eq!(after.bot.telegram.operator_user_id, Some(12345));
    assert!(!after.peer_mailbox_enabled);
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

/// Spec T3 (highest-value test): mutate ONLY tui.edit_mode and verify every
/// sibling sub-struct survives the read-modify-write cycle. Catches the
/// partial-write bug class where a buggy serializer or closure could
/// silently zero unrelated fields.
#[test]
fn update_config_preserves_all_sibling_substructs() {
    let dir = tempdir().unwrap();
    let path = dir.path().join(".spur").join("config.toml");

    // Seed values across every top-level sub-struct of SpurConfig
    // (except `agents.entries`, which has a deep nested schema; we cover
    // its parent and the field via separate test in T1's roundtrip suite).
    let initial = r#"
peer_mailbox_enabled = true

[brain]
default = "kimi"
fallback = ["codex", "gemini"]

[failover]
cooldown_minutes = 17

[worktree]
max_concurrent = 9
stale_cleanup_hours = 72

[cost]
db_path = "/tmp/spur-test-cost.db"

[pm.beads]
enabled = true
auto_sync = true

[bot.telegram]
enabled = true
operator_user_id = 99999

[delegation]
inline_wait_ms = 250

[spur]
auto_merge_approved_plans = true

[tui]
edit_mode = "emacs"
"#;
    write_seed(&path, initial);

    // Snapshot the parsed form before mutation so we can compare per-field.
    let before: SpurConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(before.tui.edit_mode, EditorMode::Emacs);

    update_config(&path, |c| c.tui.edit_mode = EditorMode::Vim).unwrap();

    let after: SpurConfig = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();

    // The only change.
    assert_eq!(after.tui.edit_mode, EditorMode::Vim);

    // Every sibling sub-struct's seeded fields must survive.
    assert_eq!(after.brain.default, before.brain.default);
    assert_eq!(after.brain.fallback, before.brain.fallback);
    assert_eq!(
        after.failover.cooldown_minutes,
        before.failover.cooldown_minutes
    );
    assert_eq!(
        after.worktree.max_concurrent,
        before.worktree.max_concurrent
    );
    assert_eq!(
        after.worktree.stale_cleanup_hours,
        before.worktree.stale_cleanup_hours
    );
    assert_eq!(after.cost.db_path, before.cost.db_path);
    assert_eq!(
        after.pm.beads.as_ref().map(|b| b.enabled),
        before.pm.beads.as_ref().map(|b| b.enabled)
    );
    assert_eq!(
        after.pm.beads.as_ref().map(|b| b.auto_sync),
        before.pm.beads.as_ref().map(|b| b.auto_sync)
    );
    assert_eq!(after.bot.telegram.enabled, before.bot.telegram.enabled);
    assert_eq!(
        after.bot.telegram.operator_user_id,
        before.bot.telegram.operator_user_id
    );
    assert_eq!(
        after.delegation.inline_wait_ms,
        before.delegation.inline_wait_ms
    );
    assert_eq!(
        after.spur.auto_merge_approved_plans,
        before.spur.auto_merge_approved_plans
    );
    assert_eq!(after.peer_mailbox_enabled, before.peer_mailbox_enabled);
}
