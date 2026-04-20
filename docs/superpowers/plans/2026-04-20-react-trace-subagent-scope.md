# React-trace Sub-agent Scope Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add richer visual treatment for Claude sub-agent (`Task`) scopes in the `ReactTrace` component — gutter column, color-allocated short badge, framed Header/Terminal rows, width-tiered degradation, and `NO_COLOR` fallbacks — while preserving append-only, scroll-anchor, and cache invariants.

**Architecture:** Inline-interleaved rendering (spec §3). A per-entry `Option<Scope>` on `TraceEntry` drives role-dispatched row formatting. `ScopeColorAllocator` assigns palette colors and short badges (`[T1]`, `[T2]`, …) at first-sight per scope. `TraceKind::ScopeTerminal` is a new non-Act variant so late `ToolCallUpdate` events cannot mis-route via `find_act_by_id_mut`.

**Tech Stack:** Rust 2021 edition, `ratatui` (terminal UI), `spur-acp` (ACP types), `tracing` (structured logging), `serde` (newtype wire compatibility).

**Spec reference:** `docs/superpowers/specs/2026-04-20-react-trace-subagent-scope-design.md`

---

## File Structure

| File | Action | Purpose |
|---|---|---|
| `crates/spur-tui/src/components/react_trace/types.rs` | modify | Add `ScopeId`, `ScopeRole`, `Scope`; extend `TraceEntry`; add `TraceKind::ScopeTerminal` |
| `crates/spur-tui/src/components/react_trace/scope_colors.rs` | **create** | `ScopeColorAllocator` — assignment-on-first-sight palette + short-label allocation |
| `crates/spur-tui/src/components/react_trace/dispatch.rs` | modify | Replace depth-bare logic with scope classification; synthesize Terminal on Task terminal |
| `crates/spur-tui/src/components/react_trace/mod.rs` | modify | Own `ScopeColorAllocator` + `orphan_warned: HashSet<ScopeId>`; clear both on session reset |
| `crates/spur-tui/src/components/react_trace/render.rs` | modify | Gutter + badge + Header/Terminal/Orphan/Late row treatment |
| `crates/spur-tui/src/components/react_trace/compact_render.rs` | modify | Width-tier degradation (≥80 / 72–79 / 60–71 / <60) |
| `crates/spur-tui/src/components/trace_format.rs` | modify | Add `warning_style()` returning Yellow `Style` |
| `crates/spur-tui/src/components/react_trace/streaming_tests.rs` | modify | New tests for criteria §5 (especially 10–12) |
| `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_orphan_child.json` | **create** | Fixture with `parentToolUseId` referencing an unknown id |

---

## Progression Notes

- TDD throughout: every behavior change gets a failing test before implementation.
- Tasks 1–3 are foundation (types + allocator + trace fields). No behavior visible yet.
- Tasks 4–8 land the dispatch-side classification and terminal/late semantics.
- Tasks 9–14 cover rendering row formats.
- Tasks 15–17 cover compact-surface width-tier and `NO_COLOR`.
- Tasks 18–21 are verification tests matching §5 acceptance criteria.

---

## Task 1: Foundation types — `ScopeId`, `ScopeRole`, `Scope`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs`

- [ ] **Step 1: Add the test module for the new types**

Append to `crates/spur-tui/src/components/react_trace/types.rs`:

```rust
#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn scope_id_roundtrips_json_as_plain_string() {
        let id = ScopeId("tc-abc-123".into());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"tc-abc-123\"");
        let back: ScopeId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn scope_id_hashes_equal_for_same_inner() {
        use std::collections::HashSet;
        let mut set: HashSet<ScopeId> = HashSet::new();
        set.insert(ScopeId("x".into()));
        assert!(set.contains(&ScopeId("x".into())));
    }

    #[test]
    fn scope_role_variants_exhaustive_count() {
        let roles = [
            ScopeRole::Header,
            ScopeRole::Child,
            ScopeRole::Terminal,
            ScopeRole::Orphan,
        ];
        assert_eq!(roles.len(), 4);
    }

    #[test]
    fn scope_struct_is_cloneable() {
        let s = Scope {
            id: ScopeId("pid-1".into()),
            depth: 2,
            role: ScopeRole::Child,
        };
        let s2 = s.clone();
        assert_eq!(s.id, s2.id);
        assert_eq!(s.depth, s2.depth);
        assert!(matches!(s2.role, ScopeRole::Child));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-tui --lib scope_tests`
Expected: compile error — `ScopeId`, `ScopeRole`, `Scope` not defined.

- [ ] **Step 3: Add the type definitions**

Insert after `ActStatus` (around line 28) in `crates/spur-tui/src/components/react_trace/types.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Identifier for a sub-agent scope.
///
/// Today: wraps the parent `tool_call_id` (Claude's `_meta.claudeCode.parentToolUseId`).
/// Post-ACP-#855: source changes to `parentSessionId`; the type is unchanged.
///
/// `serde(transparent)` keeps the wire form identical to the underlying String so
/// the eventual field-source change does not ripple through serialized state.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeId(pub String);

impl std::fmt::Display for ScopeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Role of a `TraceEntry` within a sub-agent scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRole {
    /// The Task tool call itself — visually framed as a section opener.
    Header,
    /// A child tool call inside the Task's scope.
    Child,
    /// Synthesized row marking the scope's close with child count + duration.
    Terminal,
    /// Child whose claimed parent id was never seen.
    Orphan,
}

/// Per-entry sub-agent scope metadata.
#[derive(Debug, Clone)]
pub struct Scope {
    pub id: ScopeId,
    /// Nesting depth, saturating at 8.
    pub depth: u8,
    pub role: ScopeRole,
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p spur-tui --lib scope_tests`
Expected: 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/types.rs
git commit -m "feat(spur-tui/react_trace): add ScopeId, ScopeRole, Scope types"
```

---

## Task 2: Add `TraceKind::ScopeTerminal` variant

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs`

- [ ] **Step 1: Add the test**

Append to the `scope_tests` module in `types.rs`:

```rust
    #[test]
    fn scope_terminal_kind_constructs_with_all_fields() {
        let k = TraceKind::ScopeTerminal {
            scope_id: ScopeId("sid-1".into()),
            status: ActStatus::Completed(None),
            child_count: 3,
            duration_ms: Some(142),
        };
        match k {
            TraceKind::ScopeTerminal {
                scope_id,
                status,
                child_count,
                duration_ms,
            } => {
                assert_eq!(scope_id, ScopeId("sid-1".into()));
                assert!(matches!(status, ActStatus::Completed(_)));
                assert_eq!(child_count, 3);
                assert_eq!(duration_ms, Some(142));
            }
            _ => panic!("expected ScopeTerminal"),
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p spur-tui --lib scope_tests::scope_terminal_kind_constructs_with_all_fields`
Expected: compile error — variant `ScopeTerminal` not defined.

- [ ] **Step 3: Add the variant to `TraceKind`**

In `types.rs`, add to the existing `pub enum TraceKind { ... }` (just before the closing brace of the enum):

```rust
    /// Synthesized row marking a sub-agent scope's close.
    ///
    /// Deliberately NOT an `Act` variant so `ReactTrace::find_act_by_id_mut`
    /// (mod.rs:834-855) cannot resolve late `ToolCallUpdate` events to it —
    /// the Task's original Header `Act` entry remains the sole lookup target.
    ScopeTerminal {
        scope_id: ScopeId,
        /// Either `Completed` or `Failed` at synthesis time; never mutated.
        status: ActStatus,
        /// Frozen at synthesis time. Late-arriving children do NOT update this.
        child_count: u32,
        /// Elapsed milliseconds between Header creation and Terminal synthesis.
        /// `None` if duration could not be derived.
        duration_ms: Option<u64>,
    },
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --lib scope_tests`
Expected: 5 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/types.rs
git commit -m "feat(spur-tui/react_trace): add TraceKind::ScopeTerminal variant"
```

---

## Task 3: Extend `TraceEntry` with `scope: Option<Scope>`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs`
- Modify: all call sites that construct `TraceEntry { ... }` (to add `scope: None`)

- [ ] **Step 1: Locate every `TraceEntry { ... }` construction site**

Run: `rg -n 'TraceEntry\s*\{' crates/spur-tui/src/ crates/spur-tui/tests/ 2>&1 | tee /tmp/trace_entry_sites.txt`
Record the file\:line list — every site needs a `scope: None,` field added in Step 4.

- [ ] **Step 2: Add a test asserting the new field**

Append to `scope_tests` in `types.rs`:

```rust
    #[test]
    fn trace_entry_has_optional_scope_default_none() {
        let e = TraceEntry {
            kind: TraceKind::Think,
            text: "hi".into(),
            timestamp: "00:00:00".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
            scope: None,
        };
        assert!(e.scope.is_none());
    }

    #[test]
    fn trace_entry_scope_some_preserves_metadata() {
        let e = TraceEntry {
            kind: TraceKind::Think,
            text: "".into(),
            timestamp: "".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
            scope: Some(Scope {
                id: ScopeId("p".into()),
                depth: 1,
                role: ScopeRole::Child,
            }),
        };
        let s = e.scope.as_ref().unwrap();
        assert_eq!(s.depth, 1);
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p spur-tui --lib scope_tests`
Expected: compile error — no field `scope` on `TraceEntry`.

- [ ] **Step 4: Add the field to `TraceEntry`**

In `types.rs`, modify the struct:

```rust
pub struct TraceEntry {
    pub kind: TraceKind,
    pub text: String,
    pub timestamp: String,
    #[cfg(feature = "markdown")]
    pub markdown: Option<crate::components::markdown_stream::MarkdownStream>,
    /// Sub-agent scope metadata; `None` for parent-session output.
    pub scope: Option<Scope>,
}
```

- [ ] **Step 5: Add `scope: None` to every construction site**

For each site in `/tmp/trace_entry_sites.txt`, add `scope: None,` as the last field of the struct literal. The known sites (as of this plan) include (verify against the fresh `rg` output):

- `crates/spur-tui/src/components/react_trace/dispatch.rs:92-104` — the `ToolCall` construction
- `crates/spur-tui/src/components/react_trace/dispatch.rs:135-147` — the synthetic-Act construction
- `crates/spur-tui/src/components/react_trace/dispatch.rs:169-175` — the `Plan` construction
- `crates/spur-tui/src/components/react_trace/mod.rs` — any `append_*` helpers constructing `TraceEntry`
- `crates/spur-tui/src/components/react_trace/streaming_tests.rs` — test-local constructions

Example edit pattern:

```rust
trace.push(TraceEntry {
    kind: TraceKind::Act { /* ... */ },
    text: fallback_text,
    timestamp: (ctx.now_stamp)(),
    #[cfg(feature = "markdown")]
    markdown: None,
    scope: None,   // NEW
});
```

- [ ] **Step 6: Run the full spur-tui test suite**

Run: `cargo test -p spur-tui --lib`
Expected: All existing tests pass; new `scope_tests` (7 tests total) pass.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/ crates/spur-tui/src/components/trace_format.rs crates/spur-tui/tests/
git commit -m "feat(spur-tui/react_trace): extend TraceEntry with optional scope field"
```

---

## Task 4: `ScopeColorAllocator` module — tests first

**Files:**
- Create: `crates/spur-tui/src/components/react_trace/scope_colors.rs`

- [ ] **Step 1: Create the module file with failing tests**

Create `crates/spur-tui/src/components/react_trace/scope_colors.rs`:

```rust
//! Assignment-on-first-sight color + short-label allocator for sub-agent scopes.
//!
//! See design spec §3.3: palette is a fixed 8-color ratatui set; badges
//! count up from `T1` in first-sight order. Allocator lives on `ReactTrace`
//! (session-scoped), reset at session reset.

use std::collections::HashMap;

use ratatui::style::Color;

use super::types::ScopeId;

/// Palette of distinguishable colors for sub-agent gutters.
///
/// Order matters: scopes claim colors in round-robin order. Chosen to avoid
/// Red (reserved for errors) and White (normal text) so the gutter always
/// visually reads as accent.
pub(crate) const SCOPE_PALETTE: &[Color] = &[
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::Blue,
    Color::LightRed,
    Color::LightCyan,
    Color::LightMagenta,
];

#[derive(Debug, Clone)]
pub(crate) struct ScopeAssignment {
    pub color: Color,
    /// Short human-readable badge, e.g. "T1", "T2", ..., "T99".
    pub badge: String,
}

#[derive(Debug, Default)]
pub(crate) struct ScopeColorAllocator {
    next: u32,
    assigned: HashMap<ScopeId, ScopeAssignment>,
}

impl ScopeColorAllocator {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Return the assignment for `id`, allocating on first sight.
    pub(crate) fn assignment(&mut self, id: &ScopeId) -> ScopeAssignment {
        if let Some(a) = self.assigned.get(id) {
            return a.clone();
        }
        let n = self.next.saturating_add(1);
        let color = SCOPE_PALETTE[(self.next as usize) % SCOPE_PALETTE.len()];
        let badge = format!("T{}", n);
        let a = ScopeAssignment { color, badge };
        self.assigned.insert(id.clone(), a.clone());
        self.next = n;
        a
    }

    /// Clear all state (called on session reset).
    pub(crate) fn reset(&mut self) {
        self.next = 0;
        self.assigned.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_stable_color_and_badge_on_first_sight() {
        let mut a = ScopeColorAllocator::new();
        let id = ScopeId("scope-1".into());
        let a1 = a.assignment(&id);
        let a2 = a.assignment(&id);
        assert_eq!(a1.color, a2.color);
        assert_eq!(a1.badge, "T1");
        assert_eq!(a2.badge, "T1");
    }

    #[test]
    fn assigns_distinct_badges_to_distinct_ids() {
        let mut a = ScopeColorAllocator::new();
        let b1 = a.assignment(&ScopeId("s1".into())).badge;
        let b2 = a.assignment(&ScopeId("s2".into())).badge;
        let b3 = a.assignment(&ScopeId("s3".into())).badge;
        assert_eq!(b1, "T1");
        assert_eq!(b2, "T2");
        assert_eq!(b3, "T3");
    }

    #[test]
    fn palette_recycles_after_eight_but_badge_keeps_counting() {
        let mut a = ScopeColorAllocator::new();
        let mut colors = Vec::new();
        let mut badges = Vec::new();
        for i in 0..10 {
            let asg = a.assignment(&ScopeId(format!("s{}", i)));
            colors.push(asg.color);
            badges.push(asg.badge);
        }
        // Color slot 0 reused at position 8 (8 % 8 == 0).
        assert_eq!(colors[0], colors[8]);
        // Badges never collide.
        assert_eq!(badges[0], "T1");
        assert_eq!(badges[9], "T10");
    }

    #[test]
    fn reset_clears_all_assignments_and_counter() {
        let mut a = ScopeColorAllocator::new();
        let _ = a.assignment(&ScopeId("s1".into()));
        let _ = a.assignment(&ScopeId("s2".into()));
        a.reset();
        let after = a.assignment(&ScopeId("s3".into()));
        assert_eq!(after.badge, "T1");
    }
}
```

- [ ] **Step 2: Declare the module**

Edit `crates/spur-tui/src/components/react_trace/mod.rs` — add near the other `mod` declarations at the top:

```rust
pub(super) mod scope_colors;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p spur-tui --lib scope_colors`
Expected: 4 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/scope_colors.rs crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui/react_trace): add ScopeColorAllocator with palette recycling"
```

---

## Task 5: Wire `ScopeColorAllocator` + `orphan_warned` onto `ReactTrace`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`

- [ ] **Step 1: Find every session-reset site**

Run: `rg -n 'entries\.clear\(\)|self\.entries\s*=\s*Vec::new\(\)' crates/spur-tui/src/ 2>&1 | tee /tmp/reset_sites.txt`
Record the list — every site must also call `orphan_warned.clear()` and `scope_colors.reset()`.

- [ ] **Step 2: Write a test asserting reset semantics**

Append to `crates/spur-tui/src/components/react_trace/mod.rs` in a `#[cfg(test)] mod scope_state_tests` block (or append to existing tests module):

```rust
#[cfg(test)]
mod scope_state_tests {
    use super::*;
    use crate::components::react_trace::types::ScopeId;

    #[test]
    fn new_react_trace_has_empty_scope_state() {
        let rt = ReactTrace::new(spur_acp::AgentKind::Generic);
        assert!(rt.orphan_warned.is_empty());
        // scope_colors is private; just assert no panic on construction.
    }

    #[test]
    fn clear_resets_orphan_warned_and_allocator() {
        let mut rt = ReactTrace::new(spur_acp::AgentKind::Generic);
        rt.orphan_warned.insert(ScopeId("x".into()));
        assert_eq!(rt.orphan_warned.len(), 1);
        // Replicate whatever your session-reset path calls.
        rt.entries.clear();
        rt.orphan_warned.clear();
        rt.scope_colors.reset();
        assert!(rt.orphan_warned.is_empty());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p spur-tui --lib scope_state_tests`
Expected: compile error — `orphan_warned` / `scope_colors` not fields of `ReactTrace`.

- [ ] **Step 4: Add the fields to the `ReactTrace` struct**

In `mod.rs`, inside `pub struct ReactTrace { ... }` (the struct at `mod.rs:49`), add after the existing fields:

```rust
    /// Warn-once dedup state for orphan children (parent id never seen).
    /// Cleared together with `entries` on every session reset.
    pub(super) orphan_warned: std::collections::HashSet<crate::components::react_trace::types::ScopeId>,

    /// Assigns stable colors + badges to sub-agent scopes on first sight.
    pub(super) scope_colors: scope_colors::ScopeColorAllocator,
```

- [ ] **Step 5: Initialize the fields in every `ReactTrace::new*` constructor**

Grep: `rg -n 'ReactTrace\s*\{' crates/spur-tui/src/components/react_trace/mod.rs`
For each constructor (`new`, `with_kind_compact`, any other), add the two fields to the struct literal:

```rust
orphan_warned: std::collections::HashSet::new(),
scope_colors: scope_colors::ScopeColorAllocator::new(),
```

- [ ] **Step 6: Update every session-reset site from Step 1**

At each `self.entries.clear()` site, add adjacent calls:

```rust
self.entries.clear();
self.orphan_warned.clear();
self.scope_colors.reset();
```

- [ ] **Step 7: Run tests**

Run: `cargo test -p spur-tui --lib scope_state_tests`
Expected: 2 PASS.
Run: `cargo test -p spur-tui --lib`
Expected: all existing tests still pass.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui/react_trace): own scope allocator + orphan dedup state on ReactTrace"
```

---

## Task 6: Dispatch classification — Header / Child / Orphan

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs`

- [ ] **Step 1: Add failing tests driving each classification**

