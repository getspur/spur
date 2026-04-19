//! Universal command palette — Ctrl+K.
//!
//! A modal overlay that fuzzy-searches across sessions, workers-in-lineage,
//! commands, and the current-session trace. Dispatches an `Action` on Enter.

use crate::components::palette_sources::PaletteSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteKind {
    Command,
    Session,
    Worker,
    Trace,
}

#[derive(Debug, Clone)]
pub enum PalettePayload {
    Command { name: String },
    Session { session_id: String },
    Worker { session_id: spur_acp::SessionId },
    Trace { entry_idx: usize },
}

#[derive(Debug, Clone)]
pub struct PaletteResult {
    pub kind: PaletteKind,
    pub label: String,
    pub subtitle: String,
    pub payload: PalettePayload,
}

pub struct PaletteState {
    query: String,
    raw: Vec<PaletteResult>,
    ranked: Vec<PaletteResult>,
    cursor: usize,
}

impl PaletteState {
    pub fn new() -> Self {
        Self { query: String::new(), raw: Vec::new(), ranked: Vec::new(), cursor: 0 }
    }

    pub fn query(&self) -> &str { &self.query }
    pub fn ranked(&self) -> &[PaletteResult] { &self.ranked }
    pub fn cursor(&self) -> usize { self.cursor }

    /// Populate from a source batch. Call once per source at open time.
    pub fn push_raw(&mut self, mut results: Vec<PaletteResult>) {
        self.raw.append(&mut results);
        self.rerank();
    }

    /// Pull results from every registered source. Convenience for tests and
    /// for the App-level open path.
    pub fn load_from_sources(&mut self, sources: &[Box<dyn PaletteSource>]) {
        self.raw.clear();
        for src in sources {
            self.raw.extend(src.collect());
        }
        self.rerank();
    }

    fn rerank(&mut self) {
        // Empty query: preserve input order (same semantics as commands::fuzzy::rank).
        if self.query.is_empty() {
            self.ranked = self.raw.clone();
        } else {
            self.ranked = rank_results(&self.raw, &self.query);
        }
        self.cursor = self.cursor.min(self.ranked.len().saturating_sub(1));
    }
}

impl Default for PaletteState {
    fn default() -> Self { Self::new() }
}

/// Nucleo-fuzzy rank across all sources by matching `query` against `label`.
/// Unmatched results are dropped. Ties broken by insertion order.
fn rank_results(entries: &[PaletteResult], query: &str) -> Vec<PaletteResult> {
    use nucleo_matcher::{pattern::{CaseMatching, Normalization, Pattern}, Matcher};

    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, PaletteResult)> = entries
        .iter()
        .filter_map(|e| {
            let score = pattern.score(
                nucleo_matcher::Utf32Str::new(&e.label, &mut Vec::new()),
                &mut matcher,
            )?;
            Some((score, e.clone()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, e)| e).collect()
}
