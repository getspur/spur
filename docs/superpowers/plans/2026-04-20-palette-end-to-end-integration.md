# Palette End-to-End Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the broken `Command` seam in the Ctrl+K palette, add subtitle-aware fuzzy search, and add an empty-query grouped view with honest UX — without introducing any new public types, `Action` variants, or cross-crate ripples.

**Architecture:** Reuses `submit_router::route` as the single slash-command dispatch source of truth. `palette.rs` stays a pure context-free ranker: all source ordering and population policy decisions live at source-side `collect()` or in `app.rs::open_palette`. Recency sort happens at source-side `collect()` time (not in `rerank`), preserving the 2-allocation rerank invariant verbatim.

**Tech Stack:** Rust, `nucleo-matcher` (fuzzy scoring), ratatui (TUI rendering), crossterm (key events), `tracing` (instrumentation).

**Spec:** `docs/superpowers/specs/2026-04-20-palette-end-to-end-integration-design.md`

---

## File Structure

| File | Role |
|---|---|
| `crates/spur-tui/src/components/palette.rs` | Context-free ranker (F1 subtitle scoring + 1-line invariant doc update) |
| `crates/spur-tui/src/components/palette_overlay.rs` | ratatui widget — grouped empty-state render, no-match hints, hints row content |
| `crates/spur-tui/src/components/palette_sources.rs` | Data sources — `SessionSource` gains recency-sort via `last_opened_at` |
| `crates/spur-tui/src/app.rs` | `open_palette` (C1a live registry + U3c skip Trace); `result_to_action` → `&self` method routing through `submit_router`; `current_acp_session_id` accessor; tracing |
| `crates/spur-tui/tests/palette_state.rs` | F1 ranking assertions |
| `crates/spur-tui/tests/palette_sources.rs` | Recency-sort assertion |
| `crates/spur-tui/tests/palette_integration.rs` | C1a + C1b end-to-end via `App` + `last_action_for_test` |
| `crates/spur-tui/tests/palette_render.rs` | U1 grouped render, U2 hint variants, U7 hints row |

**Public API surface added: zero.**

---

## Preflight

- [ ] **Step 0: Verify baseline builds and tests pass**

Run:
```bash
cargo build -p spur-tui
cargo test -p spur-tui --tests
```

Expected: clean build, all tests PASS. If baseline is broken, stop and report — this plan assumes a green starting point.

---

## Task 1: Tracing Instrumentation

**Rationale:** Lands first because it has no behavioral change and gives us measurement baselines for every subsequent task. No TDD — tracing output is not part of the behavioral contract.

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (inside `open_palette`, around lines 343–368)
- Modify: `crates/spur-tui/src/components/palette.rs` (inside `rerank`, around lines 105–128)

- [ ] **Step 1: Add `tracing::debug!` at `open_palette` start**

In `crates/spur-tui/src/app.rs`, locate `fn open_palette(&mut self)` (starts at line 343). Immediately after the `if self.help_visible || ...` guard, add:

```rust
tracing::debug!(target: "palette", "open_palette: start");
```

- [ ] **Step 2: Add `tracing::debug!` after sources are collected**

In the same function, find the line `let mut batches = vec![` and the block that pushes trace. Replace the whole block that constructs `batches` with:

```rust
let cmd_batch = cmd_src.collect();
let sess_batch = sess_src.collect();
let worker_batch = worker_src.collect();
let trace_skipped = self.session_detail.is_some();
tracing::debug!(
    target: "palette",
    commands = cmd_batch.len(),
    sessions = sess_batch.len(),
    workers = worker_batch.len(),
    trace_count_skipped = trace_skipped,
    "open_palette: sources collected"
);
let batches = vec![cmd_batch, sess_batch, worker_batch];
// Trace source is intentionally skipped until trace-dispatch lands;
// see docs/superpowers/specs/2026-04-20-palette-end-to-end-integration-design.md (U3c).
if let Some(_view) = self.session_detail.as_ref() {
    // Trace will be re-enabled when Action::ScrollToTraceEntry is wired.
    let _ = _view; // borrow-check silence
}
self.palette_state.extend_raw(batches);
```

**Note:** This step combines Task 1 (tracing) and the U3c change (skipping trace). The trace push block is removed as part of U3c — keeping the structural change here is cleaner than splitting across tasks.

- [ ] **Step 3: Add `tracing::debug!` at end of `rerank`**

In `crates/spur-tui/src/components/palette.rs`, inside `fn rerank(&mut self)`, immediately before the final `self.cursor = self.cursor.min(...)` line (around line 127), add:

```rust
tracing::debug!(
    target: "palette",
    query_len = self.query.len(),
    n = self.raw.len(),
    m = self.order.len(),
    "rerank: complete"
);
```

- [ ] **Step 4: Verify the crate still builds**

Run:
```bash
cargo build -p spur-tui
```

Expected: clean build.

- [ ] **Step 5: Verify tests still pass**

Run:
```bash
cargo test -p spur-tui --tests
```

Expected: all PASS (no behavioral change yet — Trace source was already gated on `session_detail` presence, so hiding it only affects behavior when a session is active; existing tests do not depend on Trace results appearing in the palette).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/components/palette.rs
git commit -m "$(cat <<'EOF'
feat(palette): add tracing instrumentation; skip Trace source from extend_raw

Adds debug-level tracing at open_palette start, after source collection
(with per-source counts + trace_skipped flag), and at end of rerank
(with N/M/query_len).

Also removes TraceSource from extend_raw population until the
forthcoming trace-dispatch work (Action::ScrollToTraceEntry) lands with
a stable-id design. The `PalettePayload::Trace` arm is kept as a
type-exhaustiveness anchor.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: F1 — Subtitle-Aware Fuzzy Scoring

**Rationale:** Fixes the latent correctness bug where typing a session-id substring matches nothing because id lives in `subtitle`. Preserves the 2-allocation rerank invariant by reusing `self.scratch` between label and subtitle scoring.

**Files:**
- Modify: `crates/spur-tui/src/components/palette.rs` (inside `rerank`, lines 105–128)
- Test: `crates/spur-tui/tests/palette_state.rs`

- [ ] **Step 1: Write the failing test — subtitle-only match**

In `crates/spur-tui/tests/palette_state.rs`, add at the end of the file:

```rust
#[test]
fn subtitle_only_match_is_ranked() {
    // Label has no match; subtitle (session id) does.
    let mut s = PaletteState::new();
    s.push_raw(vec![PaletteResult {
        kind: PaletteKind::Session,
        label: "human friendly title".into(),
        subtitle: "session · 7f3b0c1d".into(),
        payload: PalettePayload::Session {
            session_id: "7f3b0c1d".into(),
        },
    }]);
    s.set_query("7f3b");
    assert_eq!(
        s.ranked_len(),
        1,
        "query matching only the subtitle should still rank the row"
    );
    assert_eq!(s.nth_ranked(0).unwrap().label, "human friendly title");
}

#[test]
fn label_match_beats_weaker_subtitle_match() {
    // Two rows: first has the query in its subtitle only; second has it in the label.
    // The label-match row should rank above the subtitle-match row given the 0.7x weight.
    let mut s = PaletteState::new();
    s.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Session,
            label: "zzz unrelated".into(),
            subtitle: "session · alpha-match".into(),
            payload: PalettePayload::Session {
                session_id: "sub-id".into(),
            },
        },
        PaletteResult {
            kind: PaletteKind::Session,
            label: "alpha in label".into(),
            subtitle: "session · unrelated".into(),
            payload: PalettePayload::Session {
                session_id: "lbl-id".into(),
            },
        },
    ]);
    s.set_query("alpha");
    let labels: Vec<&str> = s.iter_ranked().map(|r| r.label.as_str()).collect();
    assert_eq!(labels.len(), 2);
    assert_eq!(
        labels[0], "alpha in label",
        "label match should rank above subtitle-only match"
    );
}
```

- [ ] **Step 2: Run new tests to verify they FAIL**

Run:
```bash
cargo test -p spur-tui --test palette_state -- subtitle_only_match_is_ranked label_match_beats_weaker_subtitle_match
```

