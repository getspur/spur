# TUI Edit Mode Config — Design

**Date:** 2026-04-27
**Status:** Approved (post-MCTS adversarial review)
**Scope:** Foundation for `[tui]` config namespace. Persists vim-vs-emacs choice.
**Out of scope:** Full keymap rebinding (separate follow-up spec, lands as `[tui.keymap]`).

---

## Problem

Vim mode is fully implemented in the TUI input bar (`crates/spur-tui/src/components/input_bar.rs:49-831`). Runtime toggles work via `Alt-i` and the `/vim` slash command. But the choice does **not survive a process restart** because:

- `App.edit_mode` is hardcoded to `EditMode::Emacs` at `crates/spur-tui/src/app.rs:374` — config is never consulted.
- `SpurConfig` (`crates/spur-acp/src/config/mod.rs:348-376`) has zero TUI fields. No `[tui]`, no `[keybindings]`.

Users who want vim mode have to toggle it every time they launch `spur tui`.

## Goal

Make the user's edit-mode choice persist across restarts via the configuration file. Establish the `[tui]` config namespace as the home for future TUI presentation preferences, including the eventual keymap-as-source-of-truth follow-up work.

## Non-goals

- Full keybinding rebinding (every chord configurable). Tracked separately; lands under `[tui.keymap]` in the same namespace.
- Comment-preserving TOML round-trips (would require migrating from `toml` to `toml_edit`).
- Upgrading `init.rs:163` to use atomic writes (it currently uses plain `fs::write`; can be done in a follow-up using the helper introduced here).
- Hot-reloading config while the TUI is running.
- Generic schema-aware `spur config set` for arbitrary keys. This spec adds one hardcoded match arm; future PRs add more arms.

## Architectural principle

**TUI = runtime. CLI = persistence.**

- TUI surfaces (`Alt-i`, `/vim`) toggle session state only. They never silently rewrite the config file.
- CLI surface (`spur config set`) persists. Same place `spur init` lives. Discoverable. Composable in scripts.

This matches Unix tradition (config files are written by config tools), matches `spur init`'s existing model, and avoids the "did `/vim` just rewrite my dotfiles?" surprise.

## Architecture

### Crate: `spur-acp`

`crates/spur-acp/src/config/mod.rs`:

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    #[default]
    Emacs,
    Vim,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TuiConfig {
    pub edit_mode: EditorMode,
}

impl TuiConfig {
    fn is_default(&self) -> bool { *self == TuiConfig::default() }
}

// SpurConfig gains:
//   #[serde(default, skip_serializing_if = "TuiConfig::is_default")]
//   pub tui: TuiConfig,

