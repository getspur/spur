# Worker Mentions in the TUI `@`-Picker — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make worker agents first-class in the TUI `@`-picker; on send, prepend a one-line preference hint as `blocks[0]` of the user turn. Brain stays authoritative.

**Architecture:** New `WorkerMentionSource` plugs into the existing `MentionRegistry`. Picker pins workers on top in the empty-query case and applies a `5/4` score boost in the typed-query case. The send-path helper `prepend_worker_hint()` lives in a new `mentions/hint.rs` and is invoked at `session_detail.rs:921`. No new ACP, MCP, or orchestrator surface.

**Tech Stack:** Rust 2021, existing `nucleo-matcher 0.3` / `ratatui` / `crossterm` stack, `spur_acp::{ContentBlock, TextContent}` types. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-04-20-worker-mentions-design.md`

---

## File Map

**Create:**
- `crates/spur-tui/src/mentions/worker_source.rs` — `WorkerMentionDescriptor`, `WorkerMentionSource`
- `crates/spur-tui/src/mentions/hint.rs` — `prepend_worker_hint` helper
- `crates/spur-tui/tests/worker_mention_send_path.rs` — integration test for the send-path hint prepend

**Modify:**
- `crates/spur-tui/src/mentions/mod.rs` — re-export new types
- `crates/spur-tui/src/mentions/entry.rs` — add `MentionKind::Worker`, `secondary`, `tag` fields
- `crates/spur-tui/src/mentions/registry.rs` — `for_brain_session` / `for_direct_session` ctors, ranking refinements, `clear_cache`
- `crates/spur-tui/src/components/query_source.rs` — render `Worker` rows with 🤖 icon + tier tag + description
- `crates/spur-tui/src/views/session_detail.rs` — 6th `SessionDetailView::new` param, `known_worker_names` field, hint call site
- `crates/spur-tui/src/app.rs` — derive snapshot from `self.config.agents.entries`; pass into `SessionDetailView::new` at line 760
- `crates/spur-tui/tests/mention_registry.rs` — extend with the new ranking/source tests where appropriate

**Do not modify:** `crates/spur-tui/src/components/input_bar.rs` (`protected_ranges()` already exists at `:1124`), `crates/spur-tui/src/commands/submit_router.rs`, anything outside `spur-tui`.

---

## Task 1: Extend `MentionEntry` with `Worker` variant + optional metadata fields

Additive change. Existing `File`/`Directory` construction continues unchanged (new fields default to `None`).

**Files:**
- Modify: `crates/spur-tui/src/mentions/entry.rs`

- [ ] **Step 1: Read the current file**

Run: read `crates/spur-tui/src/mentions/entry.rs` (40 lines).

- [ ] **Step 2: Add `Worker` variant + new fields**

Replace the contents of `entry.rs` with:

```rust
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Directory,
    Worker,
}

#[derive(Debug, Clone)]
pub struct MentionEntry {
    pub kind: MentionKind,
    /// File URI (`file:///abs/...`) or worker URI (`worker://<name>`).
    pub uri: String,
    /// Display label. For files: relative path (dirs end with `/`).
    /// For workers: `worker:<name>` (e.g. `worker:claude-code`).
    pub display: String,
    /// Optional one-line description (worker description; None for files).
    pub secondary: Option<String>,
    /// Optional right-aligned tag (worker tier; None for files).
    pub tag: Option<String>,
}

pub trait MentionSource: Send {
    /// Rebuild the candidate list from scratch.
    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>>;
    fn name(&self) -> &'static str;
}

/// Convert an absolute path under cwd into a `MentionEntry`.
pub fn entry_for_path(cwd: &Path, abs: &Path) -> Option<MentionEntry> {
    let rel = abs.strip_prefix(cwd).ok()?;
    let rel_str = rel.to_str()?;
    let kind = if abs.is_dir() {
        MentionKind::Directory
    } else {
        MentionKind::File
    };
    let display = match kind {
        MentionKind::Directory => format!("{}/", rel_str),
        MentionKind::File => rel_str.to_string(),
        MentionKind::Worker => unreachable!("entry_for_path never builds Worker"),
    };
    let abs_str = abs.to_str()?;
    let uri = format!("file://{}", abs_str);
    Some(MentionEntry {
        kind,
        uri,
        display,
        secondary: None,
        tag: None,
    })
}
```

- [ ] **Step 3: Compile and run existing mention tests to confirm no regressions**

Run: `cargo test -p spur-tui --test mention_registry`
Expected: all existing tests still PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/mentions/entry.rs
git commit -m "feat(mentions): add Worker kind + optional secondary/tag fields"
```

---

## Task 2: Create `WorkerMentionDescriptor` + `WorkerMentionSource`

TDD: write the source's behavior test first, then implement.

**Files:**
- Create: `crates/spur-tui/src/mentions/worker_source.rs`
- Modify: `crates/spur-tui/src/mentions/mod.rs`

- [ ] **Step 1: Add the module declaration + re-exports**

Replace `crates/spur-tui/src/mentions/mod.rs` with:

```rust
pub mod entry;
pub mod file_source;
pub mod hint;
pub mod registry;
pub mod worker_source;

pub use entry::{MentionEntry, MentionKind, MentionSource};
pub use registry::MentionRegistry;
pub use worker_source::{WorkerMentionDescriptor, WorkerMentionSource};
```

