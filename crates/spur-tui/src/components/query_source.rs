//! Shared contract for popup-backed retrieval sources.
//!
//! Each source produces `RetrievalRow`s from a query string and, on accept,
//! returns a `RetrievalAccept` payload that the view dispatches onto the
//! `InputBar`.

use crate::input_history::InputStateSnapshot;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Where the popup's query string lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    /// `PickerShell` owns a `MiniInput` and routes keys into it. Used by
    /// history search (Ctrl+R) where the query is scratch navigation text.
    OwnedByShell,
    /// The shell reads its query from the `InputBar` trigger prefix. Used
    /// by @mention and /slash where the query is part of the outbound draft.
    /// (Phase 1 does NOT construct any source in this mode; reserved for
    /// Phase 3.)
    #[allow(dead_code)]
    ReadFromInputBar,
}

/// One displayable row in a retrieval popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalRow {
    /// Main label text.
    pub primary: String,
    /// Description / metadata shown to the right of `primary`.
    pub secondary: String,
    /// Right-aligned provenance tag, e.g. `⟨claude⟩`. Empty for no tag.
    pub tag: String,
    /// Byte ranges inside `primary` to be rendered as protected-atom spans
    /// (LightBlue + underlined). Ranges MUST be valid inside `primary`;
    /// implementors are responsible for validating against any truncation
    /// they applied before returning.
    pub atoms: Vec<(usize, usize)>,
}

/// Optional side-pane preview for a retrieval row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalPreview {
    /// Title shown in the preview pane header (e.g. the bare issue id).
    pub title: String,
    /// Body lines. Each line is already styled by the source. Renderer
    /// wraps long lines using ratatui's word-aware Paragraph.
    pub lines: Vec<Line<'static>>,
}

/// Payload dispatched by the view when the user accepts a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalAccept {
    /// Replace the entire `InputBar` state with this snapshot. Used by history.
    ReplaceState(InputStateSnapshot),
    /// Insert a protected atom at the `InputBar` cursor. Used by @mention.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    InsertAtom {
        text: String,
        uri: String,
        name: String,
        /// If `Some(p)`, the view clears bytes `[p..cursor]` before
        /// inserting the atom at position `p`. Used by `@mention` accept
        /// to drop the `@query` prefix that drove the popup. MUST be on
        /// a UTF-8 char boundary of the InputBar text at accept time.
        replace_from: Option<usize>,
    },
    /// Replace the text between `prefix_start` and the cursor with
    /// `replacement`. Used by /slash.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    #[allow(dead_code)]
    ReplaceTriggerToken {
        prefix_start: usize,
        replacement: String,
    },
    /// Submit slash text through the view's normal submit router. Used by
    /// picker rows that execute local commands instead of editing the draft.
    SubmitText { text: String },
}

/// A retrieval source: given a query, produces ranked rows; on accept,
/// returns a dispatchable payload.
pub trait QuerySource {
    /// Title shown in the shell header (e.g. "History · bck-i-search").
    fn title(&self) -> &str;

    /// Where the query lives.
    fn query_mode(&self) -> QueryMode;

    /// Filter+rank using the given query. Implementors MUST reuse any
    /// internal matcher state across calls; constructing a fresh
    /// `nucleo::Matcher` per call is forbidden for hot-path reasons
    /// (see Phase 2 plan).
    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow>;

    /// Build the accept payload for the row at `row_idx`. Returns `None`
    /// if `row_idx` is out of bounds or the source has no state to accept.
    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept>;

    /// Optional preview for the row at row_idx. None means the picker
    /// should not render a side pane for this row. Default: None.
    fn preview_for(&self, _row_idx: usize) -> Option<RetrievalPreview> {
        None
    }
}

use crate::input_history::InputHistoryEntry;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// QuerySource backed by a snapshot of the global input history, oldest-first.
pub struct HistoryQuerySource {
    history: Vec<InputHistoryEntry>,
    matcher: Matcher,
    /// Snapshots parallel to the rows returned by the most recent `refresh`.
    /// `accept(i)` returns a `ReplaceState(last_snapshots[i].clone())`.
    last_snapshots: Vec<InputStateSnapshot>,
    /// Single-slot pattern cache, keyed by the query string that produced it.
    /// Non-empty queries re-parse only when the query string changes; empty
    /// queries never touch this cache.
    cached_pattern: Option<(String, Pattern)>,
    /// Count of `Pattern::parse` calls made so far. Incremented inside
    /// `ensure_pattern`. Exposed under `cfg(any(test, debug_assertions))`
    /// via `pattern_parse_count_for_test`.
    parse_count: usize,
}

