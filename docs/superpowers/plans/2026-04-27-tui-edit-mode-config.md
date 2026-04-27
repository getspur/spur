# TUI Edit Mode Config Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist the user's vim-vs-emacs edit-mode choice across `spur tui` restarts via a new `[tui]` section in `.spur/config.toml`, and add `spur config set tui.edit_mode <vim|emacs>` to write it.

**Architecture:** Adds `EditorMode` enum + `TuiConfig` struct to `spur-acp`, threads `config.tui.edit_mode` into the TUI's `App::edit_mode` initializer, and exposes persistence through a new `spur config set` subcommand backed by a generic `update_config` helper that does an atomic `tempfile + rename`. Runtime toggles (`Alt-i`, `/vim`) remain session-only and surface a divergence hint pointing at the persist command.

**Tech Stack:** Rust 2024 edition · `serde` / `toml` for config · `tempfile` for atomic writes · `directories::BaseDirs` for global-config path · `clap` for the CLI subcommand · `ratatui` for the TUI.

**Spec:** `docs/superpowers/specs/2026-04-27-tui-edit-mode-config-design.md`

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-acp/src/config/mod.rs` | Modify | Add `EditorMode` enum, `TuiConfig` struct, `SpurConfig.tui` field, `update_config<F>` helper. |
| `crates/spur-acp/tests/tui_config_section.rs` | Create | Backward-compat parse + roundtrip tests (T1, T2). |
| `crates/spur-acp/tests/update_config_helper.rs` | Create | Sibling-field preservation test for the writer (T3). |
| `crates/spur-tui/src/components/input_bar.rs` | Modify | Add `From<EditorMode> for EditMode`. |
| `crates/spur-tui/src/app.rs` | Modify | (a) Replace hardcoded `EditMode::default()` at line 374. (b) Add divergence-aware persist hint inside `Action::ToggleVimMode` handler at line 1941. |
| `crates/spur-tui/src/views/session_detail.rs` | Modify | Add `push_persist_hint(EditorMode)` method (mirrors existing `push_cost_note`). |
| `crates/spur-tui/tests/edit_mode_from_config.rs` | Create | App boot from config = vim → input bar starts in `Vim(Normal)` (T5). |
| `crates/spur-cli/src/commands/config_set.rs` | Create | New `spur config set` handler. |
| `crates/spur-cli/src/commands/mod.rs` | Modify | Register `config_set` module. |
| `crates/spur-cli/src/main.rs` | Modify | Extend `ConfigCommands` enum with `Set { key, value, --global }` variant; dispatch in match. |
| `crates/spur-cli/tests/config_set_cli.rs` | Create | End-to-end: invoke `commands::config_set::run` against a temp repo, verify file content (T4). |

**Boundary discipline:** schema types and writer live in `spur-acp::config` (where the schema lives). CLI handler in `spur-cli`. TUI consumes only — no writes from `spur-tui`. The TUI never imports `update_config`.

---

### Task 1: Add `EditorMode`, `TuiConfig`, and `SpurConfig.tui` field

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs:348-376` (extend `SpurConfig`)
- Modify: `crates/spur-acp/src/config/mod.rs` (append new types — locate insertion point near other config sub-structs, e.g., after `DelegationConfig` at line 393)
- Create: `crates/spur-acp/tests/tui_config_section.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/tui_config_section.rs`:

```rust
//! Backward-compat and round-trip coverage for the `[tui]` config section.

use spur_acp::config::{EditorMode, SpurConfig, TuiConfig};

#[test]
fn parse_config_without_tui_section_uses_emacs_default() {
    // Old config, predating the [tui] section. Must parse cleanly and yield
    // EditorMode::Emacs so existing users see zero behavior change.
    let toml = r#"
        peer_mailbox_enabled = false

        [brain]
        default = "claude-code"

        [agents]
    "#;
    let cfg: SpurConfig = toml::from_str(toml).expect("old config must parse");
    assert_eq!(cfg.tui.edit_mode, EditorMode::Emacs);
}

#[test]
fn roundtrip_tui_edit_mode_vim_preserves_value() {
    let toml = r#"
        [tui]
        edit_mode = "vim"
    "#;
    let cfg: SpurConfig = toml::from_str(toml).expect("must parse");
    assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);

    let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
    let cfg2: SpurConfig = toml::from_str(&serialized).expect("must reparse");
    assert_eq!(cfg2.tui.edit_mode, EditorMode::Vim);
}

#[test]
fn default_tui_config_is_skipped_on_serialize() {
    // Default TuiConfig must NOT emit a [tui] block — keeps existing user
    // configs visually unchanged after a round-trip through `spur init`.
    let cfg = SpurConfig::default();
    let serialized = toml::to_string_pretty(&cfg).expect("must serialize");
    assert!(
        !serialized.contains("[tui]"),
        "default config must not emit [tui] section, got:\n{serialized}"
    );
}

#[test]
fn invalid_edit_mode_value_fails_to_parse() {
    let toml = r#"
        [tui]
        edit_mode = "wim"
    "#;
    let result: Result<SpurConfig, _> = toml::from_str(toml);
    assert!(result.is_err(), "invalid value must fail to parse");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p spur-acp --test tui_config_section 2>&1 | tail -20
```

Expected: FAIL with errors like *"unresolved import `spur_acp::config::EditorMode`"* and *"no field `tui` on type `SpurConfig`"*.

- [ ] **Step 3: Add the new types and field**

In `crates/spur-acp/src/config/mod.rs`, immediately after `DelegationConfig` (around line 393) add:

```rust
/// Editing-mode preference for the TUI input bar. Stored in config so the
/// choice survives `spur tui` restarts. Maps to runtime `spur_tui::EditMode`
/// via `From` impl in `spur-tui`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    /// Emacs-style keybindings (default; uses tui-textarea built-ins).
    #[default]
    Emacs,
    /// Vim modal editing.
    Vim,
}

/// TUI presentation preferences. New fields land here without schema churn.
/// Today contains only `edit_mode`; future additions (mouse, density,
/// keymap) extend this struct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    pub edit_mode: EditorMode,
}

impl TuiConfig {
    /// True when this `TuiConfig` equals its `Default`. Used by
    /// `#[serde(skip_serializing_if = ...)]` on `SpurConfig.tui` so existing
    /// configs do not gain a noisy `[tui]` block on round-trip.
    pub fn is_default(&self) -> bool {
        *self == TuiConfig::default()
    }
}
```

In the `SpurConfig` struct (lines 348-376), add the new field. Insert it between `peer_mailbox_enabled` (line 375) and the closing brace, OR alongside other UI-adjacent fields — pick the position that reads cleanly. The exact insertion uses `Edit`:

Replace:
```rust
    /// Stage-1 worker peer mailbox feature flag. Default off until explicitly
    /// opted in by production callers.
    #[serde(default)]
    pub peer_mailbox_enabled: bool,
}
```

With:
```rust
    /// Stage-1 worker peer mailbox feature flag. Default off until explicitly
    /// opted in by production callers.
    #[serde(default)]
    pub peer_mailbox_enabled: bool,
    /// TUI presentation preferences (edit mode today; mouse/density/keymap
    /// in the future). Skipped on serialize when default to keep existing
    /// configs visually unchanged.
    #[serde(default, skip_serializing_if = "TuiConfig::is_default")]
    pub tui: TuiConfig,
}
```

- [ ] **Step 4: Verify the public re-exports**

`spur-acp/src/lib.rs` re-exports config types. Check whether `EditorMode`/`TuiConfig` need to be re-exported.

```bash
grep -n "pub use.*config" /Volumes/Projects/spur/crates/spur-acp/src/lib.rs
```

If `pub use crate::config::*` is present, no edit needed. Otherwise add `pub use config::{EditorMode, TuiConfig};` near the existing config re-exports. Use `Read` + `Edit` to make the change idempotently — do not blanket-replace.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p spur-acp --test tui_config_section 2>&1 | tail -20
```

Expected: 4 tests pass.