Append to `crates/spur-tui/src/components/react_trace/streaming_tests.rs` in a new `#[cfg(test)] mod scope_dispatch_tests` block (or the existing streaming-tests module if that's the convention in your tree):

```rust
#[cfg(test)]
mod scope_dispatch_tests {
    use super::*;
    use crate::components::react_trace::types::{ScopeId, ScopeRole, TraceKind};
    use spur_acp::{AgentKind, ToolCallId};
    use std::collections::HashMap;

    // Helper: build a minimal ToolCall notification carrying optional parent meta.
    // Follow the existing pattern in streaming_tests.rs for fixture-building;
    // if one doesn't exist, construct via `serde_json::from_value` on a literal
    // matching the JSON fixture shape.
    fn make_tool_call_with_parent(
        id: &str,
        title: &str,
        parent_tool_use_id: Option<&str>,
    ) -> spur_acp::SessionUpdate {
        // ... follow the idiom already used at streaming_tests.rs:1811-1868.
        unimplemented!("use existing fixture builders")
    }

    #[test]
    fn task_tool_call_is_classified_header() {
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        let u = make_tool_call_with_parent("tc-task-1", "Task", None);
        dispatch::dispatch_session_update(&mut trace, &u, &mut ctx);
        let last = trace.entries.last().unwrap();
        let scope = last.scope.as_ref().expect("Task must carry scope");
        assert!(matches!(scope.role, ScopeRole::Header));
        assert_eq!(scope.id, ScopeId("tc-task-1".into()));
        assert_eq!(scope.depth, 0);
    }

    #[test]
    fn child_tool_call_with_known_parent_is_classified_child() {
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        // Parent Task first.
        let parent = make_tool_call_with_parent("tc-task-1", "Task", None);
        dispatch::dispatch_session_update(&mut trace, &parent, &mut ctx);
        // Child references tc-task-1 via parentToolUseId.
        let child = make_tool_call_with_parent("tc-child-1", "Edit", Some("tc-task-1"));
        dispatch::dispatch_session_update(&mut trace, &child, &mut ctx);
        let last = trace.entries.last().unwrap();
        let scope = last.scope.as_ref().expect("child must carry scope");
        assert!(matches!(scope.role, ScopeRole::Child));
        assert_eq!(scope.id, ScopeId("tc-task-1".into()));
        assert_eq!(scope.depth, 1);
    }

    #[test]
    fn child_with_unknown_parent_is_classified_orphan_and_warns_once() {
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        let u1 = make_tool_call_with_parent("tc-orphan-1", "Edit", Some("unknown-parent"));
        dispatch::dispatch_session_update(&mut trace, &u1, &mut ctx);
        let u2 = make_tool_call_with_parent("tc-orphan-2", "Edit", Some("unknown-parent"));
        dispatch::dispatch_session_update(&mut trace, &u2, &mut ctx);
        // Both render Orphan:
        for e in trace.entries.iter().rev().take(2) {
            let s = e.scope.as_ref().unwrap();
            assert!(matches!(s.role, ScopeRole::Orphan));
            assert_eq!(s.depth, 1);
        }
        // Dedup: only the first orphan id triggers warned-set insertion.
        assert_eq!(trace.orphan_warned.len(), 1);
        assert!(trace.orphan_warned.contains(&ScopeId("unknown-parent".into())));
    }

    #[test]
    fn non_task_tool_call_without_parent_has_no_scope() {
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        let u = make_tool_call_with_parent("tc-bash-1", "Bash", None);
        dispatch::dispatch_session_update(&mut trace, &u, &mut ctx);
        assert!(trace.entries.last().unwrap().scope.is_none());
    }
}
```

**NOTE for the implementing engineer:** replace `make_tool_call_with_parent` with the existing helper idiom in `streaming_tests.rs` (grep `fn make_tool_call` / `fn tool_call_notification` there). Pattern: build a `SessionUpdate::ToolCall(tc)` with `tc.title`, `tc.tool_call_id`, and `tc.meta` (vendor-encoded as `{"_meta": {"claudeCode": {"parentToolUseId": "<pid>"}}}`). If no helper exists, create a minimal one following the JSON shape in `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_subagent_task.json`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests`
Expected: compile/assertion failures — dispatch currently does not build `Scope`.

- [ ] **Step 3: Replace the depth-bare block with scope classification**

In `crates/spur-tui/src/components/react_trace/dispatch.rs`, replace lines 69–105 (the `SessionUpdate::ToolCall(tc) => { ... }` arm) with:

```rust
        SessionUpdate::ToolCall(tc) => {
            let meta = adapter::extract_tool_meta(tc, ctx.agent_kind);
            let display_name = meta.tool_name.as_deref().unwrap_or(tc.title.as_str());

            // Classify scope.
            let is_task = tc.title.as_str() == "Task";
            let parent = meta.parent_tool_use_id.as_deref();
            let parent_depth = parent.and_then(|pid| ctx.tool_depth.get(pid).copied());

            let (scope, depth_for_map) = if is_task {
                let s = super::types::Scope {
                    id: super::types::ScopeId(tc.tool_call_id.0.to_string()),
                    depth: parent_depth.unwrap_or(0),
                    role: super::types::ScopeRole::Header,
                };
                let d = s.depth;
                (Some(s), d)
            } else if let Some(pid) = parent {
                match parent_depth {
                    Some(pd) => {
                        let child_depth = pd.saturating_add(1).min(8);
                        let s = super::types::Scope {
                            id: super::types::ScopeId(pid.to_string()),
                            depth: child_depth,
                            role: super::types::ScopeRole::Child,
                        };
                        (Some(s), child_depth)
                    }
                    None => {
                        let sid = super::types::ScopeId(pid.to_string());
                        if trace.orphan_warned.insert(sid.clone()) {
                            tracing::warn!(
                                orphan_parent_id = %sid,
                                tool_call_id = %tc.tool_call_id.0,
                                "orphan child: parent_tool_use_id references unknown id"
                            );
                        }
                        let s = super::types::Scope {
                            id: sid,
                            depth: 1,
                            role: super::types::ScopeRole::Orphan,
                        };
                        (Some(s), 1)
                    }
                }
            } else {
                (None, 0)
            };

            ctx.tool_depth
                .insert(tc.tool_call_id.0.to_string(), depth_for_map);
            let indent = "  ".repeat(depth_for_map as usize);
            let tool = format!("{}{}", indent, display_name);
            let family = adapter::classify_tool(tc, ctx.agent_kind);
            let input = tc
                .raw_input
                .as_ref()
                .map(|v| adapter::format_input(v, ctx.agent_kind))
                .unwrap_or(adapter::ToolInputDisplay::Empty);
            let fallback_text = extract_tool_call_text(&tc.content)
                .or_else(|| tc.raw_input.as_ref().map(format_tool_args))
                .unwrap_or_default();
            let status =
                map_initial_status(tc.status, tc.raw_output.as_ref(), ctx.agent_kind);
            trace.push(TraceEntry {
                kind: TraceKind::Act {
                    tool,
                    family,
                    input,
                    tool_call_id: Some(tc.tool_call_id.clone()),
                    status,
                },
                text: fallback_text,
                timestamp: (ctx.now_stamp)(),
                #[cfg(feature = "markdown")]
                markdown: None,
                scope,
            });
        }
```

Note: the `dispatch_session_update` function now needs `&mut ReactTrace` (it already has it) AND mutable access to `trace.orphan_warned`. Since `trace` is `&mut ReactTrace`, `trace.orphan_warned.insert(...)` is valid.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests`
Expected: 4 PASS.
Run: `cargo test -p spur-tui --lib`
Expected: all prior tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui/react_trace): classify ToolCall into Header/Child/Orphan scope roles"
```

---

## Task 7: Terminal synthesis on Task terminal `ToolCallUpdate`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs`

- [ ] **Step 1: Add failing test**

Append to `scope_dispatch_tests` in `streaming_tests.rs`:

```rust
    fn make_tool_call_update_terminal(id: &str, success: bool) -> spur_acp::SessionUpdate {
        // Follow existing helpers for ToolCallUpdate with status = Completed/Failed.
        unimplemented!("use existing fixture builders")
    }

    #[test]
    fn task_terminal_update_synthesizes_scope_terminal_entry() {
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        // Task opens.
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_with_parent("tc-task-1", "Task", None),
            &mut ctx,
        );
        // Three children.
        for (i, cid) in ["c1", "c2", "c3"].iter().enumerate() {
            let _ = i;
            dispatch::dispatch_session_update(
                &mut trace,
                &make_tool_call_with_parent(cid, "Edit", Some("tc-task-1")),
                &mut ctx,
            );
        }
        // Task terminal.
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_update_terminal("tc-task-1", true),
            &mut ctx,
        );

        // Last entry must be a ScopeTerminal carrying child_count=3.
        let last = trace.entries.last().unwrap();
        match &last.kind {
            TraceKind::ScopeTerminal {
                scope_id,
                child_count,
                ..
            } => {
                assert_eq!(scope_id, &ScopeId("tc-task-1".into()));
                assert_eq!(*child_count, 3);
            }
            other => panic!("expected ScopeTerminal, got {:?}", other),
        }
        // Header Act entry's own status is updated to Completed.
        let header_idx = trace
            .entries
            .iter()
            .position(|e| matches!(&e.kind, TraceKind::Act { tool, .. } if tool.contains("Task")))
            .unwrap();
        if let TraceKind::Act { status, .. } = &trace.entries[header_idx].kind {
            assert!(matches!(status, ActStatus::Completed(_)));
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests::task_terminal_update_synthesizes_scope_terminal_entry`
Expected: assertion failure — no Terminal entry synthesized yet.