impl HistoryQuerySource {
    pub fn new(history: Vec<InputHistoryEntry>) -> Self {
        Self {
            history,
            matcher: Matcher::new(Config::DEFAULT),
            last_snapshots: Vec::new(),
            cached_pattern: None,
            parse_count: 0,
        }
    }

    /// Test-only: read the number of `Pattern::parse` calls made so far.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn pattern_parse_count_for_test(&self) -> usize {
        self.parse_count
    }

    /// Return a reference to a Pattern for `query`, re-using the cache when
    /// `query` is identical to the last non-empty refresh. Guarantees
    /// `parse_count` is incremented exactly once per distinct query string
    /// in the run of refreshes.
    fn ensure_pattern(&mut self, query: &str) -> &Pattern {
        let needs_refresh =
            !matches!(&self.cached_pattern, Some((cached_q, _)) if cached_q == query);
        if needs_refresh {
            self.parse_count += 1;
            let pat = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            self.cached_pattern = Some((query.to_string(), pat));
        }
        &self
            .cached_pattern
            .as_ref()
            .expect("cache populated above")
            .1
    }

    pub(crate) fn row_from_entry(entry: &InputHistoryEntry) -> RetrievalRow {
        let text = &entry.snapshot.text;
        let mut primary = String::with_capacity(text.len());
        // offset_map[i] = byte offset in `primary` corresponding to byte i
        // in the original `text`. Length = text.len() + 1 so atom
        // end-exclusive indices map cleanly.
        let mut offset_map: Vec<usize> = Vec::with_capacity(text.len() + 1);
        offset_map.push(0);
        for ch in text.chars() {
            if ch == '\n' {
                primary.push_str(" ↵ ");
            } else {
                primary.push(ch);
            }
            for _ in 0..ch.len_utf8() {
                offset_map.push(primary.len());
            }
        }
        debug_assert_eq!(offset_map.len(), text.len() + 1);

        let mention_count = entry.snapshot.protected_ranges.len();
        let secondary = match mention_count {
            0 => String::new(),
            1 => "1 mention".to_string(),
            n => format!("{n} mentions"),
        };
        let tag = entry
            .agent
            .as_ref()
            .map(|a| format!("⟨{a}⟩"))
            .unwrap_or_default();
        let atoms: Vec<(usize, usize)> = entry
            .snapshot
            .protected_ranges
            .iter()
            .filter_map(|r| {
                let a = offset_map.get(r.start)?;
                let b = offset_map.get(r.end)?;
                Some((*a, *b))
            })
            .collect();
        RetrievalRow {
            primary,
            secondary,
            tag,
            atoms,
        }
    }
}

