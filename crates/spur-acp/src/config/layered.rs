use crate::config::SpurConfig;
use anyhow::{anyhow, Result};
use directories::BaseDirs;
use std::path::Path;
use toml::value::Table;
use toml::Value;

/// Deep-merge `over` onto `base`.
///
/// Tables recurse, scalars and plain arrays from `over` replace, and
/// `agents.entries` arrays merge by `name` with matching entries deep-merged.
pub fn merge_tables(base: &mut Table, over: Table) {
    merge_at(base, over, &[]);
}

fn merge_at(base: &mut Table, over: Table, path: &[&str]) {
    for (key, over_value) in over {
        let is_agents_entries = path == ["agents"] && key == "entries";
        match (base.get_mut(&key), over_value) {
            (Some(Value::Table(base_table)), Value::Table(over_table)) => {
                let mut child_path = path.to_vec();
                child_path.push(key.as_str());
                merge_at(base_table, over_table, &child_path);
            }
            (Some(Value::Array(base_array)), Value::Array(over_array)) if is_agents_entries => {
                merge_agents(base_array, over_array);
            }
            (_, over_value) => {
                base.insert(key, over_value);
            }
        }
    }
}

fn merge_agents(base: &mut Vec<Value>, over: Vec<Value>) {
    for over_value in over {
        let override_name = over_value
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let existing_pos = override_name.as_deref().and_then(|name| {
            base.iter()
                .position(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
        });

        match (existing_pos, over_value) {
            (Some(index), Value::Table(over_table)) => {
                if let Some(Value::Table(base_table)) = base.get_mut(index) {
                    merge_at(base_table, over_table, &[]);
                } else {
                    base[index] = Value::Table(over_table);
                }
            }
            (Some(index), other) => base[index] = other,
            (None, other) => base.push(other),
        }
    }
}

fn read_table(path: &Path) -> Result<Table> {
    let raw = std::fs::read_to_string(path)
        .map_err(|err| anyhow!("failed to read {}: {err}", path.display()))?;
    match toml::from_str::<Value>(&raw)
        .map_err(|err| anyhow!("failed to parse {}: {err}", path.display()))?
    {
        Value::Table(table) => Ok(table),
        _ => Err(anyhow!("{} is not a TOML table", path.display())),
    }
}

fn load_layered_from_paths(repo_root: &Path, user_path: Option<&Path>) -> Result<SpurConfig> {
    let project_path = repo_root.join(".spur").join("config.toml");
    let mut merged = Table::new();

    if let Some(user_path) = user_path.filter(|path| path.exists()) {
        merge_tables(&mut merged, read_table(user_path)?);
    }
    if project_path.exists() {
        merge_tables(&mut merged, read_table(&project_path)?);
    }

    Value::Table(merged)
        .try_into()
        .map_err(|err| anyhow!("failed to build merged SpurConfig: {err}"))
}

/// Load the effective config for `repo_root` with precedence:
/// built-in defaults < `~/.spur/config.toml` < `<repo>/.spur/config.toml`.
pub fn load_layered(repo_root: &Path) -> Result<SpurConfig> {
    let user_path = BaseDirs::new().map(|dirs| dirs.home_dir().join(".spur/config.toml"));
    load_layered_from_paths(repo_root, user_path.as_deref())
}

#[cfg(test)]
mod tests {
    use crate::config::SpurConfig;
    use std::fs;
    use toml::Value;

    use super::*;

    fn t(s: &str) -> toml::value::Table {
        match toml::from_str::<Value>(s).unwrap() {
            Value::Table(t) => t,
            _ => panic!("not a table"),
        }
    }

    #[test]
    fn scalar_override_and_table_deep_merge() {
        let mut base = t("[brain]\ndefault='claude-code'\nfallback=['kiro']\n");
        merge_tables(&mut base, t("[brain]\ndefault='codex'\n"));
        let brain = base["brain"].as_table().unwrap();
        assert_eq!(brain["default"].as_str(), Some("codex"));
        assert_eq!(brain["fallback"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn plain_array_replaces() {
        let mut base = t("[brain]\nfallback=['kiro','codex']\n");
        merge_tables(&mut base, t("[brain]\nfallback=['gemini']\n"));
        let fb = base["brain"]["fallback"].as_array().unwrap();
        assert_eq!(fb.len(), 1);
        assert_eq!(fb[0].as_str(), Some("gemini"));
    }

    #[test]
    fn agents_entries_merge_by_name_with_field_override() {
        let mut base = t(
            "[[agents.entries]]\nname='claude-code'\ncommand='claude'\ncapabilities=['x']\n\
             [[agents.entries]]\nname='codex'\ncommand='codex'\n",
        );
        merge_tables(
            &mut base,
            t(
                "[[agents.entries]]\nname='claude-code'\ncapabilities=['y']\n\
               [[agents.entries]]\nname='gemini'\ncommand='gemini'\n",
            ),
        );
        let entries = base["agents"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        let cc = entries
            .iter()
            .find(|e| e["name"].as_str() == Some("claude-code"))
            .unwrap();
        assert_eq!(cc["command"].as_str(), Some("claude"));
        assert_eq!(
            cc["capabilities"].as_array().unwrap()[0].as_str(),
            Some("y")
        );
    }

    #[test]
    fn load_layered_merges_user_and_project_with_project_precedence() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".spur")).unwrap();
        let user = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            user.path(),
            "[brain]\ndefault='claude-code'\nfallback=['kiro']\n",
        )
        .unwrap();
        fs::write(
            repo.path().join(".spur/config.toml"),
            "[brain]\ndefault='codex'\n",
        )
        .unwrap();

        let cfg = load_layered_from_paths(repo.path(), Some(user.path())).unwrap();

        assert_eq!(cfg.brain.default, "codex");
        assert_eq!(cfg.brain.fallback, vec!["kiro"]);
    }

    #[test]
    fn all_missing_is_default() {
        let repo = tempfile::tempdir().unwrap();

        let cfg = load_layered_from_paths(repo.path(), None).unwrap();

        assert_eq!(cfg.brain.default, SpurConfig::default().brain.default);
    }

    #[test]
    fn malformed_project_file_errors_with_path() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join(".spur")).unwrap();
        let project_path = repo.path().join(".spur/config.toml");
        fs::write(&project_path, "[brain\n").unwrap();

        let err = load_layered_from_paths(repo.path(), None).unwrap_err();

        assert!(err
            .to_string()
            .contains(&project_path.display().to_string()));
    }

    #[test]
    fn malformed_user_file_errors_with_path() {
        let repo = tempfile::tempdir().unwrap();
        let user = tempfile::NamedTempFile::new().unwrap();
        let user_path = user.path().to_path_buf();
        fs::write(&user_path, "[brain\n").unwrap();

        let err = load_layered_from_paths(repo.path(), Some(&user_path)).unwrap_err();

        assert!(err.to_string().contains(&user_path.display().to_string()));
    }
}
