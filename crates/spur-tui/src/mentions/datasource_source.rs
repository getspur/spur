//! Notebook datasource `@`-mention source. Emits one entry per datasource
//! snapshot pushed by the notebook daemon bridge.

use std::path::Path;
use std::sync::Arc;

use spur_acp::{DatasourceEntry, DatasourceKind};

use super::entry::{MentionEntry, MentionKind, MentionSource};

pub struct DatasourceMentionSource {
    snapshot: Vec<DatasourceEntry>,
    prompt_hints: Vec<(String, Arc<String>)>,
}

impl DatasourceMentionSource {
    pub fn new(snapshot: Vec<DatasourceEntry>) -> Self {
        Self {
            snapshot,
            prompt_hints: Vec::new(),
        }
    }
}

impl MentionSource for DatasourceMentionSource {
    fn name(&self) -> &'static str {
        "datasource"
    }

    fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        self.prompt_hints.clear();
        Ok(self
            .snapshot
            .iter()
            .map(|entry| {
                let uri = datasource_uri(&entry.name);
                self.prompt_hints
                    .push((uri.clone(), Arc::new(datasource_prompt_hint(entry, &uri))));
                MentionEntry {
                    section_header: None,
                    kind: MentionKind::Datasource,
                    uri,
                    display: entry.name.clone(),
                    secondary: Some(datasource_secondary(entry)),
                    code_path: None,
                    code_scope: None,
                    tag: Some(datasource_kind_label(entry.kind).to_string()),
                    search_text: Some(datasource_search_text(entry)),
                    atom_text: Some(format!("@{}", entry.name)),
                    issue_preview: None,
                }
            })
            .collect())
    }

    fn datasource_hints(&self) -> &[(String, Arc<String>)] {
        &self.prompt_hints
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
    text
}

fn datasource_kind_label(kind: DatasourceKind) -> &'static str {
    match kind {
        DatasourceKind::Csv => "csv",
        DatasourceKind::Parquet => "parquet",
        DatasourceKind::Json => "json",
    }
}
