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
}

impl Adapter {
    /// All 7 adapters in deterministic iteration order.
    pub fn all() -> &'static [Adapter] {
        &[
            Adapter::SpurHermetic,
            Adapter::ClaudeCode,
            Adapter::Codex,
            Adapter::Gemini,
            Adapter::Kiro,
            Adapter::OpenCode,
            Adapter::Cursor,
        ]
    }

    /// Render a single skill into this adapter's target path.
    pub fn render(&self, skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
        match self {
            Adapter::SpurHermetic => {
                render_agentskills(skill, &repo_root.join(".spur/skills"), "")
            }
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
        }
    }
}

// --- render helpers land in tasks 7-10 ---

fn render_agentskills(
    skill: &SkillPayload,
    target_root: &Path,
    name_prefix: &str,
) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let id = format!("{name_prefix}{}", skill.id);
    let path = target_root.join(&id).join("SKILL.md");
    let body = &skill.body;
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(body.as_bytes()),
    };
    let bytes = format!(
        "---\nname: {id}\ndescription: {desc}\n---\n{marker}{body}",
        id = id,
        desc = skill.description,
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
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(skill.body.as_bytes()),
    };
    let bytes = format!("{}{}", marker.render(), skill.body).into_bytes();
    RenderedFile { path, bytes }
}

fn render_cursor(_skill: &SkillPayload, _repo_root: &Path) -> RenderedFile {
    unimplemented!("Task 9")
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
        }
    }

    #[test]
    fn adapter_all_has_seven_unique() {
        let all = Adapter::all();
        assert_eq!(all.len(), 7);
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
        assert!(s.starts_with("---\nname: tdd\ndescription: Use for TDD\n---\n"));
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
    fn codex_render_has_no_frontmatter_and_marker_on_line_one() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_codex(&skill, &root);
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.codex/skills/spurpower-tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(
            s.starts_with("<!-- SPUR-MANAGED v=1 skill=spurpower-tdd sha256="),
            "marker must be on line 1, got: {s:?}",
        );
        assert!(s.contains("Write the test first."));
    }
}