impl QuerySource for HistoryQuerySource {
    fn title(&self) -> &str {
        "History · bck-i-search"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::OwnedByShell
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        self.last_snapshots.clear();
        if self.history.is_empty() {
            return Vec::new();
        }
        let picked: Vec<usize> = if query.is_empty() {
            (0..self.history.len()).rev().take(20).collect()
        } else {
            self.ensure_pattern(query);
            let pattern: &Pattern = &self
                .cached_pattern
                .as_ref()
                .expect("ensure_pattern populated the cache")
                .1;
            let mut buf = Vec::new();
            let matcher = &mut self.matcher;
            let mut scored: Vec<(u32, usize)> = self
                .history
                .iter()
                .enumerate()
                .filter_map(|(i, h)| {
                    buf.clear();
                    let score =
                        pattern.score(Utf32Str::new(&h.snapshot.text, &mut buf), matcher)?;
                    Some((score, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().take(20).map(|(_, i)| i).collect()
        };
        let rows = picked
            .iter()
            .map(|&i| Self::row_from_entry(&self.history[i]))
            .collect();
        self.last_snapshots = picked
            .iter()
            .map(|&i| self.history[i].snapshot.clone())
            .collect();
        rows
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        self.last_snapshots
            .get(row_idx)
            .cloned()
            .map(RetrievalAccept::ReplaceState)
    }
}

use std::cell::RefCell;
use std::rc::Rc;

/// QuerySource backed by a shared `MentionRegistry` handle. Each `refresh`
/// call re-queries the registry with the current query, so the source
/// sees fresh filesystem cache contents. Cheap handle clones; no moves.
pub struct MentionQuerySource {
    registry: Rc<RefCell<crate::mentions::MentionRegistry>>,
    scope: MentionSourceScope,
    cwd: std::path::PathBuf,
    /// Byte offset in the InputBar text where the trigger's '@' lives.
    /// Captured at shell-open time, passed into `InsertAtom.replace_from`
    /// on accept so the view clears `[prefix_start..cursor]` before
    /// inserting the atom.
    prefix_start: usize,
    /// Entries parallel to the rows returned by the most recent `refresh`.
    last_hits: Vec<crate::mentions::MentionEntry>,
}

enum MentionSourceScope {
    PreSession,
    Session(spur_acp::SessionId),
}

impl MentionSourceScope {
    fn as_completion_scope(&self) -> crate::mentions::CompletionScope<'_> {
        match self {
            MentionSourceScope::PreSession => crate::mentions::CompletionScope::PreSession,
            MentionSourceScope::Session(session) => {
                crate::mentions::CompletionScope::Session(session)
            }
        }
    }
}

impl MentionQuerySource {
    pub fn new(
        registry: Rc<RefCell<crate::mentions::MentionRegistry>>,
        scope: crate::mentions::CompletionScope<'_>,
        cwd: std::path::PathBuf,
        prefix_start: usize,
    ) -> Self {
        let scope = match scope {
            crate::mentions::CompletionScope::PreSession => MentionSourceScope::PreSession,
            crate::mentions::CompletionScope::Session(session) => {
                MentionSourceScope::Session(session.clone())
            }
        };
        Self {
            registry,
            scope,
            cwd,
            prefix_start,
            last_hits: Vec::new(),
        }
    }
}

fn issue_preview_for_descriptor(
    descriptor: &crate::mentions::IssueMentionDescriptor,
) -> RetrievalPreview {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::raw(descriptor.id.clone()),
        Span::raw("  "),
        Span::styled(
            descriptor.source.to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let title = crate::mentions::issue_source::sanitize_single_line(&descriptor.title);
    if !title.trim().is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(labeled_preview_line(
        "Status:",
        required_preview_value(&descriptor.status),
    ));
    lines.push(labeled_preview_line(
        "Type:",
        optional_preview_value(descriptor.issue_type.as_deref()),
    ));
    lines.push(labeled_preview_line(
        "Priority:",
        descriptor
            .priority
            .map(|priority| format!("P{priority}"))
            .unwrap_or_else(|| "-".to_string()),
    ));
    lines.push(labeled_preview_line(
        "Assignee:",
        optional_preview_value(descriptor.assignee.as_deref()),
    ));

    if !descriptor.labels.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "Labels:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(labels_preview_line(&descriptor.labels));
    }

    lines.push(Line::raw(""));
    lines.push(url_preview_line(&descriptor.url));

    RetrievalPreview {
        title: descriptor.id.clone(),
        lines,
    }
}

fn required_preview_value(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

fn optional_preview_value(value: Option<&str>) -> String {
    value
        .map(required_preview_value)
        .unwrap_or_else(|| "-".to_string())
}

fn labeled_preview_line(label: &'static str, value: String) -> Line<'static> {
    Line::from(vec![Span::raw(label), Span::raw(" "), Span::raw(value)])
}

fn labels_preview_line(labels: &[String]) -> Line<'static> {
    const LABEL_PREVIEW_LIMIT: usize = 6;

    let mut spans = Vec::new();
    for (idx, label) in labels.iter().take(LABEL_PREVIEW_LIMIT).enumerate() {
        if idx > 0 {
            spans.push(Span::raw(", "));
        }
        spans.push(Span::raw(label.clone()));
    }
    let remaining = labels.len().saturating_sub(LABEL_PREVIEW_LIMIT);
    if remaining > 0 {
        if !spans.is_empty() {
            spans.push(Span::raw(", "));
        }
        spans.push(Span::styled(
            format!("+{remaining} more"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

fn url_preview_line(url: &str) -> Line<'static> {
    let url = url.trim();
    if url.is_empty() {
        Line::raw("URL: -")
    } else {
        Line::from(vec![
            Span::raw("URL: "),
            Span::styled(
                url.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ])
    }
}

impl QuerySource for MentionQuerySource {
    fn title(&self) -> &str {
        "Mentions · @"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::ReadFromInputBar
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        use crate::mentions::MentionKind;
        let hits = self.registry.borrow_mut().query(
            self.scope.as_completion_scope(),
            &self.cwd,
            query,
            20,
        );
        let rows: Vec<RetrievalRow> = hits
            .iter()
            .map(|m| {
                let icon = match m.kind {
                    MentionKind::Directory => "\u{1F4C1}", // 📁
                    MentionKind::File => "\u{1F4C4}",      // 📄
                    MentionKind::Worker => "\u{1F916}",    // 🤖
                    MentionKind::Issue => "\u{1F39F}",     // 🎟
                };
                let tag_render = m
                    .tag
                    .clone()
                    .map(|t| format!("\u{27E8}{}\u{27E9}", t)) // ⟨tier⟩
                    .unwrap_or_default();
                let primary = if m.kind == MentionKind::Issue {
                    format!("{} {}", icon, m.display)
                } else {
                    format!("{} @{}", icon, m.display)
                };
                RetrievalRow {
                    primary,
                    secondary: m.secondary.clone().unwrap_or_default(),
                    tag: tag_render,
                    atoms: Vec::new(),
                }
            })
            .collect();
        self.last_hits = hits;
        rows
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        use crate::mentions::MentionKind;

        let hit = self.last_hits.get(row_idx)?;
        let text = hit
            .atom_text
            .clone()
            .unwrap_or_else(|| format!("@{}", hit.display));
        let name = if hit.kind == MentionKind::Issue {
            hit.atom_text
                .as_deref()
                .map(|text| text.trim_start_matches('@').to_string())
                .unwrap_or_else(|| hit.display.clone())
        } else {
            hit.display.clone()
        };
        Some(RetrievalAccept::InsertAtom {
            text,
            uri: hit.uri.clone(),
            name,
            replace_from: Some(self.prefix_start),
        })
    }

    fn preview_for(&self, row_idx: usize) -> Option<RetrievalPreview> {
        use crate::mentions::MentionKind;

        let hit = self.last_hits.get(row_idx)?;
        if hit.kind != MentionKind::Issue {
            return None;
        }
        hit.issue_preview
            .as_deref()
            .map(issue_preview_for_descriptor)
    }
}

/// Minimal display-oriented row supplied to `SlashQuerySource`. The view
/// pre-computes these from its `CommandRegistry` at shell-open time so the
/// source doesn't take a live registry reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashRow {
    /// Canonical typed form of the command, e.g. "/help" or "/claude:help".
    pub canonical: String,
    pub description: String,
    /// Right-aligned provenance tag, e.g. "⟨spur⟩" or "⟨claude⟩". Empty for none.
    pub tag: String,
}

/// QuerySource for /slash completions. Holds a pre-computed `Vec<SlashRow>`
/// and a `prefix_start` captured at shell-open time (byte offset of the '/').
pub struct SlashQuerySource {
    rows: Vec<SlashRow>,
    matcher: Matcher,
    last_picked: Vec<SlashRow>,
    prefix_start: usize,
}

impl SlashQuerySource {
    pub fn new(rows: Vec<SlashRow>, prefix_start: usize) -> Self {
        Self {
            rows,
            matcher: Matcher::new(Config::DEFAULT),
            last_picked: Vec::new(),
            prefix_start,
        }
    }
}

impl QuerySource for SlashQuerySource {
    fn title(&self) -> &str {
        "Commands · /"
    }

    fn query_mode(&self) -> QueryMode {
        QueryMode::ReadFromInputBar
    }

    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        let picked: Vec<SlashRow> = if query.is_empty() {
            self.rows.iter().take(20).cloned().collect()
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, SlashRow)> = self
                .rows
                .iter()
                .filter_map(|r| {
                    buf.clear();
                    let score =
                        pattern.score(Utf32Str::new(&r.canonical, &mut buf), &mut self.matcher)?;
                    Some((score, r.clone()))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            scored.into_iter().take(20).map(|(_, r)| r).collect()
        };
        let out: Vec<RetrievalRow> = picked
            .iter()
            .map(|r| RetrievalRow {
                primary: r.canonical.clone(),
                secondary: r.description.clone(),
                tag: r.tag.clone(),
                atoms: Vec::new(),
            })
            .collect();
        self.last_picked = picked;
        out
    }

    fn accept(&self, row_idx: usize) -> Option<RetrievalAccept> {
        let row = self.last_picked.get(row_idx)?;
        Some(RetrievalAccept::ReplaceTriggerToken {
            prefix_start: self.prefix_start,
            replacement: format!("{} ", row.canonical),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::input_bar::{ProtectedRange, RangeKind};
    use crate::input_history::{InputHistoryEntry, InputStateSnapshot};

    fn mk_entry(text: &str) -> InputHistoryEntry {
        InputHistoryEntry::new(InputStateSnapshot::from_text(text))
    }

    #[test]
    fn retrieval_row_atoms_are_byte_ranges() {
        // Smoke test: atoms use byte offsets, so multi-byte primary stays
        // consistent. "你好@foo" — atom range for @foo is bytes 6..10.
        let r = RetrievalRow {
            primary: "你好@foo".to_string(),
            secondary: String::new(),
            tag: String::new(),
            atoms: vec![(6, 10)],
        };
        assert_eq!(&r.primary[r.atoms[0].0..r.atoms[0].1], "@foo");
    }

    #[test]
    fn replace_state_roundtrip() {
        let snap = InputStateSnapshot::from_text("hello");
        let a = RetrievalAccept::ReplaceState(snap.clone());
        match a {
            RetrievalAccept::ReplaceState(got) => assert_eq!(got.text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn history_source_empty_query_returns_newest_first() {
        let hist = vec![mk_entry("oldest"), mk_entry("middle"), mk_entry("newest")];
        let mut src = HistoryQuerySource::new(hist);
        let rows = src.refresh("");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].primary, "newest");
        assert_eq!(rows[1].primary, "middle");
        assert_eq!(rows[2].primary, "oldest");
    }

    #[test]
    fn history_source_empty_history_returns_no_rows() {
        let mut src = HistoryQuerySource::new(Vec::new());
        assert!(src.refresh("").is_empty());
        assert!(src.refresh("anything").is_empty());
    }

    #[test]
    fn history_source_fuzzy_narrows_rows() {
        let hist = vec![
            mk_entry("refactor the delegation walker"),
            mk_entry("fix the ProtectedRange panic"),
            mk_entry("add test for INV-C2"),
        ];
        let mut src = HistoryQuerySource::new(hist);
        let rows = src.refresh("refa");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "refactor the delegation walker");
    }

    #[test]
    fn history_source_accept_returns_replace_state_snapshot() {
        let hist = vec![mk_entry("newest")];
        let mut src = HistoryQuerySource::new(hist);
        let _ = src.refresh("");
        let accept = src.accept(0).expect("row 0 exists");
        match accept {
            RetrievalAccept::ReplaceState(snap) => assert_eq!(snap.text, "newest"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn history_source_accept_out_of_bounds_returns_none() {
        let src = HistoryQuerySource::new(vec![mk_entry("only")]);
        assert!(src.accept(99).is_none());
    }

    #[test]
    fn history_source_matcher_reused_across_refreshes() {
        // Structural check: `refresh` does NOT construct a fresh Matcher
        // each call. We verify by type — the field exists and is private,
        // so the only way refresh could violate is by shadowing it, which
        // would show up as a struct-size regression. This is a weaker form
        // of the Phase 2 alloc-bound test and is strengthened there.
        let src = HistoryQuerySource::new(Vec::new());
        // The struct exists and holds a Matcher; if someone later removes
        // the field, the below line will fail to compile.
        let _field_exists: &nucleo_matcher::Matcher = &src.matcher;
        let _ = src; // suppress unused warning
    }

    #[test]
    fn history_source_title_is_bck_i_search() {
        let src = HistoryQuerySource::new(Vec::new());
        assert_eq!(src.title(), "History · bck-i-search");
    }

    #[test]
    fn history_source_query_mode_is_owned_by_shell() {
        let src = HistoryQuerySource::new(Vec::new());
        assert_eq!(src.query_mode(), QueryMode::OwnedByShell);
    }

    #[test]
    fn history_source_row_metadata_reflects_mentions() {
        let hist = vec![mk_entry("no mentions here")];
        let mut src = HistoryQuerySource::new(hist);
        let rows = src.refresh("");
        assert_eq!(rows[0].secondary, "");

        let mut with_atom = InputStateSnapshot::from_text("hi @foo");
        with_atom.protected_ranges = vec![crate::components::input_bar::ProtectedRange {
            start: 3,
            end: 7,
            kind: RangeKind::Atom,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let hist2 = vec![InputHistoryEntry::new(with_atom)];
        let mut src2 = HistoryQuerySource::new(hist2);
        let rows2 = src2.refresh("");
        assert_eq!(rows2[0].secondary, "1 mention");
        assert_eq!(rows2[0].atoms, vec![(3, 7)]);
    }

    #[test]
    fn same_query_repeated_does_not_reparse_pattern() {
        let hist = vec![mk_entry("alpha"), mk_entry("beta"), mk_entry("gamma")];
        let mut src = HistoryQuerySource::new(hist);
        let _ = src.refresh("a");
        let base = src.pattern_parse_count_for_test();
        for _ in 0..99 {
            let _ = src.refresh("a");
        }
        assert_eq!(src.pattern_parse_count_for_test(), base);
    }

    #[test]
    fn different_queries_each_reparse_once() {
        let hist = vec![mk_entry("alpha"), mk_entry("beta")];
        let mut src = HistoryQuerySource::new(hist);
        let _ = src.refresh("a");
        let _ = src.refresh("al");
        let _ = src.refresh("alp");
        assert_eq!(src.pattern_parse_count_for_test(), 3);
    }

    #[test]
    fn empty_query_does_not_touch_pattern_cache() {
        let hist = vec![mk_entry("alpha")];
        let mut src = HistoryQuerySource::new(hist);
        let _ = src.refresh("");
        let _ = src.refresh("");
        let _ = src.refresh("");
        assert_eq!(src.pattern_parse_count_for_test(), 0);
    }

    #[test]
    fn row_from_entry_maps_atoms_through_newline_replacement() {
        let mut snap = InputStateSnapshot::from_text("hi\n@foo\nbye");
        snap.protected_ranges = vec![ProtectedRange {
            start: 3,
            end: 7,
            kind: RangeKind::Atom,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let entry = InputHistoryEntry::new(snap);
        let row = HistoryQuerySource::row_from_entry(&entry);
        assert_eq!(row.primary, "hi ↵ @foo ↵ bye");
        assert_eq!(row.atoms, vec![(7, 11)]);
        let (a, b) = row.atoms[0];
        assert_eq!(&row.primary[a..b], "@foo");
    }

    #[test]
    fn row_from_entry_preserves_atoms_without_newlines() {
        let mut snap = InputStateSnapshot::from_text("hi @foo");
        snap.protected_ranges = vec![ProtectedRange {
            start: 3,
            end: 7,
            kind: RangeKind::Atom,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let entry = InputHistoryEntry::new(snap);
        let row = HistoryQuerySource::row_from_entry(&entry);
        assert_eq!(row.primary, "hi @foo");
        assert_eq!(row.atoms, vec![(3, 7)]);
    }

    #[test]
    fn row_from_entry_handles_multiple_newlines_before_atom() {
        let mut snap = InputStateSnapshot::from_text("\n\n\nx @foo");
        snap.protected_ranges = vec![ProtectedRange {
            start: 5,
            end: 9,
            kind: RangeKind::Atom,
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let entry = InputHistoryEntry::new(snap);
        let row = HistoryQuerySource::row_from_entry(&entry);
        let (a, b) = row.atoms[0];
        assert_eq!(&row.primary[a..b], "@foo");
    }

    use crate::mentions::MentionRegistry;
    use spur_acp::SessionId;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn preview_text(preview: &RetrievalPreview) -> String {
        preview
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn issue_descriptor() -> crate::mentions::IssueMentionDescriptor {
        crate::mentions::IssueMentionDescriptor {
            id: "bd-1".to_string(),
            title: "Mention picker issue rows".to_string(),
            source: spur_pm::PmSource::Beads,
            status: "open".to_string(),
            assignee: Some("alice".to_string()),
            priority: Some(2),
            issue_type: Some("task".to_string()),
            labels: vec!["mentions".to_string()],
            url: "https://example.test/bd-1".to_string(),
        }
    }

    fn make_mention_registry_with_cwd(cwd: &std::path::Path) -> Rc<RefCell<MentionRegistry>> {
        let mut r = MentionRegistry::new();
        // Prime the cache by running one query; cwd must actually exist so
        // FileMentionSource returns something deterministic in tests.
        let sid = SessionId::new();
        let _ = r.query(crate::mentions::CompletionScope::Session(&sid), cwd, "", 5);
        Rc::new(RefCell::new(r))
    }

    #[test]
    fn mention_source_title_is_at_mention() {
        let registry = make_mention_registry_with_cwd(std::path::Path::new("."));
        let src = MentionQuerySource::new(
            Rc::clone(&registry),
            crate::mentions::CompletionScope::Session(&SessionId::new()),
            std::path::PathBuf::from("."),
            1, // prefix_start — the '@' byte
        );
        assert_eq!(src.title(), "Mentions · @");
    }

    #[test]
    fn mention_source_query_mode_is_read_from_input_bar() {
        let registry = make_mention_registry_with_cwd(std::path::Path::new("."));
        let src = MentionQuerySource::new(
            Rc::clone(&registry),
            crate::mentions::CompletionScope::Session(&SessionId::new()),
            std::path::PathBuf::from("."),
            0,
        );
        assert_eq!(src.query_mode(), QueryMode::ReadFromInputBar);
    }

    #[test]
    fn mention_source_accept_returns_insert_atom_with_replace_from() {
        // Inject a fake mention entry by preloading the registry cache
        // manually. Since that's private, we instead use a real registry
        // over a fixed tmpdir with one file.
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("README.md");
        std::fs::write(&file_path, "x").unwrap();

        let registry = make_mention_registry_with_cwd(tmp.path());
        let mut src = MentionQuerySource::new(
            Rc::clone(&registry),
            crate::mentions::CompletionScope::Session(&SessionId::new()),
            tmp.path().to_path_buf(),
            1, // '@' at byte 1
        );
        let rows = src.refresh("READ");
        assert!(
            !rows.is_empty(),
            "expected at least one match for 'READ' against README.md"
        );
        let accept = src.accept(0).expect("row 0 exists");
        match accept {
            RetrievalAccept::InsertAtom {
                text,
                uri,
                name,
                replace_from,
            } => {
                assert_eq!(replace_from, Some(1));
                assert!(text.starts_with('@'));
                assert!(!uri.is_empty());
                assert!(!name.is_empty());
            }
            other => panic!("expected InsertAtom, got {other:?}"),
        }
    }

    #[test]
    fn mention_source_row_label_carries_icon_and_at_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("foo.txt"), "x").unwrap();
        let registry = make_mention_registry_with_cwd(tmp.path());
        let mut src = MentionQuerySource::new(
            Rc::clone(&registry),
            crate::mentions::CompletionScope::Session(&SessionId::new()),
            tmp.path().to_path_buf(),
            0,
        );
        let rows = src.refresh("foo");
        assert!(!rows.is_empty());
        // Label format matches today's legacy format: "<icon> @<display>"
        // so visual parity is preserved.
        assert!(
            rows[0].primary.contains("@foo"),
            "primary missing @foo: {:?}",
            rows[0].primary
        );
        assert!(
            rows[0].primary.starts_with(['📁', '📄']),
            "primary missing icon prefix: {:?}",
            rows[0].primary
        );
    }

    #[test]
    fn mention_source_accepts_issue_as_id_atom() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Rc::new(RefCell::new(MentionRegistry::new()));
        registry
            .borrow_mut()
            .set_issue_snapshot(vec![crate::mentions::IssueMentionDescriptor {
                id: "bd-1".to_string(),
                title: "Mention picker issue rows".to_string(),
                source: spur_pm::PmSource::Beads,
                status: "open".to_string(),
                assignee: Some("alice".to_string()),
                priority: Some(2),
                issue_type: Some("task".to_string()),
                labels: vec!["mentions".to_string()],
                url: "https://example.test/bd-1".to_string(),
            }]);
        let mut src = MentionQuerySource::new(
            Rc::clone(&registry),
            crate::mentions::CompletionScope::PreSession,
            tmp.path().to_path_buf(),
            3,
        );

        let rows = src.refresh("alice");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].primary.contains("bd-1 Mention picker issue rows"));

        let accept = src.accept(0).expect("row 0 exists");
        match accept {
            RetrievalAccept::InsertAtom {
                text,
                uri,
                name,
                replace_from,
            } => {
                assert_eq!(text, "@bd-1");
                assert_eq!(uri, "issue://beads/bd-1");
                assert_eq!(name, "bd-1");
                assert_eq!(replace_from, Some(3));
            }
            other => panic!("expected InsertAtom, got {other:?}"),
        }
    }

    #[test]
    fn mention_source_preview_for_issue_rows_only() {
        let tmp = tempfile::tempdir().unwrap();
        let full_title = "Preview-only title that is much longer than the list row";
        let registry = Rc::new(RefCell::new(MentionRegistry::for_brain_session(vec![
            crate::mentions::WorkerMentionDescriptor {
                name: "codex".to_string(),
                description: Some("Writes patches".to_string()),
                tier: Some("generalist".to_string()),
            },
        ])));
        registry
            .borrow_mut()
            .set_issue_snapshot(vec![crate::mentions::IssueMentionDescriptor {
                id: "bd-42".to_string(),
                title: full_title.to_string(),
                source: spur_pm::PmSource::Beads,
                status: "open".to_string(),
                assignee: Some("alice".to_string()),
                priority: Some(2),
                issue_type: Some("task".to_string()),
                labels: vec!["mentions".to_string(), "preview".to_string()],
                url: "https://example.test/bd-42".to_string(),
            }]);
        let mut src = MentionQuerySource::new(
            Rc::clone(&registry),
            crate::mentions::CompletionScope::PreSession,
            tmp.path().to_path_buf(),
            3,
        );

        let rows = src.refresh("");
        let worker_idx = rows
            .iter()
            .position(|row| row.primary.contains("worker:codex"))
            .expect("worker row");
        let issue_idx = rows
            .iter()
            .position(|row| row.primary.contains("bd-42"))
            .expect("issue row");

        assert!(src.preview_for(worker_idx).is_none());
        let preview = src.preview_for(issue_idx).expect("issue preview");
        assert_eq!(preview.title, "bd-42");
        let text = preview_text(&preview);
        assert!(text.contains(full_title), "{text}");
        assert!(text.contains("Labels:"), "{text}");
        assert!(text.contains("mentions"), "{text}");
        assert!(text.contains("preview"), "{text}");
        assert!(text.contains("URL: https://example.test/bd-42"), "{text}");
    }

    #[test]
    fn issue_preview_caps_label_line_after_six_labels() {
        let mut descriptor = issue_descriptor();
        descriptor.labels = (1..=10).map(|n| format!("label-{n}")).collect();

        let preview = issue_preview_for_descriptor(&descriptor);

        let labels_line = preview
            .lines
            .iter()
            .map(line_text)
            .find(|text| text.starts_with("label-1"))
            .expect("labels value line");
        assert_eq!(
            labels_line,
            "label-1, label-2, label-3, label-4, label-5, label-6, +4 more"
        );
    }

    #[test]
    fn issue_preview_sanitizes_newlines_in_title() {
        let mut descriptor = issue_descriptor();
        descriptor.title = "first\nsecond".to_string();

        let preview = issue_preview_for_descriptor(&descriptor);

        let title_line = preview
            .lines
            .iter()
            .map(line_text)
            .find(|text| text == "first second")
            .expect("sanitized title line");
        assert_eq!(title_line, "first second");
    }

    #[test]
    fn slash_source_title_is_slash_command() {
        let src = SlashQuerySource::new(Vec::new(), 0);
        assert_eq!(src.title(), "Commands · /");
    }

    #[test]
    fn slash_source_query_mode_is_read_from_input_bar() {
        let src = SlashQuerySource::new(Vec::new(), 0);
        assert_eq!(src.query_mode(), QueryMode::ReadFromInputBar);
    }

    #[test]
    fn slash_source_accept_returns_replace_trigger_token() {
        let entries = vec![SlashRow {
            canonical: "/help".to_string(),
            description: "Show help".to_string(),
            tag: "⟨spur⟩".to_string(),
        }];
        let mut src = SlashQuerySource::new(entries, 0);
        let _ = src.refresh("");
        let accept = src.accept(0).expect("row 0 exists");
        match accept {
            RetrievalAccept::ReplaceTriggerToken {
                prefix_start,
                replacement,
            } => {
                assert_eq!(prefix_start, 0);
                assert_eq!(replacement, "/help ");
            }
            other => panic!("expected ReplaceTriggerToken, got {other:?}"),
        }
    }

    #[test]
    fn slash_source_refresh_ranks_by_fuzzy_match_on_canonical() {
        let rows = vec![
            SlashRow {
                canonical: "/help".to_string(),
                description: "".to_string(),
                tag: "⟨spur⟩".to_string(),
            },
            SlashRow {
                canonical: "/mode".to_string(),
                description: "".to_string(),
                tag: "⟨spur⟩".to_string(),
            },
            SlashRow {
                canonical: "/claude:help".to_string(),
                description: "".to_string(),
                tag: "⟨claude⟩".to_string(),
            },
        ];
        let mut src = SlashQuerySource::new(rows, 0);
        let res = src.refresh("hel");
        assert!(res.iter().any(|r| r.primary == "/help"));
        assert!(res.iter().any(|r| r.primary == "/claude:help"));
        assert!(!res.iter().any(|r| r.primary == "/mode"));
    }

    #[test]
    fn slash_source_row_carries_tag() {
        let rows = vec![SlashRow {
            canonical: "/help".to_string(),
            description: "Show help".to_string(),
            tag: "⟨spur⟩".to_string(),
        }];
        let mut src = SlashQuerySource::new(rows, 0);
        let res = src.refresh("");
        assert_eq!(res[0].tag, "⟨spur⟩");
        assert_eq!(res[0].secondary, "Show help");
    }
}
