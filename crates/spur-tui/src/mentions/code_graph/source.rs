//! TUI compatibility surface over the shared code-graph mention source
//! (sud-m4, decoupling spec §4.5).
//!
//! Discovery, artifact caching, index compaction, and payload hydration
//! moved to [`spur_mentions::code::source`] behind the `code` feature and
//! are re-exported here so existing `crate::mentions::code_graph::source`
//! paths keep compiling. What stays in the TUI:
//!
//! - the [`MentionSource`] (TUI trait) impl for the shared type: it builds
//!   the neutral snapshot and maps each neutral row onto the TUI's richer
//!   row (`code_path`, `tag` dressing for file rows), keeping the M3
//!   facade/adapter flow unchanged;
//! - [`entry_for_candidate`] and the secondary-line rendering: symbol row
//!   dressing (tags, `@scope::name` atoms) is frontend row policy, not
//!   shared-crate data.

use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use spur_graph::GraphSymbolArtifact;
use spur_graph::{CodeMentionPayload, CODE_SYMBOL_URI_PREFIX};
use spur_mentions::code::source::CodeGraphMentionSource as SharedCodeGraphSource;
use spur_mentions::{
    MentionEntry as NeutralMentionEntry, MentionSource as NeutralMentionSource, SourceContext,
};

use crate::mentions::entry::{MentionEntry, MentionKind, MentionSource};

pub use spur_mentions::code::source::{CodeGraphMentionSource, CodeMentionCandidate};

impl MentionSource for CodeGraphMentionSource {
    fn name(&self) -> &'static str {
        "code_graph"
    }

    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        let snapshot = NeutralMentionSource::build(self, cwd, &SourceContext::default())?;
        Ok(snapshot
            .entries
            .iter()
            .map(tui_entry_from_neutral)
            .collect())
    }

    fn code_payloads(&self) -> &[(String, Arc<CodeMentionPayload>)] {
        SharedCodeGraphSource::code_payloads(self)
    }

    fn code_candidates(&self) -> Arc<Vec<CodeMentionCandidate>> {
        SharedCodeGraphSource::code_candidates(self)
    }

    fn hydrate_code_payloads(
        &self,
        stable_symbol_ids: &[String],
    ) -> anyhow::Result<Vec<(String, Arc<CodeMentionPayload>)>> {
        SharedCodeGraphSource::hydrate_code_payloads(self, stable_symbol_ids)
    }
}

/// Map one neutral code-graph row onto the TUI row shape. File rows regain
/// their TUI dressing (`code_path`, the `file` tag); anything else keeps the
/// neutral data and default dressing.
fn tui_entry_from_neutral(neutral: &NeutralMentionEntry) -> MentionEntry {
    let kind = crate::mentions::sidecar::tui_kind(&neutral.kind).unwrap_or(MentionKind::File);
    let (code_path, tag) = if matches!(kind, MentionKind::CodeFile) {
        (Some(neutral.display.clone()), Some("file".to_owned()))
    } else {
        (None, None)
    };
    MentionEntry {
        section_header: None,
        kind,
        uri: neutral.uri.clone(),
        display: neutral.display.clone(),
        secondary: neutral.secondary.clone(),
        agent: None,
        model: None,
        effort: None,
        worker_kind: None,
        worker_cli_identity: None,
        code_path,
        code_scope: None,
        tag,
        search_text: neutral.search_text.clone(),
        atom_text: None,
        unconsumed_suffix: None,
        issue_preview: None,
    }
}