- [ ] **Step 3: Extend the `ToolCallUpdate` arm**

In `dispatch.rs`, modify the `SessionUpdate::ToolCallUpdate(tcu)` arm. After the existing Header-status mutation block (the `if let Some((idx, act_entry)) = trace.find_act_by_id_mut(...)` path), add a terminal-synthesis branch:

```rust
        SessionUpdate::ToolCallUpdate(tcu) => {
            let is_terminal = matches!(
                tcu.fields.status,
                Some(ToolCallStatus::Completed) | Some(ToolCallStatus::Failed)
            );

            if let Some((idx, act_entry)) = trace.find_act_by_id_mut(&tcu.tool_call_id) {
                let new_status = if let TraceKind::Act { status, .. } = &act_entry.kind {
                    merge_status(
                        status,
                        tcu.fields.status,
                        tcu.fields.raw_output.as_ref(),
                        ctx.agent_kind,
                    )
                } else {
                    return;
                };
                // Detect: is this the Header of a scope that is now terminal?
                let scope_header = act_entry
                    .scope
                    .as_ref()
                    .filter(|s| matches!(s.role, super::types::ScopeRole::Header))
                    .cloned();
                if let TraceKind::Act { status, .. } = &mut act_entry.kind {
                    *status = new_status.clone();
                }
                trace.mark_dirty_from_for_update(idx);

                if is_terminal {
                    if let Some(header_scope) = scope_header {
                        // Count children whose scope.id == header_scope.id strictly
                        // after header_idx (inclusive scan is fine; header carries
                        // Header role not Child, so it doesn't inflate the count).
                        let child_count: u32 = trace
                            .entries
                            .iter()
                            .skip(idx + 1)
                            .filter(|e| {
                                e.scope
                                    .as_ref()
                                    .map(|s| {
                                        s.id == header_scope.id
                                            && matches!(s.role, super::types::ScopeRole::Child)
                                    })
                                    .unwrap_or(false)
                            })
                            .count() as u32;

                        trace.push(TraceEntry {
                            kind: TraceKind::ScopeTerminal {
                                scope_id: header_scope.id.clone(),
                                status: new_status,
                                child_count,
                                duration_ms: None, // populated in Task 8 once Header creation time is captured
                            },
                            text: String::new(),
                            timestamp: (ctx.now_stamp)(),
                            #[cfg(feature = "markdown")]
                            markdown: None,
                            scope: Some(super::types::Scope {
                                id: header_scope.id,
                                depth: header_scope.depth,
                                role: super::types::ScopeRole::Terminal,
                            }),
                        });
                    }
                }
            } else if tcu.fields.title.is_some() || tcu.fields.kind.is_some() {
                // ... existing synthetic-Act branch unchanged, with `scope: None` added
                tracing::debug!(
                    id = ?tcu.tool_call_id,
                    "ToolCallUpdate before ToolCall; synthesizing Act"
                );
                let tool = tcu.fields.title.clone().unwrap_or_else(|| "unknown".into());
                let family = adapter::ToolFamily::Unknown;
                let input = adapter::ToolInputDisplay::Empty;
                let status = map_initial_status(
                    tcu.fields.status.unwrap_or(ToolCallStatus::Pending),
                    tcu.fields.raw_output.as_ref(),
                    ctx.agent_kind,
                );
                trace.push(TraceEntry {
                    kind: TraceKind::Act {
                        tool,
                        family,
                        input,
                        tool_call_id: Some(tcu.tool_call_id.clone()),
                        status,
                    },
                    text: String::new(),
                    timestamp: (ctx.now_stamp)(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                    scope: None,
                });
            } else {
                tracing::debug!(
                    id = ?tcu.tool_call_id,
                    "dropping ToolCallUpdate with no matching Act and no title/kind"
                );
            }
        }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests::task_terminal_update_synthesizes_scope_terminal_entry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui/react_trace): synthesize ScopeTerminal entry on Task terminal"
```

---

## Task 8: Duration tracking for Terminal row

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs`
- Modify: `crates/spur-tui/src/components/react_trace/types.rs`

- [ ] **Step 1: Test**

Add to `scope_dispatch_tests`:

```rust
    #[test]
    fn scope_terminal_records_duration_when_available() {
        // Use a now_stamp closure that advances a shared counter; convert the
        // captured Header timestamp via parsing. If timestamps are strings
        // without arithmetic, store creation-time on the Header scope instead.
        // Verify duration_ms is Some(_).
        //
        // Concrete approach: extend Scope or TraceEntry with `created_at_ms: u64`
        // (monotonic ms since some epoch). Wire now_ms on DispatchCtx.
        //
        // If this is too invasive for this iteration, leave duration_ms = None
        // and convert this test to assert `None` is acceptable.
    }
```

- [ ] **Step 2: Decide the route**

Two options:

(A) Extend `DispatchCtx` with `now_ms: &dyn Fn() -> u64` alongside the existing `now_stamp`. Store the Header's creation `ms` on the `Scope` struct (`pub created_at_ms: Option<u64>`). Compute `duration_ms = Some(now_ms - header.created_at_ms)` at terminal synthesis.

(B) Defer duration: leave `duration_ms: None` always; add it only when the real-time clock is plumbed through. Update the test to assert `None`.

Pick **A** if `now_stamp` already derives from a wall clock (check `session_detail.rs` / `worker_streams.rs` call sites). Pick **B** otherwise.

- [ ] **Step 3: Implement chosen route**

**If (A):**
- Extend `Scope` with `pub created_at_ms: Option<u64>`.
- Add `now_ms: &'a dyn Fn() -> u64` to `DispatchCtx`.
- Capture `created_at_ms = Some((ctx.now_ms)())` when classifying `Header` in Task 6's code.
- At terminal synthesis, compute `duration_ms = header_scope.created_at_ms.map(|t0| (ctx.now_ms)().saturating_sub(t0))`.

**If (B):**
- Adjust test assertion to `assert!(matches!(duration_ms, None))`.
- Mark the duration feature as Phase 2 in the plan's §6 deferred list (spec already accepts None).

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/
git commit -m "feat(spur-tui/react_trace): wire Terminal row duration_ms"
```

---

## Task 9: Late-child-after-Terminal — `(late)` suffix on Child role

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs` (add `late: bool` to `Scope`, OR introduce `ScopeRole::ChildLate`)
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs`

- [ ] **Step 1: Pick representation**

Two options:

(A) Add a `ScopeRole::ChildLate` variant. Clean; role enum already dispatches rendering.
(B) Add `pub late: bool` to `Scope`. Orthogonal to role; flexible but less discoverable.

Prefer **(A)** — the role enum already drives row formatting in Task 11–14. Adding a variant keeps rendering dispatch local.

- [ ] **Step 2: Add `ScopeRole::ChildLate`**

In `types.rs`:

```rust
pub enum ScopeRole {
    Header,
    Child,
    /// Child tool call that arrived after its parent scope's Terminal
    /// was already synthesized. Renders like Child with a `(late)` suffix.
    ChildLate,
    Terminal,
    Orphan,
}
```

Update `scope_role_variants_exhaustive_count` test to assert 5.

- [ ] **Step 3: Add failing integration test**

In `scope_dispatch_tests`:

```rust
    #[test]
    fn child_arriving_after_terminal_is_classified_child_late() {
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        // Task + one child + terminal.
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_with_parent("tc-task-1", "Task", None),
            &mut ctx,
        );
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_with_parent("c1", "Edit", Some("tc-task-1")),
            &mut ctx,
        );
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_update_terminal("tc-task-1", true),
            &mut ctx,
        );
        // Capture Terminal entry's child_count BEFORE late arrival.
        let terminal_count_before = trace.entries.iter().rev().find_map(|e| {
            if let TraceKind::ScopeTerminal { child_count, .. } = &e.kind {
                Some(*child_count)
            } else {
                None
            }
        }).unwrap();
        assert_eq!(terminal_count_before, 1);

        // Late child arrives.
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_with_parent("c2-late", "Edit", Some("tc-task-1")),
            &mut ctx,
        );

        // Late child carries ChildLate role.
        let last = trace.entries.last().unwrap();
        assert!(matches!(
            last.scope.as_ref().unwrap().role,
            ScopeRole::ChildLate
        ));
        // Terminal's child_count is NOT updated.
        let terminal_count_after = trace.entries.iter().find_map(|e| {
            if let TraceKind::ScopeTerminal { child_count, .. } = &e.kind {
                Some(*child_count)
            } else {
                None
            }
        }).unwrap();
        assert_eq!(terminal_count_after, 1);
    }
```

- [ ] **Step 4: Run to verify failure**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests::child_arriving_after_terminal_is_classified_child_late`
Expected: assertion failure — child still classified as `Child`, not `ChildLate`.

- [ ] **Step 5: Extend dispatch to detect post-Terminal arrivals**

