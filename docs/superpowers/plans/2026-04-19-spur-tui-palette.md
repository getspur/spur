# spur-tui Universal Palette — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a `Ctrl+K` universal palette in `crates/spur-tui` that fuzzy-searches across sessions, workers-in-lineage, commands, and the current-session trace — with `Enter` dispatching the correct `Action` for each result type.

**Architecture:** A new `components::palette` module owns a `PaletteOverlay` widget + `PaletteState` + a `PaletteSource` trait. Four built-in sources (`CommandSource`, `SessionSource`, `WorkerSource`, `TraceSource`) each produce `PaletteResult` rows. `PaletteState::rank` merges + nucleo-fuzzy-ranks results across all sources. The overlay is rendered by `App` with higher priority than views but lower than `QuitConfirm` / `HelpOverlay`. `Ctrl+K` is a new global binding in `App::handle_crossterm_event`.

**Tech Stack:** Rust, `ratatui`, `nucleo_matcher` (already a dep — see `crates/spur-tui/src/commands/fuzzy.rs`), `crossterm` key events. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-19-spur-tui-ux-best-approach.md` §4.2–§4.3 and §5.1.

**Out of scope (per spec §5.1):** scope-prefix filtering (`>` `#` `@` `$` `!` in query), cross-session trace indexing, clipboard-prefill double-press, mention search. These are Phase F1.5 / F2.

---

## File Structure

**New files:**

- `crates/spur-tui/src/components/palette.rs` — `PaletteState`, `PaletteResult`, `PaletteKind`, key dispatch, `Action` mapping.
- `crates/spur-tui/src/components/palette_overlay.rs` — `PaletteOverlay` ratatui widget (render + layout).
- `crates/spur-tui/src/components/palette_sources.rs` — `PaletteSource` trait + `CommandSource` / `SessionSource` / `WorkerSource` / `TraceSource` structs.
- `crates/spur-tui/tests/palette_sources.rs` — per-source unit tests.
- `crates/spur-tui/tests/palette_state.rs` — ranking/cursor tests.
- `crates/spur-tui/tests/palette_dispatch.rs` — end-to-end: key events → `Action` emission.

**Modified files:**

- `crates/spur-tui/src/components/mod.rs` — `pub mod palette; pub mod palette_overlay; pub mod palette_sources;`
- `crates/spur-tui/src/app.rs` — add `palette_visible: bool` + `palette_state: PaletteState` to `App`; wire `Ctrl+K` into the priority chain in `handle_crossterm_event`; render overlay in `App::render`; map `Action`s from palette into existing dispatch.
- `crates/spur-tui/src/components/status_bar.rs` — add `[Ctrl+K: go]` badge to the right-hand metrics cluster.

---

## Type Reference (committed signatures — used across tasks)

These are defined in Task 1 and referenced verbatim in later tasks. Do not rename.

```rust
// components/palette.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteKind {
    Command,  // badge: >
    Session,  // badge: $
    Worker,   // badge: !
    Trace,    // badge: #
}

#[derive(Debug, Clone)]
pub struct PaletteResult {
    pub kind: PaletteKind,
    /// Short label shown in the list. E.g. session title, command name.
    pub label: String,
    /// Right-aligned subtitle. E.g. "session · 2h ago", "cmd · toggle plan".
    pub subtitle: String,
    /// Opaque payload used by `Action` mapping. Never rendered.
    pub payload: PalettePayload,
}

#[derive(Debug, Clone)]
pub enum PalettePayload {
    Command { name: String },                         // dispatched via existing slash-command path
    Session { session_id: String },                   // → Action::ResumeSession
    Worker { session_id: spur_acp::SessionId },       // → Action::NavigateTo(SessionDetail(id))
    Trace { entry_idx: usize },                       // → scroll current session's trace to entry
}
```

```rust
// components/palette_sources.rs
pub trait PaletteSource {
    /// Return all candidate results from this source. Called once per palette
    /// open; ranking + filtering happens in `PaletteState::rank`.
    fn collect(&self) -> Vec<PaletteResult>;
}
```

```rust
// components/palette.rs
pub struct PaletteState {
    query: String,
    /// Raw, unranked results accumulated from all registered sources.
    raw: Vec<PaletteResult>,
    /// Ranked + filtered view; regenerated on every query change.
    ranked: Vec<PaletteResult>,
    cursor: usize,
}
```

---

## Task 1: Scaffold palette module + core types

**Files:**
- Create: `crates/spur-tui/src/components/palette.rs`
- Create: `crates/spur-tui/src/components/palette_overlay.rs` (stub)
- Create: `crates/spur-tui/src/components/palette_sources.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Test: `crates/spur-tui/tests/palette_state.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/palette_state.rs`:

```rust
use spur_tui::components::palette::{PaletteKind, PalettePayload, PaletteResult, PaletteState};

#[test]
fn empty_state_has_empty_query_and_no_cursor_movement() {
    let state = PaletteState::new();
    assert_eq!(state.query(), "");
    assert_eq!(state.ranked().len(), 0);
    assert_eq!(state.cursor(), 0);
}

#[test]
fn push_raw_accumulates_without_ranking() {
    let mut state = PaletteState::new();
    state.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/plan".into(),
            subtitle: "cmd · toggle plan mode".into(),
            payload: PalettePayload::Command { name: "/plan".into() },
        },
    ]);
    // With empty query, raw results pass through as ranked (input order preserved,
    // matching `commands::fuzzy::rank` semantics).
    assert_eq!(state.ranked().len(), 1);
    assert_eq!(state.ranked()[0].label, "/plan");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test palette_state`
Expected: FAIL with `unresolved import spur_tui::components::palette`.

- [ ] **Step 3: Write the module and core types**

Create `crates/spur-tui/src/components/palette.rs`:

```rust
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
```

Create `crates/spur-tui/src/components/palette_sources.rs`:

```rust
//! Palette data sources.
//!
//! Each source is a pure function of some view of app state (metadata store,
//! lineage, trace). Sources do not filter — ranking happens in `PaletteState`.