pub fn update_config<F>(path: &Path, mutate: F) -> Result<()>
where
    F: FnOnce(&mut SpurConfig),
{
    let raw = std::fs::read_to_string(path)?;
    let mut cfg: SpurConfig = toml::from_str(&raw)?;
    mutate(&mut cfg);
    let serialized = toml::to_string_pretty(&cfg)?;

    let dir = path.parent().ok_or_else(|| anyhow!("config path has no parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, serialized.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| anyhow!("rename failed: {}", e.error))?;
    Ok(())
}
```

### Crate: `spur-tui`

`crates/spur-tui/src/components/input_bar.rs` — add config-to-runtime mapping:

```rust
impl From<spur_acp::config::EditorMode> for EditMode {
    fn from(pref: spur_acp::config::EditorMode) -> Self {
        match pref {
            EditorMode::Emacs => EditMode::Emacs,
            EditorMode::Vim   => EditMode::Vim(VimMode::Normal),
        }
    }
}
```

`crates/spur-tui/src/app.rs:374` — replace hardcoded default:

```rust
// before
edit_mode: EditMode::default(),
// after
edit_mode: EditMode::from(config.tui.edit_mode),
```

**Status hint on divergence.** The `Action::ToggleVimMode` handler at `crates/spur-tui/src/app.rs:1941` is the single dispatch site for both `Alt-i` (chord) and `/vim` (slash command). After it flips `app.edit_mode`, if the new runtime mode differs from `EditMode::from(config.tui.edit_mode)`, surface a one-shot status message via the existing `StatusBar` component:

> Vim mode (session). Persist: `spur config set tui.edit_mode vim`

When the runtime mode matches the configured mode, suppress the hint. This is contextually correct: it appears only when the user has diverged from their saved preference.

**`Action::ToggleVimMode` is unchanged.** It does not write to disk. Both `Alt-i` and `/vim` continue to dispatch this action, session-only.

### Crate: `spur-cli`

New file `crates/spur-cli/src/commands/config_set.rs`:

```rust
pub fn run(repo_root: PathBuf, key: String, value: String, global: bool) -> Result<()> {
    let target = resolve_target_path(&repo_root, global)?;

    match key.as_str() {
        "tui.edit_mode" => {
            let mode = parse_editor_mode(&value)?;
            spur_acp::config::update_config(&target, |c| {
                c.tui.edit_mode = mode;
            })?;
            println!("Set tui.edit_mode = {value} in {}", target.display());
            Ok(())
        }
        _ => Err(anyhow!(
            "unknown key '{key}'. Supported: tui.edit_mode"
        )),
    }
}

fn parse_editor_mode(s: &str) -> Result<EditorMode> {
    match s {
        "vim"   => Ok(EditorMode::Vim),
        "emacs" => Ok(EditorMode::Emacs),
        other   => Err(anyhow!(
            "invalid value '{other}' for tui.edit_mode. Expected: vim, emacs."
        )),
    }
}

fn resolve_target_path(repo_root: &Path, global: bool) -> Result<PathBuf> {
    // Mirrors the load precedence in main.rs:916-932 — uses `directories::BaseDirs`
    // for cross-platform home discovery (already in the dep tree).
    if global {
        let base = directories::BaseDirs::new()
            .ok_or_else(|| anyhow!("could not resolve home dir"))?;
        return Ok(base.home_dir().join(".spur").join("config.toml"));
    }
    let local = repo_root.join(".spur").join("config.toml");
    if local.exists() {
        return Ok(local);
    }
    Err(anyhow!(
        "no .spur/config.toml found in this repo. Run `spur init` first, \
         or pass --global to write to ~/.spur/config.toml."
    ))
}
```

`crates/spur-cli/src/main.rs:309` — extend the `ConfigCommands` enum:

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

And in the dispatch match (`main.rs:577-582`):

```rust
ConfigCommands::Set { key, value, global } => {
    commands::config_set::run(repo_root, key, value, global)?;
    Ok(())
}
```

## Data flow

**Boot path:**

1. `spur tui` invokes `load_config()` (`main.rs:915-932`), returning `SpurConfig` parsed from `.spur/config.toml` or `~/.spur/config.toml`.
2. `App::new` receives `Arc<SpurConfig>`. Reads `config.tui.edit_mode`.
3. `App.edit_mode` initialized via `EditMode::from(config.tui.edit_mode)`.
4. `InputBar` inherits `app.edit_mode` at construction.

**Persist path:**

1. User runs `spur config set tui.edit_mode vim`.
2. CLI resolves target path (repo-local by default; `--global` opts into home).
3. `update_config` reads the file, mutates the `tui.edit_mode` field via closure, writes to a `NamedTempFile` in the same directory, `fsync`s, and atomically renames over the target.
4. Other config fields (agents, brain, bot, etc.) round-trip untouched.
5. CLI prints confirmation. Exit 0.

**Runtime toggle path (existing behavior, new feedback):**

1. User presses `Alt-i` or types `/vim` in the TUI.
2. `Action::ToggleVimMode` dispatches as today. `app.edit_mode` flips. `InputBar` rebinds keys.
3. **New:** if the post-toggle `app.edit_mode` differs from `EditMode::from(app.config.tui.edit_mode)`, the status bar shows a one-shot hint with the exact `spur config set` command. If they match, no hint.
4. No file write. Restart restores `config.tui.edit_mode`.

## Error handling

| Failure mode | Behavior |
|---|---|
| `spur config set` with no repo config and no `--global` | Error with hint to run `spur init` or pass `--global`. |
| `spur config set tui.edit_mode foo` | Error: *"invalid value 'foo' for tui.edit_mode. Expected: vim, emacs."* Exit non-zero. |
| `spur config set unknown.key value` | Error: *"unknown key 'unknown.key'. Supported: tui.edit_mode"*. Exit non-zero. |
| Tempfile create fails (perms, disk full) | Error from `tempfile::NamedTempFile::new_in`, propagated with context. No partial write. |
| Cross-filesystem rename | `NamedTempFile::new_in(dir)` guarantees same-FS placement. Cannot occur. |
| Existing config has invalid TOML | `update_config` returns parse error before mutating. Original file untouched. User can repair via `spur init`. |
| Concurrent `spur config set` invocations | Last-rename-wins. Acceptable for a preference. No file locking. |
| TUI open during CLI write | TUI's `Arc<SpurConfig>` is stale; no hot-reload. Restart picks up new value. **Documented behavior, not a bug.** |

## Backward compatibility

- `SpurConfig.tui` is `#[serde(default)]`. Configs without `[tui]` parse cleanly to `TuiConfig::default() = { edit_mode: Emacs }`.
- `EditMode::from(EditorMode::Emacs) == EditMode::Emacs`, identical to today's hardcoded default at `app.rs:374`. Zero behavior change for existing users.
- `#[serde(skip_serializing_if = "TuiConfig::is_default")]` on `SpurConfig.tui` ensures that round-tripping an old config through `spur init` does NOT inject a noisy `[tui]` block. The section appears in the file only after the user explicitly configures something non-default.
- `spur config check` does not need new validation logic — invalid `edit_mode` values fail at TOML parse time with a clear serde error.

## Forward compatibility

The `[tui]` namespace is now established. Future presentation prefs slot in without schema churn:

```toml
[tui]
edit_mode = "vim"
mouse_enabled = true       # future
density = "compact"        # future
palette_key = "ctrl-k"     # future

[tui.keymap]               # future, lands in the "Keymap SoT" follow-up spec
"Ctrl-p" = "history.prev"
"Alt-i"  = "edit_mode.toggle"
```

The existing prior spec `docs/superpowers/specs/2026-04-19-spur-tui-ux-best-approach.md:365` flags Keymap SoT as P0 future work. This design is the foundation for it.

## Tests

| ID | Crate | Description |
|---|---|---|
| T1 | `spur-acp` | `parse_config_without_tui_section_uses_emacs_default` — backward-compat parse + verify default. |
| T2 | `spur-acp` | `roundtrip_tui_edit_mode_vim` — parse `[tui] edit_mode = "vim"`, serialize, parse again, value preserved. |
| T3 | `spur-acp` | `update_config_preserves_sibling_fields` — read config with bot/pm/agents/brain populated, mutate only `tui.edit_mode`, write. Verify all sibling fields byte-identical to their pre-mutation serialized form. **Highest-value test.** |
| T4 | `spur-cli` | `config_set_tui_edit_mode_vim_writes_file` — invoke `commands::config_set::run` against a temp repo with a seeded `.spur/config.toml`. Assert resulting file parses with `tui.edit_mode == EditorMode::Vim`. |
| T5 | `spur-tui` | `app_init_from_config_vim_starts_in_vim_normal` — synthesize `SpurConfig { tui: TuiConfig { edit_mode: Vim }, .. }`, build `App`, assert input bar starts in `EditMode::Vim(VimMode::Normal)`. |

T3 catches the partial-write bug class (where a buggy serializer or closure inadvertently zeroes other fields). T4 is the smallest end-to-end check. T1 is the regression-protection for every existing user's config.

## Effort estimate

- ~80 LoC in `spur-acp/src/config/mod.rs` (new types + helper).
- ~10 LoC in `spur-tui/src/components/input_bar.rs` (`From` impl).
- ~3 LoC in `spur-tui/src/app.rs` (replace hardcoded default).
- ~30 LoC at the existing `Action::ToggleVimMode` handler (`spur-tui/src/app.rs:1941`) — divergence-aware status hint dispatched via the existing `StatusBar` component (`spur-tui/src/components/status_bar.rs`).
- ~70 LoC in new `spur-cli/src/commands/config_set.rs`.
- ~10 LoC in `spur-cli/src/main.rs` (subcommand wiring).
- ~5 tests, ~150 LoC.

**Total: ~350 LoC. One day for an experienced Rust engineer.**

## Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Forgotten `#[serde(default)]` on `SpurConfig.tui` | Low | Old configs fail to parse on upgrade | T1 covers this directly. |
| `update_config` partial-writes due to bug in closure | Low | User loses unrelated config fields | T3 covers this directly. Atomic rename ensures all-or-nothing on disk. |
| `From<EditorMode> for EditMode` drifts as `EditMode` evolves | Low | Mode mismatch on boot | T5 covers boot-time mapping. |
| Status-bar hint becomes annoying | Medium | UX regression | Hint is divergence-conditional; matches user's actual state. Show transient (auto-clear) not persistent. |
| User runs `spur config set` while TUI is open and is confused that the TUI hasn't updated | Medium | UX confusion | Document in the help text: *"Takes effect on next `spur tui` invocation."* |

## Open questions

None. Seven rounds of MCTS adversarial review converged on this design with no remaining ambiguity. Ready for implementation planning.

## References

- `crates/spur-acp/src/config/mod.rs:348-376` — `SpurConfig` definition.
- `crates/spur-tui/src/app.rs:374` — current hardcoded `EditMode::Emacs`.
- `crates/spur-tui/src/components/input_bar.rs:49-64` — `EditMode` and `VimMode` enums.
- `crates/spur-tui/src/components/input_bar.rs:413-831` — full vim implementation.
- `crates/spur-tui/src/commands/spur_local.rs:94-101` — existing `/vim` slash command.
- `crates/spur-cli/src/main.rs:308-312` — existing `ConfigCommands::Check` subcommand.
- `crates/spur-cli/src/commands/init.rs:41-54` — convergence philosophy (config is mutable, owned by user).
- `docs/superpowers/specs/2026-04-19-spur-tui-ux-best-approach.md:365` — prior "Keymap SoT (P0)" note that this spec creates the foundation for.
