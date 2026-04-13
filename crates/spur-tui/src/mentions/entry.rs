use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub struct MentionEntry {
    pub kind: MentionKind,
    /// File URI, e.g. "file:///abs/src/foo.rs".
    pub uri: String,
    /// Relative path for display (directories end with '/').
    pub display: String,
}

pub trait MentionSource: Send {
    /// Rebuild the candidate list from scratch.
    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>>;
    fn name(&self) -> &'static str;
}

/// Convert an absolute path under cwd into a `MentionEntry`.
pub fn entry_for_path(cwd: &Path, abs: &Path) -> Option<MentionEntry> {
    let rel = abs.strip_prefix(cwd).ok()?;
    let rel_str = rel.to_str()?;
    let kind = if abs.is_dir() {
        MentionKind::Directory
    } else {
        MentionKind::File
    };
    let display = match kind {
        MentionKind::Directory => format!("{}/", rel_str),
        MentionKind::File => rel_str.to_string(),
    };
    let abs_str = abs.to_str()?;
    let uri = format!("file://{}", abs_str);
    Some(MentionEntry { kind, uri, display })
}
