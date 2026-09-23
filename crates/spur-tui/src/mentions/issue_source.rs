//! Issue/work-item `@`-mention source. Emits one entry per tracked issue
//! snapshot supplied by the dashboard/app.
//!
//! sud-m3: implements the *neutral* [`spur_mentions::MentionSource`]
//! directly (the engine builds it without a TUI adapter). TUI-only row
//! state (atom text, priority tag, the issue preview descriptor) rides the
//! sidecar keyed by `MentionId`, and every snapshot replace bumps the
//! data-revision token so the engine cache key invalidates immediately.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use spur_mentions::{
    MentionSource as NeutralMentionSource, SourceBuildError, SourceContext, SourceSnapshot,
};
use spur_pm::{IssueSummary, PmSource};

use super::entry::{MentionEntry, MentionKind};
use super::issue_search::push_issue_search_text;
use super::session::SessionMentionSource;
use super::sidecar::{split_rows, TuiMentionSidecar};

const DISPLAY_CHAR_LIMIT: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueMentionDescriptor {
    pub id: String,
    pub title: String,
    pub source: PmSource,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<i32>,
    pub issue_type: Option<String>,
    pub labels: Vec<String>,
    pub url: String,
    pub description: Option<String>,
}

impl From<&IssueSummary> for IssueMentionDescriptor {
    fn from(issue: &IssueSummary) -> Self {
        Self {
            id: issue.id.clone(),
            title: issue.title.clone(),
            source: issue.source.clone(),
            status: issue.status.clone(),
            assignee: issue.assignee.clone(),
            priority: issue.priority,
            issue_type: issue.issue_type.clone(),
            labels: issue.labels.clone(),
            url: issue.url.clone(),
            description: issue.description.clone(),
        }
    }
}

pub struct IssueMentionSource {
    snapshot: Vec<Arc<IssueMentionDescriptor>>,
    /// Data-revision token participating in the engine cache key: bumped by
    /// every snapshot replace so the next query rebuilds instead of serving
    /// the retained snapshot until the TTL expires.
    token: u64,
    /// Sidecar records from the most recent build, keyed by row id.
    sidecar: TuiMentionSidecar,
}

impl IssueMentionSource {
    /// Callers must provide snapshot rows in most-recent-first order.
    /// Empty-query `@` preserves this order inside the registry's ISSUE_CAP.
    pub fn new(snapshot: Vec<IssueMentionDescriptor>) -> Self {
        Self::with_token(snapshot, 0)
    }

    /// Construct with an explicit data-revision token; the registry stamps
    /// `previous + 1` when swapping a session source so cache identity
    /// changes on every replace.
    pub(crate) fn with_token(snapshot: Vec<IssueMentionDescriptor>, token: u64) -> Self {
        Self {
            snapshot: snapshot.into_iter().map(Arc::new).collect(),
            token,
            sidecar: TuiMentionSidecar::new(),
        }
    }

    /// Replace the issue snapshot in place (newest-first order), bumping the
    /// data-revision token.
    pub fn set_snapshot(&mut self, snapshot: Vec<IssueMentionDescriptor>) {
        self.snapshot = snapshot.into_iter().map(Arc::new).collect();
        self.token = self.token.wrapping_add(1);
    }

    /// TUI rows for the current snapshot: the pre-sidecar shape the neutral
    /// entries and the sidecar records split from.
    fn tui_rows(&self) -> Vec<MentionEntry> {
        self.snapshot
            .iter()
            .map(|descriptor| {
                let preview = Arc::clone(descriptor);
                let title = sanitize_single_line(&descriptor.title);
                let mut search_text = String::new();
                push_issue_search_text(
                    &mut search_text,
                    &descriptor.id,
                    &descriptor.title,
                    &descriptor.labels,
                    descriptor.assignee.as_deref(),
                    descriptor.issue_type.as_deref(),
                    &descriptor.status,
                );
                MentionEntry {
                    section_header: None,
                    kind: MentionKind::Issue,
                    uri: format!(
                        "issue://{}/{}",
                        source_slug(&descriptor.source),
                        descriptor.id
                    ),
                    display: truncate_chars(
                        &format!("{} {}", descriptor.id, title),
                        DISPLAY_CHAR_LIMIT,
                    ),
                    secondary: issue_secondary(&descriptor.status, descriptor.assignee.as_deref()),
                    agent: None,
                    model: None,
                    effort: None,
                    worker_kind: None,
                    worker_cli_identity: None,
                    code_path: None,
                    code_scope: None,
                    tag: descriptor.priority.map(|priority| format!("P{}", priority)),
                    search_text: Some(search_text),
                    atom_text: Some(format!("@{}", descriptor.id)),
                    unconsumed_suffix: None,
                    issue_preview: Some(preview),
                }
            })
            .collect()
    }
}

impl NeutralMentionSource for IssueMentionSource {
    fn key(&self) -> &str {
        "issue"
    }

    fn source_token(&self) -> u64 {
        self.token
    }

    fn build(
        &mut self,
        _root: &Path,
        _context: &SourceContext,
    ) -> Result<SourceSnapshot, SourceBuildError> {
        let rows = self.tui_rows();
        let (entries, sidecar) = split_rows(&rows);
        self.sidecar = sidecar;
        Ok(SourceSnapshot::new(entries, 0))
    }
}

impl SessionMentionSource for IssueMentionSource {
    fn slot_name(&self) -> &'static str {
        "issue"
    }

    fn sidecar(&self) -> &TuiMentionSidecar {
        &self.sidecar
    }

    fn datasource_hints(&self) -> Option<&HashMap<String, Arc<String>>> {
        None
    }
}