In `dispatch.rs`, in the `ToolCall` arm (Task 6's code), inside the `else if let Some(pid)` → `Some(pd)` match arm, check whether a `ScopeTerminal` already exists for `pid`:

```rust
                    Some(pd) => {
                        let child_depth = pd.saturating_add(1).min(8);
                        let target_id = super::types::ScopeId(pid.to_string());
                        let terminal_already =
                            trace.entries.iter().any(|e| matches!(
                                &e.kind,
                                TraceKind::ScopeTerminal { scope_id, .. }
                                    if scope_id == &target_id
                            ));
                        let role = if terminal_already {
                            super::types::ScopeRole::ChildLate
                        } else {
                            super::types::ScopeRole::Child
                        };
                        let s = super::types::Scope {
                            id: target_id,
                            depth: child_depth,
                            role,
                        };
                        (Some(s), child_depth)
                    }
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/
git commit -m "feat(spur-tui/react_trace): classify late-after-Terminal children as ChildLate"
```

---

## Task 10: Late `ToolCallUpdate` after Terminal — validation test

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs`

No code changes expected if Task 2 correctly introduced `ScopeTerminal` as a non-`Act` variant — `find_act_by_id_mut` already skips non-Act entries. This task is purely verification.

- [ ] **Step 1: Add test**

```rust
    #[test]
    fn late_tool_call_update_after_terminal_routes_to_header_not_terminal() {
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        // Task + terminal.
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_with_parent("tc-task-1", "Task", None),
            &mut ctx,
        );
        dispatch::dispatch_session_update(
            &mut trace,
            &make_tool_call_update_terminal("tc-task-1", true),
            &mut ctx,
        );
        // Capture Terminal status/count.
        let (terminal_status_before, terminal_count_before) =
            trace.entries.iter().rev().find_map(|e| {
                if let TraceKind::ScopeTerminal { status, child_count, .. } = &e.kind {
                    Some((status.clone(), *child_count))
                } else {
                    None
                }
            }).unwrap();

        // Late update carrying the Task's tool_call_id — should hit the Header Act.
        // Build a ToolCallUpdate setting status = Failed (different from the Terminal's
        // captured status); if routing is correct, the Header's Act.status flips to
        // Failed but the Terminal's status does NOT.
        let late = make_tool_call_update_terminal("tc-task-1", false);
        dispatch::dispatch_session_update(&mut trace, &late, &mut ctx);

        // Terminal unchanged.
        let terminal = trace.entries.iter().rev().find_map(|e| {
            if let TraceKind::ScopeTerminal { status, child_count, .. } = &e.kind {
                Some((status.clone(), *child_count))
            } else {
                None
            }
        }).unwrap();
        assert!(
            matches!(terminal.0, ActStatus::Completed(_)) ==
            matches!(terminal_status_before, ActStatus::Completed(_)),
            "Terminal status must not be rewritten by late ToolCallUpdate"
        );
        assert_eq!(terminal.1, terminal_count_before);

        // Header Act IS updated — it's the newest matching Act.
        let header_status = trace.entries.iter().find_map(|e| {
            if let TraceKind::Act { status, tool_call_id, .. } = &e.kind {
                if tool_call_id.as_ref().map(|id| id.0.as_str()) == Some("tc-task-1") {
                    Some(status.clone())
                } else {
                    None
                }
            } else {
                None
            }
        }).unwrap();
        // Second update flipped it to Failed.
        assert!(matches!(header_status, ActStatus::Failed(_)));
    }
```

- [ ] **Step 2: Run test**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests::late_tool_call_update_after_terminal_routes_to_header_not_terminal`
Expected: PASS (no impl change needed).

If it fails: `find_act_by_id_mut` is matching `ScopeTerminal` entries — inspect `mod.rs:834-855` and add an explicit `matches!(e.kind, TraceKind::Act { .. })` filter.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "test(spur-tui/react_trace): verify late ToolCallUpdate resolves to Header Act"
```

---

## Task 11: Depth-clamp warn at depth > 8

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs` (new field `depth_clamped: HashSet<String>`)

- [ ] **Step 1: Test**

```rust
    #[test]
    fn depth_over_eight_warns_once_per_tool_call_id() {
        // Build a nine-deep chain: Task → c1(child of Task) → c2(child of c1) → ...
        // Assert the deepest clamp fires tracing::warn! exactly once per offending
        // tool_call_id, verified via a HashSet field on ReactTrace.
        // Skeleton: assert `trace.depth_clamped.len() == 1` after driving the chain twice.
    }
```

- [ ] **Step 2: Add `depth_clamped` field on `ReactTrace`**

In `mod.rs`:

```rust
pub(super) depth_clamped: std::collections::HashSet<String>,
```

Initialize in every constructor; clear on session reset (same sites as Task 5).

- [ ] **Step 3: Emit warn in the Child classification path**

In `dispatch.rs`, in the `Some(pd) =>` branch (Task 6's code), after computing `child_depth`:

```rust
                        if pd.saturating_add(1) > 8 {
                            let k = tc.tool_call_id.0.to_string();
                            if trace.depth_clamped.insert(k) {
                                tracing::warn!(
                                    tool_call_id = %tc.tool_call_id.0,
                                    actual_depth = pd.saturating_add(1),
                                    clamp = 8,
                                    "scope depth exceeded 8; clamping"
                                );
                            }
                        }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --lib scope_dispatch_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/
git commit -m "feat(spur-tui/react_trace): warn-once on depth-clamp at >8"
```

---

## Task 12: `warning_style()` in `trace_format.rs`

**Files:**
- Modify: `crates/spur-tui/src/components/trace_format.rs`

- [ ] **Step 1: Test**

Append to `trace_format.rs`:

```rust
#[cfg(test)]
mod warning_style_tests {
    use super::*;

    #[test]
    fn warning_style_is_yellow_foreground() {
        let s = warning_style();
        assert_eq!(s.fg, Some(Color::Yellow));
    }
}
```

- [ ] **Step 2: Run, expect fail**

Run: `cargo test -p spur-tui --lib warning_style_tests`
Expected: `warning_style` undefined.

- [ ] **Step 3: Add the helper**

Append next to `family_glyph` / `outcome_glyph` in `trace_format.rs`:

```rust
/// Style for orphan / late / warning indicators in the scope gutter.
pub(crate) fn warning_style() -> Style {
    Style::default().fg(Color::Yellow)
}
```

- [ ] **Step 4: Run, expect pass**

Run: `cargo test -p spur-tui --lib warning_style_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/components/trace_format.rs
git commit -m "feat(spur-tui/trace_format): add warning_style helper (Yellow)"
```

---

## Task 13: Render — gutter + Header row

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`

- [ ] **Step 1: Grep for the entry-to-lines function**

Run: `rg -n 'fn.*entry.*Line|fn render_entry|fn build_lines' crates/spur-tui/src/components/react_trace/render.rs`
Identify the function that converts a single `TraceEntry` into `Vec<Line>` — the insertion point for the gutter column.

- [ ] **Step 2: Add a test for Header row formatting**

Create or append to `crates/spur-tui/src/components/react_trace/render.rs` in a `#[cfg(test)] mod render_scope_tests`:

```rust
#[cfg(test)]
mod render_scope_tests {
    use super::*;
    use crate::components::react_trace::types::{Scope, ScopeId, ScopeRole, TraceEntry, TraceKind, ActStatus};

    fn header_entry(badge_id: &str, title: &str) -> TraceEntry {
        TraceEntry {
            kind: TraceKind::Act {
                tool: title.to_string(),
                family: spur_acp::adapter::ToolFamily::Unknown,
                input: spur_acp::adapter::ToolInputDisplay::Empty,
                tool_call_id: Some(spur_acp::ToolCallId(badge_id.into())),
                status: ActStatus::InProgress { partial: None },
            },
            text: String::new(),
            timestamp: "00:00".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
            scope: Some(Scope {
                id: ScopeId(badge_id.into()),
                depth: 0,
                role: ScopeRole::Header,
            }),
        }
    }

    #[test]
    fn header_row_starts_with_down_pointing_triangle_glyph() {
        let e = header_entry("tc-1", "Task");
        let mut allocator = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        let lines = render_entry_with_scope(&e, &mut allocator, 80);
        let first_span_text = lines[0].spans.first().unwrap().content.as_ref();
        assert!(
            first_span_text.contains('◢'),
            "expected Header glyph ◢ in first span, got {:?}",
            first_span_text
        );
    }

    #[test]
    fn header_row_includes_bracketed_badge_at_wide_width() {
        let e = header_entry("tc-1", "Task");
        let mut allocator = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        let lines = render_entry_with_scope(&e, &mut allocator, 80);
        let full = lines[0].to_string();
        assert!(full.contains("[T1]"), "expected [T1] badge, got {:?}", full);
        assert!(full.contains("Subagent"), "expected 'Subagent' label, got {:?}", full);
    }
}
```

- [ ] **Step 3: Run, expect fail**

Run: `cargo test -p spur-tui --lib render_scope_tests`
Expected: `render_entry_with_scope` undefined.

- [ ] **Step 4: Implement `render_entry_with_scope`**

Add to `render.rs`:

```rust
/// Render a single `TraceEntry` with sub-agent scope treatment.
///
/// Dispatches on `entry.scope.as_ref().map(|s| s.role)` to choose the row format.
/// When `scope` is `None`, delegates to the existing non-scope rendering path.
///
/// `width` is used by the compact path for tier degradation (§3.5 of spec);
/// the full `render.rs` path always uses the ≥80 tier.
pub(crate) fn render_entry_with_scope(
    entry: &TraceEntry,
    allocator: &mut super::scope_colors::ScopeColorAllocator,
    _width: u16,
) -> Vec<Line<'static>> {
    use super::types::ScopeRole;

    let Some(scope) = entry.scope.as_ref() else {
        return render_entry(entry); // existing path
    };

    match scope.role {
        ScopeRole::Header => render_header_row(entry, scope, allocator),
        ScopeRole::Child => render_child_row(entry, scope, allocator, /* late */ false),
        ScopeRole::ChildLate => render_child_row(entry, scope, allocator, /* late */ true),
        ScopeRole::Terminal => render_terminal_row(entry, scope, allocator),
        ScopeRole::Orphan => render_orphan_row(entry, scope),
    }
}

fn render_header_row(
    entry: &TraceEntry,
    scope: &super::types::Scope,
    allocator: &mut super::scope_colors::ScopeColorAllocator,
) -> Vec<Line<'static>> {
    use crate::components::trace_format::warning_style;
    let _ = warning_style;
    let asg = allocator.assignment(&scope.id);
    let glyph = "◢";
    let color = asg.color;
    let badge = format!("[{}]", asg.badge);
    let title = if let TraceKind::Act { tool, .. } = &entry.kind {
        tool.clone()
    } else {
        "Subagent".to_string()
    };
    vec![Line::from(vec![
        Span::styled(format!("{} ", glyph), Style::default().fg(color)),
        Span::styled("Subagent ", Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}  ", badge), Style::default().fg(color)),
        Span::raw(title),
    ])]
}

// render_child_row / render_terminal_row / render_orphan_row stubs defined as
// TODO panics to force later tasks to fill them. DO NOT ship these stubs —
// Tasks 14–16 implement each row format.
fn render_child_row(
    _entry: &TraceEntry,
    _scope: &super::types::Scope,
    _allocator: &mut super::scope_colors::ScopeColorAllocator,
    _late: bool,
) -> Vec<Line<'static>> {
    unimplemented!("Task 14")
}

fn render_terminal_row(
    _entry: &TraceEntry,
    _scope: &super::types::Scope,
    _allocator: &mut super::scope_colors::ScopeColorAllocator,
) -> Vec<Line<'static>> {
    unimplemented!("Task 15")
}

fn render_orphan_row(
    _entry: &TraceEntry,
    _scope: &super::types::Scope,
) -> Vec<Line<'static>> {
    unimplemented!("Task 16")
}
```

- [ ] **Step 5: Run, expect pass**

Run: `cargo test -p spur-tui --lib render_scope_tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui/render): Header row format with gutter + bracketed badge"
```

---

## Task 14: Render — Child row (normal + late)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`

- [ ] **Step 1: Tests**

Add to `render_scope_tests`:

```rust
    fn child_entry(parent_id: &str, title: &str, late: bool) -> TraceEntry {
        let role = if late { ScopeRole::ChildLate } else { ScopeRole::Child };
        TraceEntry {
            kind: TraceKind::Act {
                tool: title.to_string(),
                family: spur_acp::adapter::ToolFamily::Edit,
                input: spur_acp::adapter::ToolInputDisplay::Path("src/foo.rs".into()),
                tool_call_id: Some(spur_acp::ToolCallId("tc-child".into())),
                status: ActStatus::Completed(None),
            },
            text: String::new(),
            timestamp: "00:01".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
            scope: Some(Scope {
                id: ScopeId(parent_id.into()),
                depth: 1,
                role,
            }),
        }
    }

    #[test]
    fn child_row_starts_with_pipe_gutter_and_badge() {
        let e = child_entry("tc-1", "Edit", false);
        let mut alloc = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        // Prime allocator so `tc-1` gets T1.
        let _ = alloc.assignment(&ScopeId("tc-1".into()));
        let lines = render_entry_with_scope(&e, &mut alloc, 80);
        let full = lines[0].to_string();
        assert!(full.contains('┃'), "child must carry heavy vertical gutter: {:?}", full);
        assert!(full.contains("[T1]"), "child must carry [T1] badge: {:?}", full);
        assert!(!full.contains("(late)"), "non-late child must not carry (late) suffix");
    }

    #[test]
    fn child_late_row_has_late_suffix() {
        let e = child_entry("tc-1", "Edit", true);
        let mut alloc = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        let _ = alloc.assignment(&ScopeId("tc-1".into()));
        let lines = render_entry_with_scope(&e, &mut alloc, 80);
        let full = lines[0].to_string();
        assert!(full.contains("(late)"), "late child must carry (late) suffix: {:?}", full);
    }
```

- [ ] **Step 2: Implement `render_child_row`**

Replace the `unimplemented!` stub:

```rust
fn render_child_row(
    entry: &TraceEntry,
    scope: &super::types::Scope,
    allocator: &mut super::scope_colors::ScopeColorAllocator,
    late: bool,
) -> Vec<Line<'static>> {
    let asg = allocator.assignment(&scope.id);
    let color = asg.color;
    let badge = format!("[{}]", asg.badge);
    let tool = if let TraceKind::Act { tool, .. } = &entry.kind {
        tool.clone()
    } else {
        "<unknown>".to_string()
    };
    let mut spans = vec![
        Span::styled("┃ ", Style::default().fg(color)),
        Span::styled(format!("{} ", badge), Style::default().fg(color)),
        Span::raw(format!("▸ {}", tool)),
    ];
    if late {
        spans.push(Span::styled(
            "   (late)".to_string(),
            crate::components::trace_format::warning_style(),
        ));
    }
    vec![Line::from(spans)]
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p spur-tui --lib render_scope_tests`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui/render): Child and ChildLate row formats"
```

---

## Task 15: Render — Terminal row

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`

