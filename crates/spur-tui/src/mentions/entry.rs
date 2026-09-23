use std::path::Path;
use std::sync::Arc;

use super::issue_source::IssueMentionDescriptor;
use spur_acp::AgentKind;
use spur_graph::CodeMentionPayload;

#[derive(Debug, Clone)]
pub struct CodeMentionCandidate {
    pub(crate) stable_symbol_id: Box<str>,
    pub(crate) entity_name: Box<str>,
    pub(crate) file_path: Arc<str>,
    pub(crate) line_range: [usize; 2],
    pub(crate) symbol_kind: Arc<str>,
    pub(crate) enclosing_scope: Option<Arc<str>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MentionKind {
    #[default]
    File,
    Directory,
    CodeFile,
    CodeSymbol,
    Worker,
    Issue,
    Datasource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionEntry {
    /// Optional synthetic section header marker (empty-query grouping rows).
    pub section_header: Option<&'static str>,
    pub kind: MentionKind,
    /// File URI (`file:///abs/...`), worker URI (`worker://<name>`), or
    /// datasource URI (`datasource://<name>`).
    pub uri: String,
    /// Display label. For files: relative path (dirs end with `/`).
    /// For workers: `worker:<name>` (e.g. `worker:claude-code`).
    pub display: String,
    /// Optional one-line description (worker description; None for files).
    pub secondary: Option<String>,
    /// Optional worker persona/profile selected by a composed worker mention.
    pub agent: Option<String>,
    /// Optional model selected by a composed worker mention.
    pub model: Option<String>,
    /// Optional effort selected by a composed worker mention.
    pub effort: Option<String>,
    /// Registered kind for worker mention rows.
    pub worker_kind: Option<AgentKind>,
    /// Command + effective args used to decide whether a cached catalog row is stale.
    pub worker_cli_identity: Option<String>,
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
    /// Plain text that followed the consumed mention slots and must remain
    /// outside the protected atom when the picker accepts the row.
    pub unconsumed_suffix: Option<String>,
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
            agent: None,
            model: None,
            effort: None,
            worker_kind: None,
            worker_cli_identity: None,
            code_path: None,
            code_scope: None,
            tag: None,
            search_text: None,
            atom_text: None,
            unconsumed_suffix: None,
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

    fn code_candidates(&self) -> Arc<Vec<CodeMentionCandidate>> {
        Arc::default()
    }

    fn hydrate_code_payloads(
        &self,
        _stable_symbol_ids: &[String],
    ) -> anyhow::Result<Vec<(String, Arc<CodeMentionPayload>)>> {
        Ok(Vec::new())
    }

    fn datasource_hints(&self) -> &[(String, Arc<String>)] {
        &[]
    }
}