fn source_slug(source: &PmSource) -> &'static str {
    match source {
        PmSource::Beads => "beads",
        PmSource::GitHub => "github",
        PmSource::Linear => "linear",
        PmSource::Plane => "plane",
    }
}

fn issue_secondary(status: &str, assignee: Option<&str>) -> Option<String> {
    if status.is_empty() {
        return None;
    }
    match assignee {
        Some(assignee) => Some(format!("{} · {}", status, assignee)),
        None => Some(status.to_string()),
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        value.chars().take(max_chars).collect()
    }
}

pub(crate) fn sanitize_single_line(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_line_break = false;
    for ch in value.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_line_break {
                out.push(' ');
                in_line_break = true;
            }
        } else {
            out.push(ch);
            in_line_break = false;
        }
    }
    out
}

pub(crate) fn sanitize_multi_line(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut out_lines = Vec::new();
    let mut current = String::new();
    for ch in normalized.chars() {
        match ch {
            '\n' => {
                out_lines.push(current.trim_end().to_string());
                current.clear();
            }
            '\t' => {}
            _ if ch.is_control() => {}
            _ => current.push(ch),
        }
    }
    out_lines.push(current.trim_end().to_string());
    out_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::mentions::sidecar::rejoin_snapshot;
    use crate::mentions::MentionKind;
    use spur_pm::PmSource;

    fn descriptor(source: PmSource, id: &str) -> IssueMentionDescriptor {
        IssueMentionDescriptor {
            id: id.to_string(),
            title: "Fix mention picker matching".to_string(),
            source,
            status: "in_progress".to_string(),
            assignee: Some("alice".to_string()),
            priority: Some(1),
            issue_type: Some("bug".to_string()),
            labels: vec!["mentions".to_string(), "tui".to_string()],
            url: format!("https://example.test/{id}"),
            description: None,
        }
    }

    fn build_neutral(source: &mut IssueMentionSource) -> SourceSnapshot {
        source
            .build(Path::new("."), &SourceContext::default())
            .expect("build succeeds")
    }

    #[test]
    fn token_changes_when_the_snapshot_is_replaced() {
        let mut src = IssueMentionSource::new(vec![descriptor(PmSource::Beads, "bd-1")]);
        let first = NeutralMentionSource::source_token(&src);
        src.set_snapshot(vec![descriptor(PmSource::Beads, "bd-2")]);
        assert_ne!(first, NeutralMentionSource::source_token(&src));
    }

    #[test]
    fn build_emits_neutral_entries_that_rejoin_into_tui_rows() {
        let mut source = IssueMentionSource::new(vec![
            descriptor(PmSource::Beads, "bd-1"),
            descriptor(PmSource::GitHub, "GH-2"),
            descriptor(PmSource::Linear, "LIN-3"),
            descriptor(PmSource::Plane, "PLN-4"),
        ]);
        let expected = source.tui_rows();

        let snapshot = build_neutral(&mut source);

        assert_eq!(snapshot.entries.len(), 4);
        assert_eq!(snapshot.entries[0].uri, "issue://beads/bd-1");
        assert_eq!(snapshot.entries[1].uri, "issue://github/GH-2");
        assert_eq!(snapshot.entries[2].uri, "issue://linear/LIN-3");
        assert_eq!(snapshot.entries[3].uri, "issue://plane/PLN-4");
        let search_text = snapshot.entries[0]
            .search_text
            .as_deref()
            .expect("search text");
        for expected_fragment in [
            "bd-1",
            "Fix mention picker matching",
            "mentions",
            "tui",
            "alice",
            "bug",
            "in_progress",
        ] {
            assert!(
                search_text.contains(expected_fragment),
                "search text {search_text:?} missing {expected_fragment:?}",
            );
        }

        let entries = rejoin_snapshot(&snapshot.entries, source.sidecar());
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].kind, MentionKind::Issue);
        assert_eq!(entries[0].atom_text.as_deref(), Some("@bd-1"));
        assert_eq!(entries[0].secondary.as_deref(), Some("in_progress · alice"));
        assert_eq!(entries[0].tag.as_deref(), Some("P1"));
        assert!(entries[0]
            .display
            .starts_with("bd-1 Fix mention picker matching"));
        assert_eq!(entries, expected);
    }

    #[test]
    fn rejoin_carries_the_issue_preview_descriptor_handle() {
        let mut source = IssueMentionSource::new(vec![descriptor(PmSource::Beads, "bd-1")]);
        let first_source_preview = source.snapshot[0].clone();

        let snapshot = build_neutral(&mut source);
        let entries = rejoin_snapshot(&snapshot.entries, source.sidecar());

        let preview = entries[0].issue_preview.as_ref().expect("issue preview");
        assert!(std::sync::Arc::ptr_eq(&first_source_preview, preview));
        assert_eq!(preview.id, "bd-1");
        assert_eq!(preview.title, "Fix mention picker matching");
        assert_eq!(preview.labels, vec!["mentions", "tui"]);
    }

    #[test]
    fn display_truncation_preserves_char_boundaries() {
        let mut issue = descriptor(PmSource::Beads, "bd-unicode");
        issue.title = "á".repeat(100);
        let mut source = IssueMentionSource::new(vec![issue]);

        let snapshot = build_neutral(&mut source);
        let entries = rejoin_snapshot(&snapshot.entries, source.sidecar());

        assert!(entries[0].display.chars().count() <= 80);
        assert!(entries[0]
            .display
            .is_char_boundary(entries[0].display.len()));
    }
}