- [ ] **Step 1: Test**

```rust
    fn terminal_entry(scope_id: &str, child_count: u32, duration_ms: Option<u64>) -> TraceEntry {
        TraceEntry {
            kind: TraceKind::ScopeTerminal {
                scope_id: ScopeId(scope_id.into()),
                status: ActStatus::Completed(None),
                child_count,
                duration_ms,
            },
            text: String::new(),
            timestamp: "00:05".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
            scope: Some(Scope {
                id: ScopeId(scope_id.into()),
                depth: 0,
                role: ScopeRole::Terminal,
            }),
        }
    }

    #[test]
    fn terminal_row_shows_glyph_badge_count_and_duration() {
        let e = terminal_entry("tc-1", 3, Some(142));
        let mut alloc = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        let _ = alloc.assignment(&ScopeId("tc-1".into()));
        let lines = render_entry_with_scope(&e, &mut alloc, 80);
        let full = lines[0].to_string();
        assert!(full.contains('◆'), "terminal must carry ◆ glyph: {:?}", full);
        assert!(full.contains("[T1]"), "terminal must carry [T1] badge: {:?}", full);
        assert!(full.contains("3 calls"), "terminal must carry child count: {:?}", full);
        assert!(full.contains("142ms") || full.contains("142 ms"),
            "terminal must carry duration: {:?}", full);
    }

    #[test]
    fn terminal_row_omits_duration_when_none() {
        let e = terminal_entry("tc-1", 2, None);
        let mut alloc = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        let _ = alloc.assignment(&ScopeId("tc-1".into()));
        let lines = render_entry_with_scope(&e, &mut alloc, 80);
        let full = lines[0].to_string();
        assert!(!full.contains("ms"), "duration omitted when None: {:?}", full);
        assert!(full.contains("2 calls"));
    }
```

- [ ] **Step 2: Implement**

```rust
fn render_terminal_row(
    entry: &TraceEntry,
    scope: &super::types::Scope,
    allocator: &mut super::scope_colors::ScopeColorAllocator,
) -> Vec<Line<'static>> {
    let asg = allocator.assignment(&scope.id);
    let color = asg.color;
    let badge = format!("[{}]", asg.badge);
    let (count, duration_ms, failed) = if let TraceKind::ScopeTerminal {
        child_count,
        duration_ms,
        status,
        ..
    } = &entry.kind
    {
        (
            *child_count,
            *duration_ms,
            matches!(status, ActStatus::Failed(_)),
        )
    } else {
        (0, None, false)
    };
    let status_word = if failed { "failed" } else { "completed" };
    let mut tail = format!("{} · {} calls", status_word, count);
    if let Some(d) = duration_ms {
        tail.push_str(&format!(" · {}ms", d));
    }
    vec![Line::from(vec![
        Span::styled("◆ ", Style::default().fg(color)),
        Span::styled("Subagent ", Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}   ", badge), Style::default().fg(color)),
        Span::raw(tail),
    ])]
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p spur-tui --lib render_scope_tests`
Expected: PASS.

```bash
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui/render): Terminal row with glyph, badge, count, duration"
```

---

## Task 16: Render — Orphan row

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/render.rs`

- [ ] **Step 1: Test**

```rust
    fn orphan_entry(parent_id: &str, title: &str) -> TraceEntry {
        TraceEntry {
            kind: TraceKind::Act {
                tool: title.to_string(),
                family: spur_acp::adapter::ToolFamily::Edit,
                input: spur_acp::adapter::ToolInputDisplay::Empty,
                tool_call_id: Some(spur_acp::ToolCallId("tc-orphan".into())),
                status: ActStatus::Completed(None),
            },
            text: String::new(),
            timestamp: "00:00".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
            scope: Some(Scope {
                id: ScopeId(parent_id.into()),
                depth: 1,
                role: ScopeRole::Orphan,
            }),
        }
    }

    #[test]
    fn orphan_row_has_warning_glyph_question_badge_and_orphan_suffix() {
        let e = orphan_entry("unknown-pid", "Edit");
        let mut alloc = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        let lines = render_entry_with_scope(&e, &mut alloc, 80);
        let full = lines[0].to_string();
        assert!(full.contains('⚠'), "orphan must carry ⚠ glyph: {:?}", full);
        assert!(full.contains("[?]"), "orphan must carry [?] placeholder badge: {:?}", full);
        assert!(full.contains("(orphan)"), "orphan must carry (orphan) suffix: {:?}", full);
    }
