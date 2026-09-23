//! File-tree mention source for the TUI.
//!
//! Traversal itself is delegated to the shared engine source
//! (`spur_mentions::FileMentionSource`) under the TUI compatibility
//! `FilesystemProfile`: the walker flags, ignore-rule handling, directory
//! rows, and path→row conversion are owned by the neutral crate and pinned
//! byte-for-byte to the profile table (2026-08-27 mentions spec §3). This
//! adapter exists only to map the neutral file rows onto the TUI entry
//! shape — every TUI-only field is `None` for file rows — so the registry's
//! sources stay homogeneous behind the facade's `MentionSource` trait.

use std::path::Path;

use spur_mentions::entry::{
    MentionEntry as NeutralMentionEntry, MentionKind as NeutralMentionKind, SourceContext,
};
use spur_mentions::{
    FileMentionSource as SharedFileMentionSource, FilesystemProfile, MentionSource as _,
};

use super::entry::{MentionEntry, MentionKind, MentionSource};

/// Walker-backed filesystem source producing TUI file-tree rows.
pub struct FileMentionSource {
    shared: SharedFileMentionSource,
}

impl FileMentionSource {
    pub fn new() -> Self {
        Self {
            shared: SharedFileMentionSource::new(),
        }
    }
}

impl Default for FileMentionSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MentionSource for FileMentionSource {
    fn name(&self) -> &'static str {
        "file"
    }

    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        let context = SourceContext {
            filesystem_profile: FilesystemProfile::tui(),
        };
        let snapshot = self.shared.build(cwd, &context).map_err(|error| {
            // The TUI profile never fails typed (walk errors stay
            // flattened), so this arm is unreachable in practice.
            anyhow::anyhow!("file mention traversal failed: {}", error.message)
        })?;
        Ok(snapshot.entries.iter().map(file_entry).collect())
    }
}

/// Map one neutral file-tree row onto the TUI entry shape. File and
/// directory rows carry no TUI-only metadata, so the mapping is total.
fn file_entry(entry: &NeutralMentionEntry) -> MentionEntry {
    MentionEntry {
        kind: match entry.kind {
            NeutralMentionKind::Directory => MentionKind::Directory,
            _ => MentionKind::File,
        },
        uri: entry.uri.clone(),
        display: entry.display.clone(),
        ..MentionEntry::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegates_traversal_to_the_shared_profile() {
        let tmp = tempfile::tempdir().unwrap();
        // Mark the tempdir as a git worktree so .gitignore rules activate.
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(tmp.path().join("vis.rs"), "// visible").unwrap();
        std::fs::write(tmp.path().join("ignored.rs"), "// ignored").unwrap();
        std::fs::write(tmp.path().join(".hidden.rs"), "// hidden").unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/lib.rs"), "// lib").unwrap();

        let mut source = FileMentionSource::new();
        let entries = source.build(tmp.path()).expect("build ok");

        let displays: Vec<&str> = entries.iter().map(|e| e.display.as_str()).collect();
        assert!(displays.contains(&"vis.rs"), "{displays:?}");
        assert!(
            displays.contains(&"src/"),
            "directory rows keep their slash"
        );
        assert!(
            !displays
                .iter()
                .any(|d| d.contains("ignored") || d.starts_with('.')),
            "hidden and ignored entries are skipped by the shared profile: {displays:?}"
        );

        let file = entries.iter().find(|e| e.display == "vis.rs").unwrap();
        assert_eq!(file.kind, MentionKind::File);
        assert_eq!(
            file.uri,
            format!("file://{}", tmp.path().join("vis.rs").display())
        );
        let dir = entries.iter().find(|e| e.display == "src/").unwrap();
        assert_eq!(dir.kind, MentionKind::Directory);
    }
}
