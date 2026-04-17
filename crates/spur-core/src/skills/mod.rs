//! Skill-based brain prompt resources (Amendment A1).
//!
//! Loads SKILL.md files for brain prompt assembly. Bundled defaults are
//! compiled in via `include_str!`; per-project overrides in `.spur/skills/`
//! take precedence.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

static BUNDLED: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn bundled() -> &'static HashMap<&'static str, &'static str> {
    BUNDLED.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert(
            "brain-delegation",
            strip_frontmatter(include_str!("brain-delegation/SKILL.md")),
        );
        let claude_skill =
            strip_frontmatter(include_str!("brain-delegation-claude-code-acp/SKILL.md"));
        m.insert("brain-delegation-claude-code-acp", claude_skill);
        m.insert("brain-delegation-claude-code", claude_skill); // deprecated alias
        m.insert(
            "brain-delegation-kiro",
            strip_frontmatter(include_str!("brain-delegation-kiro/SKILL.md")),
        );
        m.insert(
            "brain-delegation-codex",
            strip_frontmatter(include_str!("brain-delegation-codex/SKILL.md")),
        );
        m.insert(
            "brain-delegation-gemini",
            strip_frontmatter(include_str!("brain-delegation-gemini/SKILL.md")),
        );
        m
    })
}

/// Load a skill body: user override wins, else bundled default.
/// Frontmatter is stripped in both cases.
pub fn load_skill(name: &str, repo_root: &Path) -> Option<String> {
    let override_path = repo_root.join(".spur/skills").join(name).join("SKILL.md");
    if override_path.exists() {
        return std::fs::read_to_string(&override_path)
            .ok()
            .map(|s| strip_frontmatter_owned(&s));
    }
    bundled().get(name).map(|s| s.to_string())
}

/// Strip YAML frontmatter delimited by `---\n...\n---\n`.
fn strip_frontmatter(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---\n") {
            return &rest[idx + 5..];
        }
        // Closing `---` at EOF (no trailing newline).
        if rest.ends_with("\n---") {
            return "";
        }
    }
    s
}

/// Owned variant for user-override files read from disk.
fn strip_frontmatter_owned(s: &str) -> String {
    strip_frontmatter(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn bundled_skills_parse_and_strip_frontmatter() {
        let map = bundled();
        for name in [
            "brain-delegation",
            "brain-delegation-claude-code-acp",
            "brain-delegation-kiro",
            "brain-delegation-codex",
            "brain-delegation-gemini",
        ] {
            let body = map
                .get(name)
                .unwrap_or_else(|| panic!("missing bundled skill: {name}"));
            assert!(!body.starts_with("---"), "{name}: frontmatter not stripped");
            assert!(!body.is_empty(), "{name}: body is empty after strip");
        }
    }

    #[test]
    fn load_skill_returns_bundled_when_no_override() {
        let fake_root = PathBuf::from("/nonexistent-spur-test-root");
        let body = load_skill("brain-delegation", &fake_root);
        assert!(body.is_some(), "expected bundled skill");
        assert!(
            body.unwrap().contains("delegate"),
            "expected delegation content"
        );
    }

    #[test]
    fn load_skill_returns_none_for_unknown() {
        let fake_root = PathBuf::from("/nonexistent-spur-test-root");
        assert!(load_skill("nonexistent-skill", &fake_root).is_none());
    }

    #[test]
    fn load_skill_prefers_user_override() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".spur/skills/brain-delegation");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: brain-delegation\n---\nCustom override body\n",
        )
        .unwrap();
        let body = load_skill("brain-delegation", dir.path()).unwrap();
        assert_eq!(body.trim(), "Custom override body");
    }

    #[test]
    fn strip_frontmatter_no_frontmatter() {
        assert_eq!(strip_frontmatter("just body"), "just body");
    }

    #[test]
    fn strip_frontmatter_normal() {
        let input = "---\nfoo: bar\n---\nbody text\n";
        assert_eq!(strip_frontmatter(input), "body text\n");
    }

    #[test]
    fn strip_frontmatter_eof_no_trailing_newline() {
        let input = "---\nfoo: bar\n---";
        assert_eq!(strip_frontmatter(input), "");
    }

    #[test]
    fn strip_frontmatter_value_containing_dashes() {
        // YAML value with --- should not be treated as closing delimiter.
        let input = "---\nfoo: some---value\n---\nbody\n";
        assert_eq!(strip_frontmatter(input), "body\n");
    }

    #[test]
    fn claude_code_deprecated_alias_resolves() {
        let fake = PathBuf::from("/nonexistent");
        let acp = load_skill("brain-delegation-claude-code-acp", &fake).unwrap();
        let alias = load_skill("brain-delegation-claude-code", &fake).unwrap();
        assert_eq!(acp, alias);
    }
}
