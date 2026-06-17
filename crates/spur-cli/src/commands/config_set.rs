//! Implementation of `spur config set <key> <value> [--global]`.
//!
//! Supported keys include `tui.edit_mode`, `tui.disable_paste_burst`, and `tui.theme`.
//! Future keys are added by extending the match in `run` with one arm each.

use anyhow::{anyhow, bail, Context, Result};
use spur_acp::config::{layered::set_key_path, EditorMode};
use std::io::Write;
use std::path::{Path, PathBuf};
use toml::value::Table;

pub fn run(repo_root: &Path, key: &str, value: &str, global: bool) -> Result<()> {
    let target = resolve_target_path(repo_root, global)?;

    let (toml_value, printed_value) = match key {
        "tui.edit_mode" => {
            parse_editor_mode(value)?;
            (toml::Value::String(value.to_string()), value.to_string())
        }
        "tui.disable_paste_burst" => {
            let disabled = parse_bool(value)?;
            (toml::Value::Boolean(disabled), disabled.to_string())
        }
        "tui.theme" => {
            (toml::Value::String(value.to_string()), value.to_string())
        }
        _ => bail!("unknown key '{key}'. Supported keys: tui.edit_mode, tui.disable_paste_burst, tui.theme"),
    };

    set_config_value(&target, key, toml_value)
        .with_context(|| format!("update {}", target.display()))?;
    println!("Set {key} = {printed_value} in {}", target.display());
    println!("Takes effect on next `spur tui` invocation.");
    Ok(())
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

fn set_config_value(target: &Path, key: &str, value: toml::Value) -> Result<()> {
    let mut table = read_target_table(target)?;
    let path_segments = key.split('.').collect::<Vec<_>>();
    set_key_path(&mut table, &path_segments, value);
    write_table_atomic(target, table)
}

fn read_target_table(target: &Path) -> Result<Table> {
    if !target.exists() {
        return Ok(Table::new());
    }

    let raw = std::fs::read_to_string(target)
        .with_context(|| format!("read config at {}", target.display()))?;
    match toml::from_str::<toml::Value>(&raw)
        .with_context(|| format!("parse config at {}", target.display()))?
    {
        toml::Value::Table(table) => Ok(table),
        _ => Ok(Table::new()),
    }
}

fn write_table_atomic(path: &Path, table: Table) -> Result<()> {
    let serialized =
        toml::to_string_pretty(&toml::Value::Table(table)).context("serialize config TOML")?;

    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;
    let (tmp_path, mut tmp) = create_temp_file(dir, path)?;
    if let Err(err) = tmp
        .write_all(serialized.as_bytes())
        .context("write serialized config to tempfile")
        .and_then(|_| tmp.sync_all().context("fsync tempfile before rename"))
    {
        drop(tmp);
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    drop(tmp);
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("atomic rename to {}", path.display()));
    }

    #[cfg(unix)]
    std::fs::File::open(dir)
        .with_context(|| format!("open config directory {} for fsync", dir.display()))?
        .sync_all()
        .with_context(|| format!("fsync config directory {}", dir.display()))?;
    Ok(())
}

fn create_temp_file(dir: &Path, target: &Path) -> Result<(PathBuf, std::fs::File)> {
    let target_name = target
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "config.toml".into());
    let pid = std::process::id();
    for attempt in 0..100 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!(".{target_name}.{pid}.{nanos}.{attempt}.tmp"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("create tempfile in {}", dir.display()));
            }
        }
    }
    bail!("failed to create unique tempfile in {}", dir.display())
}

/// Resolve the target config path. Mirrors the load precedence in
/// `main.rs::load_config`: prefer the repo-local `.spur/config.toml`.
/// With `--global`, write to `~/.spur/config.toml` (resolved via
/// `directories::BaseDirs`, the same crate `main.rs` uses for reads).
///
/// `--global` auto-creates an empty `~/.spur/config.toml` if missing. Without
/// this, a first-time `--global` call would fail with a raw "No such file or
/// directory" — contradicting the local-mode error message that promises
/// `--global` is the recovery path.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn config_set_preserves_sparse_file_when_setting_one_key() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        let path = repo.join(".spur").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "[tui]\ntheme = 'dark'\n").unwrap();

        run(repo, "tui.disable_paste_burst", "true", false).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[tui]"), "expected tui table in:\n{raw}");
        assert!(
            raw.contains("theme") && raw.contains("dark"),
            "expected existing sparse key to remain in:\n{raw}"
        );
        assert!(
            raw.contains("disable_paste_burst") && raw.contains("true"),
            "expected new key in:\n{raw}"
        );
        assert!(!raw.contains("[brain]"), "must not re-expand brain:\n{raw}");
        assert!(
            !raw.contains("[worktree]"),
            "must not re-expand worktree:\n{raw}"
        );
    }
}