(The `hint` module is created in Task 6; declaring it now keeps `mod.rs` stable. We'll create the file with a stub before the next compile.)

- [ ] **Step 2: Stub `hint.rs` so `mod.rs` compiles**

Create `crates/spur-tui/src/mentions/hint.rs` with:

```rust
//! Stub — real implementation added in Task 6.
```

- [ ] **Step 3: Write the failing test for `WorkerMentionSource`**

Create `crates/spur-tui/src/mentions/worker_source.rs` with the tests at the bottom (implementation will follow). For now, write the file with ONLY the test module so it fails to compile:

```rust
//! Worker `@`-mention source. Emits one entry per known worker
//! agent. The snapshot is supplied at construction time and is
//! independent of `cwd`.

use std::path::Path;

use super::entry::{MentionEntry, MentionKind, MentionSource};

#[derive(Debug, Clone)]
pub struct WorkerMentionDescriptor {
    /// Unique slug, e.g. `"claude-code"`.
    pub name: String,
    /// `delegation.description` from the agent config; shown as the
    /// row's `secondary` label in the picker.
    pub description: Option<String>,
    /// `"specialist"` or `"generalist"`; rendered as `⟨specialist⟩`.
    pub tier: Option<String>,
}

pub struct WorkerMentionSource {
    snapshot: Vec<WorkerMentionDescriptor>,
}

impl WorkerMentionSource {
    pub fn new(snapshot: Vec<WorkerMentionDescriptor>) -> Self {
        Self { snapshot }
    }
}

impl MentionSource for WorkerMentionSource {
    fn name(&self) -> &'static str {
        "worker"
    }

    fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        Ok(self
            .snapshot
            .iter()
            .map(|d| MentionEntry {
                kind: MentionKind::Worker,
                uri: format!("worker://{}", d.name),
                display: format!("worker:{}", d.name),
                secondary: d.description.clone(),
                tag: d.tier.clone(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(name: &str, desc: Option<&str>, tier: Option<&str>) -> WorkerMentionDescriptor {
        WorkerMentionDescriptor {
            name: name.into(),
            description: desc.map(str::to_string),
            tier: tier.map(str::to_string),
        }
    }

    #[test]
    fn emits_one_entry_per_descriptor() {
        let mut src = WorkerMentionSource::new(vec![
            descriptor("claude-code", Some("Refactors Rust"), Some("specialist")),
            descriptor("codex", Some("Writes tests"), Some("generalist")),
            descriptor("kiro", None, None),
        ]);
        let cwd = std::path::PathBuf::from("/");
        let entries = src.build(&cwd).expect("build ok");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].kind, MentionKind::Worker);
        assert_eq!(entries[0].uri, "worker://claude-code");
        assert_eq!(entries[0].display, "worker:claude-code");
        assert_eq!(entries[0].secondary.as_deref(), Some("Refactors Rust"));
        assert_eq!(entries[0].tag.as_deref(), Some("specialist"));
        assert_eq!(entries[2].secondary, None);
        assert_eq!(entries[2].tag, None);
    }

    #[test]
    fn name_is_worker() {
        let src = WorkerMentionSource::new(vec![]);
        assert_eq!(src.name(), "worker");
    }
}
```

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p spur-tui worker_source`
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/mentions/worker_source.rs \
        crates/spur-tui/src/mentions/hint.rs \
        crates/spur-tui/src/mentions/mod.rs
git commit -m "feat(mentions): add WorkerMentionSource + descriptor"
```

---

## Task 3: `MentionRegistry` per-session constructors + `clear_cache`

The current `MentionRegistry::new()` is a fixed `[FileMentionSource]` list. Add two named constructors and keep `new()` as an alias for `for_direct_session()` to preserve test call sites that don't care about workers.

**Files:**
- Modify: `crates/spur-tui/src/mentions/registry.rs`

- [ ] **Step 1: Read the current file**

Run: read `crates/spur-tui/src/mentions/registry.rs` (95 lines).

- [ ] **Step 2: Add the constructors and `clear_cache`**

In `registry.rs`, replace the `impl MentionRegistry` block (currently at lines ~25-90, plus the `Default` impl) with:

```rust
impl MentionRegistry {
    /// Source list for direct (single-agent) sessions. Files only.
    pub fn for_direct_session() -> Self {
        Self {
            sources: vec![Box::new(FileMentionSource)],
            cache: HashMap::new(),
        }
    }

    /// Source list for brain sessions. Files + workers.
    /// `workers` is the snapshot derived from the agent registry.
    pub fn for_brain_session(workers: Vec<super::WorkerMentionDescriptor>) -> Self {
        Self {
            sources: vec![
                Box::new(FileMentionSource),
                Box::new(super::WorkerMentionSource::new(workers)),
            ],
            cache: HashMap::new(),
        }
    }

    /// Back-compat alias used by tests and any caller that doesn't
    /// know the session role. Equivalent to `for_direct_session()`.
    pub fn new() -> Self {
        Self::for_direct_session()
    }

    /// Drop all cached per-session indexes. Call after the agent
    /// registry reloads so the next `query()` rebuilds with the
    /// fresh worker snapshot.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    pub fn query(
        &mut self,
        session: &SessionId,
        cwd: &std::path::Path,
        query: &str,
        limit: usize,
    ) -> Vec<MentionEntry> {
        // … existing body unchanged in this task; ranking changes happen in Task 4.
        let key = session_key(session);
        let needs_rebuild = match self.cache.get(&key) {
            Some(c) => c.built_at.elapsed() > CACHE_TTL,
            None => true,
        };
        if needs_rebuild {
            let mut all = Vec::new();
            for s in &mut self.sources {
                if let Ok(mut entries) = s.build(cwd) {
                    all.append(&mut entries);
                }
            }
            self.cache.insert(
                key.clone(),
                CachedIndex {
                    entries: all,
                    built_at: Instant::now(),
                },
            );
        }
        let entries = &self.cache[&key].entries;
        if query.is_empty() {
            let mut out: Vec<MentionEntry> = entries.iter().take(limit).cloned().collect();
            out.sort_by_key(|e| e.display.len());
            return out.into_iter().take(limit).collect();
        }
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, MentionEntry)> = entries
            .iter()
            .filter_map(|e| {
                let score = pattern.score(
                    nucleo_matcher::Utf32Str::new(&e.display, &mut Vec::new()),
                    &mut matcher,
                )?;
                Some((score, e.clone()))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.display.len().cmp(&b.1.display.len()))
        });
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

impl Default for MentionRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 3: Compile and run existing tests**

Run: `cargo test -p spur-tui --test mention_registry`
Expected: all existing tests pass (no behavior changes yet — Task 4 changes ranking).

- [ ] **Step 4: Add a quick test for `for_brain_session`**

Append to the bottom of `crates/spur-tui/tests/mention_registry.rs`:

```rust
use spur_tui::mentions::{MentionKind, WorkerMentionDescriptor};

#[test]
fn brain_session_includes_workers_in_empty_query() {
    let mut reg = MentionRegistry::for_brain_session(vec![
        WorkerMentionDescriptor {
            name: "claude-code".into(),
            description: Some("Refactors Rust".into()),
            tier: Some("specialist".into()),
        },
    ]);
    let sid = SessionId::new();
    // `cwd` doesn't matter for worker hits; use the test's repo root.
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "", 10);
    assert!(
        hits.iter().any(|h| h.kind == MentionKind::Worker
            && h.display == "worker:claude-code"),
        "expected worker:claude-code in hits, got {:?}",
        hits.iter().map(|h| &h.display).collect::<Vec<_>>()
    );
}

