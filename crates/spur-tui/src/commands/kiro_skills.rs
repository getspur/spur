//! Filesystem synthesis of Kiro skill slash commands.
//!
//! Kiro's native TUI exposes `.kiro/skills/<name>/SKILL.md` as `/<name>`.
//! ACP `_kiro.dev/commands/available` does not currently advertise those
//! names, so Spur synthesizes prompt-text entries from disk.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use spur_acp::{AgentKind, CommandsConfig};

use crate::agents::build_entry;
use crate::commands::entry::CommandEntry;

/// A discovered Kiro skill that can be invoked as `/<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredKiroSkill {
    pub name: String,
    pub description: String,
}

/// Discover skill slash commands from workspace then global roots.
///
/// Workspace `.kiro/skills` overrides `global_skills_root` on the same name.
/// Invalid directories (missing SKILL.md, empty description, name mismatch,
/// or illegal charset) are skipped.
pub fn discover_kiro_skill_slashes(
    workspace_root: &Path,
    global_skills_root: Option<&Path>,
) -> Vec<DiscoveredKiroSkill> {
    let mut by_name = BTreeMap::new();
    if let Some(global) = global_skills_root {
        for skill in scan_skills_dir(global) {
            by_name.insert(skill.name.clone(), skill);
        }
    }
    for skill in scan_skills_dir(&workspace_root.join(".kiro").join("skills")) {
        by_name.insert(skill.name.clone(), skill);
    }
    by_name.into_values().collect()
}

/// Default `~/.kiro/skills` root when HOME/USERPROFILE is set.
pub fn default_global_skills_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|home| PathBuf::from(home).join(".kiro").join("skills"))
}

/// Build prompt-text command entries for discovered skills.
pub fn skill_command_entries(
    handle: &str,
    cfg: &CommandsConfig,
    skills: &[DiscoveredKiroSkill],
) -> Vec<CommandEntry> {
    skills
        .iter()
        .map(|skill| {
            let cmd =
                spur_acp::AvailableCommand::new(skill.name.clone(), skill.description.clone());
            build_entry(handle, cfg, &cmd)
        })
        .collect()
}

/// Append skill entries whose names are not already occupied by ACP-advertised
/// commands. Advertised names win (e.g. builtin `/model`).
pub fn merge_skill_entries(
    advertised: Vec<CommandEntry>,
    skills: Vec<CommandEntry>,
) -> Vec<CommandEntry> {
    let occupied: std::collections::HashSet<String> =
        advertised.iter().map(|entry| entry.name.clone()).collect();
    advertised
        .into_iter()
        .chain(
            skills
                .into_iter()
                .filter(|entry| !occupied.contains(&entry.name)),
        )
        .collect()
}

/// Synthesize Kiro skill commands when `kind` is Kiro; otherwise return
/// advertised entries unchanged.
pub fn maybe_merge_kiro_skills(
    kind: AgentKind,
    handle: &str,
    cfg: &CommandsConfig,
    workspace_root: &Path,
    global_skills_root: Option<&Path>,
    advertised: Vec<CommandEntry>,
) -> Vec<CommandEntry> {
    if kind != AgentKind::Kiro {
        return advertised;
    }
    let skills = discover_kiro_skill_slashes(workspace_root, global_skills_root);
    let skill_entries = skill_command_entries(handle, cfg, &skills);
    merge_skill_entries(advertised, skill_entries)
}

fn scan_skills_dir(dir: &Path) -> Vec<DiscoveredKiroSkill> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(folder_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_valid_skill_name(folder_name) {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        let Ok(raw) = fs::read_to_string(&skill_md) else {
            continue;
        };
        let Some((front_name, description)) = parse_skill_frontmatter(&raw) else {
            continue;
        };
        if front_name != folder_name || description.is_empty() {
            continue;
        }
        skills.push(DiscoveredKiroSkill {
            name: folder_name.to_string(),
            description,
        });
    }
    skills
}

fn is_valid_skill_name(name: &str) -> bool {
    let len = name.len();
    (1..=64).contains(&len)
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn parse_skill_frontmatter(raw: &str) -> Option<(String, String)> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let yaml = &rest[..end];
    let mut name = None;
    let mut description = None;
    let lines: Vec<&str> = yaml.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(unquote(value.trim()));
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(parse_description(value.trim(), &lines[idx + 1..]));
        }
    }
    Some((name?, description?))
}