- [ ] **Step 6: Run the full `spur-acp` test suite to catch regressions**

```bash
cargo test -p spur-acp 2>&1 | tail -10
```

Expected: existing tests still pass; no new failures.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs crates/spur-acp/src/lib.rs crates/spur-acp/tests/tui_config_section.rs
git commit -m "feat(spur-acp): add [tui] config section with edit_mode field

Adds EditorMode { Emacs, Vim } enum and TuiConfig { edit_mode } struct.
Backward-compatible via #[serde(default)] on SpurConfig.tui and
skip_serializing_if = is_default to keep existing user configs visually
unchanged.

Foundation for the [tui] namespace; future additions (mouse, density,
keymap) extend TuiConfig without further schema churn."
```

---

### Task 2: Add `update_config<F>` helper with atomic `tempfile + rename`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` (append helper near the bottom of the file or in a logical location after the existing `impl SpurConfig` blocks)
- Create: `crates/spur-acp/tests/update_config_helper.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/update_config_helper.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p spur-acp --test update_config_helper 2>&1 | tail -20
```

Expected: FAIL with *"unresolved import `spur_acp::config::update_config`"*.

- [ ] **Step 3: Add the helper**

Append to `crates/spur-acp/src/config/mod.rs`:

```rust
/// Atomically read-modify-write a `SpurConfig` on disk.
///
/// Reads `path`, deserializes into `SpurConfig`, applies `mutate`, serializes
/// via `toml::to_string_pretty`, writes to a `NamedTempFile` in the same
/// directory, fsyncs, and atomically renames over `path`.
///
/// Errors:
/// - `path` does not exist → returns the underlying read error.
/// - `path` contains invalid TOML → returns the parse error; original file
///   is left untouched.
/// - tempfile/write/fsync/rename failures are propagated with context.
///
/// Concurrency: two concurrent callers will produce a last-rename-wins
/// outcome. This is acceptable for preference-class fields; do NOT use this
/// helper for fields requiring CAS semantics.
pub fn update_config<F>(path: &std::path::Path, mutate: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut SpurConfig),
{
    use anyhow::Context;
    use std::io::Write;

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read config at {}", path.display()))?;
    let mut cfg: SpurConfig = toml::from_str(&raw)
        .with_context(|| format!("parse config at {}", path.display()))?;
    mutate(&mut cfg);
    let serialized =
        toml::to_string_pretty(&cfg).context("serialize SpurConfig to TOML")?;

    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent: {}", path.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("create tempfile in {}", dir.display()))?;
    tmp.write_all(serialized.as_bytes())
        .context("write serialized config to tempfile")?;
    tmp.as_file()
        .sync_all()
        .context("fsync tempfile before rename")?;
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("atomic rename to {}: {}", path.display(), e.error))?;
    Ok(())
}
```

Confirm `anyhow` is in `spur-acp`'s `Cargo.toml`. Check:

```bash
grep -n "^anyhow" /Volumes/Projects/spur/crates/spur-acp/Cargo.toml
```

If missing, add `anyhow = { workspace = true }` to `[dependencies]`. (Most spur crates already have it.)

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p spur-acp --test update_config_helper 2>&1 | tail -15
```

Expected: 3 tests pass.

- [ ] **Step 5: Run `spur-acp` full suite**

```bash
cargo test -p spur-acp 2>&1 | tail -10
```

Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs crates/spur-acp/Cargo.toml crates/spur-acp/tests/update_config_helper.rs
git commit -m "feat(spur-acp): add update_config helper with atomic write

Generic read-modify-write helper. Uses tempfile::NamedTempFile in the same
directory + fsync + persist (atomic rename) so partial writes cannot leave
.spur/config.toml in a torn state. Preserves all sibling fields by
deserializing the whole SpurConfig before applying the mutation closure.

Foundation for spur config set; reusable for any future config writer
on the CLI side."
```

---

