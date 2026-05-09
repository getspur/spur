//! Implementation of `spur config set <key> <value> [--global]`.
//!
//! Supported keys include `tui.edit_mode` and `tui.disable_paste_burst`.
//! Future keys are added by extending the match in `run` with one arm each.

use anyhow::{anyhow, bail, Context, Result};
use spur_acp::config::{update_config, EditorMode};
use std::path::{Path, PathBuf};

pub fn run(repo_root: &Path, key: &str, value: &str, global: bool) -> Result<()> {
    let target = resolve_target_path(repo_root, global)?;

    match key {
        "tui.edit_mode" => {
            let mode = parse_editor_mode(value)?;
            update_config(&target, |c| {
                c.tui.edit_mode = mode;
            })
            .with_context(|| format!("update {}", target.display()))?;
            println!("Set tui.edit_mode = {value} in {}", target.display());
            println!("Takes effect on next `spur tui` invocation.");
            Ok(())
        }
        "tui.disable_paste_burst" => {
            let disabled = parse_bool(value)?;
            update_config(&target, |c| {
                c.tui.disable_paste_burst = disabled;
            })
            .with_context(|| format!("update {}", target.display()))?;
            println!(
                "Set tui.disable_paste_burst = {disabled} in {}",
                target.display()
            );
            println!("Takes effect on next `spur tui` invocation.");
            Ok(())
        }
        "tui.theme" => {
            update_config(&target, |c| {
                c.tui.theme = value.to_string();
            })
            .with_context(|| format!("update {}", target.display()))?;
            println!("Set tui.theme = {value} in {}", target.display());
            println!("Takes effect on next `spur tui` invocation.");
            Ok(())
        }
        _ => bail!("unknown key '{key}'. Supported keys: tui.edit_mode, tui.disable_paste_burst, tui.theme"),
    }
}

fn parse_editor_mode(s: &str) -> Result<EditorMode> {
    match s {
        "vim" => Ok(EditorMode::Vim),
        "emacs" => Ok(EditorMode::Emacs),
        other => bail!("invalid value '{other}' for tui.edit_mode. Expected: vim, emacs."),
    }
}

fn parse_bool(s: &str) -> Result<bool> {
    match s {
        "true" => Ok(true),
        "false" => Ok(false),
        other => {
            bail!("invalid value '{other}' for tui.disable_paste_burst. Expected: true, false.")
        }
    }
}

/// Resolve the target config path. Mirrors the load precedence in
/// `main.rs::load_config`: prefer the repo-local `.spur/config.toml`.
/// With `--global`, write to `~/.spur/config.toml` (resolved via
/// `directories::BaseDirs`, the same crate `main.rs` uses for reads).
///
/// `--global` auto-creates an empty `~/.spur/config.toml` if missing so the
/// downstream `update_config` helper (which is read-modify-write) sees a
/// readable file. Without this, a first-time `--global` call would fail with
/// a raw "No such file or directory" — contradicting the local-mode error
/// message that promises `--global` is the recovery path.
fn resolve_target_path(repo_root: &Path, global: bool) -> Result<PathBuf> {
    if global {
        let base = directories::BaseDirs::new()
            .ok_or_else(|| anyhow!("could not resolve home directory"))?;
        let path = base.home_dir().join(".spur").join("config.toml");
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap())
                .with_context(|| format!("create {}", path.parent().unwrap().display()))?;
            std::fs::write(&path, "").with_context(|| format!("create {}", path.display()))?;
        }
        return Ok(path);
    }
    let local = repo_root.join(".spur").join("config.toml");
    if local.exists() {
        return Ok(local);
    }
    bail!(
        "no .spur/config.toml found in this repo. Run `spur init` first, or pass --global to write to ~/.spur/config.toml."
    )
}