Expected: `subtitle_only_match_is_ranked` FAILS with `assertion failed: ranked_len == 1` (currently 0 because subtitle isn't scored). The second test may PASS today because the label match wins trivially — that's fine, we keep it as a regression guard against over-weighting subtitle after F1 lands.

- [ ] **Step 3: Implement F1 in `rerank`**

In `crates/spur-tui/src/components/palette.rs`, replace the non-empty-query branch inside `fn rerank(&mut self)` (lines ~113–125). Find:

```rust
let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
let mut tmp: Vec<(u32, u32)> = Vec::with_capacity(self.raw.len());
for (i, entry) in self.raw.iter().enumerate() {
    self.scratch.clear();
    let utf = Utf32Str::new(&entry.label, &mut self.scratch);
    if let Some(score) = pattern.score(utf, &mut self.matcher) {
        tmp.push((score, i as u32));
    }
}
```

Replace with:

```rust
let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
let mut tmp: Vec<(u32, u32)> = Vec::with_capacity(self.raw.len());
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
        (Some(a), Some(b)) => Some(a.max(((b as f32) * 0.7) as u32)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(((b as f32) * 0.7) as u32),
        (None, None) => None,
    };
    if let Some(score) = weighted {
        tmp.push((score, i as u32));
    }
}
```

- [ ] **Step 4: Update the rerank-invariant doc comment**

In the same file, update the module-level doc block at lines 37–45. Find:

```rust
///   - **Exactly 2 allocations** on the non-empty-query path: one `Pattern`
///     parse and one scratch `Vec<(u32, u32)>` for scoring.
```

Replace with:

```rust
///   - **Exactly 2 allocations** on the non-empty-query path: one `Pattern`
///     parse and one scratch `Vec<(u32, u32)>` for scoring. `self.scratch`
///     is reused between label and subtitle scoring (one `clear()` between)
///     so the second Utf32Str conversion does not allocate.
```

- [ ] **Step 5: Run F1 tests to verify they PASS**

Run:
```bash
cargo test -p spur-tui --test palette_state -- subtitle_only_match_is_ranked label_match_beats_weaker_subtitle_match
```

Expected: both PASS.

- [ ] **Step 6: Run the rerank invariant smoke test**

Run:
```bash
cargo test -p spur-tui --test palette_rerank_bench_smoke
```

Expected: PASS. If it fails, the implementation is wrong — fix the implementation, do not loosen the threshold.

- [ ] **Step 7: Run all palette tests**

Run:
```bash
cargo test -p spur-tui --tests palette
```

Expected: all PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/palette.rs crates/spur-tui/tests/palette_state.rs
git commit -m "$(cat <<'EOF'
feat(palette): score subtitle alongside label with weighted max

rerank now scores both entry.label and entry.subtitle, taking the max
of (label, 0.7 * subtitle). Fixes the latent correctness bug where
typing a session-id substring (id lives in subtitle) matched nothing.

self.scratch is reused between the two Utf32Str conversions so the
2-allocation rerank invariant (fenced by palette_rerank_bench_smoke)
is preserved verbatim; the module doc comment is updated to reflect
the reuse.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Recency Sort in `SessionSource`

**Rationale:** Sessions in the empty-query palette render should be ordered by most-recently-opened. `SessionEntry.last_opened_at` (ISO-8601 string, `session_metadata.rs:24`) sorts correctly via lexicographic ordering.

**Files:**
- Modify: `crates/spur-tui/src/components/palette_sources.rs` (lines 42–76, `SessionSource`)
- Test: `crates/spur-tui/tests/palette_sources.rs`

- [ ] **Step 1: Write the failing recency-sort test**

In `crates/spur-tui/tests/palette_sources.rs`, add at the end of the file:

```rust
#[test]
fn session_source_sorts_by_last_opened_at_descending() {
    let mut meta = SessionMetadata::default();
    meta.sessions.insert(
        "old".to_string(),
        SessionEntry {
            title_override: Some("old-session".to_string()),
            last_opened_at: "2020-01-01T00:00:00Z".to_string(),
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "new".to_string(),
        SessionEntry {
            title_override: Some("new-session".to_string()),
            last_opened_at: "2026-04-20T12:00:00Z".to_string(),
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "mid".to_string(),
        SessionEntry {
            title_override: Some("mid-session".to_string()),
            last_opened_at: "2023-06-15T08:30:00Z".to_string(),
            ..Default::default()
        },
    );

    let src = SessionSource::from_metadata(&meta);
    let results = src.collect();
    let labels: Vec<&str> = results.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["new-session", "mid-session", "old-session"],
        "sessions should be ordered by last_opened_at descending"
    );
}
```

- [ ] **Step 2: Run the test to verify it FAILS**

Run:
```bash
cargo test -p spur-tui --test palette_sources -- session_source_sorts_by_last_opened_at_descending
```

Expected: FAIL — today `SessionSource` preserves `BTreeMap` insertion order (alphabetical by key), not recency.

- [ ] **Step 3: Modify `SessionSource` to capture and sort by `last_opened_at`**

In `crates/spur-tui/src/components/palette_sources.rs`, replace `SessionSource` struct and its impl blocks (lines 42–76) with:

```rust
pub struct SessionSource {
    /// Snapshot taken at palette-open time, pre-sorted by recency
    /// (`last_opened_at` descending). Owned to avoid lifetime gymnastics.
    entries: Vec<(String, String)>, // (session_id, display_label)
}

impl SessionSource {
    pub fn from_metadata(meta: &SessionMetadata) -> Self {
        // Capture (session_id, label, last_opened_at) so we can sort.
        let mut ranked: Vec<(String, String, String)> = meta
            .sessions
            .iter()
            .map(|(id, entry)| {
                let label = entry
                    .title_override
                    .clone()
                    .unwrap_or_else(|| id.clone());
                (id.clone(), label, entry.last_opened_at.clone())
            })
            .collect();
        // ISO-8601 timestamps sort correctly via lexicographic order.
        // Descending: newest first.
        ranked.sort_by(|a, b| b.2.cmp(&a.2));
        let entries = ranked
            .into_iter()
            .map(|(id, label, _ts)| (id, label))
            .collect();
        Self { entries }
    }
}

impl PaletteSource for SessionSource {
    fn collect(&self) -> Vec<PaletteResult> {
        self.entries
            .iter()
            .map(|(id, label)| PaletteResult {
                kind: PaletteKind::Session,
                label: label.clone(),
                subtitle: format!("session · {}", id),
                payload: PalettePayload::Session { session_id: id.clone() },
            })
            .collect()
    }
}
```

- [ ] **Step 4: Run the recency test to verify it PASSES**

Run:
```bash
cargo test -p spur-tui --test palette_sources -- session_source_sorts_by_last_opened_at_descending
```

Expected: PASS.

- [ ] **Step 5: Run all palette source tests to check no regressions**

Run:
```bash
cargo test -p spur-tui --test palette_sources
```

Expected: all PASS (the pre-existing tests use `SessionEntry::default()` which has `last_opened_at: ""` — equal timestamps sort stably per `sort_by` docs, so the `session_source_yields_session_kind_rows_with_title_as_label` test should still pass regardless of order).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/palette_sources.rs crates/spur-tui/tests/palette_sources.rs
git commit -m "$(cat <<'EOF'
feat(palette): sort sessions by last_opened_at descending

SessionSource::from_metadata now captures SessionEntry.last_opened_at
(ISO-8601) and sorts results with newest first. Lexicographic sort
on ISO-8601 strings yields correct chronological order.

This unblocks the upcoming empty-query grouped view (U1) where sessions
need to appear in recency order for discoverability.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: C1a — Borrow Live `CommandRegistry` in `open_palette`

**Rationale:** `CommandRegistry::new()` at `app.rs:350` drops agent-static and agent-dynamic commands; replacing it with a borrow of `self.session_detail.command_registry` (with owned fallback when no session) surfaces the full merged command set in the palette. `CommandRegistry` is not `Clone`, so the shim uses a declared-then-assigned `owned_fallback` to extend the borrow lifetime.

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (inside `open_palette`, around lines 349–355)
- Test: `crates/spur-tui/tests/palette_integration.rs`

- [ ] **Step 1: Find and skim the existing palette integration test file**

Read `crates/spur-tui/tests/palette_integration.rs` end-to-end. Note the helpers (if any) for constructing an `App` with a session and command registry already loaded. If a helper that seeds a session-detail with dynamic commands does not exist, we'll add one.

- [ ] **Step 2: Write a failing test asserting agent-dynamic commands surface**

This test needs to construct an `App` with an active `session_detail` whose `command_registry` has at least one dynamic command, then call `App::try_open_palette_for_test()` and inspect the palette state for the dynamic command's name.

Because constructing an `App` fully in-process is heavy, use the smallest available test entry point. If the existing `palette_integration.rs` has no `App` builder helper, prefer a unit test directly on `open_palette`'s effect observable via the existing `cfg(test)` accessors. Concretely, add this test to `crates/spur-tui/tests/palette_integration.rs`:

```rust
#[test]
fn open_palette_surfaces_session_command_registry_entries() {
    // PRECONDITION: There is a test-helper on App that exposes an App
    // with a session_detail whose command_registry contains at least one
    // dynamic agent command. If such a helper does not yet exist,
    // fail this test with a clear message so the implementer adds it.

    let mut app = crate::util::app_with_seeded_session_and_dynamic_command(
        "codex",
        "review",
        "Review the current diff",
    );
    app.try_open_palette_for_test();
    let state = app.palette_state_for_test();
    let labels: Vec<&str> = state.iter_ranked().map(|r| r.label.as_str()).collect();
    assert!(
        labels.iter().any(|l| *l == "review"),
        "expected the dynamic /review command to appear in the palette; got: {labels:?}"
    );
}
```

Also add the module reference at the top of `crates/spur-tui/tests/palette_integration.rs`:

```rust
mod util; // see tests/palette_integration/util.rs (created below)
```

- [ ] **Step 3: Create the test helper module**

Create the file `crates/spur-tui/tests/palette_integration/util.rs` with:

```rust
//! Shared helpers for palette_integration tests.
//!
//! Centralizes the thorny construction of an `App` with a pre-seeded
//! session_detail + command registry, so individual tests stay short.

use spur_tui::app::App;

/// Construct an `App` whose `session_detail` has a `CommandRegistry`
/// containing a single dynamic command `/{name}` with `description` for
/// agent handle `handle`. The session_detail is otherwise minimal.
///
/// Intended only for palette-integration tests.
pub fn app_with_seeded_session_and_dynamic_command(
    handle: &str,
    name: &str,
    description: &str,
) -> App {
    // Build a minimal App via the test-only builder. Implementation
    // detail: this relies on App exposing a cfg(test) constructor that
    // wires up a SessionDetailView with a mutable command_registry.
    // See App::new_for_palette_test (added alongside this helper).
    let mut app = App::new_for_palette_test();
    app.seed_session_detail_with_dynamic_command_for_test(handle, name, description);
    app
}
```

- [ ] **Step 4: Add the `cfg(test)` App builder**

The helper relies on two new `cfg(any(test, debug_assertions))` accessors on `App`. Add them to `crates/spur-tui/src/app.rs`, inside the `impl App` block that already contains `try_open_palette_for_test` (near line 375):

```rust
#[cfg(any(test, debug_assertions))]
pub fn palette_state_for_test(&self) -> &crate::components::palette::PaletteState {
    &self.palette_state
}

#[cfg(any(test, debug_assertions))]
pub fn new_for_palette_test() -> Self {
    // Minimal App built for palette-integration tests. Wires enough
    // state that open_palette() can run: empty metadata store, empty
    // lineage, no session_detail until seed_* is called. Matches
    // the subset of App::build_with_license_state needed by the tests.
    use crate::config::SpurConfig;
    use crate::session_metadata::SessionMetadataStore;
    use spur_core::lineage::projection::ExecutorLineage;
    let (user_input_tx, _user_input_rx) = tokio::sync::mpsc::channel(16);
    Self {
        config: std::sync::Arc::new(SpurConfig::default()),
        metadata_store: SessionMetadataStore::load(std::path::Path::new(
            "/tmp/spur-palette-test-metadata.json",
        )),
        lineage: ExecutorLineage::new(),
        session_detail: None,
        palette_state: crate::components::palette::PaletteState::new(),
        palette_visible: false,
        help_visible: false,
        quit_confirm_visible: false,
        dirty: false,
        should_quit: false,
        last_action: None,
        user_input_tx,
        // All other fields default / none — tests must not exercise them.
        ..Default::default()
    }
}

#[cfg(any(test, debug_assertions))]
pub fn seed_session_detail_with_dynamic_command_for_test(
    &mut self,
    handle: &str,
    name: &str,
    description: &str,
) {
    use crate::commands::registry::CommandRegistry;
    use spur_acp::{AvailableCommand, CommandsConfig};
    let cfg = CommandsConfig::default();
    let entry = crate::agents::build_entry(
        handle,
        &cfg,
        &AvailableCommand::new(name, description),
    );
    let mut registry = CommandRegistry::new();
    registry.set_agent_commands(handle, vec![entry]);
    // Minimal SessionDetailView shim: we only need command_registry
    // accessible via `self.session_detail.as_ref().map(|v| &v.command_registry)`.
    // If SessionDetailView does not have a public test-ctor, add one.
    self.session_detail = Some(
        crate::views::session_detail::SessionDetailView::new_for_palette_test(registry),
    );
}
```

**If `App` does not `#[derive(Default)]` or if its fields differ from this list**, adjust the struct-literal initializer to match `App::build_with_license_state`'s existing defaults, keeping the same set of populated fields (`config`, `metadata_store`, `lineage`, `session_detail`, `palette_state`, `palette_visible`, `help_visible`, `quit_confirm_visible`, `dirty`, `should_quit`, `last_action`, `user_input_tx`). This is the ONLY place in the plan where you may need to adapt to struct shape not known at plan time; keep the adaptation strictly local.

- [ ] **Step 5: Add `SessionDetailView::new_for_palette_test`**

In `crates/spur-tui/src/views/session_detail.rs`, add inside `impl SessionDetailView`:

```rust
#[cfg(any(test, debug_assertions))]
pub fn new_for_palette_test(
    command_registry: crate::commands::registry::CommandRegistry,
) -> Self {
    // Construct a minimal SessionDetailView whose only populated field
    // relevant to palette tests is `command_registry`. All other fields
    // default. Not suitable for any test that exercises the view's
    // render/input-bar/trace paths.
    let mut view = Self::new_minimal_for_test();
    view.command_registry = command_registry;
    view
}
```

And add a helper `new_minimal_for_test()` that returns a `SessionDetailView` using whatever minimal constructor exists. If no minimal constructor exists, add one alongside — construct the struct with `Default::default()` values for every field except `command_registry` and `session_id`, using a synthetic session id like `spur_acp::SessionId("palette-test".into())`. Example scaffold:

```rust
#[cfg(any(test, debug_assertions))]
fn new_minimal_for_test() -> Self {
    use spur_acp::SessionId;
    Self {
        session_id: SessionId("palette-test".into()),
        command_registry: crate::commands::registry::CommandRegistry::new(),
        react_trace: crate::components::react_trace::ReactTrace::default(),
        // … populate remaining fields with their Default values …
        ..Default::default()
    }
}
```

Adapt field population to the actual `SessionDetailView` struct (not known in full at plan time; inspect once, populate every field literally in this helper). Prefer struct-literal with all fields listed over `..Default::default()` if the struct does not derive `Default`.

- [ ] **Step 6: Run the new test — confirm it FAILS with the expected error**

Run:
```bash
cargo test -p spur-tui --test palette_integration -- open_palette_surfaces_session_command_registry_entries
```

Expected: FAIL — assertion `"expected the dynamic /review command to appear in the palette"` because `open_palette` currently constructs a fresh empty `CommandRegistry::new()` instead of borrowing `self.session_detail.command_registry`.

- [ ] **Step 7: Implement C1a in `open_palette`**

In `crates/spur-tui/src/app.rs`, inside `fn open_palette(&mut self)`, replace lines 349–351. Find:

```rust
// Load sources: Commands, Sessions, Workers, Trace.
let cmd_registry = crate::commands::registry::CommandRegistry::new();
let cmd_src = CommandSource::new(&cmd_registry);
```

Replace with:

```rust
// Load sources: Commands, Sessions, Workers. (Trace deferred — see U3c.)
// CommandRegistry is not Clone; borrow from the active session_detail
// or fall back to a fresh empty one (SpurLocal commands are still
// included unconditionally via registry's ensure_cache).
let owned_fallback;
let cmd_registry: &crate::commands::registry::CommandRegistry =
    match self.session_detail.as_ref() {
        Some(view) => &view.command_registry,
        None => {
            owned_fallback = crate::commands::registry::CommandRegistry::new();
            &owned_fallback
        }
    };
let cmd_src = CommandSource::new(cmd_registry);
```

- [ ] **Step 8: Run the integration test — confirm it PASSES**

Run:
```bash
cargo test -p spur-tui --test palette_integration -- open_palette_surfaces_session_command_registry_entries
```

Expected: PASS.

- [ ] **Step 9: Run the full palette test suite**

Run:
```bash
cargo test -p spur-tui --tests palette
```

Expected: all PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/session_detail.rs crates/spur-tui/tests/palette_integration.rs crates/spur-tui/tests/palette_integration/util.rs
git commit -m "$(cat <<'EOF'
feat(palette): borrow live CommandRegistry from session_detail in open_palette

open_palette now borrows the active SessionDetailView's command_registry
(with an owned empty fallback when no session is active) so agent-static
and agent-dynamic commands surface in the palette alongside spur-local
meta-commands. CommandRegistry is not Clone; the borrow lifetime is
extended via a declared-then-assigned owned_fallback shim.

Adds cfg(test) App::new_for_palette_test +
SessionDetailView::new_for_palette_test helpers plus a tests/util shim
to keep palette-integration tests short.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: C1b — Route `Command` Accept Through `submit_router`

**Rationale:** `PalettePayload::Command` accept currently returns `None`. Route it through the existing `submit_router::route(...)` primitive (the same one the input bar uses) so spur-local commands dispatch their concrete `Action` directly, and agent-static / agent-dynamic commands dispatch `Action::SendMessage` / `Action::VendorExec` using the active session's id. Zero new `Action` variants.

**Files:**
- Modify: `crates/spur-tui/src/app.rs`:
  - `result_to_action` free fn (lines 2139–2163) → `&self` method
  - Call site in the `PaletteIntent::Accept` arm (line 488)
  - Add `current_acp_session_id` accessor
- Test: `crates/spur-tui/tests/palette_integration.rs`

- [ ] **Step 1: Write failing tests for the three dispatch shapes**

Append to `crates/spur-tui/tests/palette_integration.rs`:

```rust
#[test]
fn accept_spur_local_command_emits_concrete_action() {
    // /help is a spur-local command → SubmitDecision::Local { Action::ShowHelp }.
    // Palette should emit Action::ShowHelp on Accept.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::action::Action;
    let mut app = crate::util::app_with_seeded_session_and_dynamic_command(
        "codex",
        "anything",
        "unused",
    );
    app.try_open_palette_for_test();
    app.palette_state_for_test_mut().set_query("help");
    // Press Enter on the best match
    let _ = app.handle_crossterm_event_for_test(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ));
    assert!(
        matches!(app.last_action_for_test(), Some(Action::ShowHelp)),
        "expected Action::ShowHelp; got {:?}",
        app.last_action_for_test()
    );
}

#[test]
fn accept_agent_dynamic_command_emits_send_or_vendor_exec() {
    // With a dynamic /review command registered, accepting /review from the
    // palette should produce either SendMessage or VendorExec depending on
    // the agent's CommandsConfig.dispatch. Either is acceptable; None is not.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::action::Action;
    let mut app = crate::util::app_with_seeded_session_and_dynamic_command(
        "codex",
        "review",
        "Review the current diff",
    );
    app.try_open_palette_for_test();
    app.palette_state_for_test_mut().set_query("review");
    let _ = app.handle_crossterm_event_for_test(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ));
    let action = app.last_action_for_test();
    assert!(
        matches!(
            action,
            Some(Action::SendMessage { .. }) | Some(Action::VendorExec { .. })
        ),
        "expected SendMessage or VendorExec; got {:?}",
        action
    );
}

#[test]
fn accept_command_without_session_emits_none_for_agent_commands() {
    // When no session_detail is active, agent-dynamic commands cannot
    // dispatch (no session id). Result: last_action is left unchanged
    // (None). Spur-local /help still works without a session.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_tui::action::Action;
    let mut app = spur_tui::app::App::new_for_palette_test();
    // No session_detail seeded → only spur-local commands are available.
    app.try_open_palette_for_test();
    app.palette_state_for_test_mut().set_query("help");
    let _ = app.handle_crossterm_event_for_test(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    ));
    assert!(
        matches!(app.last_action_for_test(), Some(Action::ShowHelp)),
        "spur-local /help should work without a session"
    );
}
```

- [ ] **Step 2: Add the `palette_state_for_test_mut` and `handle_crossterm_event_for_test` accessors**

In `crates/spur-tui/src/app.rs`, add beside the other `cfg(any(test, debug_assertions))` helpers:

```rust
#[cfg(any(test, debug_assertions))]
pub fn palette_state_for_test_mut(&mut self) -> &mut crate::components::palette::PaletteState {
    &mut self.palette_state
}

#[cfg(any(test, debug_assertions))]
pub fn handle_crossterm_event_for_test(
    &mut self,
    key: crossterm::event::KeyEvent,
) {
    use crossterm::event::Event;
    self.handle_crossterm_event(Event::Key(key));
}
```

If `handle_crossterm_event` takes a different signature (e.g., `KeyEvent` directly, or `&Event`), adjust the wrapper to match. Verify by grepping `fn handle_crossterm_event` in `app.rs` once before writing.

- [ ] **Step 3: Run the new tests — confirm they FAIL**

Run:
```bash
cargo test -p spur-tui --test palette_integration -- accept_spur_local_command_emits_concrete_action accept_agent_dynamic_command_emits_send_or_vendor_exec accept_command_without_session_emits_none_for_agent_commands
```

Expected: all three FAIL because today `PalettePayload::Command` returns `None` from `result_to_action`; `last_action` stays at the previous `None` and the `matches!(... Action::ShowHelp)` assertion fails.

- [ ] **Step 4: Add `current_acp_session_id` accessor on `App`**

In `crates/spur-tui/src/app.rs`, inside the main `impl App` block (any location near other small accessors), add:

```rust
/// Current ACP session id, if a `session_detail` is active.
/// Used by the palette's `Command` accept path to construct
/// `Action::SendMessage` / `Action::VendorExec` without a round-trip
/// through the session-detail view.
fn current_acp_session_id(&self) -> Option<spur_acp::SessionId> {
    self.session_detail
        .as_ref()
        .map(|v| v.session_id().clone())
}
```

- [ ] **Step 5: Refactor `result_to_action` from free fn to `&self` method and route Command**

In `crates/spur-tui/src/app.rs`, remove the free function `fn result_to_action(...)` starting at line 2139. Add as a method in the main `impl App` block:

```rust
fn result_to_action(
    &self,
    result: crate::components::palette::PaletteResult,
) -> Option<crate::action::Action> {
    use crate::action::{Action, ViewId};
    use crate::commands::submit_router::{route, SubmitDecision};
    use crate::components::palette::PalettePayload;
    match result.payload {
        PalettePayload::Session { session_id } => {
            Some(Action::ResumeSession { session_id })
        }
        PalettePayload::Worker { session_id } => {
            Some(Action::NavigateTo(ViewId::SessionDetail(session_id)))
        }
        PalettePayload::Command { name } => {
            let view = self.session_detail.as_ref()?;
            let registry = &view.command_registry;
            match route(&format!("/{name}"), &[], registry, false) {
                SubmitDecision::Local { action } => Some(action),
                SubmitDecision::Send { blocks, interrupt } => {
                    let session = self.current_acp_session_id()?;
                    Some(Action::SendMessage {
                        session,
                        blocks,
                        interrupt,
                    })
                }
                SubmitDecision::VendorExec { method, params } => {
                    let session = self.current_acp_session_id()?;
                    Some(Action::VendorExec {
                        session,
                        method,
                        params,
                    })
                }
                SubmitDecision::Empty => None,
            }
        }
        PalettePayload::Trace { entry_idx: _ } => {
            // TODO(palette-trace-dispatch): wire when stable-id design lands.
            // Unreachable in practice because TraceSource is omitted from
            // extend_raw (see open_palette). Kept as a type-exhaustiveness
            // anchor and a forward-compat hook.
            None
        }
    }
}
```

**Note:** `Action::SendMessage` has additional fields beyond `{ session, blocks, interrupt }` per `action.rs:17–`. Verify the exact struct shape by reading `action.rs` lines 17–30; populate any additional required fields (e.g., `worker: None` if present) with their defaults. The struct-literal above lists only the fields named in the spec — extend to match the actual variant.

- [ ] **Step 6: Update the call site in the Accept arm**

In `crates/spur-tui/src/app.rs`, find the line that calls `result_to_action(result)` inside the `PaletteIntent::Accept(result)` arm (around line 488). Change it from:

```rust
if let Some(action) = result_to_action(result) {
```

to:

```rust
if let Some(action) = self.result_to_action(result) {
```

**Important:** Watch for borrow-checker friction. `handle_key` is called on `&mut self.palette_state`, returning a `PaletteIntent` that owns `PaletteResult`. The subsequent `self.result_to_action(result)` takes `&self`. If the compiler complains that `self` is still mutably borrowed through `palette_state`, NLL should release it after the `match` scrutinee resolves. If not, bind the intent into a local first:

```rust
let intent = self.palette_state.handle_key(key);
match intent {
    Some(PaletteIntent::Dismiss) => { /* ... */ }
    Some(PaletteIntent::Accept(result)) => {
        self.palette_visible = false;
        if let Some(action) = self.result_to_action(result) {
            // existing dispatch code
        }
    }
    None => {}
}
```

- [ ] **Step 7: Run the three Command-dispatch tests — confirm they PASS**

Run:
```bash
cargo test -p spur-tui --test palette_integration -- accept_spur_local_command_emits_concrete_action accept_agent_dynamic_command_emits_send_or_vendor_exec accept_command_without_session_emits_none_for_agent_commands
```

Expected: all three PASS.

- [ ] **Step 8: Run all palette tests**

Run:
```bash
cargo test -p spur-tui --tests palette
```

Expected: all PASS. If any pre-existing palette test relies on the free-function form of `result_to_action`, update it to use `App::result_to_action`. If a test is not reachable because `result_to_action` is now a private method, consider keeping it pub(crate) or exposing a `cfg(test)` wrapper.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/tests/palette_integration.rs
git commit -m "$(cat <<'EOF'
feat(palette): route Command accept through submit_router

result_to_action is now a &self method on App. PalettePayload::Command
dispatches via the existing submit_router::route primitive (the same
used by the input bar), lifting SubmitDecision::{Local, Send,
VendorExec, Empty} into the corresponding semantic Actions.

Spur-local commands (/help, /clear, /cost, /vim, …) dispatch as their
concrete Action directly. Agent-static and agent-dynamic commands
dispatch Action::SendMessage or Action::VendorExec using the active
ACP session id. No new Action variants are added.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: U2 — Enhanced No-Match Hint

**Rationale:** Today the empty-result placeholder renders either `"type to filter"` or `"no matches"`. Make the non-empty-query-no-match path actionable: suggest shortening the query, and special-case the `/`-prefix-without-session path.

**Files:**
- Modify: `crates/spur-tui/src/components/palette_overlay.rs`
- Test: `crates/spur-tui/tests/palette_render.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/spur-tui/tests/palette_render.rs`, add:

```rust
#[test]
fn overlay_renders_no_match_hint_when_query_nonempty_and_ranked_empty() {
    // Non-empty query with no matches → "No matches. Try shorter or different keywords."
    let mut state = PaletteState::new();
    state.push_raw(vec![PaletteResult {
        kind: PaletteKind::Session,
        label: "zzz".into(),
        subtitle: "".into(),
        payload: PalettePayload::Session { session_id: "z".into() },
    }]);
    state.set_query("xyzzyfoobar");
    let rendered = render_to_string_with_session_flag(&state, 60, 12, true);
    assert!(
        rendered.contains("No matches"),
        "missing no-match hint; got:\n{rendered}"
    );
}

#[test]
fn overlay_renders_slash_hint_when_no_session_and_query_starts_with_slash() {
    // `/` prefix + no active session → hint about needing a session.
    let mut state = PaletteState::new();
    state.set_query("/something");
    let rendered = render_to_string_with_session_flag(&state, 60, 12, false);
    assert!(
        rendered.contains("Slash commands need an active session"),
        "missing slash-without-session hint; got:\n{rendered}"
    );
}
```

Also update the existing `render_to_string` helper to accept a session-active flag. Replace the helper (lines 5–18) with:

```rust
fn render_to_string(state: &PaletteState, width: u16, height: u16) -> String {
    render_to_string_with_session_flag(state, width, height, true)
}

fn render_to_string_with_session_flag(
    state: &PaletteState,
    width: u16,
    height: u16,
    session_active: bool,
) -> String {
    let backend = TestBackend::new(width, height);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect { x: 0, y: 0, width, height };
        let overlay = PaletteOverlay::new(state).with_session_active(session_active);
        f.render_widget(overlay, area);
    }).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
```

- [ ] **Step 2: Run the new tests to verify they FAIL**

Run:
```bash
cargo test -p spur-tui --test palette_render -- overlay_renders_no_match_hint_when_query_nonempty_and_ranked_empty overlay_renders_slash_hint_when_no_session_and_query_starts_with_slash
```

Expected: both FAIL — `with_session_active` doesn't compile yet, and the new hint strings aren't rendered.

- [ ] **Step 3: Add `with_session_active` builder to `PaletteOverlay`**

In `crates/spur-tui/src/components/palette_overlay.rs`, replace the `PaletteOverlay` struct and its `new` impl (lines 13–21) with:

```rust
pub struct PaletteOverlay<'a> {
    state: &'a PaletteState,
    session_active: bool,
}

impl<'a> PaletteOverlay<'a> {
    pub fn new(state: &'a PaletteState) -> Self {
        Self {
            state,
            session_active: false,
        }
    }

    pub fn with_session_active(mut self, active: bool) -> Self {
        self.session_active = active;
        self
    }
}
```

- [ ] **Step 4: Enhance the empty-state render to use the new hints**

In the same file, inside `impl<'a> Widget for PaletteOverlay<'a>`, replace the empty-state branch (lines 79–89) with:

```rust
if self.state.ranked_len() == 0 {
    let msg: String = if self.state.query().is_empty() {
        "type to filter".to_string()
    } else if self.state.query().starts_with('/') && !self.session_active {
        "Slash commands need an active session.".to_string()
    } else {
        "No matches. Try shorter or different keywords.".to_string()
    };
    Paragraph::new(Line::from(Span::styled(
        msg,
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
    )))
    .render(list_area, buf);
} else {
    // existing populated-list render (unchanged)
```

Keep the closing brace that matches `else {` from the original populated-list block unchanged.

- [ ] **Step 5: Wire `session_active` from `App` at render time**

In `crates/spur-tui/src/app.rs`, find the site that constructs `PaletteOverlay::new(&self.palette_state)` (the palette render call, expected around line 1862 based on the earlier map). Update it to:

```rust
PaletteOverlay::new(&self.palette_state)
    .with_session_active(self.session_detail.is_some())
```

- [ ] **Step 6: Run the new tests — confirm they PASS**

Run:
```bash
cargo test -p spur-tui --test palette_render -- overlay_renders_no_match_hint_when_query_nonempty_and_ranked_empty overlay_renders_slash_hint_when_no_session_and_query_starts_with_slash
```

Expected: both PASS.

- [ ] **Step 7: Run all render tests to check no regressions**

Run:
```bash
cargo test -p spur-tui --test palette_render
```

Expected: all PASS. The pre-existing `overlay_renders_empty_state_placeholder` test checks for `"type to filter"` or `"no matches"` — both strings still appear (empty-query still shows `"type to filter"`; non-empty with no match now shows `"No matches. …"` which contains `"No matches"`).

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/palette_overlay.rs crates/spur-tui/src/app.rs crates/spur-tui/tests/palette_render.rs
git commit -m "$(cat <<'EOF'
feat(palette): forgiving no-match + slash-without-session hints

PaletteOverlay gains a builder-style with_session_active(bool) flag.
Empty-state rendering now distinguishes three paths:

  * empty query            → "type to filter"
  * non-empty + no matches → "No matches. Try shorter or different keywords."
  * `/`-prefix + no session → "Slash commands need an active session."

The session flag is threaded from App::session_detail at render time
so the overlay stays a pure widget.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: U1 — Empty-Query Grouped View

**Rationale:** Empty open should be self-documenting. Render kind-grouped sections (`COMMANDS`, `SESSIONS`, `WORKERS`) with a per-kind cap of 5 (auto-scaling down to 2 on small terminals), plus a `TRACE — coming soon` placeholder row. When the query is non-empty, fall back to today's flat scored render.

**Files:**
- Modify: `crates/spur-tui/src/components/palette_overlay.rs`
- Test: `crates/spur-tui/tests/palette_render.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/spur-tui/tests/palette_render.rs`, add:

```rust
#[test]
fn overlay_renders_grouped_sections_when_query_empty() {
    // Seed one of each kind; empty query; expect section headers.
    let mut state = PaletteState::new();
    state.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/help".into(),
            subtitle: "cmd · show help".into(),
            payload: PalettePayload::Command { name: "help".into() },
        },
        PaletteResult {
            kind: PaletteKind::Session,
            label: "refactor-auth".into(),
            subtitle: "session · s1".into(),
            payload: PalettePayload::Session { session_id: "s1".into() },
        },
        PaletteResult {
            kind: PaletteKind::Worker,
            label: "codex".into(),
            subtitle: "worker · running".into(),
            payload: PalettePayload::Worker {
                session_id: spur_acp::SessionId("w1".into()),
            },
        },
    ]);
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    assert!(rendered.contains("COMMANDS"), "missing COMMANDS header:\n{rendered}");
    assert!(rendered.contains("SESSIONS"), "missing SESSIONS header:\n{rendered}");
    assert!(rendered.contains("WORKERS"), "missing WORKERS header:\n{rendered}");
    assert!(
        rendered.contains("TRACE — coming soon") || rendered.contains("TRACE - coming soon"),
        "missing TRACE placeholder:\n{rendered}"
    );
    assert!(rendered.contains("/help"));
    assert!(rendered.contains("refactor-auth"));
    assert!(rendered.contains("codex"));
}

#[test]
fn overlay_falls_back_to_flat_render_when_query_nonempty() {
    let mut state = PaletteState::new();
    state.push_raw(vec![PaletteResult {
        kind: PaletteKind::Command,
        label: "/help".into(),
        subtitle: "cmd · show help".into(),
        payload: PalettePayload::Command { name: "help".into() },
    }]);
    state.set_query("help");
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    // Flat render = no section headers.
    assert!(!rendered.contains("COMMANDS"), "unexpected header in flat render:\n{rendered}");
    assert!(rendered.contains("/help"));
}

#[test]
fn overlay_grouped_view_caps_rows_per_kind() {
    // 10 sessions; default cap is 5 → at most 5 session labels render.
    let mut state = PaletteState::new();
    let mut batch = Vec::new();
    for i in 0..10 {
        batch.push(PaletteResult {
            kind: PaletteKind::Session,
            label: format!("session-{i:02}"),
            subtitle: format!("session · s{i}"),
            payload: PalettePayload::Session {
                session_id: format!("s{i}"),
            },
        });
    }
    state.push_raw(batch);
    let rendered = render_to_string_with_session_flag(&state, 80, 24, true);
    let shown = (0..10)
        .filter(|i| rendered.contains(&format!("session-{i:02}")))
        .count();
    assert!(
        shown <= 5,
        "expected cap of 5 sessions in grouped view; got {shown}:\n{rendered}"
    );
    assert!(shown >= 2, "expected at least 2 sessions rendered; got {shown}");
}
```

- [ ] **Step 2: Run the new tests to verify they FAIL**

Run:
```bash
cargo test -p spur-tui --test palette_render -- overlay_renders_grouped_sections_when_query_empty overlay_falls_back_to_flat_render_when_query_nonempty overlay_grouped_view_caps_rows_per_kind
```

Expected: all three FAIL — no section headers exist; current render is always flat.

- [ ] **Step 3: Implement the grouped render path**

In `crates/spur-tui/src/components/palette_overlay.rs`, replace the populated-list render branch (the `else` block starting around line 90 that builds `items: Vec<ListItem>`) with a dispatch:

```rust
} else if self.state.query().is_empty() {
    self.render_grouped(list_area, buf);
} else {
    self.render_flat(list_area, buf);
}
```

Add the two methods as associated functions on `PaletteOverlay`. Insert above the `impl<'a> Widget for PaletteOverlay<'a>` block:

```rust
impl<'a> PaletteOverlay<'a> {
    fn render_flat(&self, area: Rect, buf: &mut Buffer) {
        let items: Vec<ListItem> = self
            .state
            .iter_ranked()
            .enumerate()
            .map(|(i, r)| {
                let selected = i == self.state.cursor();
                let style = if selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                let spans = vec![
                    Span::styled(
                        format!("  {}  ", badge_for(&r.kind)),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(r.label.clone(), style),
                    Span::raw("   "),
                    Span::styled(
                        r.subtitle.clone(),
                        Style::default().fg(Color::DarkGray),
                    ),
                ];
                ListItem::new(Line::from(spans))
            })
            .collect();
        List::new(items).render(area, buf);
    }

    fn render_grouped(&self, area: Rect, buf: &mut Buffer) {
        // Partition ranked results by kind, preserving order within each kind.
        let mut commands: Vec<&crate::components::palette::PaletteResult> = Vec::new();
        let mut sessions: Vec<&crate::components::palette::PaletteResult> = Vec::new();
        let mut workers: Vec<&crate::components::palette::PaletteResult> = Vec::new();
        for r in self.state.iter_ranked() {
            match r.kind {
                PaletteKind::Command => commands.push(r),
                PaletteKind::Session => sessions.push(r),
                PaletteKind::Worker => workers.push(r),
                PaletteKind::Trace => { /* skipped upstream; placeholder rendered below */ }
            }
        }

        // Per-kind cap: default 5, scale down to fit available height.
        // Three headers + a trace placeholder row + per-kind rows must fit
        // in `area.height`. Minimum cap is 2.
        let headers: u16 = 3 + 1; // COMMANDS, SESSIONS, WORKERS, TRACE placeholder
        let available = area.height.saturating_sub(headers);
        let cap = (available / 3).max(2).min(5) as usize;

        let mut y = area.y;
        let mut render_section = |title: &str, rows: &[&crate::components::palette::PaletteResult]| {
            if y >= area.y + area.height { return; }
            Paragraph::new(Line::from(Span::styled(
                title.to_string(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )))
            .render(
                Rect { x: area.x, y, width: area.width, height: 1 },
                buf,
            );
            y = y.saturating_add(1);
            for r in rows.iter().take(cap) {
                if y >= area.y + area.height { return; }
                let spans = vec![
                    Span::styled(
                        format!("  {}  ", badge_for(&r.kind)),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(r.label.clone()),
                    Span::raw("   "),
                    Span::styled(
                        r.subtitle.clone(),
                        Style::default().fg(Color::DarkGray),
                    ),
                ];
                Paragraph::new(Line::from(spans))
                    .render(
                        Rect { x: area.x, y, width: area.width, height: 1 },
                        buf,
                    );
                y = y.saturating_add(1);
            }
        };

        render_section("COMMANDS", &commands);
        render_section("SESSIONS", &sessions);
        render_section("WORKERS", &workers);

        // Trace placeholder — honest about the deferred feature.
        if y < area.y + area.height {
            Paragraph::new(Line::from(Span::styled(
                "TRACE — coming soon",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )))
            .render(
                Rect { x: area.x, y, width: area.width, height: 1 },
                buf,
            );
        }
    }
}
```

- [ ] **Step 4: Run the grouped-view tests — confirm they PASS**

Run:
```bash
cargo test -p spur-tui --test palette_render -- overlay_renders_grouped_sections_when_query_empty overlay_falls_back_to_flat_render_when_query_nonempty overlay_grouped_view_caps_rows_per_kind
```

Expected: all three PASS.

- [ ] **Step 5: Run all render tests**

Run:
```bash
cargo test -p spur-tui --test palette_render
```

Expected: all PASS. Note: the pre-existing `overlay_renders_title_query_and_rows` test uses an empty query (implicit) and checks for `refactor-auth`, `/plan`, badges `$` and `>`. With grouped rendering on empty query, the labels still appear (inside their sections) and badges are still rendered — this test should still pass.

If it fails because the pre-existing test relies on flat rendering with an empty query, set a query in that test first (`state.set_query(" ")` or similar non-empty value) or update the assertions to accept the grouped layout (look for `COMMANDS` / `SESSIONS` headers as additional evidence).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/palette_overlay.rs crates/spur-tui/tests/palette_render.rs
git commit -m "$(cat <<'EOF'
feat(palette): kind-grouped empty-query render

When the query is empty, PaletteOverlay now renders kind-grouped
sections (COMMANDS, SESSIONS, WORKERS) with a per-kind cap of 5 rows,
plus a 'TRACE — coming soon' placeholder. The cap auto-scales down to
a minimum of 2 when modal height is constrained.

When the query is non-empty, the existing flat scored render is used.
Makes first-open self-documenting and preserves the ranked result
ordering on query.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: U7 — Hints Row Content

**Rationale:** The spec mandates an exact hints-row wording. Verify and update in place.

**Files:**
- Modify: `crates/spur-tui/src/components/palette_overlay.rs`
- Test: `crates/spur-tui/tests/palette_render.rs`

- [ ] **Step 1: Write the failing test**

In `crates/spur-tui/tests/palette_render.rs`, add:

```rust
#[test]
fn overlay_hints_row_has_select_accept_dismiss() {
    let state = PaletteState::new();
    let rendered = render_to_string_with_session_flag(&state, 80, 12, true);
    assert!(rendered.contains("select"), "hints missing 'select': {rendered}");
    assert!(rendered.contains("accept"), "hints missing 'accept': {rendered}");
    assert!(rendered.contains("dismiss"), "hints missing 'dismiss': {rendered}");
}
```

- [ ] **Step 2: Run the test — confirm it FAILS**

Run:
```bash
cargo test -p spur-tui --test palette_render -- overlay_hints_row_has_select_accept_dismiss
```

Expected: FAIL — today's hints row says `"↑↓ select · ↵ go · esc close · type to filter"`, missing `"accept"` and `"dismiss"`.

- [ ] **Step 3: Update the hints row string**

In `crates/spur-tui/src/components/palette_overlay.rs`, find (around line 122):

```rust
"↑↓ select · ↵ go · esc close · type to filter",
```

Replace with:

```rust
"↑↓ select   ⏎ accept   esc dismiss",
```

- [ ] **Step 4: Run the test — confirm it PASSES**

Run:
```bash
cargo test -p spur-tui --test palette_render -- overlay_hints_row_has_select_accept_dismiss
```

Expected: PASS.

- [ ] **Step 5: Run all render tests**

Run:
```bash
cargo test -p spur-tui --test palette_render
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/palette_overlay.rs crates/spur-tui/tests/palette_render.rs
git commit -m "$(cat <<'EOF'
chore(palette): tighten hints row wording to select/accept/dismiss

Updates the overlay hints row from the ad-hoc
'↑↓ select · ↵ go · esc close · type to filter' to the spec's
'↑↓ select   ⏎ accept   esc dismiss'. The 'type to filter' hint
moves into the empty-state placeholder where it's actually needed.

Co-Authored-By: Claude Opus 4 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Final Verification

- [ ] **Step 1: Run the entire spur-tui test suite**

Run:
```bash
cargo test -p spur-tui --tests
```

Expected: all tests PASS.

- [ ] **Step 2: Run the whole workspace build**

Run:
```bash
cargo build --workspace
```

Expected: clean build.

- [ ] **Step 3: Run the workspace test suite**

Run:
```bash
cargo test --workspace
```

Expected: all PASS.

- [ ] **Step 4: Run clippy on spur-tui**

Run:
```bash
cargo clippy -p spur-tui --tests -- -D warnings
```

Expected: no warnings. If clippy flags a new warning introduced by these changes, fix it; do not suppress with `#[allow]`.

- [ ] **Step 5: Manual smoke test (optional but recommended)**

Run `cargo run --bin spur` (or the TUI binary entry point). Press Ctrl+K. Verify:

1. The modal opens with section headers `COMMANDS`, `SESSIONS`, `WORKERS`, and a `TRACE — coming soon` placeholder.
2. Typing a session-id substring matches the correct session (F1 working).
3. With a session active: typing `/help` and pressing Enter closes the palette and shows the help overlay (C1a + C1b spur-local path).
4. With a session active and an agent that has dynamic commands: typing the name of a dynamic command and pressing Enter dispatches the expected send/vendor-exec.
5. Without a session: typing `/something-bogus` and seeing the `Slash commands need an active session.` hint.

---

## Spec Coverage Self-Check

| Spec section | Task(s) |
|---|---|
| C1a — Borrow live registry | Task 4 |
| C1b — `submit_router` routing + `&self` method | Task 5 |
| F1 — Subtitle fuzzy scoring | Task 2 |
| Tracing instrumentation | Task 1 |
| U1 — Empty grouped view | Task 7 |
| U2 — No-match hints | Task 6 |
| U3c — Skip Trace from `extend_raw` | Task 1 (Step 2) |
| U7 — Hints row content | Task 8 |
| Recency sort (SessionSource) | Task 3 |
| Invariant doc update | Task 2 (Step 4) |
| Zero new Action variants | Enforced by Task 5 implementation |

All spec requirements mapped. No placeholder steps.