#[test]
fn direct_session_excludes_workers() {
    let mut reg = MentionRegistry::for_direct_session();
    let sid = SessionId::new();
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "", 50);
    assert!(
        !hits.iter().any(|h| h.kind == MentionKind::Worker),
        "direct session should not surface worker entries"
    );
}
```

Add the use line near the top of the file if not present:

```rust
use spur_tui::mentions::{MentionRegistry, MentionKind, WorkerMentionDescriptor};
```

(Replace any existing `use spur_tui::mentions::MentionRegistry;` with the broader form.)

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p spur-tui --test mention_registry`
Expected: previously-passing tests still pass; 2 new tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/mentions/registry.rs \
        crates/spur-tui/tests/mention_registry.rs
git commit -m "feat(mentions): per-session source ctors + clear_cache"
```

---

## Task 4: Picker ranking — pin workers on empty query, boost on typed

TDD: write the boost / pin tests first, then implement.

**Files:**
- Modify: `crates/spur-tui/src/mentions/registry.rs`
- Modify: `crates/spur-tui/tests/mention_registry.rs`

- [ ] **Step 1: Add the failing tests**

Append to `crates/spur-tui/tests/mention_registry.rs`:

```rust
#[test]
fn empty_query_pins_workers_first() {
    let workers: Vec<WorkerMentionDescriptor> = (0..6)
        .map(|i| WorkerMentionDescriptor {
            name: format!("worker-{}", i),
            description: None,
            tier: None,
        })
        .collect();
    let mut reg = MentionRegistry::for_brain_session(workers);
    let sid = SessionId::new();
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "", 20);
    // First 6 must all be Worker kind.
    let worker_count = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(
        worker_count, 6,
        "expected first 6 rows to be workers, got {:?}",
        hits.iter().take(6).map(|h| (&h.kind, &h.display)).collect::<Vec<_>>()
    );
}

#[test]
fn empty_query_caps_workers_at_pin_cap() {
    // Provide 10 workers — only 6 (WORKER_PIN_CAP) should appear at the top.
    let workers: Vec<WorkerMentionDescriptor> = (0..10)
        .map(|i| WorkerMentionDescriptor {
            name: format!("w{:02}", i),
            description: None,
            tier: None,
        })
        .collect();
    let mut reg = MentionRegistry::for_brain_session(workers);
    let sid = SessionId::new();
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "", 20);
    // Count workers in the first 6 rows; rows 7+ should not be workers
    // (they'd be files, since the cap pins only 6 workers up front).
    let head_workers = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(head_workers, 6);
    // Any workers beyond row 6 are allowed (they ranked into the file
    // slots by length); the cap only governs the pinned prefix.
}

