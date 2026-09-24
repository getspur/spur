//! Shared issue-search haystack construction used by the issue panel and
//! `@`-mention issue source.
//!
//! sud-m3 decision (decoupling spec §8 Q2 — "`issue_search.rs` final
//! home"): **stays in spur-tui.** Both consumers (the issues panel and
//! `IssueMentionSource`) are TUI-side, and the haystack shape is coupled to
//! `IssueSummary`/descriptor field policy (`spur-pm`), which the neutral
//! crate must not absorb (spec §1.2: the `spur-pm` users stay in the TUI
//! façade/adapters). Revisit only if a second frontend needs byte-identical
//! issue ranking.

use spur_pm::IssueSummary;

pub fn push_issue_search_text(
    search: &mut String,
    id: &str,
    title: &str,
    labels: &[String],
    assignee: Option<&str>,
    issue_type: Option<&str>,
    status: &str,
) {
    search.push_str(id);
    search.push(' ');
    search.push_str(title);
    for label in labels {
        search.push(' ');
        search.push_str(label);
    }
    if let Some(assignee) = assignee {
        search.push(' ');
        search.push_str(assignee);
    }
    if let Some(issue_type) = issue_type {
        search.push(' ');
        search.push_str(issue_type);
    }
    search.push(' ');
    search.push_str(status);
}

pub fn push_issue_summary_search_text(search: &mut String, issue: &IssueSummary) {
    push_issue_search_text(
        search,
        &issue.id,
        &issue.title,
        &issue.labels,
        issue.assignee.as_deref(),
        issue.issue_type.as_deref(),
        &issue.status,
    );
}
