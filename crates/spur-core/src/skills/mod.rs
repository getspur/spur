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
        m.insert("spur-way", include_str!("spur-way/SKILL.md"));
        m.insert("beads-lifecycle", include_str!("beads-lifecycle/SKILL.md"));
        m.insert("worker-signals", include_str!("worker-signals/SKILL.md"));
        m.insert(
            "brain-review-gate",
            include_str!("brain-review-gate/SKILL.md"),
        );
        m.insert(
            "plan-task-discipline",
            include_str!("plan-task-discipline/SKILL.md"),
        );
        m.insert(
            "worker-mention-routing",
            include_str!("worker-mention-routing/SKILL.md"),
        );
        m.insert("brainstorming", include_str!("brainstorming/SKILL.md"));
        m.insert("code-explore", include_str!("code-explore/SKILL.md"));
        m.insert("writing-plans", include_str!("writing-plans/SKILL.md"));
        m.insert("writing-skills", include_str!("writing-skills/SKILL.md"));
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
        if let Ok(raw) = std::fs::read_to_string(&override_path) {
            let parsed = frontmatter::parse_source(&raw);
            if !is_unedited_spur_managed_source(&raw, parsed.body)
                && !is_legacy_generated_spur_source(&raw)
            {
                return Some(parsed.body.to_string());
            }
        }
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
    SKILL_ID_RE
        .get_or_init(|| regex::Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").expect("static regex"))
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
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "empty",
        });
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

/// Which agent role a skill targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillRole {
    /// Injected into brain prompts; not rendered to worker agent adapters.
    Brain,
    /// Rendered to worker agent adapters; not injected into brain prompts.
    Worker,
    /// Both brain and worker contexts.
    #[default]
    Both,
}

impl std::str::FromStr for SkillRole {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "brain" => Ok(SkillRole::Brain),
            "worker" => Ok(SkillRole::Worker),
            "both" => Ok(SkillRole::Both),
            _ => Err(()),
        }
    }
}

/// A resolved skill ready for rendering across adapters.
#[derive(Debug, Clone)]
pub struct SkillPayload {
    pub id: String,
    pub description: String,
    pub body: String,
    pub source: SkillSource,
    pub role: SkillRole,
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
                description: parsed.description.as_deref().unwrap_or("").to_string(),
                body: parsed.body.to_string(),
                source: SkillSource::Bundled,
                role: parsed.role.unwrap_or(SkillRole::Both),
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
            if is_unedited_spur_managed_source(&raw, parsed.body)
                || is_legacy_generated_spur_source(&raw)
            {
                continue;
            }
            by_id.insert(
                id.clone(),
                SkillPayload {
                    id,
                    description: parsed.description.as_deref().unwrap_or("").to_string(),
                    body: parsed.body.to_string(),
                    source: SkillSource::Override,
                    role: parsed.role.unwrap_or(SkillRole::Both),
                },
            );
        }
    }

    Ok(by_id.into_values().collect())
}

fn is_unedited_spur_managed_source(raw: &str, body: &str) -> bool {
    raw.lines()
        .filter_map(|line| installer::parse_marker(line.trim()))
        .any(|marker| installer::sha256_hex(body.as_bytes()) == marker.sha256)
}

