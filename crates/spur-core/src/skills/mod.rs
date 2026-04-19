//! Skill-based brain prompt resources (Amendment A1).
//!
//! Loads SKILL.md files for brain prompt assembly. Bundled defaults are
//! compiled in via `include_str!`; per-project overrides in `.spur/skills/`
//! take precedence.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

mod frontmatter;

pub mod adapters;
pub mod installer;

static BUNDLED: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
static BUNDLED_RAW: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn bundled_raw() -> &'static HashMap<&'static str, &'static str> {
    BUNDLED_RAW.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert(
            "brain-delegation",
            include_str!("brain-delegation/SKILL.md"),
        );
        let claude_skill = include_str!("brain-delegation-claude-code-acp/SKILL.md");
        m.insert("brain-delegation-claude-code-acp", claude_skill);
        m.insert("brain-delegation-claude-code", claude_skill);
        m.insert(
            "brain-delegation-kiro",
            include_str!("brain-delegation-kiro/SKILL.md"),
        );
        m.insert(
            "brain-delegation-codex",
            include_str!("brain-delegation-codex/SKILL.md"),
        );
        m.insert(
            "brain-delegation-gemini",
            include_str!("brain-delegation-gemini/SKILL.md"),
        );
        m.insert(
            "test-driven-development",
            include_str!("test-driven-development/SKILL.md"),
        );
        m.insert(
            "systematic-debugging",
            include_str!("systematic-debugging/SKILL.md"),
        );
        m.insert(
            "verification-before-completion",
            include_str!("verification-before-completion/SKILL.md"),
        );
        m.insert(
            "receiving-code-review",
            include_str!("receiving-code-review/SKILL.md"),
        );
        m.insert(
            "requesting-code-review",
            include_str!("requesting-code-review/SKILL.md"),
        );
        m
    })
}

/// Returns all bundled skills (raw content including frontmatter) for CLI extraction.
pub fn all_bundled_raw() -> &'static HashMap<&'static str, &'static str> {
    bundled_raw()
}

fn bundled() -> &'static HashMap<&'static str, &'static str> {
    BUNDLED.get_or_init(|| {
        let mut m = HashMap::new();
        for (k, v) in bundled_raw() {
            let leaked: &'static str = strip_frontmatter_owned(v).leak();
            m.insert(*k, leaked);
        }
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

static SKILL_ID_RE: OnceLock<regex::Regex> = OnceLock::new();

fn skill_id_regex() -> &'static regex::Regex {
    SKILL_ID_RE.get_or_init(|| {
        regex::Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").expect("static regex")
    })
}

/// Error returned when a skill directory name violates the naming rules.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid skill id `{id}`: {reason}")]
pub struct InvalidSkillId {
    pub id: String,
    pub reason: &'static str,
}

/// Validate a skill id: regex `^[a-z0-9]+(-[a-z0-9]+)*$`, length 1..=54.
///
/// The 54-char cap is OpenCode's 64-char skill-name limit minus the
/// `spurpower-` 10-char prefix we add in adapter output.
pub fn validate_id(id: &str) -> Result<(), InvalidSkillId> {
    if id.is_empty() {
        return Err(InvalidSkillId { id: id.to_string(), reason: "empty" });
    }
    if id.len() > 54 {
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "longer than 54 characters",
        });
    }
    if !skill_id_regex().is_match(id) {
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "must match ^[a-z0-9]+(-[a-z0-9]+)*$",
        });
    }
    Ok(())
}

/// Where a skill's body came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    Bundled,
    Override,
}

/// A resolved skill ready for rendering across adapters.
#[derive(Debug, Clone)]
pub struct SkillPayload {
    pub id: String,
    pub description: String,
    pub body: String,
    pub source: SkillSource,
}

