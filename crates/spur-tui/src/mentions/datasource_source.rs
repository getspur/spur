//! Notebook datasource `@`-mention source. Emits one entry per datasource
//! snapshot pushed by the notebook daemon bridge.
//!
//! sud-m3: implements the *neutral* [`spur_mentions::MentionSource`]
//! directly (the engine builds it without a TUI adapter). TUI-only row
//! state (atom text, kind tag) rides the sidecar keyed by `MentionId`;
//! the adapter-owned prompt-hint store stays here (decoupling spec §2:
//! datasource prompt hints are not part of the sidecar mapping), and every
//! snapshot replace bumps the data-revision token so the engine cache key
//! invalidates immediately.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use spur_acp::{DatasourceEntry, DatasourceKind};
use spur_mentions::{
    MentionSource as NeutralMentionSource, SourceBuildError, SourceContext, SourceSnapshot,
};

use super::entry::{MentionEntry, MentionKind};
use super::session::SessionMentionSource;
use super::sidecar::{split_rows, TuiMentionSidecar};

pub struct DatasourceMentionSource {
    snapshot: Vec<DatasourceEntry>,
    /// Data-revision token participating in the engine cache key: bumped by
    /// every snapshot replace so the next query rebuilds instead of serving
    /// the retained snapshot until the TTL expires.
    token: u64,
    /// Sidecar records from the most recent build, keyed by row id.
    sidecar: TuiMentionSidecar,
    /// Adapter-owned prompt hints (one per built row), replaced each build.
    prompt_hints: HashMap<String, Arc<String>>,
}

impl DatasourceMentionSource {
    pub fn new(snapshot: Vec<DatasourceEntry>) -> Self {
        Self::with_token(snapshot, 0)
    }

    /// Construct with an explicit data-revision token; the registry stamps
    /// `previous + 1` when swapping a session source so cache identity
    /// changes on every replace.
    pub(crate) fn with_token(snapshot: Vec<DatasourceEntry>, token: u64) -> Self {
        Self {
            snapshot,
            token,
            sidecar: TuiMentionSidecar::new(),
            prompt_hints: HashMap::new(),
        }
    }

    /// Replace the datasource snapshot in place, bumping the data-revision
    /// token.
    pub fn set_snapshot(&mut self, snapshot: Vec<DatasourceEntry>) {
        self.snapshot = snapshot;
        self.token = self.token.wrapping_add(1);
    }

    /// TUI rows plus adapter-owned prompt hints for the current snapshot:
    /// the pre-sidecar row shape the neutral entries and the sidecar
    /// records split from, and the hint store the facade adopts per build.
    fn tui_rows(&self) -> (Vec<MentionEntry>, HashMap<String, Arc<String>>) {
        let mut prompt_hints = HashMap::with_capacity(self.snapshot.len());
        let rows = self
            .snapshot
            .iter()
            .map(|entry| {
                let uri = datasource_uri(&entry.name);
                prompt_hints.insert(uri.clone(), Arc::new(datasource_prompt_hint(entry, &uri)));
                MentionEntry {
                    section_header: None,
                    kind: MentionKind::Datasource,
                    uri,
                    display: entry.name.clone(),
                    secondary: Some(datasource_secondary(entry)),
                    agent: None,
                    model: None,
                    effort: None,
                    worker_kind: None,
                    worker_cli_identity: None,
                    code_path: None,
                    code_scope: None,
                    tag: Some(datasource_kind_label(entry.kind).to_string()),
                    search_text: Some(datasource_search_text(entry)),
                    atom_text: Some(format!("@{}", entry.name)),
                    unconsumed_suffix: None,
                    issue_preview: None,
                }
            })
            .collect();
        (rows, prompt_hints)
    }
}

impl NeutralMentionSource for DatasourceMentionSource {
    fn key(&self) -> &str {
        "datasource"
    }

    fn source_token(&self) -> u64 {
        self.token
    }

    fn build(
        &mut self,
        _root: &Path,
        _context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError> {
        let (rows, prompt_hints) = self.tui_rows();
        let (entries, sidecar) = split_rows(&rows);
        self.sidecar = sidecar;
        self.prompt_hints = prompt_hints;
        Ok(SourceSnapshot::new(entries, 0))
    }
}

impl SessionMentionSource for DatasourceMentionSource {
    fn slot_name(&self) -> &'static str {
        "datasource"
    }

    fn sidecar(&self) -> &TuiMentionSidecar {
        &self.sidecar
    }

    fn datasource_hints(&self) -> Option<&HashMap<String, Arc<String>>> {
        Some(&self.prompt_hints)
    }
}

fn datasource_uri(name: &str) -> String {
    format!("datasource://{name}")
}

fn datasource_secondary(entry: &DatasourceEntry) -> String {
    match entry.group.as_deref() {
        Some(group) if !group.is_empty() => format!("{group} - {}", entry.path),
        _ => entry.path.clone(),
    }
}

