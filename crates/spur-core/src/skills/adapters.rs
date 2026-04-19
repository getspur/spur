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
    _skill: &SkillPayload,
    _target_root: &Path,
    _name_prefix: &str,
) -> RenderedFile {
    unimplemented!("Task 7")
}

fn render_codex(_skill: &SkillPayload, _repo_root: &Path) -> RenderedFile {
    unimplemented!("Task 8")
}

fn render_cursor(_skill: &SkillPayload, _repo_root: &Path) -> RenderedFile {
    unimplemented!("Task 9")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_all_has_seven_unique() {
        let all = Adapter::all();
        assert_eq!(all.len(), 7);
        let mut seen = std::collections::HashSet::new();
        for a in all {
            assert!(seen.insert(*a), "{a:?} appears twice");
        }
    }
}
