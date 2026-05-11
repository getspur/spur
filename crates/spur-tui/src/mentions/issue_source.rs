//! Issue/work-item `@`-mention source. Emits one entry per tracked issue
//! snapshot supplied by the dashboard/app.

use std::path::Path;
use std::sync::Arc;

use spur_pm::{IssueSummary, PmSource};

use super::entry::{MentionEntry, MentionKind, MentionSource};
use super::issue_search::push_issue_search_text;

const DISPLAY_CHAR_LIMIT: usize = 80;

#[derive(Debug, Clone)]
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
}

impl IssueMentionSource {
    pub fn new(snapshot: Vec<IssueMentionDescriptor>) -> Self {
        Self {
            snapshot: snapshot.into_iter().map(Arc::new).collect(),
        }
    }
}

impl MentionSource for IssueMentionSource {
    fn name(&self) -> &'static str {
        "issue"
    }

    fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        Ok(self
            .snapshot
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
                    tag: descriptor.priority.map(|priority| format!("P{}", priority)),
                    search_text: Some(search_text),
                    atom_text: Some(format!("@{}", descriptor.id)),
                    issue_preview: Some(preview),
                }
            })
            .collect())
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::mentions::{MentionKind, MentionSource};
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

    #[test]
    fn descriptors_emit_entries_with_issue_metadata() {
        let descriptors = vec![
            descriptor(PmSource::Beads, "bd-1"),
            descriptor(PmSource::GitHub, "GH-2"),
            descriptor(PmSource::Linear, "LIN-3"),
            descriptor(PmSource::Plane, "PLN-4"),
        ];
        let mut source = IssueMentionSource::new(descriptors);

        let entries = source.build(Path::new(".")).expect("build succeeds");

        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].kind, MentionKind::Issue);
        assert_eq!(entries[0].uri, "issue://beads/bd-1");
        assert_eq!(entries[1].uri, "issue://github/GH-2");
        assert_eq!(entries[2].uri, "issue://linear/LIN-3");
        assert_eq!(entries[3].uri, "issue://plane/PLN-4");
        assert_eq!(entries[0].atom_text.as_deref(), Some("@bd-1"));
        assert_eq!(entries[0].secondary.as_deref(), Some("in_progress · alice"));
        assert_eq!(entries[0].tag.as_deref(), Some("P1"));
        assert!(entries[0]
            .display
            .starts_with("bd-1 Fix mention picker matching"));

        let search_text = entries[0].search_text.as_deref().expect("search text");
        for expected in [
            "bd-1",
            "Fix mention picker matching",
            "mentions",
            "tui",
            "alice",
            "bug",
            "in_progress",
        ] {
            assert!(
                search_text.contains(expected),
                "search text {search_text:?} missing {expected:?}",
            );
        }
    }

    #[test]
    fn issue_entries_carry_preview_descriptor_handle() {
        let mut source = IssueMentionSource::new(vec![
            descriptor(PmSource::Beads, "bd-1"),
            descriptor(PmSource::GitHub, "GH-2"),
        ]);
        let first_source_preview = source.snapshot[0].clone();

        let entries = source.build(Path::new(".")).expect("build succeeds");

        assert!(entries.iter().all(|entry| entry.issue_preview.is_some()));
        let preview = entries[0].issue_preview.as_ref().expect("issue preview");
        assert!(std::sync::Arc::ptr_eq(&first_source_preview, preview));
        assert_eq!(preview.id, "bd-1");
        assert_eq!(preview.title, "Fix mention picker matching");
        assert_eq!(preview.labels, vec!["mentions", "tui"]);
        let second_preview = entries[1].issue_preview.as_ref().expect("issue preview");
        assert_eq!(second_preview.id, "GH-2");
    }

    #[test]
    fn display_truncation_preserves_char_boundaries() {
        let mut issue = descriptor(PmSource::Beads, "bd-unicode");
        issue.title = "á".repeat(100);
        let mut source = IssueMentionSource::new(vec![issue]);

        let entries = source.build(Path::new(".")).expect("build succeeds");

        assert!(entries[0].display.chars().count() <= 80);
        assert!(entries[0]
            .display
            .is_char_boundary(entries[0].display.len()));
    }
}