/// Resolve the active skill set: bundled corpus merged with
/// `.spur/skills/<id>/SKILL.md` overrides (override wins per id).
///
/// Validates every skill id (bundled and override) through `validate_id`.
pub fn list_active_skills(repo_root: &Path) -> Result<Vec<SkillPayload>, InvalidSkillId> {
    let mut by_id: std::collections::BTreeMap<String, SkillPayload> =
        std::collections::BTreeMap::new();

    // Bundled first.
    for (id, raw) in bundled_raw() {
        validate_id(id)?;
        let parsed = frontmatter::parse_source(raw);
        by_id.insert(
            id.to_string(),
            SkillPayload {
                id: id.to_string(),
                description: parsed.description.unwrap_or("").to_string(),
                body: parsed.body.to_string(),
                source: SkillSource::Bundled,
            },
        );
    }

    // Overrides.
    let override_dir = repo_root.join(".spur/skills");
    if override_dir.is_dir() {
        let entries = match std::fs::read_dir(&override_dir) {
            Ok(e) => e,
            Err(_) => return Ok(by_id.into_values().collect()),
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            validate_id(&id)?;
            let skill_md = entry.path().join("SKILL.md");
            let raw = match std::fs::read_to_string(&skill_md) {
                Ok(r) => r,
                Err(_) => continue, // no SKILL.md in that dir
            };
            let parsed = frontmatter::parse_source(&raw);
            by_id.insert(
                id.clone(),
                SkillPayload {
                    id,
                    description: parsed.description.unwrap_or("").to_string(),
                    body: parsed.body.to_string(),
                    source: SkillSource::Override,
                },
            );
        }
    }

    Ok(by_id.into_values().collect())
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

    #[test]
    fn validate_id_accepts_standard_names() {
        for ok in [
            "tdd",
            "test-driven-development",
            "a",
            "verification-before-completion",
        ] {
            assert!(validate_id(ok).is_ok(), "should accept {ok}");
        }
    }

    #[test]
    fn validate_id_rejects_bad_names() {
        for bad in [
            "",
            "Uppercase",
            "has space",
            "has_underscore",
            "trailing-",
            "-leading",
            "double--hyphen",
            "with/slash",
            "..",
            "../evil",
        ] {
            assert!(validate_id(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn validate_id_enforces_length_cap() {
        let ok_54 = "a".to_string() + &"b".repeat(53);
        let too_long_55 = "a".to_string() + &"b".repeat(54);
        assert!(validate_id(&ok_54).is_ok());
        assert!(validate_id(&too_long_55).is_err());
    }

    #[test]
    fn list_active_skills_returns_bundled_when_no_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let skills = list_active_skills(dir.path()).unwrap();
        let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"test-driven-development"));
        assert!(ids.contains(&"brain-delegation"));
        // All bundled entries should have non-empty body.
        for s in &skills {
            assert!(!s.body.is_empty(), "{}: empty body", s.id);
        }
    }

    #[test]
    fn list_active_skills_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("SKILL.md"),
            "---\nname: test-driven-development\ndescription: MY OVERRIDE\n---\nMy body here\n",
        )
        .unwrap();

        let skills = list_active_skills(dir.path()).unwrap();
        let tdd = skills
            .iter()
            .find(|s| s.id == "test-driven-development")
            .unwrap();
        assert_eq!(tdd.description, "MY OVERRIDE");
        assert_eq!(tdd.body, "My body here\n");
        assert!(matches!(tdd.source, SkillSource::Override));
    }

    #[test]
    fn list_active_skills_rejects_invalid_override_id() {
        let dir = tempfile::tempdir().unwrap();
        let bad_override = dir.path().join(".spur/skills/Bad_Name");
        std::fs::create_dir_all(&bad_override).unwrap();
        std::fs::write(
            bad_override.join("SKILL.md"),
            "---\nname: bad\ndescription: x\n---\nbody",
        )
        .unwrap();

        let err = list_active_skills(dir.path()).unwrap_err();
        assert!(err.to_string().contains("Bad_Name"));
    }
}