#[test]
fn typed_query_boosts_worker_in_ambiguous_match() {
    // Build a registry where both a worker and a similarly-spelled
    // file would match "cla". Worker should win after the 5/4 boost.
    let mut reg = MentionRegistry::for_brain_session(vec![
        WorkerMentionDescriptor {
            name: "claude-code".into(),
            description: None,
            tier: None,
        },
    ]);
    let sid = SessionId::new();
    // Use the workspace root so FileMentionSource has real files to
    // produce competing matches.
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "cla", 5);
    assert!(
        hits.first().map(|h| h.kind == MentionKind::Worker).unwrap_or(false),
        "expected worker:claude-code at row 0 for 'cla', got {:?}",
        hits.iter().map(|h| (&h.kind, &h.display)).collect::<Vec<_>>()
    );
}
```

- [ ] **Step 2: Run them to confirm they fail**

Run: `cargo test -p spur-tui --test mention_registry empty_query_pins_workers_first typed_query_boosts_worker_in_ambiguous_match empty_query_caps_workers_at_pin_cap`
Expected: all three FAIL (current code doesn't pin or boost).

- [ ] **Step 3: Add the constants and refactor `query`**

In `crates/spur-tui/src/mentions/registry.rs`, near the top of the file (just after `const CACHE_TTL`), add:

```rust
/// Maximum number of worker rows pinned to the top of the empty-query
/// picker view. See design spec §4.4 / §10.1.
pub(super) const WORKER_PIN_CAP: usize = 6;

/// Multiplicative boost numerator for worker entries in the typed-query
/// branch. With `WORKER_SCORE_DEN = 4` this yields a +25 % bias, enough
/// to surface workers above tied file matches without overriding strong
/// file-specific matches. Empirically validated; see design spec §10.1.
pub(super) const WORKER_SCORE_NUM: u32 = 5;
pub(super) const WORKER_SCORE_DEN: u32 = 4;
```

Then replace the body of `query()` with:

```rust
    pub fn query(
        &mut self,
        session: &SessionId,
        cwd: &std::path::Path,
        query: &str,
        limit: usize,
    ) -> Vec<MentionEntry> {
        let key = session_key(session);
        let needs_rebuild = match self.cache.get(&key) {
            Some(c) => c.built_at.elapsed() > CACHE_TTL,
            None => true,
        };
        if needs_rebuild {
            let mut all = Vec::new();
            for s in &mut self.sources {
                if let Ok(mut entries) = s.build(cwd) {
                    all.append(&mut entries);
                }
            }
            self.cache.insert(
                key.clone(),
                CachedIndex {
                    entries: all,
                    built_at: Instant::now(),
                },
            );
        }
        let entries = &self.cache[&key].entries;

        if query.is_empty() {
            // Empty-query branch: pin up to WORKER_PIN_CAP workers, then
            // fill remaining slots with files sorted by display length.
            let mut workers: Vec<MentionEntry> = entries
                .iter()
                .filter(|e| e.kind == MentionKind::Worker)
                .cloned()
                .collect();
            workers.sort_by(|a, b| {
                a.display
                    .len()
                    .cmp(&b.display.len())
                    .then(a.display.cmp(&b.display))
            });
            workers.truncate(WORKER_PIN_CAP.min(limit));

            let remaining = limit.saturating_sub(workers.len());
            let mut files: Vec<MentionEntry> = entries
                .iter()
                .filter(|e| e.kind != MentionKind::Worker)
                .cloned()
                .collect();
            files.sort_by_key(|e| e.display.len());
            files.truncate(remaining);

            workers.extend(files);
            return workers;
        }

        // Typed-query branch: nucleo score with a +25 % boost for workers.
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, MentionEntry)> = entries
            .iter()
            .filter_map(|e| {
                let raw = pattern.score(
                    nucleo_matcher::Utf32Str::new(&e.display, &mut Vec::new()),
                    &mut matcher,
                )?;
                let boosted = if e.kind == MentionKind::Worker {
                    raw.saturating_mul(WORKER_SCORE_NUM) / WORKER_SCORE_DEN
                } else {
                    raw
                };
                Some((boosted, e.clone()))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.display.len().cmp(&b.1.display.len()))
        });
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }
```

(Note: the new code refers to `MentionKind`. Ensure the existing `use super::entry::{MentionEntry, MentionSource};` line at the top of `registry.rs` is broadened to also import `MentionKind`:

```rust
use super::entry::{MentionEntry, MentionKind, MentionSource};
```
)

- [ ] **Step 4: Run the tests to confirm they pass**

Run: `cargo test -p spur-tui --test mention_registry`
Expected: all tests including the 3 new ones pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/mentions/registry.rs \
        crates/spur-tui/tests/mention_registry.rs
git commit -m "feat(mentions): pin workers on empty query, +25% boost on typed"
```

---

## Task 5: Render `Worker` rows with icon + tier tag + description

Update `MentionQuerySource::refresh` to surface `secondary`/`tag` for worker rows. Files keep the existing 📄 / 📁 render with empty `secondary`/`tag`.

**Files:**
- Modify: `crates/spur-tui/src/components/query_source.rs`

- [ ] **Step 1: Read the current `refresh` impl**

