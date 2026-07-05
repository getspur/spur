use super::AgentProfile;
use crate::skills::installer::{parse_marker, sha256_hex, Marker};
use spur_acp::types::AgentKind;

pub struct RenderedProfile {
    /// Worktree-relative target path.
    pub rel_path: String,
    pub contents: String,
    unmarked_contents: String,
    marker_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExistingProfile {
    Unchanged,
    ManagedDifferent,
    NoMarker,
    Edited,
}

pub fn render_for_kind(profile: &AgentProfile, kind: AgentKind) -> Option<RenderedProfile> {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => Some(render_markdown_profile(
            format!(".claude/agents/{}.md", profile.name),
            profile.raw.clone(),
            &profile.name,
        )),
        AgentKind::OpenCode => Some(render_markdown_profile(
            format!(".opencode/agent/{}.md", profile.name),
            format!(
                "---\ndescription: {}\n---\n{}",
                profile.description, profile.body
            ),
            &profile.name,
        )),
        AgentKind::Kiro => {
            let unmarked = serde_json::json!({
                "name": profile.name,
                "description": profile.description,
                "prompt": profile.body,
            });
            let unmarked_contents =
                serde_json::to_string_pretty(&unmarked).expect("static profile json");
            let marker = marker_for(&profile.name, unmarked_contents.as_bytes());
            let mut marked = unmarked;
            marked["x-spur-managed"] = serde_json::Value::String(marker.render_line());
            Some(RenderedProfile {
                rel_path: format!(".kiro/agents/{}.json", profile.name),
                contents: serde_json::to_string_pretty(&marked).expect("static profile json"),
                unmarked_contents,
                marker_sha256: marker.sha256,
            })
        }
        AgentKind::CodexAcp => {
            let mut value = toml::value::Table::new();
            value.insert("name".into(), profile.name.clone().into());
            value.insert("description".into(), profile.description.clone().into());
            value.insert("developer_instructions".into(), profile.body.clone().into());
            if let Some(model) = &profile.model {
                value.insert("model".into(), model.clone().into());
            }
            if let Some(effort) = &profile.effort {
                value.insert("model_reasoning_effort".into(), effort.clone().into());
            }
            let unmarked_contents = toml::to_string_pretty(&toml::Value::Table(value.clone()))
                .expect("static profile toml");
            let marker = marker_for(&profile.name, unmarked_contents.as_bytes());
            let mut spur = toml::value::Table::new();
            spur.insert("managed".into(), marker.render_line().into());
            value.insert("spur".into(), toml::Value::Table(spur));

            Some(RenderedProfile {
                rel_path: format!(".codex/agents/{}.toml", profile.name),
                contents: toml::to_string_pretty(&toml::Value::Table(value))
                    .expect("static profile toml"),
                unmarked_contents,
                marker_sha256: marker.sha256,
            })
        }
        AgentKind::Kimi | AgentKind::Gemini | AgentKind::Generic => None,
    }
}

impl Marker {
    fn render_line(&self) -> String {
        self.render().trim_end_matches('\n').to_string()
    }
}

fn marker_for(profile_name: &str, bytes: &[u8]) -> Marker {
    Marker {
        version: 1,
        skill_id: format!("agent-profile:{profile_name}"),
        sha256: sha256_hex(bytes),
    }
}

fn render_markdown_profile(
    rel_path: String,
    mut unmarked_contents: String,
    profile_name: &str,
) -> RenderedProfile {
    if !unmarked_contents.ends_with('\n') {
        unmarked_contents.push('\n');
    }
    let marker = marker_for(profile_name, unmarked_contents.as_bytes());
    let mut contents = unmarked_contents.clone();
    contents.push_str(&marker.render());
    RenderedProfile {
        rel_path,
        contents,
        unmarked_contents,
        marker_sha256: marker.sha256,
    }
}

pub(crate) fn classify_existing(
    rendered: &RenderedProfile,
    existing: &str,
) -> Result<ExistingProfile, String> {
    let Some((marker, unmarked)) = extract_marker_and_unmarked(&rendered.rel_path, existing)?
    else {
        return Ok(ExistingProfile::NoMarker);
    };
    let disk_sha = sha256_hex(unmarked.as_bytes());
    if disk_sha != marker.sha256 {
        return Ok(ExistingProfile::Edited);
    }
    if marker.sha256 == rendered.marker_sha256 && unmarked == rendered.unmarked_contents {
        Ok(ExistingProfile::Unchanged)
    } else {
        Ok(ExistingProfile::ManagedDifferent)
    }
}

fn extract_marker_and_unmarked(
    rel_path: &str,
    existing: &str,
) -> Result<Option<(Marker, String)>, String> {
    if rel_path.ends_with(".json") {
        extract_json_marker(existing)
    } else if rel_path.ends_with(".toml") {
        extract_toml_marker(existing)
    } else {
        Ok(extract_markdown_marker(existing))
    }
}