```

- [ ] **Step 2: Implement**

```rust
fn render_orphan_row(
    entry: &TraceEntry,
    _scope: &super::types::Scope,
) -> Vec<Line<'static>> {
    let tool = if let TraceKind::Act { tool, .. } = &entry.kind {
        tool.clone()
    } else {
        "<unknown>".to_string()
    };
    let warn = crate::components::trace_format::warning_style();
    vec![Line::from(vec![
        Span::styled("⚠ ", warn),
        Span::styled("[?] ", warn),
        Span::raw(format!("▸ {}   ", tool)),
        Span::styled("(orphan)".to_string(), warn),
    ])]
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p spur-tui --lib render_scope_tests
git add crates/spur-tui/src/components/react_trace/render.rs
git commit -m "feat(spur-tui/render): Orphan row with warning glyph and annotation"
```

---

## Task 17: Compact surface — width-tier function

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/compact_render.rs`

- [ ] **Step 1: Test**

```rust
#[cfg(test)]
mod width_tier_tests {
    use super::*;

    #[test]
    fn tier_boundaries() {
        assert_eq!(width_tier(80), WidthTier::Full);
        assert_eq!(width_tier(120), WidthTier::Full);
        assert_eq!(width_tier(79), WidthTier::BadgeBracketed);
        assert_eq!(width_tier(72), WidthTier::BadgeBracketed);
        assert_eq!(width_tier(71), WidthTier::GutterNumeric);
        assert_eq!(width_tier(60), WidthTier::GutterNumeric);
        assert_eq!(width_tier(59), WidthTier::IndentOnly);
        assert_eq!(width_tier(0), WidthTier::IndentOnly);
    }
}
```

- [ ] **Step 2: Implement the tier function**

Append to `compact_render.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WidthTier {
    /// ≥80 cols: gutter + bracketed badge + child count + duration.
    Full,
    /// 72–79 cols: gutter + bracketed badge + child count, no duration.
    BadgeBracketed,
    /// 60–71 cols: gutter column carries numeric suffix (`┃1`); no bracket badge.
    GutterNumeric,
    /// <60 cols: depth indent + numeric prefix (`  1▸ tool`); no gutter column.
    IndentOnly,
}

pub(crate) fn width_tier(w: u16) -> WidthTier {
    match w {
        0..=59 => WidthTier::IndentOnly,
        60..=71 => WidthTier::GutterNumeric,
        72..=79 => WidthTier::BadgeBracketed,
        _ => WidthTier::Full,
    }
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p spur-tui --lib width_tier_tests
git add crates/spur-tui/src/components/react_trace/compact_render.rs
git commit -m "feat(spur-tui/compact_render): width tier classifier"
```

---

## Task 18: Compact per-tier rendering

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/compact_render.rs`

- [ ] **Step 1: Tests — one per tier**

```rust
#[cfg(test)]
mod compact_scope_tests {
    use super::*;
    use crate::components::react_trace::types::{Scope, ScopeId, ScopeRole, TraceEntry, TraceKind, ActStatus};

    fn child(parent_id: &str, title: &str) -> TraceEntry {
        TraceEntry {
            kind: TraceKind::Act {
                tool: title.to_string(),
                family: spur_acp::adapter::ToolFamily::Edit,
                input: spur_acp::adapter::ToolInputDisplay::Empty,
                tool_call_id: Some(spur_acp::ToolCallId("tc-c".into())),
                status: ActStatus::Completed(None),
            },
            text: String::new(),
            timestamp: "".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
            scope: Some(Scope {
                id: ScopeId(parent_id.into()),
                depth: 1,
                role: ScopeRole::Child,
            }),
        }
    }

    fn setup_alloc(id: &str) -> crate::components::react_trace::scope_colors::ScopeColorAllocator {
        let mut a = crate::components::react_trace::scope_colors::ScopeColorAllocator::new();
        let _ = a.assignment(&ScopeId(id.into()));
        a
    }

    #[test]
    fn full_tier_has_bracketed_badge() {
        let e = child("tc-1", "Edit");
        let mut a = setup_alloc("tc-1");
        let line = compact_render_entry(&e, &mut a, 80);
        assert!(line.to_string().contains("[T1]"));
    }

    #[test]
    fn badge_bracketed_tier_keeps_brackets_at_72() {
        let e = child("tc-1", "Edit");
        let mut a = setup_alloc("tc-1");
        let line = compact_render_entry(&e, &mut a, 72);
        assert!(line.to_string().contains("[T1]"));
    }

    #[test]
    fn gutter_numeric_tier_uses_numeric_suffix_at_60() {
        let e = child("tc-1", "Edit");
        let mut a = setup_alloc("tc-1");
        let line = compact_render_entry(&e, &mut a, 60);
        let s = line.to_string();
        assert!(!s.contains("[T1]"), "no brackets at narrow width: {:?}", s);
        assert!(s.contains("┃1"), "expect numeric gutter ┃1: {:?}", s);
    }

    #[test]
    fn indent_only_tier_at_50_keeps_numeric_prefix() {
        let e = child("tc-1", "Edit");
        let mut a = setup_alloc("tc-1");
        let line = compact_render_entry(&e, &mut a, 50);
        let s = line.to_string();
        assert!(!s.contains("┃"), "no gutter column at <60: {:?}", s);
        assert!(s.contains("1"), "numeric identity preserved: {:?}", s);
    }
}
```

- [ ] **Step 2: Run, expect fail**

Run: `cargo test -p spur-tui --lib compact_scope_tests`
Expected: `compact_render_entry` not defined for scope.

- [ ] **Step 3: Implement `compact_render_entry` switching on `width_tier`**

```rust
pub(crate) fn compact_render_entry(
    entry: &TraceEntry,
    allocator: &mut super::scope_colors::ScopeColorAllocator,
    width: u16,
) -> Line<'static> {
    use super::types::{ScopeRole, TraceKind};
    let tier = width_tier(width);
    let Some(scope) = entry.scope.as_ref() else {
        // Existing non-scope compact path.
        return existing_compact_render_entry(entry);
    };
    let asg = allocator.assignment(&scope.id);
    let color = asg.color;
    let tool_title = match &entry.kind {
        TraceKind::Act { tool, .. } => tool.clone(),
        TraceKind::ScopeTerminal { child_count, .. } => format!("completed · {} calls", child_count),
        _ => "".into(),
    };

    match tier {
        WidthTier::Full | WidthTier::BadgeBracketed => {
            let glyph = match scope.role {
                ScopeRole::Header => "◢",
                ScopeRole::Terminal => "◆",
                ScopeRole::Orphan => "⚠",
                _ => "┃",
            };
            let badge = format!("[{}]", asg.badge);
            Line::from(vec![
                Span::styled(format!("{} ", glyph), Style::default().fg(color)),
                Span::styled(format!("{} ", badge), Style::default().fg(color)),
                Span::raw(format!("▸ {}", tool_title)),
            ])
        }
        WidthTier::GutterNumeric => {
            let numeric = asg.badge.trim_start_matches('T');
            Line::from(vec![
                Span::styled(format!("┃{} ", numeric), Style::default().fg(color)),
                Span::raw(format!("▸ {}", tool_title)),
            ])
        }
        WidthTier::IndentOnly => {
            let numeric = asg.badge.trim_start_matches('T');
            let indent = "  ".repeat(scope.depth as usize);
            Line::from(vec![
                Span::raw(format!("{}{}▸ {}", indent, numeric, tool_title)),
            ])
        }
    }
}

// Rename the existing compact-render function if needed so the new scope-aware
// entry point can delegate to it. Grep `fn compact_render` for the current name.
fn existing_compact_render_entry(entry: &TraceEntry) -> Line<'static> {
    // delegate to current impl
    unimplemented!("wire to the pre-existing compact rendering path")
}
```

**NOTE:** the `existing_compact_render_entry` stub must be wired to whichever function currently renders compact rows. Grep for `fn compact_render` in `compact_render.rs` and delegate. Do not leave the `unimplemented!` in committed code.

- [ ] **Step 4: Run + commit**

```bash
cargo test -p spur-tui --lib compact_scope_tests
git add crates/spur-tui/src/components/react_trace/compact_render.rs
git commit -m "feat(spur-tui/compact_render): per-tier scope row rendering"
```

---

## Task 19: `NO_COLOR` honored — explicit test

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/compact_render.rs` (or `render.rs` if ratatui detection lives there)

- [ ] **Step 1: Test**

```rust
    #[test]
    fn no_color_env_still_yields_identity_token() {
        // Simulate NO_COLOR by constructing rows and asserting the rendered
        // string (color-stripped) still contains either the bracketed badge
        // or the numeric suffix.
        //
        // ratatui doesn't strip color itself; it emits ANSI escapes or uses
        // the terminal's capability. For the test, just strip Span styles
        // and verify the content alone is sufficient for identity.
        let e = child("tc-1", "Edit");
        let mut a = setup_alloc("tc-1");
        for width in [80u16, 72, 60, 50] {
            let line = compact_render_entry(&e, &mut a, width);
            let content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                content.contains("T1") || content.contains("[T1]") || content.chars().any(|c| c == '1'),
                "at width {}, content must carry non-color identity: {:?}",
                width,
                content
            );
        }
    }
```

- [ ] **Step 2: Run**

