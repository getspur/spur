//! Universal command palette — Ctrl+K.
//!
//! A modal overlay that fuzzy-searches across sessions, workers-in-lineage,
//! commands, and the current-session trace. Dispatches an `Action` on Enter.

/// Subtitle scores in `rerank` are weighted by this multiplier before
/// being compared with label scores. < 1.0 biases toward label matches
/// while still allowing strong subtitle matches to dominate weak label
/// matches. Tune in one place.
const SUBTITLE_WEIGHT: f32 = 0.7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteKind {
    View,
    Command,
    Session,
    Worker,
    Trace,
}

#[derive(Debug, Clone)]
pub enum PalettePayload {
    View { action: crate::action::Action },
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

/// State owned by the Ctrl+K palette overlay.
///
/// # Performance Invariant
///
/// `rerank()` (called on every keystroke, every query mutation, and every
/// palette open) guarantees:
///   - **O(N) time** where N = `raw.len()`.
///   - **O(M · 4 bytes)** new memory per rerank, where M = matched entries.
///   - **Exactly 2 allocations** on the non-empty-query path: one `Pattern`
///     parse and one scratch `Vec<(u32, u32)>` for scoring. `self.scratch`
///     is reused between label and subtitle scoring (one `clear()` between)
///     so the second Utf32Str conversion does not allocate.
///   - **Zero clones** of `PaletteResult` fields (label/subtitle are not
///     copied during rerank; we rank by index into `raw`).
///
/// Enforced by `tests/palette_rerank_bench_smoke.rs`. If that test fails
/// after a refactor, you have reintroduced unbounded allocation — fix it
/// rather than loosening the threshold.
///
/// The invariant assumes N < 10,000. Phase F2 cross-session trace indexing
/// will push N higher; at that point add query debouncing or an
/// approximate top-K.
pub struct PaletteState {
    query: String,
    raw: Vec<PaletteResult>,
    /// Indices into `raw` in rank order. Replaces a previous owned
    /// `ranked: Vec<PaletteResult>` field to avoid per-rerank cloning.
    order: Vec<u32>,
    cursor: usize,
    /// Reused across keystrokes to avoid per-rerank heap allocation.
    matcher: nucleo_matcher::Matcher,
    /// Scratch buffer reused by `Utf32Str::new` inside rerank's inner loop.
    /// Grows on demand to fit the longest label seen; never shrunk.
    scratch: Vec<char>,
    /// Reused score/index buffer for non-empty query ranking.
    rank_scratch: Vec<(u32, u32)>,
}

impl PaletteState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            raw: Vec::new(),
            order: Vec::new(),
            cursor: 0,
            matcher: nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT),
            scratch: Vec::new(),
            rank_scratch: Vec::new(),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn ranked_len(&self) -> usize {
        self.order.len()
    }

    /// Borrow the `i`th ranked result, if any.
    pub fn nth_ranked(&self, i: usize) -> Option<&PaletteResult> {
        self.order
            .get(i)
            .and_then(|&idx| self.raw.get(idx as usize))
    }

    /// Iterate ranked results in rank order. Zero-copy.
    pub fn iter_ranked(&self) -> impl Iterator<Item = &PaletteResult> + '_ {
        self.order
            .iter()
            .filter_map(move |&i| self.raw.get(i as usize))
    }

    /// Populate from a source batch. Call once per source at open time.
    pub fn push_raw(&mut self, mut results: Vec<PaletteResult>) {
        self.raw.append(&mut results);
        self.rerank();
    }

    /// Append multiple source batches and rerank exactly once at the end.
    /// Prefer this over repeated `push_raw` when loading all sources at open.
    pub fn extend_raw(&mut self, batches: impl IntoIterator<Item = Vec<PaletteResult>>) {
        for mut batch in batches {
            self.raw.append(&mut batch);
        }
        self.rerank();
    }

    fn rerank(&mut self) {
        use nucleo_matcher::{
            pattern::{CaseMatching, Normalization, Pattern},
            Utf32Str,
        };

        let _rerank_start = std::time::Instant::now();
        self.order.clear();
        if self.query.is_empty() {
            // Empty-query path: identity order, no scoring, no cloning.
            self.order.extend(0..self.raw.len() as u32);
        } else {
            let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            self.rank_scratch.clear();
            self.rank_scratch.reserve(self.raw.len());
            for (i, entry) in self.raw.iter().enumerate() {
                self.scratch.clear();
                let label_utf = Utf32Str::new(&entry.label, &mut self.scratch);
                let label_score = pattern.score(label_utf, &mut self.matcher);

                self.scratch.clear();
                let sub_utf = Utf32Str::new(&entry.subtitle, &mut self.scratch);
                let sub_score = pattern.score(sub_utf, &mut self.matcher);

                // Weighted max: label matches are primary; subtitle counts at 0.7x.
                // Reusing self.scratch between the two scorings keeps the rerank
                // 2-allocation budget intact (see tests/palette_rerank_bench_smoke.rs).
                let weighted = match (label_score, sub_score) {
                    (Some(a), Some(b)) => Some(a.max(((b as f32) * SUBTITLE_WEIGHT) as u32)),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(((b as f32) * SUBTITLE_WEIGHT) as u32),
                    (None, None) => None,
                };
                if let Some(score) = weighted {
                    self.rank_scratch.push((score, i as u32));
                }
            }
            // Stable sort by descending score; ties preserve insertion order.
            self.rank_scratch.sort_by(|a, b| b.0.cmp(&a.0));
            self.order.extend(self.rank_scratch.iter().map(|&(_, i)| i));
        }
        tracing::debug!(
            target: "palette",
            query_len = self.query.len(),
            n = self.raw.len(),
            m = self.order.len(),
            elapsed_us = _rerank_start.elapsed().as_micros() as u64,
            "rerank: complete"
        );
        // Clamp cursor to new ranked length.
        self.cursor = self.cursor.min(self.order.len().saturating_sub(1));
    }

    pub fn set_query(&mut self, q: impl Into<String>) {
        self.query = q.into();
        self.rerank();
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.rerank();
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.rerank();
    }

    /// Move the cursor by `delta`. Positive is down, negative is up.
    /// Clamped to `[0, order.len()-1]`. No-op when ranked is empty.
    pub fn move_cursor(&mut self, delta: isize) {
        let n = self.order.len() as isize;
        if n == 0 {
            return;
        }
        let new_cursor = (self.cursor as isize + delta).clamp(0, n - 1);
        self.cursor = new_cursor as usize;
    }

    pub fn cursor_up(&mut self) {
        self.move_cursor(-1);
    }
    pub fn cursor_down(&mut self) {
        self.move_cursor(1);
    }

    /// Move the cursor up by `n` rows (for PageUp). Clamped at 0.
    pub fn page_up(&mut self, n: usize) {
        self.move_cursor(-(n as isize));
    }
    /// Move the cursor down by `n` rows (for PageDown). Clamped at end.
    pub fn page_down(&mut self, n: usize) {
        self.move_cursor(n as isize);
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }
    pub fn cursor_end(&mut self) {
        self.cursor = self.order.len().saturating_sub(1);
    }

    pub fn selected(&self) -> Option<&PaletteResult> {
        self.nth_ranked(self.cursor)
    }

    pub fn reset(&mut self) {
        self.query.clear();
        self.raw.clear();
        self.order.clear();
        self.cursor = 0;
        // `matcher` and `scratch` retain their capacity — intentional:
        // reuse eliminates per-rerank heap allocation across opens.
        self.rank_scratch.clear();
    }
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug)]
pub enum PaletteIntent {
    Accept(PaletteResult),
    SubmitQuery(String),
    Dismiss,
}

impl PaletteState {
    /// Dispatch one key. Returns `Some(intent)` when the overlay should take
    /// a higher-level action (accept or dismiss); `None` means state was
    /// mutated but the overlay stays open.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Option<PaletteIntent> {
        // Ctrl+C always dismisses.
        if ev.modifiers.contains(KeyModifiers::CONTROL) && matches!(ev.code, KeyCode::Char('c')) {
            return Some(PaletteIntent::Dismiss);
        }

        match ev.code {
            KeyCode::Esc => Some(PaletteIntent::Dismiss),
            KeyCode::Enter | KeyCode::Tab if self.query.starts_with('/') => {
                Some(PaletteIntent::SubmitQuery(self.query.clone()))
            }
            KeyCode::Enter | KeyCode::Tab => self.selected().cloned().map(PaletteIntent::Accept),
            KeyCode::Up => {
                self.cursor_up();
                None
            }
            KeyCode::Down => {
                self.cursor_down();
                None
            }
            KeyCode::Backspace => {
                self.pop_char();
                None
            }
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                self.push_char(c);
                None
            }
            _ => None, // swallow other keys silently
        }
    }
}