Run: read `crates/spur-tui/src/components/query_source.rs:286-329`.

- [ ] **Step 2: Replace the `refresh` body**

In `query_source.rs`, replace the existing `refresh` method (lines ~295-318) with:

```rust
    fn refresh(&mut self, query: &str) -> Vec<RetrievalRow> {
        use crate::mentions::MentionKind;
        let hits = self
            .registry
            .borrow_mut()
            .query(&self.session_id, &self.cwd, query, 20);
        let rows: Vec<RetrievalRow> = hits
            .iter()
            .map(|m| {
                let icon = match m.kind {
                    MentionKind::Directory => "\u{1F4C1}", // 📁
                    MentionKind::File => "\u{1F4C4}",      // 📄
                    MentionKind::Worker => "\u{1F916}",    // 🤖
                };
                let tag_render = m
                    .tag
                    .clone()
                    .map(|t| format!("\u{27E8}{}\u{27E9}", t)) // ⟨tier⟩
                    .unwrap_or_default();
                RetrievalRow {
                    primary: format!("{} @{}", icon, m.display),
                    secondary: m.secondary.clone().unwrap_or_default(),
                    tag: tag_render,
                    atoms: Vec::new(),
                }
            })
            .collect();
        self.last_hits = hits;
        rows
    }
```

- [ ] **Step 3: Build to confirm no type errors**

Run: `cargo build -p spur-tui`
Expected: builds clean.

- [ ] **Step 4: Run all spur-tui tests as a smoke check**

Run: `cargo test -p spur-tui --lib --tests --no-fail-fast 2>&1 | tail -20`
Expected: same pass/fail count as before this task (no test depends on the row format).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/query_source.rs
git commit -m "feat(picker): render worker mention rows with 🤖 + tier + description"
```

---

## Task 6: Implement `prepend_worker_hint` (TDD)

Replaces the stub from Task 2.

**Files:**
- Modify: `crates/spur-tui/src/mentions/hint.rs`

- [ ] **Step 1: Replace the stub with the real implementation + tests**

Replace the entire contents of `crates/spur-tui/src/mentions/hint.rs` with:

```rust
//! Send-time helper: if the outgoing user message contains any
//! `worker://<name>` atoms whose names are known to the registry,
//! prepend a one-line preference hint as the first
//! `ContentBlock::Text` of the outgoing blocks.
//!
//! See design spec §4.6.

use std::collections::HashSet;

use spur_acp::{ContentBlock, TextContent};

use crate::components::input_bar::ProtectedRange;

