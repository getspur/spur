use crate::config::SpurConfig;
use anyhow::{anyhow, Result};
use directories::BaseDirs;
use std::collections::BTreeMap;
use std::path::Path;
use toml::value::Table;
use toml::Value;

pub type SectionOrigins = BTreeMap<String, &'static str>;
pub type AgentOrigins = BTreeMap<String, &'static str>;
pub type EffectiveConfigWithOrigins = (SpurConfig, SectionOrigins, AgentOrigins);

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

/// (merged config, per-top-level-section origin, per-agent-name origin).
pub fn effective_with_origins(repo_root: &Path) -> Result<EffectiveConfigWithOrigins> {
    let user_t = BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".spur/config.toml"))
        .filter(|path| path.exists())
        .map(|path| read_table(&path))
        .transpose()?;
    let project_path = repo_root.join(".spur").join("config.toml");
    let project_t = project_path
        .exists()
        .then(|| read_table(&project_path))
        .transpose()?;

    let cfg = load_layered(repo_root)?;

    let origin = |key: &str| -> &'static str {
        if project_t
            .as_ref()
            .map(|table| table.contains_key(key))
            .unwrap_or(false)
        {
            "project"
        } else if user_t
            .as_ref()
            .map(|table| table.contains_key(key))
            .unwrap_or(false)
        {
            "user"
        } else {
            "default"
        }
    };
    let merged_t = match Value::try_from(&cfg)? {
        Value::Table(table) => table,
        _ => Table::new(),
    };
    let sections = merged_t
        .keys()
        .map(|key| (key.clone(), origin(key)))
        .collect();

    let agent_origin = |name: &str, table: &Option<Table>| -> bool {
        table
            .as_ref()
            .and_then(|table| table.get("agents"))
            .and_then(|agents| agents.get("entries"))
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
            })
            .unwrap_or(false)
    };
    let agents = cfg
        .agents
        .entries
        .iter()
        .map(|agent| {
            let origin = if agent_origin(&agent.name, &project_t) {
                "project"
            } else if agent_origin(&agent.name, &user_t) {
                "user"
            } else {
                "default"
            };
            (agent.name.clone(), origin)
        })
        .collect();

    Ok((cfg, sections, agents))
}

/// Produce a table holding only what `config` adds or changes over `baseline`.
pub fn sparse_diff(config: &Value, baseline: &Value) -> Value {
    sparse_diff_at(config, baseline, &[])
}

fn sparse_diff_at(config: &Value, baseline: &Value, path: &[&str]) -> Value {
    match (config, baseline) {
        (Value::Table(config_table), Value::Table(baseline_table)) => {
            let mut out = Table::new();
            for (key, config_value) in config_table {
                let is_agents_entries = path == ["agents"] && key == "entries";
                match baseline_table.get(key) {
                    Some(baseline_value) if baseline_value == config_value => {}
                    Some(baseline_value @ Value::Table(_)) if config_value.is_table() => {
                        let mut child_path = path.to_vec();
                        child_path.push(key.as_str());
                        let diff = sparse_diff_at(config_value, baseline_value, &child_path);
                        if !diff.as_table().map(Table::is_empty).unwrap_or(false) {
                            out.insert(key.clone(), diff);
                        }
                    }
                    Some(Value::Array(baseline_array))
                        if is_agents_entries && config_value.is_array() =>
                    {
                        let kept = sparse_agents(config_value.as_array().unwrap(), baseline_array);
                        if !kept.is_empty() {
                            out.insert(key.clone(), Value::Array(kept));
                        }
                    }
                    _ => {
                        out.insert(key.clone(), config_value.clone());
                    }
                }
            }
            Value::Table(out)
        }
        _ => config.clone(),
    }
}

fn sparse_agents(config: &[Value], baseline: &[Value]) -> Vec<Value> {
    config
        .iter()
        .filter(|config_value| {
            let name = config_value.get("name").and_then(Value::as_str);
            match baseline
                .iter()
                .find(|baseline_value| baseline_value.get("name").and_then(Value::as_str) == name)
            {
                Some(baseline_value) => baseline_value != *config_value,
                None => true,
            }
        })
        .cloned()
        .collect()
}

/// Set a nested key path, creating intermediate tables and preserving siblings.
pub fn set_key_path(table: &mut Table, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current = table;
    for parent in parents {
        let entry = current
            .entry((*parent).to_string())
            .or_insert_with(|| Value::Table(Table::new()));
        if !entry.is_table() {
            *entry = Value::Table(Table::new());
        }
        current = entry.as_table_mut().unwrap();
    }
    current.insert((*last).to_string(), value);
}

/// Build the sparse-write baseline for a project layer: defaults plus user config.
pub fn default_user_baseline(_repo_root: &Path) -> Result<Value> {
    let mut base = match Value::try_from(SpurConfig::default())? {
        Value::Table(table) => table,
        _ => Table::new(),
    };
    let user_path = BaseDirs::new().map(|dirs| dirs.home_dir().join(".spur/config.toml"));
    if let Some(user_path) = user_path.as_ref().filter(|path| path.exists()) {
        merge_tables(&mut base, read_table(user_path)?);
    }
    Ok(Value::Table(base))
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

    #[test]
    fn sparse_diff_omits_equal_keeps_changed() {
        let base = toml::Value::try_from(SpurConfig::default()).unwrap();
        let mut config = base.clone();
        config
            .as_table_mut()
            .unwrap()
            .entry("brain")
            .or_insert(Value::Table(Table::new()))
            .as_table_mut()
            .unwrap()
            .insert("default".into(), Value::String("codex".into()));

        let diff = sparse_diff(&config, &base);
        let diff_table = diff.as_table().unwrap();

        assert!(diff_table.contains_key("brain"));
        assert!(!diff_table.contains_key("worktree"));
        assert_eq!(diff_table["brain"]["default"].as_str(), Some("codex"));
    }

    #[test]
    fn sparse_diff_identical_is_empty() {
        let base = toml::Value::try_from(SpurConfig::default()).unwrap();

        assert!(sparse_diff(&base, &base).as_table().unwrap().is_empty());
    }

    #[test]
    fn sparse_diff_agents_entries_drops_equal_keeps_changed_whole() {
        let base = Value::Table(t("[[agents.entries]]\nname='codex'\ncommand='codex'\n\
             [[agents.entries]]\nname='claude-code'\ncommand='claude'\n"));
        let config = Value::Table(t("[[agents.entries]]\nname='codex'\ncommand='codex'\n\
             [[agents.entries]]\nname='claude-code'\ncommand='claude-code'\n\
             [[agents.entries]]\nname='gemini'\ncommand='gemini'\n"));

        let diff = sparse_diff(&config, &base);
        let entries = diff["agents"]["entries"].as_array().unwrap();

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"].as_str(), Some("claude-code"));
        assert_eq!(entries[0]["command"].as_str(), Some("claude-code"));
        assert_eq!(entries[1]["name"].as_str(), Some("gemini"));
    }

    #[test]
    fn set_key_path_creates_nested_and_preserves_siblings() {
        let mut table = t("[tui]\ndensity='compact'\n");

        set_key_path(&mut table, &["tui", "theme"], Value::String("light".into()));

        assert_eq!(table["tui"]["theme"].as_str(), Some("light"));
        assert_eq!(table["tui"]["density"].as_str(), Some("compact"));
    }
}