/// TUI symbol row for one compact candidate (tag, secondary line, and the
/// `@scope::name` atom text are picker policy and stay TUI-owned).
pub(crate) fn entry_for_candidate(candidate: &CodeMentionCandidate) -> MentionEntry {
    let display = candidate.entity_name.to_string();
    let code_scope = candidate
        .enclosing_scope
        .as_ref()
        .map(|scope| scope.to_string());
    let atom_text = code_scope
        .as_ref()
        .filter(|scope| !scope.is_empty())
        .map(|scope| format!("@{}::{}", scope, display));
    MentionEntry {
        section_header: None,
        kind: MentionKind::CodeSymbol,
        uri: format!("{}{}", CODE_SYMBOL_URI_PREFIX, candidate.stable_symbol_id),
        display: display.clone(),
        secondary: Some(symbol_secondary_fields(
            &display,
            candidate.file_path.as_ref(),
            candidate.line_range,
            candidate.symbol_kind.as_ref(),
            code_scope.as_deref(),
        )),
        agent: None,
        model: None,
        effort: None,
        worker_kind: None,
        worker_cli_identity: None,
        code_path: Some(candidate.file_path.to_string()),
        code_scope,
        tag: Some(format!("symbol:{}", candidate.symbol_kind)),
        search_text: None,
        atom_text,
        unconsumed_suffix: None,
        issue_preview: None,
    }
}

fn symbol_secondary_fields(
    entity_name: &str,
    file_path: &str,
    line_range: [usize; 2],
    symbol_kind: &str,
    enclosing_scope: Option<&str>,
) -> String {
    if let Some(scope) = enclosing_scope {
        format!(
            "{}::{} · {}:{} ({})",
            scope, entity_name, file_path, line_range[0], symbol_kind
        )
    } else {
        format!(
            "{} · {}:{} ({})",
            entity_name, file_path, line_range[0], symbol_kind
        )
    }
}

#[cfg(test)]
fn symbol_secondary(symbol: &GraphSymbolArtifact) -> String {
    symbol_secondary_fields(
        &symbol.entity_name,
        &symbol.file_path,
        symbol.line_range,
        &symbol.symbol_kind,
        symbol.enclosing_scope.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_secondary_renders_scope_name_path_line_and_kind() {
        let scoped = GraphSymbolArtifact {
            stable_symbol_id: "symbol-cache-run".to_owned(),
            file_path: "crates/example/src/cache.rs".to_owned(),
            byte_range: [0, 20],
            line_range: [120, 145],
            entity_name: "run".to_owned(),
            qualified_name: "Cache::run".to_owned(),
            symbol_kind: "fn".to_owned(),
            anchor_hash: "anchor-cache-run".to_owned(),
            enclosing_scope: Some("Cache".to_owned()),
        };
        assert_eq!(
            symbol_secondary(&scoped),
            "Cache::run · crates/example/src/cache.rs:120 (fn)"
        );

        let bare = GraphSymbolArtifact {
            stable_symbol_id: "symbol-cache".to_owned(),
            file_path: "crates/example/src/cache.rs".to_owned(),
            byte_range: [0, 20],
            line_range: [42, 88],
            entity_name: "Cache".to_owned(),
            qualified_name: "Cache".to_owned(),
            symbol_kind: "struct".to_owned(),
            anchor_hash: "anchor-cache".to_owned(),
            enclosing_scope: None,
        };
        assert_eq!(
            symbol_secondary(&bare),
            "Cache · crates/example/src/cache.rs:42 (struct)"
        );
    }

    #[test]
    fn neutral_file_rows_regain_tui_dressing() {
        let neutral = NeutralMentionEntry {
            id: spur_mentions::MentionId::new(0),
            kind: spur_mentions::MentionKind::CodeFile,
            uri: "graph://file/file-1".to_owned(),
            display: "src/lib.rs".to_owned(),
            secondary: None,
            search_text: Some("src/lib.rs".to_owned()),
            insert_text: None,
        };

        let tui = tui_entry_from_neutral(&neutral);

        assert_eq!(tui.kind, MentionKind::CodeFile);
        assert_eq!(tui.display, "src/lib.rs");
        assert_eq!(tui.code_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(tui.tag.as_deref(), Some("file"));
        assert_eq!(tui.search_text.as_deref(), Some("src/lib.rs"));
        assert!(tui.atom_text.is_none());
        assert!(tui.secondary.is_none());
    }
}
