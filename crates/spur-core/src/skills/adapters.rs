//! Per-agent skill file renderers. Each adapter knows its target path
//! and frontmatter schema; rendering is a pure function of SkillPayload.

use std::path::{Path, PathBuf};

use super::SkillPayload;

/// A file the installer intends to write.
#[derive(Debug, Clone)]
pub struct RenderedFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// Every per-skill file destination this installer knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Adapter {
    SpurHermetic,
    ClaudeCode,
    Codex,
    Gemini,
    Kiro,
    OpenCode,
    Cursor,
    Kimi,
}

impl Adapter {
    /// All 8 adapters in deterministic iteration order.
    pub fn all() -> &'static [Adapter] {
        &[
            Adapter::SpurHermetic,
            Adapter::ClaudeCode,
            Adapter::Codex,
            Adapter::Gemini,
            Adapter::Kiro,
            Adapter::OpenCode,
            Adapter::Cursor,
            Adapter::Kimi,
        ]
    }

    /// Render a single skill into this adapter's target path.
    pub fn render(&self, skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
        match self {
            Adapter::SpurHermetic => render_agentskills(skill, &repo_root.join(".spur/skills"), ""),
            Adapter::ClaudeCode => {
                render_agentskills(skill, &repo_root.join(".claude/skills"), "spurpower-")
            }
            Adapter::Codex => render_codex(skill, repo_root),
            Adapter::Gemini => {
                render_agentskills(skill, &repo_root.join(".gemini/skills"), "spurpower-")
            }
            Adapter::Kiro => {
                render_agentskills(skill, &repo_root.join(".kiro/skills"), "spurpower-")
            }
            Adapter::OpenCode => {
                render_agentskills(skill, &repo_root.join(".opencode/skills"), "spurpower-")
            }
            Adapter::Cursor => render_cursor(skill, repo_root),
            Adapter::Kimi => {
                render_agentskills(skill, &repo_root.join(".kimi/skills"), "spurpower-")
            }
        }
    }

    /// Whether this adapter targets worker agents (as opposed to the brain).
    /// Used for role-gated rendering: brain-only skills skip worker adapters.
    pub fn targets_workers(&self) -> bool {
        match self {
            // SpurHermetic is the override directory used by SPUR itself;
            // it serves both roles depending on context.
            Adapter::SpurHermetic => true,
            // All external agent adapters target workers.
            Adapter::ClaudeCode
            | Adapter::Codex
            | Adapter::Gemini
            | Adapter::Kiro
            | Adapter::OpenCode
            | Adapter::Cursor
            | Adapter::Kimi => true,
        }
    }
}

// --- render helpers land in tasks 7-10 ---

fn render_agentskills(skill: &SkillPayload, target_root: &Path, name_prefix: &str) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    use crate::skills::SkillRole;
    let id = format!("{name_prefix}{}", skill.id);
    let path = target_root.join(&id).join("SKILL.md");
    let body = &skill.body;
    let role_line = if name_prefix.is_empty() {
        let role = match skill.role {
            SkillRole::Brain => "brain",
            SkillRole::Worker => "worker",
            SkillRole::Both => "both",
        };
        format!("role: {role}\n")
    } else {
        String::new()
    };
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(body.as_bytes()),
    };
    let bytes = format!(
        "---\nname: {id}\ndescription: {desc}\n{role_line}---\n{marker}{body}",
        id = id,
        desc = yaml_double_quoted(&skill.description),
        role_line = role_line,
        marker = marker.render(),
        body = body,
    )
    .into_bytes();
    RenderedFile { path, bytes }
}

fn render_codex(skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let id = format!("spurpower-{}", skill.id);
    let path = repo_root.join(".codex/skills").join(&id).join("SKILL.md");
    let body = &skill.body;
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(body.as_bytes()),
    };
    let bytes = format!(
        "---\nname: {id}\ndescription: {desc}\n---\n{marker}{body}",
        id = id,
        desc = yaml_double_quoted(&skill.description),
        marker = marker.render(),
        body = body,
    )
    .into_bytes();
    RenderedFile { path, bytes }
}

fn render_cursor(skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let id = format!("spurpower-{}", skill.id);
    let path = repo_root.join(".cursor/rules").join(format!("{id}.mdc"));
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(skill.body.as_bytes()),
    };
    let bytes = format!(
        "---\ndescription: {desc}\nalwaysApply: true\n---\n{marker}{body}",
        desc = yaml_double_quoted(&skill.description),
        marker = marker.render(),
        body = skill.body,
    )
    .into_bytes();
    RenderedFile { path, bytes }
}