use crate::components::palette::PaletteResult;

pub trait PaletteSource {
    fn collect(&self) -> Vec<PaletteResult>;
}
```

Create `crates/spur-tui/src/components/palette_overlay.rs` as a stub (rendered later in Task 7):

```rust
//! Palette modal overlay widget. See Task 7 for full implementation.
```

Modify `crates/spur-tui/src/components/mod.rs` — add these three module declarations alongside the existing ones:

```rust
pub mod palette;
pub mod palette_overlay;
pub mod palette_sources;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test palette_state`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/palette.rs \
        crates/spur-tui/src/components/palette_overlay.rs \
        crates/spur-tui/src/components/palette_sources.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/tests/palette_state.rs
git commit -m "feat(spur-tui): scaffold palette module with PaletteState and source trait"
```

---

## Task 2: CommandSource (wraps existing commands::registry)

**Files:**
- Modify: `crates/spur-tui/src/components/palette_sources.rs`
- Test: `crates/spur-tui/tests/palette_sources.rs`

Context: `crates/spur-tui/src/commands/registry.rs` already exposes command entries. Read that file first to find the registry-listing API (likely `CommandRegistry::entries(&self) -> &[CommandEntry]` or similar). Use the real method name in the implementation below.

- [ ] **Step 1: Read the registry API**

Run: `rg -n "impl CommandRegistry|pub fn " crates/spur-tui/src/commands/registry.rs`
Note the method that returns/enumerates entries. (Expected: a method like `entries()` or `all()` returning `&[CommandEntry]` — adjust the `map` call in Step 3 if the method name differs.)

- [ ] **Step 2: Write the failing test**

Create `crates/spur-tui/tests/palette_sources.rs`:

```rust
use spur_tui::commands::registry::CommandRegistry;
use spur_tui::components::palette::{PaletteKind, PalettePayload};
use spur_tui::components::palette_sources::{CommandSource, PaletteSource};

#[test]
fn command_source_yields_all_registered_commands_as_command_kind() {
    let registry = CommandRegistry::default();
    let src = CommandSource::new(&registry);
    let results = src.collect();

    assert!(!results.is_empty(), "default registry should contain builtin commands");
    assert!(results.iter().all(|r| r.kind == PaletteKind::Command));

    // Every result has a command payload with a non-empty name.
    for r in &results {
        match &r.payload {
            PalettePayload::Command { name } => assert!(!name.is_empty()),
            _ => panic!("expected Command payload, got {:?}", r.payload),
        }
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: FAIL with `unresolved import: CommandSource`.

- [ ] **Step 4: Implement `CommandSource`**

Append to `crates/spur-tui/src/components/palette_sources.rs`:

```rust
use crate::commands::registry::CommandRegistry;
use crate::components::palette::{PaletteKind, PalettePayload, PaletteResult};

pub struct CommandSource<'a> {
    registry: &'a CommandRegistry,
}

impl<'a> CommandSource<'a> {
    pub fn new(registry: &'a CommandRegistry) -> Self {
        Self { registry }
    }
}

impl<'a> PaletteSource for CommandSource<'a> {
    fn collect(&self) -> Vec<PaletteResult> {
        // NOTE: adjust `entries()` if the real method name on CommandRegistry differs
        // (discovered in Step 1). It must return a slice/iterator of CommandEntry-like
        // items with `.name: String` and `.description: String` (or equivalent).
        self.registry
            .entries()
            .iter()
            .map(|e| PaletteResult {
                kind: PaletteKind::Command,
                label: e.name.clone(),
                subtitle: format!("cmd · {}", e.description),
                payload: PalettePayload::Command { name: e.name.clone() },
            })
            .collect()
    }
}
```

If Step 1 revealed a different method name (e.g. `all()` or `iter()`), substitute it for `entries()` above. If the registry doesn't have a listing method at all, add one: `pub fn entries(&self) -> &[CommandEntry] { &self.entries }` (most registries store a `Vec<CommandEntry>` field).

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/palette_sources.rs \
        crates/spur-tui/tests/palette_sources.rs
git commit -m "feat(spur-tui): palette CommandSource wraps commands::registry"
```

---

## Task 3: SessionSource (reads SessionMetadataStore)

**Files:**
- Modify: `crates/spur-tui/src/components/palette_sources.rs`
- Modify: `crates/spur-tui/tests/palette_sources.rs`

