use super::AgentProfile;
use spur_acp::types::AgentKind;

pub struct RenderedProfile {
    /// Worktree-relative target path.
    pub rel_path: String,
    pub contents: String,
}

pub fn render_for_kind(profile: &AgentProfile, kind: AgentKind) -> Option<RenderedProfile> {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => Some(RenderedProfile {
            rel_path: format!(".claude/agents/{}.md", profile.name),
            contents: profile.raw.clone(),
        }),
        AgentKind::OpenCode => Some(RenderedProfile {
            rel_path: format!(".opencode/agent/{}.md", profile.name),
            contents: format!(
                "---\ndescription: {}\n---\n{}",
                profile.description, profile.body
            ),
        }),
        AgentKind::Kiro => {
            let value = serde_json::json!({
                "name": profile.name,
                "description": profile.description,
                "prompt": profile.body,
            });
            Some(RenderedProfile {
                rel_path: format!(".kiro/agents/{}.json", profile.name),
                contents: serde_json::to_string_pretty(&value).expect("static profile json"),
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

            Some(RenderedProfile {
                rel_path: format!(".codex/agents/{}.toml", profile.name),
                contents: toml::to_string_pretty(&toml::Value::Table(value))
                    .expect("static profile toml"),
            })
        }
        AgentKind::Kimi | AgentKind::Gemini | AgentKind::Generic => None,
    }
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
    fn claude_kinds_get_verbatim_canonical_source() {
        for kind in [AgentKind::ClaudeCodeAcp, AgentKind::ClaudeStreamJson] {
            let r = render_for_kind(&profile(), kind).unwrap();
            assert_eq!(r.rel_path, ".claude/agents/code-reviewer.md");
            assert_eq!(r.contents, profile().raw);
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
    }

    #[test]
    fn kiro_gets_json_with_name_description_prompt() {
        let r = render_for_kind(&profile(), AgentKind::Kiro).unwrap();
        assert_eq!(r.rel_path, ".kiro/agents/code-reviewer.json");
        let v: serde_json::Value = serde_json::from_str(&r.contents).unwrap();
        assert_eq!(v["name"], "code-reviewer");
        assert_eq!(v["description"], "Reviews diffs");
        assert_eq!(v["prompt"], "You review code.\n");
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
    }

    #[test]
    fn kinds_without_convention_render_nothing() {
        for kind in [AgentKind::Kimi, AgentKind::Gemini, AgentKind::Generic] {
            assert!(render_for_kind(&profile(), kind).is_none());
        }
    }
}