### Task 3: TUI reads `config.tui.edit_mode` at boot

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs` (add `From<EditorMode> for EditMode`)
- Modify: `crates/spur-tui/src/app.rs:374` (replace hardcoded default)
- Create: `crates/spur-tui/tests/edit_mode_from_config.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/edit_mode_from_config.rs`:

```rust
//! Verifies that the TUI seeds its EditMode from `config.tui.edit_mode`
//! at boot, not from a hardcoded default.

use spur_acp::config::EditorMode;
use spur_tui::components::input_bar::{EditMode, VimMode};

#[test]
fn editor_mode_emacs_maps_to_emacs() {
    assert_eq!(EditMode::from(EditorMode::Emacs), EditMode::Emacs);
}

#[test]
fn editor_mode_vim_maps_to_vim_normal() {
    assert_eq!(
        EditMode::from(EditorMode::Vim),
        EditMode::Vim(VimMode::Normal)
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p spur-tui --test edit_mode_from_config 2>&1 | tail -15
```

Expected: FAIL with *"the trait bound `EditMode: From<EditorMode>` is not satisfied"*.

- [ ] **Step 3: Add the `From` impl**

In `crates/spur-tui/src/components/input_bar.rs`, immediately after the `VimMode` enum (around line 64), add:

```rust
impl From<spur_acp::config::EditorMode> for EditMode {
    fn from(pref: spur_acp::config::EditorMode) -> Self {
        match pref {
            spur_acp::config::EditorMode::Emacs => EditMode::Emacs,
            spur_acp::config::EditorMode::Vim => EditMode::Vim(VimMode::Normal),
        }
    }
}
```

`EditMode` and `VimMode` are already `pub`, but verify the test can import them. If `EditMode` is `pub(crate)`, promote to `pub`. Check:

```bash
grep -n "^pub enum EditMode\|^pub(crate) enum EditMode\|^pub enum VimMode" /Volumes/Projects/spur/crates/spur-tui/src/components/input_bar.rs
```

If `pub enum`, OK. If `pub(crate)`, change to `pub` for both `EditMode` and `VimMode` — the integration test needs to construct/compare them.

- [ ] **Step 4: Run test to verify From-impl tests pass**

```bash
cargo test -p spur-tui --test edit_mode_from_config 2>&1 | tail -10
```

Expected: 2 tests pass.

- [ ] **Step 5: Replace hardcoded default at `app.rs:374`**

In `crates/spur-tui/src/app.rs`, change line 374 from:

```rust
            edit_mode: EditMode::default(),
```

to:

```rust
            edit_mode: EditMode::from(config.tui.edit_mode),
```

The surrounding struct-literal context (lines 365-381) provides `config` (the `Arc<SpurConfig>` field being set on the very next line, line 375). Confirm by reading `crates/spur-tui/src/app.rs:365-381` before editing — `config` must be in scope at line 374. If `config` is moved into the struct on line 375 BEFORE line 374's evaluation, you must reorder or use `config.tui.edit_mode` via the local variable that was passed into the function. Inspect the surrounding `App::new` (or wherever line 374 lives) to confirm.

- [ ] **Step 6: Compile-check the TUI crate**

```bash
cargo check -p spur-tui 2>&1 | tail -15
```

Expected: clean compile. If `config` was a move-then-use ordering problem, fix by reading `config.tui.edit_mode` from the function-local `config` parameter (before it's moved into the struct field).

- [ ] **Step 7: Run the TUI suite**

```bash
cargo test -p spur-tui 2>&1 | tail -10
```

Expected: all green; the new From tests already passed in step 4.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/input_bar.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/edit_mode_from_config.rs
git commit -m "feat(spur-tui): seed EditMode from config.tui.edit_mode

Adds From<spur_acp::config::EditorMode> for EditMode and replaces the
hardcoded EditMode::default() at app.rs:374 with EditMode::from(
config.tui.edit_mode). Existing users with no [tui] section see
EditorMode::Emacs (default) → EditMode::Emacs, identical to today.
Users with edit_mode = \"vim\" get Vim(Normal) at boot, persistent
across spur tui restarts."
```

---

### Task 4: `spur config set` CLI subcommand

**Files:**
- Create: `crates/spur-cli/src/commands/config_set.rs`
- Modify: `crates/spur-cli/src/commands/mod.rs` (register module)
- Modify: `crates/spur-cli/src/main.rs:309-312` (extend enum) and `:577-582` (dispatch)
- Create: `crates/spur-cli/tests/config_set_cli.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-cli/tests/config_set_cli.rs`:

```rust
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

    config_set::run(
        repo.clone(),
        "tui.edit_mode".to_string(),
        "vim".to_string(),
        false,
    )
    .expect("set must succeed");

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
    config_set::run(
        repo.clone(),
        "tui.edit_mode".to_string(),
        "emacs".to_string(),
        false,
    )
    .unwrap();
    let cfg: SpurConfig =
        toml::from_str(&fs::read_to_string(repo.join(".spur").join("config.toml")).unwrap())
            .unwrap();
    assert_eq!(cfg.tui.edit_mode, EditorMode::Emacs);
}

#[test]
fn config_set_invalid_value_errors_clearly() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    seed_repo_config(&repo, "");

    let err = config_set::run(
        repo.clone(),
        "tui.edit_mode".to_string(),
        "wim".to_string(),
        false,
    )
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

    let err = config_set::run(
        repo.clone(),
        "made.up.key".to_string(),
        "anything".to_string(),
        false,
    )
    .expect_err("unknown key must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("tui.edit_mode"),
        "error must list supported keys; got: {msg}"
    );
}

#[test]
fn config_set_missing_local_config_without_global_errors() {
    let dir = tempdir().unwrap();
    let repo = dir.path().to_path_buf();
    // Note: no .spur/config.toml seeded.

    let err = config_set::run(
        repo.clone(),
        "tui.edit_mode".to_string(),
        "vim".to_string(),
        false,
    )
    .expect_err("missing local config must error without --global");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("spur init") || msg.contains("--global"),
        "error must guide user; got: {msg}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p spur-cli --test config_set_cli 2>&1 | tail -20
```

Expected: FAIL with *"unresolved import `spur_cli::commands::config_set`"*.

- [ ] **Step 3: Verify `spur-cli` exposes `commands` via its lib**

```bash
grep -n "pub mod commands\|pub use commands" /Volumes/Projects/spur/crates/spur-cli/src/lib.rs
```

If `pub mod commands` is not present, add it. The crate has `lib.rs` (per `Cargo.toml: path = "src/lib.rs"`); the integration test imports from the library side. Add `pub mod commands;` to `lib.rs` if missing.

- [ ] **Step 4: Create the handler**

Create `crates/spur-cli/src/commands/config_set.rs`:

```rust
//! Implementation of `spur config set <key> <value> [--global]`.
//!
//! Today the only supported key is `tui.edit_mode` (values: `vim`, `emacs`).
//! Future keys are added by extending the match in `run` with one arm each.

use anyhow::{anyhow, Context, Result};
use spur_acp::config::{update_config, EditorMode};
use std::path::{Path, PathBuf};

pub fn run(repo_root: PathBuf, key: String, value: String, global: bool) -> Result<()> {
    let target = resolve_target_path(&repo_root, global)?;

    match key.as_str() {
        "tui.edit_mode" => {
            let mode = parse_editor_mode(&value)?;
            update_config(&target, |c| {
                c.tui.edit_mode = mode;
            })
            .with_context(|| format!("update {}", target.display()))?;
            println!("Set tui.edit_mode = {value} in {}", target.display());
            println!("Takes effect on next `spur tui` invocation.");
            Ok(())
        }
        _ => Err(anyhow!(
            "unknown key '{key}'. Supported keys: tui.edit_mode"
        )),
    }
}

fn parse_editor_mode(s: &str) -> Result<EditorMode> {
    match s {
        "vim" => Ok(EditorMode::Vim),
        "emacs" => Ok(EditorMode::Emacs),
        other => Err(anyhow!(
            "invalid value '{other}' for tui.edit_mode. Expected: vim, emacs."
        )),
    }
}

/// Mirrors the load precedence in `main.rs::load_config`: prefer the
/// repo-local `.spur/config.toml`. With `--global`, write to the user-level
/// `~/.spur/config.toml` (resolved via `directories::BaseDirs`, the same
/// crate `main.rs:918` uses).
fn resolve_target_path(repo_root: &Path, global: bool) -> Result<PathBuf> {
    if global {
        let base = directories::BaseDirs::new()
            .ok_or_else(|| anyhow!("could not resolve home directory"))?;
        return Ok(base.home_dir().join(".spur").join("config.toml"));
    }
    let local = repo_root.join(".spur").join("config.toml");
    if local.exists() {
        return Ok(local);
    }
    Err(anyhow!(
        "no .spur/config.toml found in this repo. Run `spur init` first, or pass --global to write to ~/.spur/config.toml."
    ))
}
```

- [ ] **Step 5: Register the module**

Edit `crates/spur-cli/src/commands/mod.rs`. Replace:

```rust
pub mod auth;
pub mod config_check;
pub mod flags;
pub mod init;
pub mod profile;
```

With:

```rust
pub mod auth;
pub mod config_check;
pub mod config_set;
pub mod flags;
pub mod init;
pub mod profile;
```

- [ ] **Step 6: Extend `ConfigCommands` enum**

In `crates/spur-cli/src/main.rs`, replace the existing enum (lines 308-312):

```rust
#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate that every [agents.entries] block has a coherent configuration.
    Check,
}
```

With:

```rust
#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate that every [agents.entries] block has a coherent configuration.
    Check,
    /// Set a configuration value (e.g., `tui.edit_mode vim`).
    Set {
        /// Dotted key path. Supported: tui.edit_mode
        key: String,
        /// Value (e.g., vim or emacs).
        value: String,
        /// Write to ~/.spur/config.toml instead of repo-local config.
        #[arg(long)]
        global: bool,
    },
}
```

- [ ] **Step 7: Wire dispatch**

In `crates/spur-cli/src/main.rs`, replace the existing match (lines 577-582):

```rust
        Commands::Config { command } => match command {
            ConfigCommands::Check => {
                let exit = commands::config_check::run(&repo_root)?;
                std::process::exit(exit);
            }
        },
```

With:

```rust
        Commands::Config { command } => match command {
            ConfigCommands::Check => {
                let exit = commands::config_check::run(&repo_root)?;
                std::process::exit(exit);
            }
            ConfigCommands::Set { key, value, global } => {
                commands::config_set::run(repo_root.clone(), key, value, global)?;
                Ok(())
            }
        },
```

The match arm returns `Result<()>`. The surrounding context already returns `Result<()>` from the same `match` (other arms do). Confirm by reading the surrounding 30 lines if uncertain.

- [ ] **Step 8: Confirm `directories` is in `spur-cli`'s deps**

```bash
grep -n "^directories" /Volumes/Projects/spur/crates/spur-cli/Cargo.toml
```

Expected: present (already used by `main.rs:918`). If absent, add `directories = { workspace = true }`.

- [ ] **Step 9: Run the new test suite**

```bash
cargo test -p spur-cli --test config_set_cli 2>&1 | tail -25
```

Expected: 5 tests pass.

- [ ] **Step 10: Smoke-test the CLI**

```bash
cd /tmp && rm -rf cfg-smoke && mkdir cfg-smoke && cd cfg-smoke && mkdir -p .spur && echo "" > .spur/config.toml && cargo run --quiet --manifest-path /Volumes/Projects/spur/Cargo.toml --bin spur -- config set tui.edit_mode vim && cat .spur/config.toml
```

Expected output ends with:
```
Set tui.edit_mode = vim in /tmp/cfg-smoke/.spur/config.toml
Takes effect on next `spur tui` invocation.
```

And the file contains:
```
[tui]
edit_mode = "vim"
```

Run again with `emacs` to verify round-trip:
```bash
cargo run --quiet --manifest-path /Volumes/Projects/spur/Cargo.toml --bin spur -- config set tui.edit_mode emacs && cat .spur/config.toml
```

Expected: `[tui]` block GONE (default emacs is skipped on serialize, per Task 1's `skip_serializing_if`). This is correct backward-compat behavior.

- [ ] **Step 11: Verify error paths from the CLI**

```bash
cd /tmp/cfg-smoke && cargo run --quiet --manifest-path /Volumes/Projects/spur/Cargo.toml --bin spur -- config set tui.edit_mode wim 2>&1 | tail -5
```

Expected: non-zero exit, message *"invalid value 'wim' for tui.edit_mode. Expected: vim, emacs."*

```bash
cd /tmp/cfg-smoke && cargo run --quiet --manifest-path /Volumes/Projects/spur/Cargo.toml --bin spur -- config set unknown.key x 2>&1 | tail -5
```

Expected: non-zero exit, message mentioning `tui.edit_mode`.

- [ ] **Step 12: Commit**

```bash
git add crates/spur-cli/src/commands/config_set.rs crates/spur-cli/src/commands/mod.rs crates/spur-cli/src/main.rs crates/spur-cli/src/lib.rs crates/spur-cli/Cargo.toml crates/spur-cli/tests/config_set_cli.rs
git commit -m "feat(spur-cli): add 'spur config set' for tui.edit_mode

Adds a new subcommand that persists tui.edit_mode (vim|emacs) to
.spur/config.toml. Supports --global to write to ~/.spur/config.toml
instead. Backed by spur_acp::config::update_config (atomic write).

Today the supported key set is just tui.edit_mode; future keys land
as additional match arms. Unknown keys and invalid values produce
clear errors guiding the user to the correct invocation."
```

---

### Task 5: Divergence-aware persist hint on runtime toggle

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (add `push_persist_hint` method near `push_cost_note` at line 957)
- Modify: `crates/spur-tui/src/app.rs:1941-1951` (extend `Action::ToggleVimMode` handler)

- [ ] **Step 1: Read the existing `push_cost_note` shape**

```bash
grep -n -B1 -A12 "pub fn push_cost_note" /Volumes/Projects/spur/crates/spur-tui/src/views/session_detail.rs
```

Note the `TraceEntry` builder pattern, the `TraceKind::Think` value, and the `Self::now_stamp()` call.

- [ ] **Step 2: Add `push_persist_hint` method**

In `crates/spur-tui/src/views/session_detail.rs`, immediately after `push_cost_note` (around line 966), add:

```rust
    /// Push a one-shot trace entry telling the user how to persist their
    /// current edit-mode choice. Called when the runtime mode diverges from
    /// the configured mode after `Alt-i` or `/vim`.
    pub fn push_persist_hint(&mut self, mode_label: &str) {
        let msg = format!(
            "{mode_label} mode (session). Persist: spur config set tui.edit_mode {}",
            mode_label.to_lowercase()
        );
        self.react_trace.push(TraceEntry {
            kind: TraceKind::Think,
            text: msg,
            timestamp: Self::now_stamp(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }
```

- [ ] **Step 3: Extend `Action::ToggleVimMode` in `app.rs:1941-1951`**

In `crates/spur-tui/src/app.rs`, replace the existing handler block:

```rust
            Action::ToggleVimMode => {
                self.edit_mode = match self.edit_mode {
                    EditMode::Emacs => EditMode::Vim(crate::components::input_bar::VimMode::Normal),
                    EditMode::Vim(_) => EditMode::Emacs,
                };
                self.dashboard.set_edit_mode(self.edit_mode);
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_edit_mode(self.edit_mode);
                }
                self.dirty = true;
            }
```

With:

```rust
            Action::ToggleVimMode => {
                self.edit_mode = match self.edit_mode {
                    EditMode::Emacs => EditMode::Vim(crate::components::input_bar::VimMode::Normal),
                    EditMode::Vim(_) => EditMode::Emacs,
                };
                self.dashboard.set_edit_mode(self.edit_mode);
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_edit_mode(self.edit_mode);

                    // Divergence hint: if the new runtime mode differs from
                    // the configured mode, tell the user how to persist.
                    let configured =
                        EditMode::from(self.config.tui.edit_mode);
                    if self.edit_mode != configured {
                        let label = match self.edit_mode {
                            EditMode::Emacs => "Emacs",
                            EditMode::Vim(_) => "Vim",
                        };
                        detail.push_persist_hint(label);
                    }
                }
                self.dirty = true;
            }
```

- [ ] **Step 4: Compile-check**

```bash
cargo check -p spur-tui 2>&1 | tail -10
```

Expected: clean compile.

- [ ] **Step 5: Run the TUI suite**

```bash
cargo test -p spur-tui 2>&1 | tail -10
```

Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): persist hint on edit-mode divergence

When the user toggles via Alt-i or /vim and the new runtime EditMode
differs from EditMode::from(config.tui.edit_mode), push a one-shot
trace note (Think kind) telling them the exact spur config set
command. Suppressed when runtime matches configured mode (the toggle
returned the user to their saved preference).

Hint is only emitted in SessionDetail view; toggling on the bare
Dashboard is intentionally silent (rare and the hint surface is
absent there)."
```

---

### Task 6: Workspace-level verification

**Files:** none modified in this task — verification only.

- [ ] **Step 1: Workspace build**

```bash
cargo build --workspace 2>&1 | tail -10
```

Expected: clean.

- [ ] **Step 2: Workspace test**

```bash
cargo test --workspace 2>&1 | tail -20
```

Expected: all green. If any unrelated tests fail, investigate before claiming done — don't paper over them.

- [ ] **Step 3: Clippy check (workspace defaults)**

```bash
cargo clippy --workspace --all-targets 2>&1 | tail -20
```

Expected: no new warnings. Address any new warnings introduced by Tasks 1–5.

- [ ] **Step 4: Manual TUI smoke**

```bash
cd /tmp/cfg-smoke && cargo run --quiet --manifest-path /Volumes/Projects/spur/Cargo.toml --bin spur -- config set tui.edit_mode vim
```

Then launch:
```bash
cd /tmp/cfg-smoke && cargo run --quiet --manifest-path /Volumes/Projects/spur/Cargo.toml --bin spur -- tui
```

Verify:
- The composer's status label shows vim mode (or the cursor is in Normal mode — letters j/k navigate without inserting).
- Press `Alt-i` to toggle to emacs. The status hint appears in the trace area: "Emacs mode (session). Persist: spur config set tui.edit_mode emacs"
- Press `Alt-i` again to return to vim. No hint (matches configured mode).
- Quit (`Ctrl-C`) and re-launch. Composer is in vim mode again. ✓

If the manual smoke fails, do not commit a "done" claim. Diagnose before proceeding.

- [ ] **Step 5: No commit** — Task 6 produces no changes; if any incidental fixes were made during Steps 1–4, commit them with an appropriate message at the time of the fix.

---

## Self-Review (filled in before handoff)

**Spec coverage:** Each spec section maps to a task:
- `EditorMode` + `TuiConfig` + `SpurConfig.tui` → Task 1
- `update_config<F>` helper with atomic write → Task 2
- `From<EditorMode> for EditMode` + `app.rs:374` rewire → Task 3
- `spur config set` CLI subcommand → Task 4
- Divergence-aware status hint → Task 5
- Test matrix T1–T5 → distributed across Tasks 1, 2, 3, 4 (T1+T2: Task 1; T3: Task 2; T5: Task 3; T4: Task 4)
- Backward compatibility (`#[serde(default)]`, `skip_serializing_if`) → Task 1's third test asserts both
- Forward-compat namespace (`[tui.keymap]` later) → no task; structural property of `TuiConfig` once it exists

**Placeholder scan:** No `TBD`, `TODO`, "appropriate error handling," or "similar to" references. All test code is full source. All file paths are absolute or repo-relative. All commands include expected output.

**Type consistency:** `EditorMode` (enum), `TuiConfig` (struct), `SpurConfig.tui` (field), `EditMode::from(EditorMode)` (impl), `update_config<F>` (helper), `commands::config_set::run` (handler), `push_persist_hint` (method) — all match across tasks.