Context: `App::metadata_store: SessionMetadataStore` (field at `app.rs:165`). The store exposes `metadata()` returning a struct that contains a `BTreeMap<String, SessionEntry>` of sessions. Read `crates/spur-tui/src/session_metadata.rs` fully (it's ~40 lines visible, plus the rest of the file) to confirm the iteration API.

- [ ] **Step 1: Confirm the metadata API**

Run: `rg -n "impl SessionMetadataStore|pub fn metadata|sessions:" crates/spur-tui/src/session_metadata.rs`
Expected fields: a top-level `SessionMetadata` with a `sessions: BTreeMap<String, SessionEntry>` (or similar). Note the exact name — used in Step 3.

- [ ] **Step 2: Write the failing test**

Append to `crates/spur-tui/tests/palette_sources.rs`:

```rust
use spur_tui::components::palette_sources::SessionSource;
use spur_tui::session_metadata::{SessionEntry, SessionMetadata};

#[test]
fn session_source_yields_session_kind_rows_with_title_as_label() {
    let mut meta = SessionMetadata::default();
    // NOTE: adjust the field name `sessions` to match session_metadata.rs.
    meta.sessions.insert(
        "sess-1".to_string(),
        SessionEntry {
            title_override: Some("refactor-auth".to_string()),
            ..Default::default()
        },
    );
    meta.sessions.insert(
        "sess-2".to_string(),
        SessionEntry {
            title_override: None, // falls back to session_id as label
            ..Default::default()
        },
    );

    let src = SessionSource::from_metadata(&meta);
    let results = src.collect();
    assert_eq!(results.len(), 2);

    let labels: Vec<&str> = results.iter().map(|r| r.label.as_str()).collect();
    assert!(labels.contains(&"refactor-auth"));
    assert!(labels.contains(&"sess-2"));

    for r in &results {
        assert!(matches!(r.kind, spur_tui::components::palette::PaletteKind::Session));
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: FAIL with `unresolved import: SessionSource`.

- [ ] **Step 4: Implement `SessionSource`**

Append to `crates/spur-tui/src/components/palette_sources.rs`:

```rust
use crate::session_metadata::SessionMetadata;

pub struct SessionSource {
    /// Snapshot taken at palette-open time. Owned to avoid lifetime gymnastics.
    entries: Vec<(String, String)>, // (session_id, display_label)
}

impl SessionSource {
    pub fn from_metadata(meta: &SessionMetadata) -> Self {
        // NOTE: adjust `&meta.sessions` if the field name differs.
        let entries = meta
            .sessions
            .iter()
            .map(|(id, entry)| {
                let label = entry
                    .title_override
                    .clone()
                    .unwrap_or_else(|| id.clone());
                (id.clone(), label)
            })
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

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: PASS (both command-source and session-source tests).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/palette_sources.rs \
        crates/spur-tui/tests/palette_sources.rs
git commit -m "feat(spur-tui): palette SessionSource reads SessionMetadataStore"
```

---

## Task 4: WorkerSource (reads ExecutorLineage)

**Files:**
- Modify: `crates/spur-tui/src/components/palette_sources.rs`
- Modify: `crates/spur-tui/tests/palette_sources.rs`

Context: `App::lineage: ExecutorLineage` (field at `app.rs:154`). `agents_tree.rs:render_lineage_to_strings` proves a walk API exists. The type is `spur_core::lineage::projection::ExecutorLineage` — read its public surface.

- [ ] **Step 1: Confirm the lineage iteration API**

Run: `rg -n "impl ExecutorLineage|pub fn nodes|pub fn iter" crates/spur-core/src/lineage/projection.rs`
Expected: a method like `nodes()` returning an iterator over `ExecutorNode` (each with `id`, `agent_name`, `session_id` or `acp_session_id`, `state`). Note the exact names.

- [ ] **Step 2: Write the failing test**

Append to `crates/spur-tui/tests/palette_sources.rs`:

```rust
use spur_core::lineage::projection::ExecutorLineage;
use spur_tui::components::palette_sources::WorkerSource;

#[test]
fn worker_source_yields_worker_kind_rows_for_each_executor_node() {
    // A fresh lineage has no executors — source should yield nothing.
    let lineage = ExecutorLineage::new();
    let src = WorkerSource::from_lineage(&lineage);
    assert_eq!(src.collect().len(), 0);

    // TODO test with populated lineage is covered by the integration test
    // in Task 9 (we avoid constructing synthetic lineage state here — the
    // projection API is private to spur-core).
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: FAIL with `unresolved import: WorkerSource`.

- [ ] **Step 4: Implement `WorkerSource`**

Append to `crates/spur-tui/src/components/palette_sources.rs`:

```rust
use spur_core::lineage::projection::ExecutorLineage;

pub struct WorkerSource {
    entries: Vec<(spur_acp::SessionId, String, String)>, // (session_id, agent_name, state_label)
}

impl WorkerSource {
    pub fn from_lineage(lineage: &ExecutorLineage) -> Self {
        // NOTE: adjust `.nodes()` to match projection.rs (Step 1).
        // Expected node fields: .acp_session_id -> Option<SessionId>,
        // .agent_name -> String, .state -> LifecycleState (has Debug or a label method).
        let entries = lineage
            .nodes()
            .filter_map(|n| {
                let sid = n.acp_session_id.clone()?;
                Some((sid, n.agent_name.clone(), format!("{:?}", n.state).to_lowercase()))
            })
            .collect();
        Self { entries }
    }
}

impl PaletteSource for WorkerSource {
    fn collect(&self) -> Vec<PaletteResult> {
        self.entries
            .iter()
            .map(|(sid, name, state)| PaletteResult {
                kind: PaletteKind::Worker,
                label: name.clone(),
                subtitle: format!("worker · {}", state),
                payload: PalettePayload::Worker { session_id: sid.clone() },
            })
            .collect()
    }
}
```

If `nodes()` is named differently (e.g. `iter_nodes()`), substitute. If nodes don't expose `acp_session_id` directly, use whichever field maps an executor to its session.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/palette_sources.rs \
        crates/spur-tui/tests/palette_sources.rs
git commit -m "feat(spur-tui): palette WorkerSource reads ExecutorLineage"
```

---

## Task 5: TraceSource (current-session trace scan)

**Files:**
- Modify: `crates/spur-tui/src/components/palette_sources.rs`
- Modify: `crates/spur-tui/tests/palette_sources.rs`

Context: `SessionDetailView` owns a `ReactTrace`. The trace stores entries of different kinds — some contain text (message, think), some don't (delegate). `TraceSource` scans entries that have text content and produces one `PaletteResult` per entry with a short preview.

- [ ] **Step 1: Confirm the trace iteration API**

Run: `rg -n "impl ReactTrace|pub fn entries|pub fn iter|TraceKind" crates/spur-tui/src/components/react_trace/mod.rs`
Identify: the public method to iterate entries, the `TraceKind` variants that contain text (likely `Message`, `Think`, `Observe`), and the fields holding that text.

- [ ] **Step 2: Write the failing test**

Append to `crates/spur-tui/tests/palette_sources.rs`:

```rust
use spur_tui::components::palette_sources::TraceSource;

#[test]
fn trace_source_handles_empty_trace() {
    let src = TraceSource::from_empty();
    assert_eq!(src.collect().len(), 0);
}
```

Full integration coverage (populated trace) comes in Task 9's end-to-end test, which constructs a real `SessionDetailView` via `test_support::new_session_state`.

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: FAIL with `unresolved import: TraceSource`.

- [ ] **Step 4: Implement `TraceSource`**

Append to `crates/spur-tui/src/components/palette_sources.rs`:

```rust
use crate::components::react_trace::ReactTrace;

pub struct TraceSource {
    /// Snapshot: (entry_idx, preview_text). Preview is truncated to 80 chars.
    entries: Vec<(usize, String)>,
}

impl TraceSource {
    pub fn from_trace(trace: &ReactTrace) -> Self {
        // NOTE: adjust `.entries()` / the `TraceKind` match-arm field names
        // based on Step 1. Only entries with user-visible text are indexed.
        let entries = trace
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                let text = preview_text(entry)?;
                Some((idx, truncate(text, 80)))
            })
            .collect();
        Self { entries }
    }

    /// Empty-trace constructor for smoke tests without a full view.
    pub fn from_empty() -> Self {
        Self { entries: Vec::new() }
    }
}

impl PaletteSource for TraceSource {
    fn collect(&self) -> Vec<PaletteResult> {
        self.entries
            .iter()
            .map(|(idx, preview)| PaletteResult {
                kind: PaletteKind::Trace,
                label: preview.clone(),
                subtitle: format!("trace · entry #{}", idx),
                payload: PalettePayload::Trace { entry_idx: *idx },
            })
            .collect()
    }
}

/// Extract preview text from a trace entry. Return None for entries that don't
/// carry user-readable text (e.g. `Delegate`, `FenceRef`-only rows).
fn preview_text(entry: &crate::components::react_trace::TraceEntry) -> Option<String> {
    // NOTE: adjust the match arms to match TraceKind variants discovered in Step 1.
    // The goal: return the text body for `Message { text }`, `Think { text }`,
    // `Observe { text }`. Return None for `Delegate { .. }` etc.
    use crate::components::react_trace::TraceKind;
    match &entry.kind {
        TraceKind::Message { text } => Some(text.clone()),
        TraceKind::Think { text } => Some(text.clone()),
        TraceKind::Observe { text } => Some(text.clone()),
        _ => None,
    }
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max { s }
    else {
        let taken: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", taken)
    }
}
```

If `TraceEntry`/`TraceKind` field names differ from `Message { text }`, adjust. The intent is: keep textual entries, drop structural-only entries.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-tui --test palette_sources`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/palette_sources.rs \
        crates/spur-tui/tests/palette_sources.rs
git commit -m "feat(spur-tui): palette TraceSource scans current-session entries"
```

---

## Task 6: PaletteState — multi-source rank + cursor navigation

**Files:**
- Modify: `crates/spur-tui/src/components/palette.rs`
- Modify: `crates/spur-tui/tests/palette_state.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-tui/tests/palette_state.rs`:

```rust
fn mk(kind: PaletteKind, label: &str) -> PaletteResult {
    PaletteResult {
        kind,
        label: label.into(),
        subtitle: String::new(),
        payload: match kind {
            PaletteKind::Command => PalettePayload::Command { name: label.into() },
            PaletteKind::Session => PalettePayload::Session { session_id: label.into() },
            PaletteKind::Worker => PalettePayload::Worker {
                session_id: spur_acp::SessionId(label.into()),
            },
            PaletteKind::Trace => PalettePayload::Trace { entry_idx: 0 },
        },
    }
}

#[test]
fn set_query_reranks_by_fuzzy_score_and_drops_unmatched() {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        mk(PaletteKind::Session, "refactor-auth"),
        mk(PaletteKind::Session, "debug-ci-flake"),
        mk(PaletteKind::Worker, "refactor-auth-async"),
    ]);
    s.set_query("refac");
    let labels: Vec<&str> = s.ranked().iter().map(|r| r.label.as_str()).collect();
    assert!(labels.contains(&"refactor-auth"));
    assert!(labels.contains(&"refactor-auth-async"));
    assert!(!labels.contains(&"debug-ci-flake"));
}

