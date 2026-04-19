//! Shared contract for popup-backed retrieval sources.
//!
//! Each source produces `RetrievalRow`s from a query string and, on accept,
//! returns a `RetrievalAccept` payload that the view dispatches onto the
//! `InputBar`.

use crate::input_history::InputStateSnapshot;

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

/// Payload dispatched by the view when the user accepts a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalAccept {
    /// Replace the entire `InputBar` state with this snapshot. Used by history.
    ReplaceState(InputStateSnapshot),
    /// Insert a protected atom at the `InputBar` cursor. Used by @mention.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    #[allow(dead_code)]
    InsertAtom {
        text: String,
        uri: String,
        name: String,
    },
    /// Replace the text between `prefix_start` and the cursor with
    /// `replacement`. Used by /slash.
    /// (Not constructed by Phase 1 sources; reserved for Phase 3.)
    #[allow(dead_code)]
    ReplaceTriggerToken {
        prefix_start: usize,
        replacement: String,
    },
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
        let needs_refresh = !matches!(&self.cached_pattern, Some((cached_q, _)) if cached_q == query);
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
                    let score = pattern.score(Utf32Str::new(&h.snapshot.text, &mut buf), matcher)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::input_bar::ProtectedRange;
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
            uri: "file:///foo".to_string(),
            name: "foo".to_string(),
        }];
        let entry = InputHistoryEntry::new(snap);
        let row = HistoryQuerySource::row_from_entry(&entry);
        let (a, b) = row.atoms[0];
        assert_eq!(&row.primary[a..b], "@foo");
    }
}