fn is_legacy_generated_spur_source(raw: &str) -> bool {
    raw.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line == "<!-- GENERATED BY SPUR. DO NOT EDIT. -->")
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
            "test-driven-development",
            "systematic-debugging",
            "verification-before-completion",
            "receiving-code-review",
            "requesting-code-review",
            "spur-way",
            "beads-lifecycle",
            "worker-signals",
            "brain-review-gate",
            "plan-task-discipline",
            "worker-mention-routing",
            "writing-skills",
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
    fn load_skill_ignores_unedited_spur_managed_override() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let stale_body = "Old generated TDD body\n";
        let marker = crate::skills::installer::Marker {
            version: 1,
            skill_id: "test-driven-development".to_string(),
            sha256: crate::skills::installer::sha256_hex(stale_body.as_bytes()),
        };
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: test-driven-development\ndescription: >\n---\n{marker}{stale_body}",
                marker = marker.render(),
            ),
        )
        .unwrap();

        let body = load_skill("test-driven-development", dir.path()).unwrap();

        assert!(!body.contains(stale_body));
        assert!(body.contains("# Test-Driven Development"));
    }

    #[test]
    fn load_skill_ignores_legacy_generated_override() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "<!-- GENERATED BY SPUR. DO NOT EDIT. -->\n\n---\nname: test-driven-development\ndescription: old\n---\nOld legacy body\n",
        )
        .unwrap();

        let body = load_skill("test-driven-development", dir.path()).unwrap();

        assert!(!body.contains("Old legacy body"));
        assert!(body.contains("# Test-Driven Development"));
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
        assert!(
            ids.contains(&"spur-way"),
            "spur-way skill should be bundled"
        );
        assert!(
            ids.contains(&"beads-lifecycle"),
            "beads-lifecycle skill should be bundled"
        );
        assert!(
            ids.contains(&"worker-signals"),
            "worker-signals skill should be bundled"
        );
        assert!(
            ids.contains(&"worker-mention-routing"),
            "worker-mention-routing skill should be bundled"
        );
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
    fn list_active_skills_ignores_unedited_spur_managed_skill_files() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&override_dir).unwrap();
        let body = "# Test-Driven Development (TDD)\n\nOld generated body\n";
        let marker = crate::skills::installer::Marker {
            version: 1,
            skill_id: "test-driven-development".to_string(),
            sha256: crate::skills::installer::sha256_hex(body.as_bytes()),
        };
        std::fs::write(
            override_dir.join("SKILL.md"),
            format!(
                "---\nname: test-driven-development\ndescription: >\nrole: both\n---\n{marker}{body}",
                marker = marker.render(),
            ),
        )
        .unwrap();

        let skills = list_active_skills(dir.path()).unwrap();
        let tdd = skills
            .iter()
            .find(|s| s.id == "test-driven-development")
            .unwrap();
        assert!(matches!(tdd.source, SkillSource::Bundled));
        assert_eq!(tdd.role, SkillRole::Worker);
        assert_eq!(
            tdd.description,
            "Use when implementing any feature or bugfix, before writing implementation code"
        );
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

    #[test]
    fn spur_way_skill_contains_beads_first_invariant() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("spur-way", &fake).unwrap();
        assert!(body.contains("beads is the sole source of truth"));
        assert!(body.contains("INTENT"));
        assert!(body.contains("ACTION"));
        assert!(body.contains("RECORD"));
    }

    #[test]
    fn beads_lifecycle_skill_contains_status_fsm() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("beads-lifecycle", &fake).unwrap();
        assert!(body.contains("open"));
        assert!(body.contains("in_progress"));
        assert!(body.contains("signal:"));
    }

    #[test]
    fn worker_signals_skill_contains_exact_format() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("worker-signals", &fake).unwrap();
        assert!(body.contains("[[spur-signal v1]]"));
        assert!(body.contains("signal_id"));
        assert!(body.contains("severity"));
    }

    #[test]
    fn brain_review_gate_skill_contains_checklist() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("brain-review-gate", &fake).unwrap();
        assert!(body.contains("NO APPROVAL WITHOUT BEADS VERIFICATION"));
        assert!(body.contains("Audit Trail Check"));
    }

    #[test]
    fn plan_task_discipline_skill_contains_dag_rules() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("plan-task-discipline", &fake).unwrap();
        assert!(body.contains("DAG"));
        assert!(body.contains("Pending"));
        assert!(body.contains("Approved"));
    }

    #[test]
    fn worker_mention_routing_skill_contains_hierarchy() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("worker-mention-routing", &fake).unwrap();
        assert!(
            body.contains("User @mention outranks your algorithm"),
            "should declare user intent supremacy"
        );
        assert!(
            body.contains("list_available_workers"),
            "should require validation"
        );
        assert!(
            body.contains("avoid_for"),
            "should reference avoid_for override condition"
        );
    }

    #[test]
    fn brainstorming_skill_contains_beads_epic_creation() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("brainstorming", &fake).unwrap();
        assert!(
            body.contains("create_issue"),
            "should instruct creating beads epic"
        );
        assert!(
            body.contains("NO IMPLEMENTATION WITHOUT AN APPROVED SPEC AND A BEADS EPIC"),
            "should enforce beads-first design gate"
        );
        assert!(
            body.contains("Invoke writing-plans"),
            "should hand off to writing-plans"
        );
    }

    #[test]
    fn writing_plans_skill_contains_dag_and_beads_integration() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("writing-plans", &fake).unwrap();
        assert!(
            body.contains("submit_plan"),
            "should reference plan submission"
        );
        assert!(
            body.contains("Depends on:"),
            "should define task dependencies"
        );
        assert!(
            body.contains("spur:plan-task-id"),
            "should reference beads plan task labels"
        );
        assert!(
            body.contains("Scope Boundary:"),
            "should define worker scope boundaries"
        );
    }

    #[test]
    fn brainstorming_description_contains_trigger_phrases() {
        let raw = all_bundled_raw().get("brainstorming").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("");
        assert!(
            desc.contains("brainstorm") || desc.contains("design"),
            "description should contain trigger phrases for matching, got: {desc}"
        );
    }

    #[test]
    fn writing_plans_description_contains_trigger_phrases() {
        let raw = all_bundled_raw().get("writing-plans").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("");
        assert!(
            desc.contains("plan") || desc.contains("tasks"),
            "description should contain trigger phrases for matching, got: {desc}"
        );
    }
}