#[test]
fn cursor_up_down_stay_in_bounds_and_wrap_disabled() {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        mk(PaletteKind::Command, "/a"),
        mk(PaletteKind::Command, "/b"),
        mk(PaletteKind::Command, "/c"),
    ]);
    assert_eq!(s.cursor(), 0);
    s.cursor_down(); assert_eq!(s.cursor(), 1);
    s.cursor_down(); assert_eq!(s.cursor(), 2);
    s.cursor_down(); assert_eq!(s.cursor(), 2); // clamped, no wrap
    s.cursor_up();   assert_eq!(s.cursor(), 1);
    s.cursor_up();   s.cursor_up(); assert_eq!(s.cursor(), 0); // clamped at 0
}

#[test]
fn selected_returns_current_cursor_row() {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        mk(PaletteKind::Command, "/alpha"),
        mk(PaletteKind::Command, "/beta"),
    ]);
    s.cursor_down();
    assert_eq!(s.selected().unwrap().label, "/beta");
}

#[test]
fn selected_returns_none_when_ranked_is_empty() {
    let s = PaletteState::new();
    assert!(s.selected().is_none());
}

#[test]
fn reset_clears_query_and_raw_but_not_state_struct() {
    let mut s = PaletteState::new();
    s.push_raw(vec![mk(PaletteKind::Command, "/x")]);
    s.set_query("x");
    s.reset();
    assert_eq!(s.query(), "");
    assert_eq!(s.ranked().len(), 0);
    assert_eq!(s.cursor(), 0);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test palette_state`
Expected: FAIL with `no method named set_query/cursor_down/cursor_up/selected/reset`.

- [ ] **Step 3: Extend `PaletteState`**

Append/replace methods in `crates/spur-tui/src/components/palette.rs` inside `impl PaletteState`:

```rust
impl PaletteState {
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

    pub fn cursor_down(&mut self) {
        if self.ranked.is_empty() { return; }
        self.cursor = (self.cursor + 1).min(self.ranked.len() - 1);
    }

    pub fn cursor_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn selected(&self) -> Option<&PaletteResult> {
        self.ranked.get(self.cursor)
    }

    pub fn reset(&mut self) {
        self.query.clear();
        self.raw.clear();
        self.ranked.clear();
        self.cursor = 0;
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test palette_state`
Expected: PASS (all 7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/palette.rs \
        crates/spur-tui/tests/palette_state.rs
git commit -m "feat(spur-tui): palette state supports query/cursor/reset with fuzzy rerank"
```

---

## Task 7: PaletteOverlay widget (render)

**Files:**
- Modify: `crates/spur-tui/src/components/palette_overlay.rs`
- Test: `crates/spur-tui/tests/palette_render.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/palette_render.rs`:

```rust
use ratatui::{backend::TestBackend, Terminal, layout::Rect};
use spur_tui::components::palette::{PaletteKind, PalettePayload, PaletteResult, PaletteState};
use spur_tui::components::palette_overlay::PaletteOverlay;

fn render_to_string(state: &PaletteState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        let area = Rect { x: 0, y: 0, width, height };
        let overlay = PaletteOverlay::new(state);
        f.render_widget(overlay, area);
    }).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn overlay_renders_title_query_and_rows() {
    let mut state = PaletteState::new();
    state.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Session,
            label: "refactor-auth".into(),
            subtitle: "session · 2h ago".into(),
            payload: PalettePayload::Session { session_id: "s1".into() },
        },
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/plan".into(),
            subtitle: "cmd · toggle plan".into(),
            payload: PalettePayload::Command { name: "/plan".into() },
        },
    ]);
    let rendered = render_to_string(&state, 60, 12);
    assert!(rendered.contains("Go to"), "title missing: {rendered}");
    assert!(rendered.contains("refactor-auth"), "session row missing");
    assert!(rendered.contains("/plan"), "command row missing");
    assert!(rendered.contains("$"), "session badge missing");
    assert!(rendered.contains(">"), "command badge missing");
}

#[test]
fn overlay_renders_empty_state_placeholder() {
    let state = PaletteState::new();
    let rendered = render_to_string(&state, 60, 12);
    assert!(rendered.contains("Go to"));
    assert!(rendered.contains("type to filter") || rendered.contains("no matches"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test palette_render`
Expected: FAIL with `unresolved import: PaletteOverlay`.

- [ ] **Step 3: Implement `PaletteOverlay`**

Replace the stub in `crates/spur-tui/src/components/palette_overlay.rs`:

```rust
//! Palette modal overlay — ratatui widget.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget},
};

use crate::components::palette::{PaletteKind, PaletteState};

pub struct PaletteOverlay<'a> {
    state: &'a PaletteState,
}

impl<'a> PaletteOverlay<'a> {
    pub fn new(state: &'a PaletteState) -> Self {
        Self { state }
    }
}

fn badge_for(kind: &PaletteKind) -> &'static str {
    match kind {
        PaletteKind::Command => ">",
        PaletteKind::Session => "$",
        PaletteKind::Worker => "!",
        PaletteKind::Trace => "#",
    }
}

fn modal_rect(outer: Rect) -> Rect {
    // Centered modal: 60% width, 60% height, min 40x8.
    let w = (outer.width as u32 * 6 / 10).max(40) as u16;
    let h = (outer.height as u32 * 6 / 10).max(8) as u16;
    let x = outer.x + (outer.width.saturating_sub(w)) / 2;
    let y = outer.y + (outer.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w.min(outer.width), height: h.min(outer.height) }
}

impl<'a> Widget for PaletteOverlay<'a> {
    fn render(self, outer: Rect, buf: &mut Buffer) {
        let area = modal_rect(outer);
        Clear.render(area, buf); // blank the modal area

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Go to…  (Ctrl+K) ")
            .title_alignment(Alignment::Left);
        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 3 || inner.width < 10 { return; }

        // Layout: row 0 = query; row 1 = blank; rows 2..=h-2 = results; last row = hints.
        let query_area = Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 };
        let hints_area = Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        };
        let list_area = Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: inner.height.saturating_sub(3),
        };

        // Query line: "> refac▮"
        let query_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::DarkGray)),
            Span::raw(self.state.query()),
            Span::styled("▮", Style::default().fg(Color::Gray)),
        ]);
        Paragraph::new(query_line).render(query_area, buf);

        // Results or empty-state placeholder.
        if self.state.ranked().is_empty() {
            let msg = if self.state.query().is_empty() {
                "type to filter"
            } else {
                "no matches"
            };
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )))
            .render(list_area, buf);
        } else {
            let items: Vec<ListItem> = self
                .state
                .ranked()
                .iter()
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
            List::new(items).render(list_area, buf);
        }

        // Hint line.
        let hint = Line::from(Span::styled(
            "↑↓ select · ↵ go · esc close · type to filter",
            Style::default().fg(Color::DarkGray),
        ));
        Paragraph::new(hint).render(hints_area, buf);
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test palette_render`
Expected: PASS (both render tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/palette_overlay.rs \
        crates/spur-tui/tests/palette_render.rs
git commit -m "feat(spur-tui): PaletteOverlay widget renders modal with badges and cursor"
```

---

## Task 8: Palette key handling → `Option<PaletteIntent>`

**Files:**
- Modify: `crates/spur-tui/src/components/palette.rs`
- Test: `crates/spur-tui/tests/palette_dispatch.rs`

This task produces a pure `handle_key` that maps `KeyEvent` → state mutation + optional `PaletteIntent` (the thing the selected result wants to do). Wiring to `Action` happens in Task 9.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/palette_dispatch.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::palette::{
    PaletteIntent, PaletteKind, PalettePayload, PaletteResult, PaletteState,
};

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn seed_state() -> PaletteState {
    let mut s = PaletteState::new();
    s.push_raw(vec![
        PaletteResult {
            kind: PaletteKind::Session,
            label: "refactor-auth".into(),
            subtitle: "".into(),
            payload: PalettePayload::Session { session_id: "s1".into() },
        },
        PaletteResult {
            kind: PaletteKind::Command,
            label: "/plan".into(),
            subtitle: "".into(),
            payload: PalettePayload::Command { name: "/plan".into() },
        },
    ]);
    s
}

#[test]
fn char_key_appends_to_query() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Char('r')));
    assert!(matches!(i, None));
    assert_eq!(s.query(), "r");
}

#[test]
fn backspace_pops_char() {
    let mut s = seed_state();
    s.set_query("refa");
    let i = s.handle_key(key(KeyCode::Backspace));
    assert!(matches!(i, None));
    assert_eq!(s.query(), "ref");
}

#[test]
fn down_moves_cursor_and_emits_no_intent() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Down));
    assert!(matches!(i, None));
    assert_eq!(s.cursor(), 1);
}

#[test]
fn enter_emits_accept_intent_with_selected_payload() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Enter));
    match i {
        Some(PaletteIntent::Accept(res)) => {
            assert_eq!(res.label, "refactor-auth");
        }
        other => panic!("expected Accept, got {:?}", other),
    }
}

#[test]
fn enter_with_empty_ranked_emits_no_intent() {
    let mut s = PaletteState::new();
    let i = s.handle_key(key(KeyCode::Enter));
    assert!(matches!(i, None));
}

#[test]
fn esc_emits_dismiss_intent() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Esc));
    assert!(matches!(i, Some(PaletteIntent::Dismiss)));
}

#[test]
fn tab_is_same_as_enter() {
    let mut s = seed_state();
    let i = s.handle_key(key(KeyCode::Tab));
    assert!(matches!(i, Some(PaletteIntent::Accept(_))));
}

#[test]
fn ctrl_c_dismisses() {
    let mut s = seed_state();
    let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let i = s.handle_key(ev);
    assert!(matches!(i, Some(PaletteIntent::Dismiss)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test palette_dispatch`
Expected: FAIL with `unresolved import: PaletteIntent` / `no method named handle_key`.

- [ ] **Step 3: Add `PaletteIntent` and `handle_key`**

Append to `crates/spur-tui/src/components/palette.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug)]
pub enum PaletteIntent {
    Accept(PaletteResult),
    Dismiss,
}

impl PaletteState {
    /// Dispatch one key. Returns `Some(intent)` when the overlay should take
    /// a higher-level action (accept or dismiss); `None` means state was
    /// mutated but the overlay stays open.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Option<PaletteIntent> {
        // Ctrl+C always dismisses.
        if ev.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(ev.code, KeyCode::Char('c'))
        {
            return Some(PaletteIntent::Dismiss);
        }

        match ev.code {
            KeyCode::Esc => Some(PaletteIntent::Dismiss),
            KeyCode::Enter | KeyCode::Tab => {
                self.selected().cloned().map(PaletteIntent::Accept)
            }
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test palette_dispatch`
Expected: PASS (all 8 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/palette.rs \
        crates/spur-tui/tests/palette_dispatch.rs
git commit -m "feat(spur-tui): palette key handling emits Accept/Dismiss intents"
```

---

## Task 9: Wire palette into `App` — global `Ctrl+K`, render, dispatch

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Test: `crates/spur-tui/tests/palette_integration.rs`

This is the integration task. It wires the palette into the app's priority chain, opens/closes it, and maps `PaletteIntent::Accept` onto existing `Action`s.

- [ ] **Step 1: Write the failing integration test**

Create `crates/spur-tui/tests/palette_integration.rs`:

```rust
use crossterm::event::{Event as CtEvent, KeyCode, KeyEvent, KeyModifiers};
use spur_tui::test_support::new_app;

fn key(c: KeyCode, m: KeyModifiers) -> CtEvent {
    CtEvent::Key(KeyEvent::new(c, m))
}

#[test]
fn ctrl_k_opens_palette_and_esc_closes() {
    let mut app = new_app();
    assert!(!app.is_palette_visible());

    app.handle_crossterm_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert!(app.is_palette_visible(), "Ctrl+K should open palette");

    app.handle_crossterm_event(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.is_palette_visible(), "Esc should close palette");
}

#[test]
fn ctrl_k_with_help_visible_is_swallowed_by_help() {
    let mut app = new_app();
    // Simulate opening help (via `?`).
    app.handle_crossterm_event(key(KeyCode::Char('?'), KeyModifiers::NONE));
    // Ctrl+K while help is up must NOT open the palette — help swallows keys.
    app.handle_crossterm_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert!(!app.is_palette_visible());
}

#[test]
fn palette_session_accept_emits_resume_session_action() {
    let mut app = new_app();
    app.handle_crossterm_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    // Inject a fake session result directly via test hook.
    app.seed_palette_with_session_for_test("s1", "refactor-auth");
    // Enter accepts.
    app.handle_crossterm_event(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.is_palette_visible(), "palette should close after accept");
    let last = app.last_action_for_test().expect("an Action should have been dispatched");
    match last {
        spur_tui::action::Action::ResumeSession { session_id } => {
            assert_eq!(session_id, "s1");
        }
        other => panic!("expected ResumeSession, got {:?}", other),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --test palette_integration`
Expected: FAIL with `no method named is_palette_visible` etc.

- [ ] **Step 3: Add palette state to `App`**

In `crates/spur-tui/src/app.rs`, inside `pub struct App { ... }` (near `help_visible: bool`), add:

```rust
    palette_visible: bool,
    palette_state: crate::components::palette::PaletteState,
    /// Last dispatched Action, for integration tests only.
    #[cfg(any(test, debug_assertions))]
    last_action: Option<crate::action::Action>,
```

In `build_with_license_state` (around `app.rs:240`), inside the `Self { ... }` literal, add:

```rust
            palette_visible: false,
            palette_state: crate::components::palette::PaletteState::new(),
            #[cfg(any(test, debug_assertions))]
            last_action: None,
```

- [ ] **Step 4: Wire Ctrl+K into the priority chain**

In `App::handle_crossterm_event` (starts at `app.rs:355`), extend the priority chain. The existing chain is:
1. QuitConfirm (lines ~358-375)
2. HelpOverlay (lines ~377-387)
3. MermaidOverlay (lines ~407-427)
4. View-level

Insert a NEW priority layer between HelpOverlay and MermaidOverlay for the palette. Add imports near the top of `app.rs`:

```rust
use crate::components::palette::{PaletteIntent, PaletteState};
use crate::components::palette_sources::{
    CommandSource, PaletteSource, SessionSource, TraceSource, WorkerSource,
};
```

Inside `handle_crossterm_event`, after the help-overlay block and before the mermaid-overlay block, add:

```rust
        // Priority 2.5 — palette overlay.
        if self.palette_visible {
            if let CtEvent::Key(ev) = event {
                match self.palette_state.handle_key(ev) {
                    Some(PaletteIntent::Dismiss) => {
                        self.palette_visible = false;
                        self.palette_state.reset();
                        self.dirty = true;
                    }
                    Some(PaletteIntent::Accept(result)) => {
                        self.palette_visible = false;
                        self.palette_state.reset();
                        if let Some(action) = result_to_action(result) {
                            self.process_action(action);
                        }
                        self.dirty = true;
                    }
                    None => { self.dirty = true; }
                }
            }
            return;
        }

        // Global Ctrl+K opens palette (checked only when no higher-priority
        // overlay is up — QuitConfirm and HelpOverlay already returned above).
        if let CtEvent::Key(ev) = &event {
            if ev.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(ev.code, KeyCode::Char('k'))
            {
                self.open_palette();
                return;
            }
        }
```

Add the helper below (inside `impl App`):

```rust
    fn open_palette(&mut self) {
        self.palette_state.reset();

        // Load sources. Order here is the tie-break order when fuzzy scores equal.
        // Commands first (stable across sessions), then Sessions, Workers, Trace.
        let cmd_registry = self.dashboard.command_registry(); // Task 2 may need to add this accessor
        let cmd_src = CommandSource::new(cmd_registry);
        self.palette_state.push_raw(cmd_src.collect());

        let sess_src = SessionSource::from_metadata(self.metadata_store.metadata());
        self.palette_state.push_raw(sess_src.collect());

        let worker_src = WorkerSource::from_lineage(&self.lineage);
        self.palette_state.push_raw(worker_src.collect());

        if let Some(view) = self.session_detail.as_ref() {
            let trace_src = TraceSource::from_trace(view.trace_for_palette()); // accessor to add
            self.palette_state.push_raw(trace_src.collect());
        }

        self.palette_visible = true;
        self.dirty = true;
    }

    #[cfg(any(test, debug_assertions))]
    pub fn is_palette_visible(&self) -> bool {
        self.palette_visible
    }

    #[cfg(any(test, debug_assertions))]
    pub fn seed_palette_with_session_for_test(&mut self, session_id: &str, label: &str) {
        use crate::components::palette::{PaletteKind, PalettePayload, PaletteResult};
        self.palette_state.push_raw(vec![PaletteResult {
            kind: PaletteKind::Session,
            label: label.to_string(),
            subtitle: format!("session · {}", session_id),
            payload: PalettePayload::Session { session_id: session_id.to_string() },
        }]);
    }

    #[cfg(any(test, debug_assertions))]
    pub fn last_action_for_test(&self) -> Option<crate::action::Action> {
        self.last_action.clone()
    }
```

In `process_action` (search for `fn process_action`), at the TOP of the function, capture for tests:

```rust
        #[cfg(any(test, debug_assertions))]
        {
            self.last_action = Some(action.clone());
        }
```

Add `result_to_action` as a free function at the bottom of `app.rs`:

```rust
fn result_to_action(
    result: crate::components::palette::PaletteResult,
) -> Option<crate::action::Action> {
    use crate::action::{Action, ViewId};
    use crate::components::palette::PalettePayload;
    match result.payload {
        PalettePayload::Session { session_id } => {
            Some(Action::ResumeSession { session_id })
        }
        PalettePayload::Worker { session_id } => {
            Some(Action::NavigateTo(ViewId::SessionDetail(session_id)))
        }
        PalettePayload::Command { name: _ } => {
            // Commands are dispatched via the existing slash-command path.
            // For MVP, palette accepts emit no direct Action; the user types the
            // command into the input bar. Phase F1.5 will wire a direct dispatch.
            None
        }
        PalettePayload::Trace { entry_idx: _ } => {
            // Scrolling current-session trace to an entry is not yet an Action
            // variant. Phase F1.5: add `Action::ScrollToTraceEntry(usize)` and
            // wire it here. For MVP, accept is a no-op (palette closes, trace
            // stays at anchor).
            None
        }
    }
}
```

Finally, render the overlay. In `App::render` (search for `pub fn render(&mut self, f: &mut Frame)` or similar), AFTER all view rendering and AFTER help/quit overlays, add:

```rust
        if self.palette_visible {
            let overlay = crate::components::palette_overlay::PaletteOverlay::new(
                &self.palette_state,
            );
            f.render_widget(overlay, f.area());
        }
```

- [ ] **Step 5: Add the two needed accessors**

The code above references `self.dashboard.command_registry()` and `view.trace_for_palette()`. Add them:

In `crates/spur-tui/src/views/dashboard.rs`, inside `impl DashboardView`:

```rust
    /// Borrow the command registry used by this view's input bar. Used by
    /// the palette open path in `App::open_palette`.
    pub fn command_registry(&self) -> &crate::commands::registry::CommandRegistry {
        &self.command_registry // adjust to match the actual field name
    }
```

(If dashboard does not own a registry — check with `rg -n "CommandRegistry" crates/spur-tui/src/views/dashboard.rs`. If SessionDetailView owns it instead, move the accessor there and update `open_palette` to read from whichever view owns the registry. Worst case, construct `CommandRegistry::default()` at the palette-open site.)

In `crates/spur-tui/src/views/session_detail.rs`, inside `impl SessionDetailView`:

```rust
    /// Borrow the trace for palette indexing. Read-only snapshot-in-time.
    pub fn trace_for_palette(&self) -> &crate::components::react_trace::ReactTrace {
        &self.trace // adjust to match the actual field name
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p spur-tui --test palette_integration`
Expected: PASS (all 3 integration tests). Also run the full test suite to catch regressions:
`cargo test -p spur-tui`
Expected: PASS (all existing tests still green).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/tests/palette_integration.rs
git commit -m "feat(spur-tui): wire Ctrl+K palette into App priority chain + dispatch"
```

---

## Task 10: Status-bar `[Ctrl+K: go]` badge

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs`
- Test: `crates/spur-tui/tests/status_bar_palette_badge.rs`

Reference: `StatusBarProps` is defined in `crates/spur-tui/src/components/status_bar.rs:67-85` and has these fields:

```rust
pub struct StatusBarProps<'a> {
    pub view: &'a ViewId,
    pub running: usize,
    pub pending_review: usize,
    pub total_cost: f64,
    pub elapsed: &'a str,
    pub current_mode: Option<&'a str>,
    pub context_used: Option<u64>,
    pub context_size: Option<u64>,
    pub stream_in_flight: bool,
    pub issue_count: usize,
    pub alert_summary: Option<(usize, usize, usize)>,
    pub license_badge: Option<&'a LicenseBadge>,
}
```

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/status_bar_palette_badge.rs`:

```rust
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use spur_tui::action::ViewId;
use spur_tui::components::status_bar::{StatusBar, StatusBarProps};

fn render_status(width: u16) -> String {
    let backend = TestBackend::new(width, 1);
    let mut term = Terminal::new(backend).unwrap();
    let view = ViewId::Dashboard;
    term.draw(|f| {
        let area = Rect { x: 0, y: 0, width, height: 1 };
        let props = StatusBarProps {
            view: &view,
            running: 0,
            pending_review: 0,
            total_cost: 0.0,
            elapsed: "",
            current_mode: None,
            context_used: None,
            context_size: None,
            stream_in_flight: false,
            issue_count: 0,
            alert_summary: None,
            license_badge: None,
        };
        StatusBar::render(f, area, props);
    }).unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.width).map(|x| buf[(x, 0)].symbol()).collect::<String>()
}

#[test]
fn status_bar_shows_ctrl_k_go_badge() {
    let line = render_status(120);
    assert!(line.contains("Ctrl+K"), "status bar missing Ctrl+K badge: {line}");
    assert!(line.contains("go"), "badge missing 'go' label: {line}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test status_bar_palette_badge`
Expected: FAIL — asserts can't find `"Ctrl+K"` in the rendered line.

- [ ] **Step 3: Add the badge to `StatusBar::render`**

Open `crates/spur-tui/src/components/status_bar.rs` and locate the right-side metrics cluster (lines 122–171 per the audit). Just BEFORE the existing `? help` badge (around line ~165), add a new span to the right-side spans vector. Follow the exact push/extend pattern the surrounding code uses — if the existing code does `spans.push(Span::styled("? help", ...))`, do the same with:

```rust
        spans.push(Span::styled(
            " [Ctrl+K: go] ",
            Style::default().fg(Color::DarkGray),
        ));
```

The visual goal: permanent, dim-gray, rendered immediately left of `? help` on every view.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-tui --test status_bar_palette_badge`
Expected: PASS.

- [ ] **Step 5: Visual smoke check**

Run: `cargo run -p spur-tui --example react_trace_bench_sim` (or any existing example that boots the TUI) and visually confirm the badge renders in the status bar. Resize terminal narrower to ensure the badge doesn't break layout at 80 cols.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs \
        crates/spur-tui/tests/status_bar_palette_badge.rs
git commit -m "feat(spur-tui): add [Ctrl+K: go] palette badge to status bar"
```

---

## Post-Plan Verification

After all tasks complete, run the full TUI test suite and lints:

```bash
cargo test -p spur-tui
cargo clippy -p spur-tui --all-targets -- -D warnings
cargo fmt -p spur-tui -- --check
```

Expected: all green.

Open the TUI with `cargo run -p spur-tui --example react_trace_bench_sim` (or the standard binary entry point) and manually verify:

- [ ] Ctrl+K opens palette from Dashboard
- [ ] Ctrl+K opens palette from SessionDetail
- [ ] Typing filters results in real time
- [ ] ↑/↓ moves cursor; Enter accepts; Esc dismisses
- [ ] Selecting a session result resumes the session
- [ ] Selecting a worker result navigates to that worker's SessionDetail
- [ ] Status bar shows `[Ctrl+K: go]` at all times
- [ ] Palette does NOT open when help overlay is up (Ctrl+K is swallowed by help priority)
- [ ] Palette does NOT open when quit-confirm is up

---

## Follow-up Plans (separate files)

- `docs/superpowers/plans/2026-04-19-spur-tui-ghost-line.md` — Intent Preview ghost-line (spec §5.2)
- `docs/superpowers/plans/2026-04-19-spur-tui-teachable-moments.md` — Teachable Moments framework + 5 tips (spec §5.3)

These three plans together form the one-sprint ship per the best-approach brief. Ship order: Palette first (this plan), then Ghost-line, then Teachable Moments — each additive, each independently shippable.

---

_End of plan._
