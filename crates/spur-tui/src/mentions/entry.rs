use std::path::Path;
use std::sync::Arc;

use super::issue_source::IssueMentionDescriptor;
use spur_graph::CodeMentionPayload;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Directory,
    CodeFile,
    CodeSymbol,
    Worker,
    Issue,
}

#[derive(Debug, Clone)]
pub struct MentionEntry {
    /// Optional synthetic section header marker (empty-query grouping rows).
    pub section_header: Option<&'static str>,
    pub kind: MentionKind,
    /// File URI (`file:///abs/...`) or worker URI (`worker://<name>`).
    pub uri: String,
    /// Display label. For files: relative path (dirs end with `/`).
    /// For workers: `worker:<name>` (e.g. `worker:claude-code`).
    pub display: String,
    /// Optional one-line description (worker description; None for files).
    pub secondary: Option<String>,
    /// Optional relative path for code graph file and symbol entries.
    pub code_path: Option<String>,
    /// Optional enclosing scope for code graph symbol entries.
    pub code_scope: Option<String>,
    /// Optional right-aligned tag (worker tier; None for files).
    pub tag: Option<String>,
    /// Optional richer haystack used for ranking.
    pub search_text: Option<String>,
    /// Optional visible InputBar atom text, including the leading `@`.
    pub atom_text: Option<String>,
    /// Optional issue descriptor retained for richer issue-row previews.
    pub issue_preview: Option<Arc<IssueMentionDescriptor>>,
}

impl Default for MentionEntry {
    fn default() -> Self {
        Self {
            section_header: None,
            kind: MentionKind::File,
            uri: String::new(),
            display: String::new(),
            secondary: None,
            code_path: None,
            code_scope: None,
            tag: None,
            search_text: None,
            atom_text: None,
            issue_preview: None,
        }
    }
}

pub trait MentionSource: Send {
    /// Rebuild the candidate list from scratch.
    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>>;
    fn name(&self) -> &'static str;

    fn code_payloads(&self) -> &[(String, Arc<CodeMentionPayload>)] {
        &[]
    }
}

/// Convert an absolute path under cwd into a `MentionEntry`.
/// Only produces `File` and `Directory` kinds; never `Worker`.
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
        MentionKind::CodeFile
        | MentionKind::CodeSymbol
        | MentionKind::Worker
        | MentionKind::Issue => {
            unreachable!("entry_for_path never builds non-file mentions")
        }
    };
    let abs_str = abs.to_str()?;
    let uri = format!("file://{}", abs_str);
    Some(MentionEntry {
        section_header: None,
        kind,
        uri,
        display,
        secondary: None,
        code_path: None,
        code_scope: None,
        tag: None,
        search_text: None,
        atom_text: None,
        issue_preview: None,
    })
}