fn datasource_search_text(entry: &DatasourceEntry) -> String {
    let mut text = format!(
        "{} {} {}",
        entry.name,
        datasource_kind_label(entry.kind),
        entry.path
    );
    if let Some(group) = entry.group.as_deref() {
        text.push(' ');
        text.push_str(group);
    }
    for column in &entry.columns {
        text.push(' ');
        text.push_str(&column.name);
        text.push(' ');
        text.push_str(&column.sql_type);
    }
    for table in &entry.tables {
        text.push(' ');
        text.push_str(&table.name);
        for column in &table.columns {
            text.push(' ');
            text.push_str(&column.name);
            text.push(' ');
            text.push_str(&column.sql_type);
        }
    }
    text
}

fn datasource_prompt_hint(entry: &DatasourceEntry, uri: &str) -> String {
    let mut text = format!(
        "DATASOURCE {}\nuri: {uri}\nkind: {}\npath: {}",
        entry.name,
        datasource_kind_label(entry.kind),
        entry.path
    );
    if let Some(group) = entry.group.as_deref() {
        text.push_str("\ngroup: ");
        text.push_str(group);
    }
    if let Some(row_count) = entry.row_count {
        text.push_str("\nrow_count: ");
        text.push_str(&row_count.to_string());
    }
    text.push_str("\ncolumns:");
    if entry.columns.is_empty() {
        text.push_str(" none");
    } else {
        for column in &entry.columns {
            text.push_str("\n- ");
            text.push_str(&column.name);
            text.push(' ');
            text.push_str(&column.sql_type);
        }
    }
    text.push_str("\ntables:");
    if entry.tables.is_empty() {
        text.push_str(" none");
    } else {
        for table in &entry.tables {
            text.push_str("\n- ");
            text.push_str(&table.name);
            if let Some(row_count) = table.row_count {
                text.push_str(" row_count=");
                text.push_str(&row_count.to_string());
            }
            for column in &table.columns {
                text.push_str("\n  - ");
                text.push_str(&column.name);
                text.push(' ');
                text.push_str(&column.sql_type);
            }
        }
    }
    text
}

fn datasource_kind_label(kind: DatasourceKind) -> &'static str {
    match kind {
        DatasourceKind::Csv => "csv",
        DatasourceKind::Parquet => "parquet",
        DatasourceKind::Json => "json",
        DatasourceKind::DuckDb => "duckdb",
        DatasourceKind::Sqlite => "sqlite",
        DatasourceKind::ApiTables => "api_tables",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::mentions::sidecar::rejoin_snapshot;
    use crate::mentions::MentionKind;
    use spur_acp::{Column, Table};

    fn entry(name: &str, group: Option<&str>) -> DatasourceEntry {
        DatasourceEntry {
            name: name.to_string(),
            path: format!("./data/{name}.csv"),
            kind: DatasourceKind::Csv,
            group: group.map(str::to_string),
            columns: vec![Column {
                name: "region".to_string(),
                sql_type: "VARCHAR".to_string(),
            }],
            row_count: Some(10),
            tables: vec![Table {
                name: "line_items".to_string(),
                columns: Vec::new(),
                row_count: None,
            }],
        }
    }

    fn build_neutral(source: &mut DatasourceMentionSource) -> SourceSnapshot {
        source
            .build(Path::new("."), &SourceContext::default())
            .expect("build succeeds")
    }

    #[test]
    fn token_changes_when_the_snapshot_is_replaced() {
        let mut src = DatasourceMentionSource::new(vec![entry("sales", None)]);
        let first = NeutralMentionSource::source_token(&src);
        src.set_snapshot(vec![]);
        assert_ne!(first, NeutralMentionSource::source_token(&src));
    }

    #[test]
    fn build_emits_neutral_entries_hints_and_rejoinable_sidecar() {
        let mut source = DatasourceMentionSource::new(vec![
            entry("sales", Some("quarterly")),
            entry("hr", None),
        ]);

        let snapshot = build_neutral(&mut source);

        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries[0].uri, "datasource://sales");
        assert_eq!(snapshot.entries[0].display, "sales");
        assert!(snapshot.entries[0]
            .search_text
            .as_deref()
            .is_some_and(|text| text.contains("region")));

        let entries = rejoin_snapshot(&snapshot.entries, source.sidecar());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, MentionKind::Datasource);
        assert_eq!(entries[0].tag.as_deref(), Some("csv"));
        assert_eq!(entries[0].atom_text.as_deref(), Some("@sales"));

        let hints = source.datasource_hints().expect("datasource hints");
        assert_eq!(hints.len(), 2);
        let sales_hint = hints.get("datasource://sales").expect("sales hint");
        assert!(sales_hint.starts_with("DATASOURCE sales"));
        assert!(sales_hint.contains("uri: datasource://sales"));
        assert!(sales_hint.contains("kind: csv"));
        assert!(sales_hint.contains("group: quarterly"));
        assert!(sales_hint.contains("row_count: 10"));

        // Hints are replaced (not merged) on every build.
        source.set_snapshot(vec![entry("ops", None)]);
        let _ = build_neutral(&mut source);
        let hints = source.datasource_hints().expect("datasource hints");
        assert_eq!(hints.len(), 1);
        assert!(hints.contains_key("datasource://ops"));
    }
}
