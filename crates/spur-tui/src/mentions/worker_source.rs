//! Worker `@`-mention source. Emits one entry per known worker
//! agent. The snapshot is supplied at construction time and is
//! independent of `cwd`.

use std::path::Path;

use super::entry::{MentionEntry, MentionKind, MentionSource};

#[derive(Debug, Clone)]
pub struct WorkerMentionDescriptor {
    /// Unique slug, e.g. `"claude-code"`.
    pub name: String,
    /// `delegation.description` from the agent config; shown as the
    /// row's `secondary` label in the picker.
    pub description: Option<String>,
    /// `"specialist"` or `"generalist"`; rendered as `⟨specialist⟩`.
    pub tier: Option<String>,
}

pub struct WorkerMentionSource {
    snapshot: Vec<WorkerMentionDescriptor>,
}

impl WorkerMentionSource {
    pub fn new(snapshot: Vec<WorkerMentionDescriptor>) -> Self {
        Self { snapshot }
    }
}

impl MentionSource for WorkerMentionSource {
    fn name(&self) -> &'static str {
        "worker"
    }

    fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        Ok(self
            .snapshot
            .iter()
            .map(|d| MentionEntry {
                kind: MentionKind::Worker,
                uri: format!("worker://{}", d.name),
                display: format!("worker:{}", d.name),
                secondary: d.description.clone(),
                tag: d.tier.clone(),
                search_text: None,
                atom_text: None,
                issue_preview: None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: &str, desc: Option<&str>, tier: Option<&str>) -> WorkerMentionDescriptor {
        WorkerMentionDescriptor {
            name: name.into(),
            description: desc.map(str::to_string),
            tier: tier.map(str::to_string),
        }
    }

    #[test]
    fn emits_one_entry_per_descriptor() {
        let mut src = WorkerMentionSource::new(vec![
            descriptor("claude-code", Some("Refactors Rust"), Some("specialist")),
            descriptor("codex", Some("Writes tests"), Some("generalist")),
            descriptor("kiro", None, None),
        ]);
        let cwd = std::path::PathBuf::from("/");
        let entries = src.build(&cwd).expect("build ok");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, MentionKind::Worker);
        assert_eq!(entries[0].uri, "worker://claude-code");
        assert_eq!(entries[0].display, "worker:claude-code");
        assert_eq!(entries[0].secondary.as_deref(), Some("Refactors Rust"));
        assert_eq!(entries[0].tag.as_deref(), Some("specialist"));
        assert_eq!(entries[2].secondary, None);
        assert_eq!(entries[2].tag, None);
    }

    #[test]
    fn name_is_worker() {
        let src = WorkerMentionSource::new(vec![]);
        assert_eq!(src.name(), "worker");
    }
}