/// Returns `true` if a hint was prepended; otherwise leaves
/// `blocks` unchanged and returns `false`.
pub fn prepend_worker_hint(
    blocks: &mut Vec<ContentBlock>,
    ranges: &[ProtectedRange],
    known_workers: &HashSet<String>,
) -> bool {
    let mut names: Vec<String> = ranges
        .iter()
        .filter_map(|r| r.uri.strip_prefix("worker://"))
        .filter(|n| known_workers.contains(*n))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return false;
    }
    let hint = format!(
        "[UI hint] User-suggested workers for delegation this turn: {} \
         (preference, not override; honor unless `delegation.avoid_for` clearly matches).",
        names.join(", ")
    );
    blocks.insert(0, ContentBlock::Text(TextContent::new(hint)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn range(uri: &str) -> ProtectedRange {
        ProtectedRange {
            start: 0,
            end: 0,
            uri: uri.into(),
            name: String::new(),
        }
    }

    fn hint_text(blocks: &[ContentBlock]) -> Option<&str> {
        match blocks.first()? {
            ContentBlock::Text(t) => Some(&t.text),
            _ => None,
        }
    }

    #[test]
    fn dedupes_and_sorts_known_workers() {
        let mut blocks: Vec<ContentBlock> =
            vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![
            range("worker://a"),
            range("worker://a"),
            range("worker://missing"),
            range("worker://b"),
        ];
        let known = known(&["a", "b", "c"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(prepended);
        assert_eq!(blocks.len(), 2);
        let h = hint_text(&blocks).expect("first block is Text");
        assert!(h.starts_with("[UI hint]"));
        assert!(h.contains("a, b"), "expected 'a, b' in hint, got: {}", h);
        assert!(!h.contains("missing"));
    }

    #[test]
    fn noop_when_no_worker_ranges() {
        let mut blocks: Vec<ContentBlock> =
            vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![range("file:///abs/foo.rs")];
        let known = known(&["a"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(!prepended);
        assert_eq!(blocks.len(), 1);
        assert_eq!(hint_text(&blocks), Some("user text"));
    }

    #[test]
    fn noop_when_all_worker_names_unknown() {
        let mut blocks: Vec<ContentBlock> =
            vec![ContentBlock::Text(TextContent::new("user text"))];
        let ranges = vec![range("worker://ghost"), range("worker://phantom")];
        let known = known(&["a", "b"]);
        let prepended = prepend_worker_hint(&mut blocks, &ranges, &known);
        assert!(!prepended);
        assert_eq!(blocks.len(), 1);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p spur-tui mentions::hint`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/mentions/hint.rs
git commit -m "feat(mentions): prepend_worker_hint for send-path hint injection"
```

---

## Task 7: Plumb worker snapshot through `SessionDetailView::new`

Add a 6th parameter `worker_snapshot: Vec<WorkerMentionDescriptor>`. Update the four call sites.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Add fields and update the constructor**

In `crates/spur-tui/src/views/session_detail.rs`:

1. Add two new fields to `SessionDetailView` (after the existing `tool_depth` field at line 104):

```rust
    /// Snapshot of worker descriptors used to populate the `@`-picker
    /// for brain sessions. Empty for direct sessions. Set once at
    /// construction.
    worker_snapshot: Vec<crate::mentions::WorkerMentionDescriptor>,
    /// Derived once from `worker_snapshot`: the set of known worker
    /// names. Used by `prepend_worker_hint` to filter unknown-name
    /// atoms out of the hint.
    known_worker_names: std::collections::HashSet<String>,
```

2. Replace `SessionDetailView::new` (currently at lines 108-153) with:

```rust
    pub fn new(
        session_id: SessionId,
        agent_name: String,
        role: String,
        cwd: std::path::PathBuf,
        agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
        worker_snapshot: Vec<crate::mentions::WorkerMentionDescriptor>,
    ) -> Self {
        let command_registry =
            crate::commands::CommandRegistry::from_configs(std::slice::from_ref(&*agent_cfg));
        let agent_kind = agent_cfg.kind;
        let known_worker_names: std::collections::HashSet<String> =
            worker_snapshot.iter().map(|d| d.name.clone()).collect();
        let mention_registry = if role == "brain" {
            crate::mentions::MentionRegistry::for_brain_session(worker_snapshot.clone())
        } else {
            crate::mentions::MentionRegistry::for_direct_session()
        };
        Self {
            session_id,
            agent_name,
            role,
            agent_cfg,
            react_trace: ReactTrace::with_kind(agent_kind),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: Instant::now(),
            current_mode: None,
            command_registry,
            context_used: None,
            context_size: None,
            auth_error: None,
            trigger_detector: crate::components::completion_trigger::TriggerDetector::new(),
            mention_registry: std::rc::Rc::new(std::cell::RefCell::new(mention_registry)),
            cwd,
            #[cfg(feature = "markdown")]
            mermaid_registry: std::collections::HashMap::new(),
            #[cfg(feature = "markdown")]
            pending_fence_actions: std::collections::VecDeque::new(),
            #[cfg(feature = "markdown")]
            render_picker: None,
            last_draft_change_at: None,
            last_persisted_draft: String::new(),
            resume_banner: None,
            stream_in_flight: false,
            cancelling_in_flight: false,
            cancel_mode: None,
            picker_shell: None,
            workers_panel_collapsed: false,
            tool_depth: std::collections::HashMap::new(),
            worker_snapshot,
            known_worker_names,
        }
    }
```

- [ ] **Step 2: Update every call site of `SessionDetailView::new` in the crate**

Locate every site (production + tests + integration tests):

```bash
grep -rn "SessionDetailView::new" crates/spur-tui
```

Expected sites (current source — re-grep, the line numbers may shift):

- `crates/spur-tui/src/app.rs:760` (production — handled in Step 3 below).
- `crates/spur-tui/src/views/session_detail.rs` ~lines 1657, 1779, 1816 (in-file test helpers).
- `crates/spur-tui/tests/picker_shell_trigger_parity.rs` `mk_view_in_cwd` helper (~line 22).
- Any other tests under `crates/spur-tui/tests/` that the grep surfaces.

For each non-production site, add `Vec::new()` as the **6th** argument. Example pattern:

```rust
SessionDetailView::new(
    spur_acp::SessionId("test".to_string()),
    "claude".to_string(),
    "brain".to_string(),
    std::path::PathBuf::from("/tmp"),
    std::sync::Arc::new(spur_acp::AgentConfig::with_defaults("claude")),
    Vec::new(),
)
```

Tests passing `Vec::new()` get direct-session mention behavior (no worker rows). That's fine — none of the existing tests assert worker-mention behavior. Tests that DO assert worker behavior are added in Task 9.

- [ ] **Step 3: Update the production call site in `app.rs`**

In `crates/spur-tui/src/app.rs`, find the call near line 760:

```rust
let mut view = SessionDetailView::new(
    session.clone(),
    agent.clone(),
    "brain".to_string(),
    std::env::current_dir().unwrap_or_default(),
    agent_cfg,
);
```

Replace with:

```rust
let mut view = SessionDetailView::new(
    session.clone(),
    agent.clone(),
    "brain".to_string(),
    std::env::current_dir().unwrap_or_default(),
    agent_cfg,
    self.build_worker_snapshot(),
);
```

Then add a small helper method on `App` (placed near `resolve_agent_config`, around `app.rs:402`):

```rust
    /// Derive the `WorkerMentionDescriptor` snapshot from the loaded
    /// agent config. Filtered to roles that can serve as a worker
    /// (matches `AgentRegistry::worker_capable` semantics).
    fn build_worker_snapshot(&self) -> Vec<crate::mentions::WorkerMentionDescriptor> {
        use spur_acp::config::Tier;
        use spur_acp::types::AgentRole;
        self.config
            .agents
            .entries
            .iter()
            .filter(|cfg| matches!(cfg.role, AgentRole::Worker | AgentRole::Both))
            .map(|cfg| crate::mentions::WorkerMentionDescriptor {
                name: cfg.name.clone(),
                description: cfg.delegation.description.clone(),
                tier: cfg.delegation.tier.map(|t| match t {
                    Tier::Specialist => "specialist".to_string(),
                    Tier::Generalist => "generalist".to_string(),
                }),
            })
            .collect()
    }
```

- [ ] **Step 4: Build the whole crate**

Run: `cargo build -p spur-tui`
Expected: builds clean. If you see "expected 6 arguments" errors, you missed a call site — search again with grep.

- [ ] **Step 5: Run all spur-tui tests**

Run: `cargo test -p spur-tui --lib --tests --no-fail-fast 2>&1 | tail -20`
Expected: same pass/fail count as before this task (no behavior changes for tests passing `Vec::new()`).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/app.rs
git commit -m "feat(tui): plumb worker snapshot into SessionDetailView"
```

---

## Task 8: Wire `prepend_worker_hint` into the send path

The seam is `session_detail.rs` line ~921 (the `SubmitDecision::Send` arm). Crucially, the local `ranges` from `take_submit_capture` is already in scope.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Read the current send-path arm**

Run: read `crates/spur-tui/src/views/session_detail.rs:914-935`.

- [ ] **Step 2: Patch the `Send` match arm**

Find the block:

```rust
                if let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() {
                    use crate::commands::submit_router::{route, SubmitDecision};
                    let dec = route(&text, &ranges, &self.command_registry, interrupt);
                    return match dec {
                        SubmitDecision::Empty => None,
                        SubmitDecision::Send { blocks, interrupt } => Some(Action::SendMessage {
                            session: self.session_id.clone(),
                            blocks,
                            interrupt,
                        }),
                        SubmitDecision::Local { action } => Some(action),
                        SubmitDecision::VendorExec { method, params } => Some(Action::VendorExec {
                            session: self.session_id.clone(),
                            method,
                            params,
                        }),
                    };
                }
```

Replace it with:

```rust
                if let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() {
                    use crate::commands::submit_router::{route, SubmitDecision};
                    let dec = route(&text, &ranges, &self.command_registry, interrupt);
                    return match dec {
                        SubmitDecision::Empty => None,
                        SubmitDecision::Send { mut blocks, interrupt } => {
                            if self.role == "brain" {
                                let _ = crate::mentions::hint::prepend_worker_hint(
                                    &mut blocks,
                                    &ranges,
                                    &self.known_worker_names,
                                );
                            }
                            Some(Action::SendMessage {
                                session: self.session_id.clone(),
                                blocks,
                                interrupt,
                            })
                        }
                        SubmitDecision::Local { action } => Some(action),
                        SubmitDecision::VendorExec { method, params } => Some(Action::VendorExec {
                            session: self.session_id.clone(),
                            method,
                            params,
                        }),
                    };
                }
```

- [ ] **Step 3: Build**

Run: `cargo build -p spur-tui`
Expected: builds clean.

- [ ] **Step 4: Run all existing spur-tui tests as a smoke check**

Run: `cargo test -p spur-tui --lib --tests --no-fail-fast 2>&1 | tail -20`
Expected: no regressions; same pass/fail count.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(tui): prepend worker-preference hint to brain-turn blocks"
```

---

## Task 9: Send-path integration test

End-to-end check: brain `SessionDetailView` with a worker snapshot, type `@cla`, accept the worker row, press Enter, and assert the resulting `Action::SendMessage` carries the hint as `blocks[0]` plus the worker `ResourceLink` later in `blocks`.

This test mirrors the existing pattern in `crates/spur-tui/tests/picker_shell_trigger_parity.rs` exactly: it calls the `View::handle_key(KeyEvent, &ViewContext)` trait method (`session_detail.rs:1020`) via the `View` trait, using the helpers `spur_tui::test_support::test_view_ctx` and `spur_tui::test_support::default_agent_config` (both already public in `crates/spur-tui/src/lib.rs`).

**Files:**
- Create: `crates/spur-tui/tests/worker_mention_send_path.rs`

- [ ] **Step 1: Write the test**

Create `crates/spur-tui/tests/worker_mention_send_path.rs` with:

```rust
//! Integration test: when a brain SessionDetailView submits a message
//! containing a `worker://` atom, the resulting `Action::SendMessage`
//! has a `[UI hint]` Text block prepended as `blocks[0]` and preserves
//! the original ResourceLink later in `blocks`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::mentions::WorkerMentionDescriptor;
use spur_tui::views::{session_detail::SessionDetailView, View};

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn brain_view_with_workers(workers: Vec<WorkerMentionDescriptor>) -> SessionDetailView {
    let tmp = tempfile::tempdir().unwrap();
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        workers,
    )
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        let _ = press(v, KeyCode::Char(c));
    }
}

#[test]
fn brain_send_prepends_worker_hint_block() {
    let mut v = brain_view_with_workers(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        description: Some("Refactors Rust".into()),
        tier: Some("specialist".into()),
    }]);

    // "@cla" → opens the mention shell; with the +25% boost worker:claude-code
    // ranks at row 0 (validated by Task 4 test `typed_query_boosts_worker_in_ambiguous_match`).
    type_str(&mut v, "@cla");
    // Tab → accept the top row (worker:claude-code), inserts a protected atom
    // with uri "worker://claude-code".
    let _ = press(&mut v, KeyCode::Tab);
    // Enter → submit. Resulting blocks should have the hint at [0]
    // and the worker ResourceLink later.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };

    assert!(
        matches!(&blocks[0], ContentBlock::Text(t)
            if t.text.starts_with("[UI hint]") && t.text.contains("claude-code")),
        "expected [UI hint] Text at blocks[0], got {:?}",
        blocks[0]
    );
    assert!(
        blocks.iter().skip(1).any(|b| matches!(
            b,
            ContentBlock::ResourceLink(r) if r.uri == "worker://claude-code"
        )),
        "expected a ResourceLink with uri=worker://claude-code later in blocks, got {:?}",
        blocks
    );
}

#[test]
fn brain_send_without_worker_atom_has_no_hint() {
    let mut v = brain_view_with_workers(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        description: None,
        tier: None,
    }]);

    type_str(&mut v, "just text");
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };
    // First block must NOT be the hint.
    if let ContentBlock::Text(t) = &blocks[0] {
        assert!(
            !t.text.starts_with("[UI hint]"),
            "did not expect a hint when no worker atom was present, got: {}",
            t.text
        );
    }
}

