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
/// crate `main.rs` uses for reads).
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