fn parse_description(first: &str, rest: &[&str]) -> String {
    if first.is_empty() {
        let mut folded = String::new();
        for line in rest {
            if line.starts_with(' ') || line.starts_with('\t') {
                let piece = line.trim();
                if piece.is_empty() {
                    break;
                }
                if !folded.is_empty() {
                    folded.push(' ');
                }
                folded.push_str(piece);
            } else {
                break;
            }
        }
        return folded;
    }
    unquote(first)
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        inner.replace("\\\"", "\"")
    } else if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        inner.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{AgentConfig, AgentKind, AvailableCommand, DispatchKind};
    use std::fs;
    use tempfile::TempDir;

    use crate::commands::entry::{CommandSource, Dispatch};

    fn write_skill(dir: &Path, name: &str, body: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).expect("skill dir");
        fs::write(skill_dir.join("SKILL.md"), body).expect("skill md");
    }

    #[test]
    fn discovers_workspace_skill_as_slash_name() {
        let tmp = TempDir::new().expect("tmp");
        write_skill(
            &tmp.path().join(".kiro/skills"),
            "probe-kiwi-skill",
            "---\nname: probe-kiwi-skill\ndescription: \"Probe-only skill.\"\n---\n# Hi\n",
        );

        let skills = discover_kiro_skill_slashes(tmp.path(), None);
        assert_eq!(
            skills,
            vec![DiscoveredKiroSkill {
                name: "probe-kiwi-skill".into(),
                description: "Probe-only skill.".into(),
            }]
        );
    }

    #[test]
    fn workspace_skill_overrides_global_same_name() {
        let tmp = TempDir::new().expect("tmp");
        write_skill(
            &tmp.path().join("global"),
            "shared-skill",
            "---\nname: shared-skill\ndescription: \"global copy\"\n---\n",
        );
        write_skill(
            &tmp.path().join(".kiro/skills"),
            "shared-skill",
            "---\nname: shared-skill\ndescription: \"workspace copy\"\n---\n",
        );

        let skills = discover_kiro_skill_slashes(tmp.path(), Some(&tmp.path().join("global")));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "workspace copy");
    }

    #[test]
    fn skips_missing_skill_md_and_name_mismatch() {
        let tmp = TempDir::new().expect("tmp");
        let skills_root = tmp.path().join(".kiro/skills");
        fs::create_dir_all(skills_root.join("empty-dir")).expect("empty");
        write_skill(
            &skills_root,
            "mismatch",
            "---\nname: other-name\ndescription: \"nope\"\n---\n",
        );
        write_skill(
            &skills_root,
            "BadName",
            "---\nname: BadName\ndescription: \"caps\"\n---\n",
        );

        let skills = discover_kiro_skill_slashes(tmp.path(), None);
        assert!(skills.is_empty(), "expected skip, got {skills:?}");
    }

    #[test]
    fn advertised_builtins_keep_filesystem_skills() {
        let tmp = TempDir::new().expect("tmp");
        write_skill(
            &tmp.path().join(".kiro/skills"),
            "probe-kiwi-skill",
            "---\nname: probe-kiwi-skill\ndescription: \"Probe-only skill.\"\n---\n",
        );
        let mut cfg = AgentConfig::with_defaults("kiro");
        cfg.kind = AgentKind::Kiro;
        let advertised = vec![build_entry(
            "kiro",
            &cfg.commands,
            &AvailableCommand::new("model", "Switch model"),
        )];
        let merged = maybe_merge_kiro_skills(
            cfg.kind,
            "kiro",
            &cfg.commands,
            tmp.path(),
            None,
            advertised,
        );
        let names: Vec<_> = merged.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"model"));
        assert!(names.contains(&"probe-kiwi-skill"));
    }

    #[test]
    fn advertised_name_wins_over_filesystem_skill() {
        let handle = "kiro";
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let advertised = vec![build_entry(
            handle,
            &cfg,
            &AvailableCommand::new("model", "ACP model"),
        )];
        let skills = vec![DiscoveredKiroSkill {
            name: "model".into(),
            description: "filesystem model".into(),
        }];
        let merged = merge_skill_entries(advertised, skill_command_entries(handle, &cfg, &skills));
        let models: Vec<_> = merged.iter().filter(|e| e.name == "model").collect();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].description, "ACP model");
    }

    #[test]
    fn skill_entries_are_prompt_text() {
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let entries = skill_command_entries(
            "kiro",
            &cfg,
            &[DiscoveredKiroSkill {
                name: "probe-kiwi-skill".into(),
                description: "Probe-only skill.".into(),
            }],
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "probe-kiwi-skill");
        assert!(matches!(
            &entries[0].source,
            CommandSource::Agent { handle } if handle == "kiro"
        ));
        assert!(matches!(
            &entries[0].dispatch,
            Dispatch::PromptText { normalized } if normalized == "/probe-kiwi-skill"
        ));
    }

    #[test]
    fn kiro_kind_merges_workspace_skill_when_acp_list_is_empty() {
        let tmp = TempDir::new().expect("tmp");
        write_skill(
            &tmp.path().join(".kiro/skills"),
            "probe-kiwi-skill",
            "---\nname: probe-kiwi-skill\ndescription: \"Probe-only skill.\"\n---\n",
        );
        let mut cfg = AgentConfig::with_defaults("kiro");
        cfg.kind = AgentKind::Kiro;
        let merged = maybe_merge_kiro_skills(
            cfg.kind,
            "kiro",
            &cfg.commands,
            tmp.path(),
            None,
            Vec::new(),
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "probe-kiwi-skill");
    }

    #[test]
    fn non_kiro_agents_do_not_receive_filesystem_skills() {
        let tmp = TempDir::new().expect("tmp");
        write_skill(
            &tmp.path().join(".kiro/skills"),
            "probe-kiwi-skill",
            "---\nname: probe-kiwi-skill\ndescription: \"Probe-only skill.\"\n---\n",
        );
        let mut cfg = AgentConfig::with_defaults("codex");
        cfg.kind = AgentKind::CodexAcp;
        let merged = maybe_merge_kiro_skills(
            cfg.kind,
            "codex",
            &cfg.commands,
            tmp.path(),
            None,
            Vec::new(),
        );
        assert!(merged.is_empty());
        let _cfg = cfg;
    }
}