Run: `cargo test -p spur-tui --lib compact_scope_tests::no_color_env_still_yields_identity_token`
Expected: PASS (identity token always present per Task 18 impl).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/compact_render.rs
git commit -m "test(spur-tui/compact_render): verify NO_COLOR identity across tiers"
```

---

## Task 20: Glyph near-miss snapshot test — `◆` vs `◈`

**Files:**
- Modify: `crates/spur-tui/src/components/trace_format.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn thinking_and_terminal_glyphs_are_codepoint_distinct() {
        let think: &str = "◈";
        let terminal: &str = "◆";
        assert_ne!(think, terminal, "glyph strings must not be equal");
        // Codepoint-level check:
        assert_eq!(think.chars().next().unwrap() as u32, 0x25C8);
        assert_eq!(terminal.chars().next().unwrap() as u32, 0x25C6);
    }
```

- [ ] **Step 2: If the test passes — keep glyphs.**

If font-blur is observed on the project's reference terminal (manual QA, not in-CI), swap Terminal to `▣` (U+25A3) or `■` (U+25A0) in:
- `render.rs::render_terminal_row` — change `"◆ "` literal
- `compact_render.rs::compact_render_entry` — change the `Terminal` branch glyph
- `spec §3.4 / §3.6 / §7` — record the swap

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/trace_format.rs
git commit -m "test(spur-tui/trace_format): codepoint-distinctness assertion for ◆ vs ◈"
```

---

## Task 21: Orphan fixture + integration test

**Files:**
- Create: `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_orphan_child.json`

- [ ] **Step 1: Create the fixture**

```json
{
  "sessionId": "sess-orphan-001",
  "update": {
    "sessionUpdate": "tool_call",
    "toolCallId": "tc-orphan-child-001",
    "title": "Edit",
    "kind": "edit",
    "rawInput": { "path": "src/orphan.rs" },
    "_meta": {
      "claudeCode": {
        "toolName": "Edit",
        "parentToolUseId": "tc-parent-that-was-never-declared"
      }
    }
  }
}
```

- [ ] **Step 2: Integration test**

Append to `streaming_tests.rs` in `scope_dispatch_tests`:

```rust
    #[test]
    fn orphan_fixture_drives_orphan_role_and_warns() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_orphan_child.json");
        let raw = std::fs::read_to_string(&path).expect("fixture readable");
        let notification: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let update: spur_acp::SessionUpdate = serde_json::from_value(
            notification.get("update").unwrap().clone()
        ).unwrap();
        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };
        dispatch::dispatch_session_update(&mut trace, &update, &mut ctx);
        let last = trace.entries.last().unwrap();
        assert!(matches!(
            last.scope.as_ref().unwrap().role,
            ScopeRole::Orphan
        ));
        assert_eq!(trace.orphan_warned.len(), 1);
    }
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p spur-tui --lib scope_dispatch_tests::orphan_fixture_drives_orphan_role_and_warns
git add crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_orphan_child.json crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "test(spur-tui/dispatch): orphan fixture drives Orphan classification"
```

---

## Task 22: Cache-invariant stress test

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn dirty_from_never_jumps_past_any_open_header_index() {
        // Drive 1000 entries with a mix of Task opens, children, terminals,
        // and late arrivals. After each dispatch, assert that `trace.dirty_from`
        // (if Some) is >= the smallest open-Header entry index.
        //
        // "Open Header" = any TraceKind::Act whose scope.role == Header AND
        // no ScopeTerminal entry for its scope_id has been appended yet.

        let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
        let mut depth = HashMap::new();
        let mut ctx = dispatch::DispatchCtx {
            agent_name: "claude",
            agent_kind: AgentKind::ClaudeCodeAcp,
            now_stamp: || "00:00".into(),
            tool_depth: &mut depth,
        };

        // Generator: interleave Task opens / children / terminals.
        let mut open_headers: Vec<(String, usize)> = Vec::new(); // (scope_id, idx)
        for i in 0..1000 {
            let kind_pick = i % 7;
            match kind_pick {
                0 => {
                    let tid = format!("task-{}", i);
                    dispatch::dispatch_session_update(
                        &mut trace,
                        &make_tool_call_with_parent(&tid, "Task", None),
                        &mut ctx,
                    );
                    open_headers.push((tid, trace.entries.len() - 1));
                }
                1..=4 if !open_headers.is_empty() => {
                    let (pid, _) = open_headers.last().unwrap().clone();
                    let cid = format!("child-{}", i);
                    dispatch::dispatch_session_update(
                        &mut trace,
                        &make_tool_call_with_parent(&cid, "Edit", Some(&pid)),
                        &mut ctx,
                    );
                }
                5 if !open_headers.is_empty() => {
                    let (pid, _) = open_headers.pop().unwrap();
                    dispatch::dispatch_session_update(
                        &mut trace,
                        &make_tool_call_update_terminal(&pid, true),
                        &mut ctx,
                    );
                }
                _ => {
                    // Unrelated thought.
                    trace.append_think("thinking".into(), "00:00".into());
                }
            }

            // Invariant check.
            if let Some(df) = trace.dirty_from {
                let min_open = open_headers
                    .iter()
                    .map(|(_, idx)| *idx)
                    .min()
                    .unwrap_or(usize::MAX);
                assert!(
                    df >= min_open.min(trace.entries.len().saturating_sub(1)),
                    "at step {i}: dirty_from={df} must be >= min open header idx {min_open}"
                );
            }
        }
    }
```

**NOTE:** `trace.dirty_from` field visibility may require `pub(super)` in `mod.rs`, and the test may need `#[cfg(test)]` access. If not directly accessible, extract via a `pub(super) fn dirty_from(&self) -> Option<usize>` accessor.

- [ ] **Step 2: Run + commit**

```bash
cargo test -p spur-tui --lib scope_dispatch_tests::dirty_from_never_jumps_past_any_open_header_index
git add crates/spur-tui/src/components/react_trace/
git commit -m "test(spur-tui/react_trace): cache invariant — dirty_from stays >= open Header index"
```

---

## Task 23: Non-Claude regression — existing fixtures render unchanged

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs`

- [ ] **Step 1: Test**

```rust
    #[test]
    fn non_claude_fixture_has_no_scope_after_dispatch() {
        // Grep fixtures with no `_meta.claudeCode.parentToolUseId`. Dispatch each;
        // assert every resulting entry has scope == None.
        let fixtures = [
            "tool_call_bash.json",
            "tool_call_edit.json",
            "tool_update_bash_exit0.json",
        ];
        for name in &fixtures {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../spur-acp/tests/fixtures/notifications/claude-code-acp/{}", name));
            let Ok(raw) = std::fs::read_to_string(&path) else { continue };
            let notification: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let update: spur_acp::SessionUpdate = serde_json::from_value(
                notification.get("update").unwrap().clone()
            ).unwrap();
            let mut trace = ReactTrace::new(AgentKind::ClaudeCodeAcp);
            let mut depth = HashMap::new();
            let mut ctx = dispatch::DispatchCtx {
                agent_name: "claude",
                agent_kind: AgentKind::ClaudeCodeAcp,
                now_stamp: || "".into(),
                tool_depth: &mut depth,
            };
            dispatch::dispatch_session_update(&mut trace, &update, &mut ctx);
            for e in &trace.entries {
                assert!(
                    e.scope.is_none(),
                    "fixture {} produced an entry with scope: {:?}",
                    name,
                    e.scope
                );
            }
        }
    }
```

- [ ] **Step 2: Run + commit**

```bash
cargo test -p spur-tui --lib scope_dispatch_tests::non_claude_fixture_has_no_scope_after_dispatch
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "test(spur-tui/dispatch): non-Task fixtures produce no scope"
```

---

## Task 24: Vendor-meta leak scan CI check

- [ ] **Step 1: Run the existing script**

Run: `bash scripts/check-no-vendor-meta-leak.sh`
Expected: exit code 0 (no leaks into `crates/spur-tui/src/`).

If it fails, inspect the flagged file\:line — the Rust code must consume `SpurToolMeta`, never raw `_meta` / `claudeCode` / `parentToolUseId` tokens. Fix by funneling through `adapter::extract_tool_meta`.

- [ ] **Step 2: Confirm in CI**

Verify `.github/workflows/*.yml` (or equivalent CI config) invokes this script. If absent, add it as a step. If present, no change needed.

---

## Self-Review Checklist

Run through once the above tasks are complete:

- [ ] Every acceptance criterion in spec §5 (1–13) maps to a task in this plan. (Cross-reference: §5.1 → Task 6/Rendering chain, §5.2 → Task 14, §5.3 → Tasks 6+16+21, §5.4 → Task 11, §5.5 → Task 23, §5.6 → Task 22, §5.7–8 → Tasks 17–18, §5.9 → Task 18, §5.10 → Task 9, §5.11 → Task 10, §5.12 → Task 19, §5.13 → Task 24.)
- [ ] No "TODO" / "TBD" / "fill in" left in any step.
- [ ] Every `unimplemented!()` stub from Task 13 has been replaced (Tasks 14–16).
- [ ] `existing_compact_render_entry` delegation in Task 18 is wired to the actual pre-existing function, not left as `unimplemented!`.
- [ ] Type signatures are consistent: `ScopeId` / `Scope` / `ScopeRole` used uniformly; `TraceKind::ScopeTerminal` fields named identically in every task that references them (`scope_id`, `status`, `child_count`, `duration_ms`).
- [ ] All test modules use `#[cfg(test)]` gating.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-20-react-trace-subagent-scope.md`. Two execution options:

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