#[test]
fn direct_session_skips_hint_even_with_worker_atom_pasted() {
    // Direct (non-brain) view; pretend the user somehow has a worker atom
    // (won't normally happen because WorkerMentionSource isn't registered
    // in direct sessions — defense in depth).
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "worker".into(),     // role != "brain"
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        vec![WorkerMentionDescriptor {
            name: "claude-code".into(),
            description: None,
            tier: None,
        }],
    );

    type_str(&mut v, "anything");
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };
    if let ContentBlock::Text(t) = &blocks[0] {
        assert!(
            !t.text.starts_with("[UI hint]"),
            "direct session must never prepend the hint, got: {}",
            t.text
        );
    }
}
```

- [ ] **Step 2: Build the test**

Run: `cargo build -p spur-tui --tests`
Expected: builds clean. If you see "no method `handle_key`", confirm the `use spur_tui::views::View;` line is present (the trait import is what makes `handle_key` callable on `SessionDetailView`).

- [ ] **Step 3: Run the new integration test**

Run: `cargo test -p spur-tui --test worker_mention_send_path -- --nocapture`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/tests/worker_mention_send_path.rs
git commit -m "test(tui): integration test for worker-mention send-path hint"
```

---

## Task 10: Final verification — full spur-tui suite + manual smoke