fn extract_markdown_marker(existing: &str) -> Option<(Marker, String)> {
    let mut line_start = 0usize;
    for line in existing.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        if let Some(marker) = parse_marker(trimmed) {
            let mut unmarked = String::with_capacity(existing.len().saturating_sub(line.len()));
            unmarked.push_str(&existing[..line_start]);
            unmarked.push_str(&existing[line_end..]);
            return Some((marker, unmarked));
        }
        line_start = line_end;
    }
    if line_start < existing.len() {
        let line = &existing[line_start..];
        if let Some(marker) = parse_marker(line) {
            let mut unmarked = String::with_capacity(line_start);
            unmarked.push_str(&existing[..line_start]);
            return Some((marker, unmarked));
        }
    }
    None
}

fn extract_json_marker(existing: &str) -> Result<Option<(Marker, String)>, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(existing).map_err(|error| error.to_string())?;
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let Some(marker_value) = object.remove("x-spur-managed") else {
        return Ok(None);
    };
    let Some(marker_text) = marker_value.as_str() else {
        return Err("x-spur-managed is not a string".to_string());
    };
    let Some(marker) = parse_marker(marker_text) else {
        return Err("x-spur-managed is not a valid SPUR-MANAGED marker".to_string());
    };
    let unmarked = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
    Ok(Some((marker, unmarked)))
}

fn extract_toml_marker(existing: &str) -> Result<Option<(Marker, String)>, String> {
    let mut value: toml::Value = existing
        .parse::<toml::Value>()
        .map_err(|error| error.to_string())?;
    let Some(table) = value.as_table_mut() else {
        return Ok(None);
    };
    let Some(spur_value) = table.remove("spur") else {
        return Ok(None);
    };
    let Some(marker_text) = spur_value
        .as_table()
        .and_then(|table| table.get("managed"))
        .and_then(toml::Value::as_str)
    else {
        return Err("spur.managed is not a string".to_string());
    };
    let Some(marker) = parse_marker(marker_text) else {
        return Err("spur.managed is not a valid SPUR-MANAGED marker".to_string());
    };
    let unmarked = toml::to_string_pretty(&value).map_err(|error| error.to_string())?;
    Ok(Some((marker, unmarked)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> crate::agent_profiles::AgentProfile {
        crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\nmodel: opus\neffort: high\n---\nYou review code.\n",
        )
        .unwrap()
    }

    #[test]
    fn claude_kinds_get_canonical_source_with_spur_marker() {
        for kind in [AgentKind::ClaudeCodeAcp, AgentKind::ClaudeStreamJson] {
            let r = render_for_kind(&profile(), kind).unwrap();
            assert_eq!(r.rel_path, ".claude/agents/code-reviewer.md");
            assert!(r.contents.starts_with(&profile().raw));
            assert!(r
                .contents
                .contains("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
        }
    }

    #[test]
    fn opencode_gets_agent_markdown_with_description_frontmatter() {
        let r = render_for_kind(&profile(), AgentKind::OpenCode).unwrap();
        assert_eq!(r.rel_path, ".opencode/agent/code-reviewer.md");
        assert!(r
            .contents
            .starts_with("---\ndescription: Reviews diffs\n---\n"));
        assert!(r.contents.contains("You review code."));
        assert!(r
            .contents
            .contains("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
    }

    #[test]
    fn kiro_gets_json_with_name_description_prompt() {
        let r = render_for_kind(&profile(), AgentKind::Kiro).unwrap();
        assert_eq!(r.rel_path, ".kiro/agents/code-reviewer.json");
        let v: serde_json::Value = serde_json::from_str(&r.contents).unwrap();
        assert_eq!(v["name"], "code-reviewer");
        assert_eq!(v["description"], "Reviews diffs");
        assert_eq!(v["prompt"], "You review code.\n");
        assert!(v["x-spur-managed"]
            .as_str()
            .unwrap()
            .starts_with("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
    }

    #[test]
    fn codex_gets_toml_with_developer_instructions_and_model_defaults() {
        let r = render_for_kind(&profile(), AgentKind::CodexAcp).unwrap();
        assert_eq!(r.rel_path, ".codex/agents/code-reviewer.toml");
        let v: toml::Value = r.contents.parse().unwrap();
        assert_eq!(v["name"].as_str(), Some("code-reviewer"));
        assert_eq!(
            v["developer_instructions"].as_str(),
            Some("You review code.\n")
        );
        assert_eq!(v["model"].as_str(), Some("opus"));
        assert_eq!(v["model_reasoning_effort"].as_str(), Some("high"));
        assert!(v["spur"]["managed"]
            .as_str()
            .unwrap()
            .starts_with("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
    }

    #[test]
    fn rendered_profiles_classify_matching_managed_files_as_unchanged() {
        for kind in [
            AgentKind::ClaudeCodeAcp,
            AgentKind::OpenCode,
            AgentKind::Kiro,
            AgentKind::CodexAcp,
        ] {
            let r = render_for_kind(&profile(), kind).unwrap();
            assert_eq!(
                classify_existing(&r, &r.contents).unwrap(),
                ExistingProfile::Unchanged
            );
        }
    }

    #[test]
    fn kinds_without_convention_render_nothing() {
        for kind in [AgentKind::Kimi, AgentKind::Gemini, AgentKind::Generic] {
            assert!(render_for_kind(&profile(), kind).is_none());
        }
    }
}
