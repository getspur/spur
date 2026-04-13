use super::entry::{entry_for_path, MentionEntry, MentionSource};
use ignore::WalkBuilder;
use std::path::Path;

/// Single walker that yields both files and directories under `cwd`,
/// honoring `.gitignore`, `.ignore`, and `.rgignore`.
pub struct FileMentionSource;

impl MentionSource for FileMentionSource {
    fn name(&self) -> &'static str {
        "file"
    }

    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        let mut out = Vec::new();
        let walker = WalkBuilder::new(cwd)
            .follow_links(false)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .ignore(true)
            .build();
        for dent in walker.flatten() {
            let p = dent.path();
            if p == cwd {
                continue;
            }
            if let Some(e) = entry_for_path(cwd, p) {
                out.push(e);
            }
        }
        Ok(out)
    }
}