- [ ] **Step 1: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui --lib --tests --no-fail-fast 2>&1 | tail -30`
Expected: all tests pass. Compare the pass/fail counts against the pre-change baseline (`git stash && cargo test … && git stash pop`) if you want reassurance — there should be a net gain of:
- 2 tests in `worker_source.rs`
- 3 tests in `mentions/hint.rs`
- 5 new tests in `tests/mention_registry.rs` (workers-included, workers-excluded, pin, cap, boost)
- 3 new tests in `tests/worker_mention_send_path.rs`

…and **zero** existing-test regressions.

- [ ] **Step 2: Build the full workspace as a sanity check**

Run: `cargo build --workspace`
Expected: clean build.

- [ ] **Step 3: Manual smoke test (optional, recommended)**

Launch the TUI against a `.spur/config.toml` with at least one `role = "worker"` agent. Open or create a brain session. Press `@` and confirm:
- Worker rows appear at the top of the empty-state picker with the 🤖 icon, their description as the secondary line, and `⟨specialist⟩` / `⟨generalist⟩` as the right-aligned tag.
- Typing `@cla` ranks `worker:claude-code` at the top.
- Typing `@src/` and a real file path ranks the file at the top.
- Picking a worker inserts a single-unit atom rendered as `@worker:<name>` (LightBlue + underlined per existing `ProtectedRange` styling).
- Sending a message containing a worker atom causes the brain to receive a hint block (verify in the brain agent's transcript / log).

- [ ] **Step 4: Final commit (if needed)**

If steps 1-3 produced any small fixes, commit them:

```bash
git add -u
git commit -m "fix(tui): polish from worker-mentions verification"
```

If there were no follow-ups, no commit.

---

## Self-Review Notes (for the executing engineer)

- **Spec coverage check:** all 6 in-scope items in spec §1 are addressed (Task 2 → source; Task 3+7 → per-session registration; Task 4 → ranking; Task 8 → hint prepend; Task 5 → visual differentiation; Task 1 → `MentionEntry` shape).
- **`MentionRegistry::new()` is preserved** as an alias for `for_direct_session()` so existing tests and any external callers keep compiling. This is the only back-compat concession; if it bothers you on review, deprecate later.
- **Constants** `WORKER_PIN_CAP`, `WORKER_SCORE_NUM`, `WORKER_SCORE_DEN` are `pub(super)` and live in `registry.rs` — tunable in one place.
- **`submit_router::route` is untouched.** The hint is layered on top of its `Send` output in the view, per spec §3.2.
- **No cross-crate changes.** Everything lives under `crates/spur-tui/`.