fn yaml_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Single per-run steering file that points Kiro's default agent at
/// `.kiro/skills/spurpower-*`. Emitted once per installer run, not per
/// skill. Uses the reserved skill id `__pointer` in its marker.
pub fn render_kiro_steering_pointer(repo_root: &Path) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let path = repo_root.join(".kiro/steering/spurpower-pointer.md");
    let body = "SpurPower tactical skills live in \
                `.kiro/skills/spurpower-*/SKILL.md` and are also available \
                as editable workspace overrides in `.spur/skills/<id>/`. \
                Use them for TDD, systematic debugging, code review, and \
                brain-delegation workflows.\n";
    let marker = Marker {
        version: 1,
        skill_id: "__pointer".to_string(),
        sha256: sha256_hex(body.as_bytes()),
    };
    let bytes = format!(
        "---\ninclusion: always\nname: spurpower-pointer\n\
         description: Pointer to SpurPower tactical skills in .kiro/skills/spurpower-*\n\
         ---\n{marker}{body}",
        marker = marker.render(),
        body = body,
    )
    .into_bytes();
    RenderedFile { path, bytes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skill() -> SkillPayload {
        SkillPayload {
            id: "tdd".to_string(),
            description: "Use for TDD".to_string(),
            body: "Write the test first.\n".to_string(),
            source: crate::skills::SkillSource::Bundled,
            role: crate::skills::SkillRole::Both,
        }
    }

    #[test]
    fn adapter_all_has_eight_unique() {
        let all = Adapter::all();
        assert_eq!(all.len(), 8);
        let mut seen = std::collections::HashSet::new();
        for a in all {
            assert!(seen.insert(*a), "{a:?} appears twice");
        }
    }

    #[test]
    fn agentskills_render_no_prefix() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_agentskills(&skill, &root.join(".spur/skills"), "");
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.spur/skills/tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\nname: tdd\ndescription: \"Use for TDD\"\nrole: both\n---\n"));
        assert!(s.contains("<!-- SPUR-MANAGED v=1 skill=tdd sha256="));
        assert!(s.trim_end().ends_with("Write the test first."));
    }

    #[test]
    fn agentskills_render_with_spurpower_prefix() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_agentskills(&skill, &root.join(".claude/skills"), "spurpower-");
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.claude/skills/spurpower-tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.contains("name: spurpower-tdd"));
        assert!(s.contains("skill=spurpower-tdd"));
    }

    #[test]
    fn agentskills_render_is_deterministic() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let a = render_agentskills(&skill, &root.join(".claude/skills"), "spurpower-");
        let b = render_agentskills(&skill, &root.join(".claude/skills"), "spurpower-");
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn codex_render_has_frontmatter_and_marker() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_codex(&skill, &root);
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.codex/skills/spurpower-tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\nname: spurpower-tdd\ndescription: \"Use for TDD\"\n---\n"));
        assert!(
            s.contains("<!-- SPUR-MANAGED v=1 skill=spurpower-tdd sha256="),
            "marker must be present, got: {s:?}",
        );
        assert!(s.contains("Write the test first."));
    }

    #[test]
    fn cursor_render_mdc_with_alwaysapply() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_cursor(&skill, &root);
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.cursor/rules/spurpower-tdd.mdc"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\ndescription: \"Use for TDD\"\nalwaysApply: true\n---\n"));
        assert!(!s.contains("globs:"));
        assert!(s.contains("<!-- SPUR-MANAGED v=1 skill=spurpower-tdd sha256="));
        assert!(s.contains("Write the test first."));
    }

    #[test]
    fn render_with_empty_prefix_uses_bare_id() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = Adapter::Codex.render_with_prefix(&skill, &root, "");
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.codex/skills/tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\nname: tdd\ndescription: \"Use for TDD\"\n---\n"));
        assert!(s.contains("<!-- SPUR-MANAGED v=1 skill=tdd sha256="));
    }

    #[test]
    fn kiro_steering_pointer_renders_once() {
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_kiro_steering_pointer(&root);
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.kiro/steering/spurpower-pointer.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\ninclusion: always\n"));
        assert!(s.contains("name: spurpower-pointer"));
        assert!(s.contains("<!-- SPUR-MANAGED v=1 skill=__pointer sha256="));
        assert!(s.contains(".kiro/skills/spurpower-"));
    }

    #[test]
    fn adapter_render_all_variants() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let expected_prefixes = [
            (Adapter::SpurHermetic, "/tmp/repo/.spur/skills/tdd/"),
            (
                Adapter::ClaudeCode,
                "/tmp/repo/.claude/skills/spurpower-tdd/",
            ),
            (Adapter::Codex, "/tmp/repo/.codex/skills/spurpower-tdd/"),
            (Adapter::Gemini, "/tmp/repo/.gemini/skills/spurpower-tdd/"),
            (Adapter::Kiro, "/tmp/repo/.kiro/skills/spurpower-tdd/"),
            (
                Adapter::OpenCode,
                "/tmp/repo/.opencode/skills/spurpower-tdd/",
            ),
            (Adapter::Cursor, "/tmp/repo/.cursor/rules/"),
            (Adapter::Kimi, "/tmp/repo/.kimi/skills/spurpower-tdd/"),
        ];
        for (a, prefix) in expected_prefixes {
            let rf = a.render(&skill, &root);
            assert!(
                rf.path.to_string_lossy().starts_with(prefix),
                "{a:?}: got {}, expected prefix {}",
                rf.path.display(),
                prefix,
            );
            assert!(!rf.bytes.is_empty());
        }
    }
}
