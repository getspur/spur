# Close Brain↔Executor Feedback Loop UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the brain↔executor close feedback loop legible in one screen via focus-followed view inversion (Loop view) with live inline executor cards, vim-modal review input, and 14 polish refinements baked in.

**Architecture:** New per-brain-session "Loop view" with two interchangeable modes (Brain mode default, Executor mode on `>`). Inline `InlineExecutorCard` components render live executor status at each `delegate_to_worker` call site in the brain's ReactTrace. A modal `ReviewModal` overlays whichever mode is active when a review is pending. A new `DelegationDispatched` event correlates brain-side delegate calls to spawned executor IDs. Three load-bearing prerequisites (DiffSummary parser, ReviewPayload diff threading, vim-modal review input) ship as part of this plan.

**Tech Stack:** Rust 2024, ratatui (TUI), tokio (async), serde (events), thiserror, tracing. Existing crates: spur-acp (events + types), spur-core (orchestrator + lineage), spur-tui (views + components), spur-mcp (MCP server, only touched for request_id threading).

**Spec:** `docs/superpowers/specs/2026-04-13-close-feedback-loop-ui-design.md`

---

## File structure

| File | Responsibility |
|---|---|
| `crates/spur-acp/src/domain/diff_stats.rs` (new) | Pure parser: unified-diff text → `DiffSummary` |
| `crates/spur-acp/src/domain/events.rs` (modify) | Add `DelegationDispatched` variant + `request_id` to `DelegationRequested` |
| `crates/spur-mcp/src/protocol.rs` or wherever `DelegationRequest` lives (modify) | Surface `request_id` field |
| `crates/spur-core/src/orchestrator.rs` (modify ~1713, ~2206) | Compute `DiffSummary` for `ReviewPayload`; emit `DelegationDispatched` |
| `crates/spur-tui/src/components/inline_executor_card.rs` (new) | Live executor card render — R1, R3, R5, R7, R14 |
| `crates/spur-tui/src/components/review_modal.rs` (new) | Vim-modal review overlay — R6d, R11 |
| `crates/spur-tui/src/components/loop_layout.rs` (new) | Body + gutter rect helper |
| `crates/spur-tui/src/components/first_use_banner.rs` (new) | One-shot banner — R2 |
| `crates/spur-tui/src/components/react_trace.rs` (modify) | `TraceKind::Delegate` gains `executor_id` + `request_id` |
| `crates/spur-tui/src/views/session_detail.rs` (modify) | Embed inline cards; consume `DelegationDispatched`; use loop_layout |
| `crates/spur-tui/src/views/executor_detail.rs` (new) | Mirror of session_detail scoped to one executor |
| `crates/spur-tui/src/views/dashboard.rs` (modify) | Remove Review tab; simplify detail pane |
| `crates/spur-tui/src/app.rs` (modify) | New actions: descend/ascend/jump-to-review-via-loop |
| `crates/spur-tui/src/action.rs` (modify) | New variants: `DescendIntoExecutor`, `AscendFromExecutor` |
| `crates/spur-tui/src/ux_state.rs` (new) | Persist seen flags to `~/.spur/ux-state.json` |
| `crates/spur-acp/Cargo.toml` (modify) | (no new deps — diff parser is pure Rust) |
| `crates/spur-tui/Cargo.toml` (modify) | (no new deps; pager uses std::process::Command) |

---

## Task 1: `DiffSummary` parser

**Files:**
- Create: `crates/spur-acp/src/domain/diff_stats.rs`
- Modify: `crates/spur-acp/src/domain/mod.rs` (add `pub mod diff_stats;`)
- Test: `crates/spur-acp/tests/diff_stats.rs`

The orchestrator currently passes `outcome.diff: Option<String>` (unified diff text) but discards it from `ReviewPayload`. We need a pure parser that turns it into the existing `DiffSummary { files_changed, insertions, deletions, files }`.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/diff_stats.rs`:
```rust
use spur_acp::domain::diff_stats::parse_unified_diff;
use std::path::PathBuf;

#[test]
fn parses_single_file_addition_deletion_counts() {
    let diff = "\
diff --git a/foo.rs b/foo.rs
index abc..def 100644
--- a/foo.rs
+++ b/foo.rs
@@ -1,3 +1,4 @@
 line 1
-line 2 old
+line 2 new
+line 2.5 added
 line 3
";
    let summary = parse_unified_diff(diff);
    assert_eq!(summary.files_changed, 1);
    assert_eq!(summary.insertions, 2);
    assert_eq!(summary.deletions, 1);
    assert_eq!(summary.files, vec![PathBuf::from("foo.rs")]);
}

#[test]
fn parses_multi_file_diff() {
    let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+new
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,0 +1,2 @@
+added one
+added two
";
    let summary = parse_unified_diff(diff);
    assert_eq!(summary.files_changed, 2);
    assert_eq!(summary.insertions, 3);
    assert_eq!(summary.deletions, 1);
    assert_eq!(
        summary.files,
        vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]
    );
}

#[test]
fn ignores_diff_header_lines_in_counts() {
    // The `+++ b/foo.rs` and `--- a/foo.rs` headers must NOT count as ins/del.
    let diff = "\
diff --git a/x.rs b/x.rs
--- a/x.rs
+++ b/x.rs
@@ -1 +1 @@
-old
+new
";
    let summary = parse_unified_diff(diff);
    assert_eq!(summary.insertions, 1, "+++ header should not count");
    assert_eq!(summary.deletions, 1, "--- header should not count");
}

#[test]
fn empty_diff_yields_zero_summary() {
    let summary = parse_unified_diff("");
    assert_eq!(summary.files_changed, 0);
    assert_eq!(summary.insertions, 0);
    assert_eq!(summary.deletions, 0);
    assert!(summary.files.is_empty());
}

#[test]
fn handles_pure_addition_file() {
    let diff = "\
diff --git a/new.rs b/new.rs
new file mode 100644
--- /dev/null
+++ b/new.rs
@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3
";
    let summary = parse_unified_diff(diff);
    assert_eq!(summary.files_changed, 1);
    assert_eq!(summary.insertions, 3);
    assert_eq!(summary.deletions, 0);
    assert_eq!(summary.files, vec![PathBuf::from("new.rs")]);
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p spur-acp --test diff_stats`
Expected: FAIL — `parse_unified_diff` does not exist.

- [ ] **Step 3: Implement the parser**

Create `crates/spur-acp/src/domain/diff_stats.rs`:
```rust
//! Unified-diff text → DiffSummary parser.
//!
//! Pure function; no I/O. Counts insertions/deletions excluding the
//! `+++`/`---` filename header lines, and collects the `b/<path>` side
//! of each `diff --git` header as the canonical file path.

use std::path::PathBuf;

use crate::domain::events::DiffSummary;

pub fn parse_unified_diff(input: &str) -> DiffSummary {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut insertions: usize = 0;
    let mut deletions: usize = 0;

    for line in input.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // Format: `a/<path> b/<path>`. Take the `b/` side.
            if let Some(b_path) = rest.split_whitespace().nth(1) {
                let path = b_path.strip_prefix("b/").unwrap_or(b_path);
                files.push(PathBuf::from(path));
            }
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            // Diff header lines — never count as ins/del.
            continue;
        }
        if line.starts_with('+') {
            insertions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }

    DiffSummary {
        files_changed: files.len(),
        insertions,
        deletions,
        files,
    }
}
```

- [ ] **Step 4: Register the module**

Add to `crates/spur-acp/src/domain/mod.rs`:
```rust
pub mod diff_stats;
```

- [ ] **Step 5: Run the test and verify it passes**

Run: `cargo test -p spur-acp --test diff_stats`
Expected: PASS — all 5 tests green.

- [ ] **Step 6: Run the workspace tests**

Run: `cargo test --workspace`
Expected: no regressions; existing tests still green.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-acp/src/domain/diff_stats.rs crates/spur-acp/src/domain/mod.rs crates/spur-acp/tests/diff_stats.rs
git commit -m "feat(spur-acp): unified-diff parser into DiffSummary"
```

---

## Task 2: Thread `DiffSummary` into `ReviewPayload` (F6 fix)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:1713-1718` (review payload construction)
- Test: `crates/spur-core/tests/review_payload_diff.rs`

Today the orchestrator hardcodes `diff_summary: None` in `ReviewPayload` despite `outcome.diff` being available. Wire the new parser in.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-core/tests/review_payload_diff.rs`:
```rust
//! Verifies that when an executor produces a non-empty diff and the
//! review gate fires, the emitted ExecutorReviewRequested.payload
//! carries a populated DiffSummary (not None).

use spur_acp::domain::diff_stats::parse_unified_diff;

#[test]
fn parser_round_trips_through_review_payload_shape() {
    // This is a unit guard ensuring the parser produces the exact
    // type the orchestrator wires. The integration assertion happens
    // in the e2e smoke test (Task 15).
    let diff = "\
diff --git a/auth.rs b/auth.rs
--- a/auth.rs
+++ b/auth.rs
@@ -1 +1,2 @@
-old
+new
+added
";
    let summary = parse_unified_diff(diff);
    assert_eq!(summary.files_changed, 1);
    assert_eq!(summary.insertions, 2);
    assert_eq!(summary.deletions, 1);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-core --test review_payload_diff`
Expected: PASS (it only exercises the parser); the wiring change is verified by code review + the smoke test in Task 15.

- [ ] **Step 3: Wire the parser in orchestrator.rs**

Locate the review-payload construction at `crates/spur-core/src/orchestrator.rs:1713-1718`:
```rust
let review_payload = ReviewPayload {
    summary: outcome.summary.clone().unwrap_or_default(),
    diff_summary: None,
    pr_url: None,
    error: None,
};
```

Replace with:
```rust
let review_payload = ReviewPayload {
    summary: outcome.summary.clone().unwrap_or_default(),
    diff_summary: outcome
        .diff
        .as_deref()
        .map(spur_acp::domain::diff_stats::parse_unified_diff),
    pr_url: None,
    error: None,
};
```

- [ ] **Step 4: Build and run all tests**

Run: `cargo build --workspace && cargo test --workspace`
Expected: build succeeds; all existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/tests/review_payload_diff.rs
git commit -m "fix(spur-core): populate ReviewPayload.diff_summary from outcome.diff"
```

---

## Task 3: `DelegationDispatched` event + `request_id` threading

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (add variant, extend `DelegationRequested`)
- Modify: `crates/spur-mcp/src/server.rs` (already constructs `request_id`; ensure it flows)
- Modify: `crates/spur-core/src/orchestrator.rs` (emit new event after executor_id known; thread `request_id`)
- Test: `crates/spur-acp/tests/delegation_dispatched.rs`

The brain-side has no link to the spawned `executor_id`. We add an event that fires after spawn carrying both the original `request_id` (already a UUID in `DelegationRequest`) and the new `executor_id`.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/delegation_dispatched.rs`:
```rust
use spur_acp::SessionId;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};

#[test]
fn delegation_dispatched_serde_roundtrip() {
    let body = SpurEventBody::DelegationDispatched {
        from: SessionId::new(),
        request_id: "req-abc".to_string(),
        executor_id: "exec-123".to_string(),
    };
    let event = SpurEvent::now(body);
    let json = serde_json::to_string(&event).expect("serialize");
    let back: SpurEvent = serde_json::from_str(&json).expect("deserialize");
    match back.body {
        SpurEventBody::DelegationDispatched { request_id, executor_id, .. } => {
            assert_eq!(request_id, "req-abc");
            assert_eq!(executor_id, "exec-123");
        }
        _ => panic!("variant mismatch"),
    }
}

#[test]
fn delegation_requested_now_carries_request_id() {
    let body = SpurEventBody::DelegationRequested {
        from: SessionId::new(),
        to_agent: "claude-coder".to_string(),
        task: "do work".to_string(),
        request_id: "req-xyz".to_string(),
    };
    let json = serde_json::to_string(&body).expect("serialize");
    assert!(json.contains("req-xyz"), "request_id must serialize: got {json}");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p spur-acp --test delegation_dispatched`
Expected: FAIL — `DelegationDispatched` variant + `request_id` field don't exist.

- [ ] **Step 3: Add the variant and field in events.rs**

Modify `crates/spur-acp/src/domain/events.rs:104` (the existing `DelegationRequested` line):
```rust
DelegationRequested {
    from: SessionId,
    to_agent: String,
    task: String,
    /// UUID matching the spur-mcp `DelegationRequest.id`. Surfaced
    /// so the brain conversation can correlate with the spawned
    /// executor via `DelegationDispatched`.
    request_id: String,
},
```

Add a new variant near the executor lineage events (after `WorkerSpawned`, before `SessionCompleted`):
```rust
/// Emitted immediately after the orchestrator spawns an executor
/// for a brain delegation. Lets the brain-side session_detail
/// view correlate its `DelegationRequested` trace entry with the
/// new executor node so an inline executor card can render.
DelegationDispatched {
    /// Brain session that issued the delegate_to_worker call.
    from: SessionId,
    /// Matches the `request_id` on `DelegationRequested` /
    /// `DelegationRequest.id` (UUID).
    request_id: String,
    /// The executor node now spawned for this delegation.
    executor_id: String,
},
```

- [ ] **Step 4: Fix exhaustive matches by adding the new arm everywhere**

Run: `cargo build --workspace 2>&1 | grep -E 'non-exhaustive|missing variants' | head -40`

Each error site is a `match event.body { ... }` that needs a new arm. For variants the consumer doesn't care about, add `_ => {}` is **not** acceptable — add an explicit named arm logging at trace level (so future spec changes are visible). Sample addition for each match site:

```rust
SpurEventBody::DelegationDispatched { .. } => {
    // No-op for this consumer; brain-side view consumes this.
    tracing::trace!("DelegationDispatched ignored by this consumer");
}
```

Compile until clean.

For `DelegationRequested` field addition: every match like `DelegationRequested { from, to_agent, task }` needs `request_id` added (use `request_id: _` if not consumed, or bind it).

- [ ] **Step 5: Thread `request_id` from spur-mcp → orchestrator**

In `crates/spur-mcp/src/server.rs:309-340` `handle_delegate_to_worker`, the `request_id: String` is already created (line 323) and stored on `DelegationRequest.id`. Verify by inspection — no change to spur-mcp itself.

In `crates/spur-core/src/orchestrator.rs` find the existing `DelegationRequested` emission at ~line 2206:
```rust
let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationRequested {
    from: session_id.clone(),
    to_agent: agent.clone(),
    task: task.clone(),
}));
```

Update to include `request_id` (the `DelegationRequest.id` is already in scope as `delegation.id` or similar — confirm by reading the function signature):
```rust
let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationRequested {
    from: session_id.clone(),
    to_agent: agent.clone(),
    task: task.clone(),
    request_id: request_id.clone(),
}));
```

If the local variable is not named `request_id`, use the actual binding (commonly `delegation.id` from the `DelegationRequest` parameter).

- [ ] **Step 6: Emit `DelegationDispatched` after executor_id is known**

In `crates/spur-core/src/orchestrator.rs` immediately after the executor_id is computed and before `ExecutorSpawned` is emitted (search for `ExecutorSpawned` to find the site), add:

```rust
let _ = event_tx.send(SpurEvent::now(SpurEventBody::DelegationDispatched {
    from: session_id.clone(),
    request_id: request_id.clone(),
    executor_id: executor_id.0.clone(),
}));
```

The variable names match what's in scope at that point — confirm by reading the surrounding function. If the brain `session_id` isn't directly named, it's stored on the `DelegationRequest` or threaded into the spawn helper.

- [ ] **Step 7: Run tests**

Run: `cargo test -p spur-acp --test delegation_dispatched && cargo test --workspace`
Expected: PASS — both new tests green; existing tests still green.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(spur-acp,spur-core): correlate brain delegations to executor IDs

New DelegationDispatched event fires after spawn carrying both the
original request_id (UUID from spur-mcp) and the spawned executor_id.
DelegationRequested gains request_id so brain-side can match."
```

---

## Task 4: `TraceKind::Delegate` gains `executor_id` + `request_id`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs` (`TraceKind::Delegate` variant)
- Modify: `crates/spur-tui/src/views/session_detail.rs` (consume both events; populate fields)
- Test: `crates/spur-tui/tests/delegate_trace_correlation.rs` (new)

Brain-side `DelegationRequested` trace entry needs to grow `executor_id` (initially `None`, populated when `DelegationDispatched` arrives).

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/delegate_trace_correlation.rs`:
```rust
use spur_acp::SessionId;
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_tui::components::react_trace::{ReactTrace, TraceKind};

fn make_event(body: SpurEventBody) -> SpurEvent {
    SpurEvent::now(body)
}

#[test]
fn delegation_dispatched_attaches_executor_id_to_delegate_entry() {
    let session_id = SessionId::new();
    let mut view = spur_tui::views::session_detail::SessionDetailView::new(session_id.clone());

    view.handle_spur_event(&make_event(SpurEventBody::DelegationRequested {
        from: session_id.clone(),
        to_agent: "claude-coder".to_string(),
        task: "do work".to_string(),
        request_id: "req-1".to_string(),
    }));

    view.handle_spur_event(&make_event(SpurEventBody::DelegationDispatched {
        from: session_id.clone(),
        request_id: "req-1".to_string(),
        executor_id: "exec-1".to_string(),
    }));

    let trace = view.react_trace();
    let last = trace.entries().last().expect("entry present");
    match &last.kind {
        TraceKind::Delegate { request_id, executor_id, .. } => {
            assert_eq!(request_id.as_deref(), Some("req-1"));
            assert_eq!(executor_id.as_deref(), Some("exec-1"));
        }
        other => panic!("expected Delegate, got {:?}", other),
    }
}
```

(The test depends on `react_trace()` and `entries()` accessor methods. If they're not `pub`, add them.)

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p spur-tui --test delegate_trace_correlation`
Expected: FAIL — `TraceKind::Delegate` doesn't have those fields.

- [ ] **Step 3: Extend `TraceKind::Delegate`**

Modify `crates/spur-tui/src/components/react_trace.rs:21`:
```rust
Delegate {
    agent: String,
    task: String,
    status: String,
    /// UUID from spur-mcp; matches the brain's delegate_to_worker call.
    /// Some once `DelegationRequested` is consumed.
    request_id: Option<String>,
    /// The spawned executor; Some after `DelegationDispatched` arrives.
    /// Used by render path to embed an inline executor card.
    executor_id: Option<String>,
},
```

Update every `TraceKind::Delegate { agent, task, status }` site (there are at least 2 — at the `kind_label` match around line 77 and rendering around line 505 / 735) to add the new bindings (use `request_id: _, executor_id: _` if not consumed).

- [ ] **Step 4: Update session_detail.rs to populate fields**

In `crates/spur-tui/src/views/session_detail.rs:697-715`, the existing `DelegationRequested` arm pushes a `Delegate` entry. Update to capture `request_id`:
```rust
SpurEventBody::DelegationRequested {
    from,
    to_agent,
    task,
    request_id,
} => {
    if from.0 != self.session_id.0 {
        return;
    }
    self.react_trace.push(TraceEntry {
        kind: TraceKind::Delegate {
            agent: to_agent.clone(),
            task: task.clone(),
            status: "delegated".to_string(),
            request_id: Some(request_id.clone()),
            executor_id: None,
        },
        text: String::new(),
        timestamp: Self::now_stamp(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
}
```

Add a new arm for `DelegationDispatched`:
```rust
SpurEventBody::DelegationDispatched {
    from,
    request_id,
    executor_id,
} => {
    if from.0 != self.session_id.0 {
        return;
    }
    // Find the most recent Delegate entry with matching request_id
    // and attach the executor_id.
    self.react_trace.attach_executor_id(&request_id, &executor_id);
}
```

Add the helper method on `ReactTrace` in `react_trace.rs`:
```rust
/// Locate the most recent `Delegate` entry whose `request_id` matches
/// the given UUID and attach the `executor_id`. No-op if not found
/// (event arrived for an entry not in this trace, or out of order).
pub fn attach_executor_id(&mut self, request_id: &str, executor_id: &str) {
    for entry in self.entries.iter_mut().rev() {
        if let TraceKind::Delegate {
            request_id: Some(rid),
            executor_id: slot @ None,
            ..
        } = &mut entry.kind
        {
            if rid == request_id {
                *slot = Some(executor_id.to_string());
                return;
            }
        }
    }
    tracing::debug!(
        request_id = %request_id,
        executor_id = %executor_id,
        "DelegationDispatched arrived but no matching Delegate entry"
    );
}
```

Add `pub fn entries(&self) -> &[TraceEntry] { &self.entries }` and `pub fn react_trace(&self) -> &ReactTrace { &self.react_trace }` accessors as needed.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui --test delegate_trace_correlation && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): correlate brain Delegate trace entries with executor IDs

TraceKind::Delegate gains request_id + executor_id Option fields.
session_detail consumes DelegationDispatched to attach executor_id.
attach_executor_id helper finds the matching entry by request_id."
```

---

## Task 5: `InlineExecutorCard` component (R1, R3, R5, R7, R14)

**Files:**
- Create: `crates/spur-tui/src/components/inline_executor_card.rs`
- Modify: `crates/spur-tui/src/components/mod.rs` (export)
- Test: `crates/spur-tui/tests/inline_executor_card_render.rs`

Live render of a single executor's card. State-driven density: Running compact, AwaitingReview/Failed taller. Focus indicator (cyan left bar + hint line). Stale color rules. Per-state glyphs.

- [ ] **Step 1: Write failing tests for each state**

Create `crates/spur-tui/tests/inline_executor_card_render.rs`:
```rust
use spur_acp::SessionId;
use spur_core::lineage::projection::ExecutorLineage;
use spur_core::ExecutorId;
use spur_acp::domain::events::{LifecycleState, Role, SpurEvent, SpurEventBody, ReviewKind, ReviewPayload};
use spur_tui::components::inline_executor_card::render_card;

fn lineage_with(events: Vec<SpurEventBody>) -> ExecutorLineage {
    let mut l = ExecutorLineage::new();
    for body in events {
        l.apply(&SpurEvent::now(body));
    }
    l
}

#[test]
fn renders_running_card_with_compact_density() {
    let lineage = lineage_with(vec![
        SpurEventBody::ExecutorSpawned {
            id: "exec-1".into(),
            parent_id: None,
            session_id: SessionId::new(),
            agent: "claude-coder".into(),
            role: Role::Executor,
            task_spec: "do work".into(),
        },
        SpurEventBody::ExecutorPhaseChanged {
            id: "exec-1".into(),
            phase: LifecycleState::Running,
        },
    ]);

    let lines = render_card(&lineage, &ExecutorId("exec-1".into()), false);
    let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
    assert!(text.contains("exec-1"), "id present");
    assert!(text.contains("claude-coder"), "agent present");
    assert!(text.contains("Running"), "phase present");
    assert!(lines.len() <= 3, "Running density is 3 lines, got {}", lines.len());
}

#[test]
fn renders_awaiting_review_card_taller_with_attention() {
    let lineage = lineage_with(vec![
        SpurEventBody::ExecutorSpawned {
            id: "exec-2".into(),
            parent_id: None,
            session_id: SessionId::new(),
            agent: "claude-coder".into(),
            role: Role::Executor,
            task_spec: "review me".into(),
        },
        SpurEventBody::ExecutorReviewRequested {
            id: "exec-2".into(),
            attempt_n: 1,
            kind: ReviewKind::Completion,
            payload: ReviewPayload {
                summary: "did stuff".into(),
                diff_summary: None,
                pr_url: None,
                error: None,
            },
        },
    ]);

    let lines = render_card(&lineage, &ExecutorId("exec-2".into()), false);
    let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
    assert!(text.contains("ATTENTION"), "attention header present");
    assert!(text.contains("Press 'r'"), "review CTA present");
    assert!(lines.len() >= 5, "AwaitingReview density >= 5, got {}", lines.len());
}

#[test]
fn focused_card_includes_hint_line() {
    let lineage = lineage_with(vec![
        SpurEventBody::ExecutorSpawned {
            id: "exec-3".into(),
            parent_id: None,
            session_id: SessionId::new(),
            agent: "claude-coder".into(),
            role: Role::Executor,
            task_spec: "task".into(),
        },
    ]);
    let lines = render_card(&lineage, &ExecutorId("exec-3".into()), true);
    let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
    assert!(text.contains("Enter"), "focused card shows Enter hint");
}

#[test]
fn unknown_executor_renders_placeholder() {
    let lineage = lineage_with(vec![]);
    let lines = render_card(&lineage, &ExecutorId("ghost".into()), false);
    let text: String = lines.iter().flat_map(|l| l.spans.iter().map(|s| s.content.as_ref())).collect();
    assert!(text.contains("spawning"), "unknown id shows spawning placeholder: {text}");
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test -p spur-tui --test inline_executor_card_render`
Expected: FAIL — `inline_executor_card` module does not exist.

- [ ] **Step 3: Implement the component**

Create `crates/spur-tui/src/components/inline_executor_card.rs`:
```rust
//! Live executor card rendered inline in the brain conversation at
//! each delegate_to_worker call site. Pure render against
//! `ExecutorLineage`; no internal state. Reactivity comes from the
//! projection.
//!
//! Implements UX refinements R1 (focus indicator), R3 (stale colors +
//! spinner), R5 (update-flash, see app.rs trigger), R7 (attention-
//! state taller cards), R14 (per-state density).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use spur_acp::domain::events::LifecycleState;
use spur_core::lineage::projection::ExecutorLineage;
use spur_core::ExecutorId;

const TASK_TRUNCATE: usize = 60;

pub fn render_card(
    lineage: &ExecutorLineage,
    executor_id: &ExecutorId,
    focused: bool,
) -> Vec<Line<'static>> {
    let node = match lineage.node(executor_id) {
        Some(n) => n,
        None => return placeholder_card(executor_id, focused),
    };

    let phase = node.phase;
    let task = truncate(&node.task_spec, TASK_TRUNCATE);
    let agent = node.agent.clone();
    let id = executor_id.0.clone();

    let header = header_line(phase, &id, &agent, &task);

    let mut lines = vec![header];

    match phase {
        LifecycleState::Spawning | LifecycleState::Running | LifecycleState::Resuming => {
            lines.push(running_status_line(node));
            lines.push(running_diff_line(node));
        }
        LifecycleState::AwaitingReview => {
            lines.push(attention_header());
            lines.push(awaiting_status_line(node));
            lines.push(awaiting_summary_line(node));
            lines.push(awaiting_cta_line());
        }
        LifecycleState::Failed => {
            lines.push(attention_header_failed());
            lines.push(failed_status_line(node));
            lines.push(failed_cta_line());
        }
        LifecycleState::Succeeded => {
            lines.push(done_status_line(node));
        }
        LifecycleState::Cancelled => {
            lines.push(cancelled_status_line(node));
        }
    }

    if focused {
        lines.push(focus_hint_line(phase));
    }

    lines
}

fn placeholder_card(executor_id: &ExecutorId, focused: bool) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![
        Span::styled(
            format!("○ exec/{} ", short_id(&executor_id.0)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled("(spawning…)", Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
    ])];
    if focused {
        out.push(Line::from(Span::styled(
            "  [ executor not yet spawned ]",
            Style::default().fg(Color::DarkGray),
        )));
    }
    out
}

fn header_line(phase: LifecycleState, id: &str, agent: &str, task: &str) -> Line<'static> {
    let (glyph, color) = phase_glyph(phase);
    Line::from(vec![
        Span::styled(format!("{glyph} "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(format!("exec/{} · ", short_id(id)), Style::default().fg(Color::White)),
        Span::styled(format!("{agent} · "), Style::default().fg(Color::Cyan)),
        Span::styled(format!("\"{task}\""), Style::default().fg(Color::Gray)),
    ])
}

fn running_status_line(node: &spur_core::lineage::projection::ExecutorNode) -> Line<'static> {
    let elapsed = format_elapsed(node.elapsed_secs());
    let tool_count = node.tool_call_count;
    let last_tool = node.latest_tool_call.as_deref().unwrap_or("(none)");
    let stale_secs = node.seconds_since_last_event();
    let stale_color = stale_color_for(stale_secs);
    let spinner = if stale_secs.unwrap_or(u64::MAX) < 10 { spinner_glyph() } else { ' ' };

    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("Running · {elapsed} · {tool_count} calls · last: {last_tool}"),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(" · {} ago {spinner}", format_elapsed(stale_secs.unwrap_or(0))),
            Style::default().fg(stale_color),
        ),
    ])
}

fn running_diff_line(node: &spur_core::lineage::projection::ExecutorNode) -> Line<'static> {
    let files = node.files_touched_count;
    let (ins, del) = node.diff_totals();
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("files: {files} · diff: +{ins}/-{del}"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn attention_header() -> Line<'static> {
    Line::from(Span::styled(
        "┌─ ⚠ ATTENTION ──────────────────────────────────────────────────────",
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    ))
}

fn attention_header_failed() -> Line<'static> {
    Line::from(Span::styled(
        "┌─ ✗ FAILED ─────────────────────────────────────────────────────────",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ))
}

fn awaiting_status_line(node: &spur_core::lineage::projection::ExecutorNode) -> Line<'static> {
    let elapsed = format_elapsed(node.elapsed_secs());
    let (ins, del) = node.diff_totals();
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("AwaitingReview · {elapsed} · diff: {} files, +{ins}/-{del}", node.files_touched_count),
            Style::default().fg(Color::Yellow),
        ),
    ])
}

fn awaiting_summary_line(node: &spur_core::lineage::projection::ExecutorNode) -> Line<'static> {
    let summary = node
        .pending_review
        .as_ref()
        .map(|r| r.payload.summary.clone())
        .unwrap_or_default();
    Line::from(vec![
        Span::raw("  Worker summary: "),
        Span::styled(format!("\"{}\"", truncate(&summary, 70)), Style::default().fg(Color::Gray)),
    ])
}

fn awaiting_cta_line() -> Line<'static> {
    Line::from(Span::styled(
        "  ▶ Press 'r' to review (this delegation is blocking the brain)",
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    ))
}

fn failed_status_line(node: &spur_core::lineage::projection::ExecutorNode) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("Failed · {}", node.last_error.as_deref().unwrap_or("(no error message)")),
            Style::default().fg(Color::Red),
        ),
    ])
}

fn failed_cta_line() -> Line<'static> {
    Line::from(Span::styled(
        "  ▶ Press 'i' to inspect events, '<' to return to brain",
        Style::default().fg(Color::Red),
    ))
}

fn done_status_line(node: &spur_core::lineage::projection::ExecutorNode) -> Line<'static> {
    let elapsed = format_elapsed(node.elapsed_secs());
    let (ins, del) = node.diff_totals();
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("Done · {elapsed} · diff: {} files, +{ins}/-{del}", node.files_touched_count),
            Style::default().fg(Color::Cyan),
        ),
    ])
}

fn cancelled_status_line(node: &spur_core::lineage::projection::ExecutorNode) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("Cancelled · {}", format_elapsed(node.elapsed_secs())),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn focus_hint_line(phase: LifecycleState) -> Line<'static> {
    let hint = match phase {
        LifecycleState::AwaitingReview => "[ press 'r' to review · Enter / > to enter executor view ]",
        LifecycleState::Failed | LifecycleState::Cancelled => "[ Enter / > to inspect events ]",
        _ => "[ Enter / > to open executor view · Tab for next ]",
    };
    Line::from(Span::styled(
        format!("  {hint}"),
        Style::default().fg(Color::Cyan),
    ))
}

fn phase_glyph(phase: LifecycleState) -> (char, Color) {
    match phase {
        LifecycleState::Spawning => ('○', Color::DarkGray),
        LifecycleState::Running | LifecycleState::Resuming => ('▶', Color::Green),
        LifecycleState::AwaitingReview => ('⚠', Color::Yellow),
        LifecycleState::Succeeded => ('✓', Color::Cyan),
        LifecycleState::Failed => ('✗', Color::Red),
        LifecycleState::Cancelled => ('💀', Color::DarkGray),
    }
}

fn stale_color_for(secs_since_last: Option<u64>) -> Color {
    match secs_since_last {
        Some(s) if s > 300 => Color::Red,
        Some(s) if s > 30 => Color::Yellow,
        _ => Color::DarkGray,
    }
}

fn spinner_glyph() -> char {
    let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let idx = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() / 80)
        .unwrap_or(0)
        % frames.len() as u128) as usize;
    frames[idx]
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(4).collect()
}

fn format_elapsed(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}
```

- [ ] **Step 4: Add accessor methods to `ExecutorNode` / `ExecutorLineage`**

The card calls methods that don't yet exist on `ExecutorNode`: `elapsed_secs()`, `seconds_since_last_event()`, `tool_call_count`, `latest_tool_call`, `files_touched_count`, `diff_totals()`, `last_error`. Add them in `crates/spur-core/src/lineage/projection.rs`:

```rust
impl ExecutorNode {
    pub fn elapsed_secs(&self) -> u64 {
        self.spawned_at
            .elapsed()
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    pub fn seconds_since_last_event(&self) -> Option<u64> {
        self.last_event_at
            .and_then(|t| t.elapsed().ok())
            .map(|d| d.as_secs())
    }

    pub fn diff_totals(&self) -> (usize, usize) {
        self.latest_diff_summary
            .as_ref()
            .map(|d| (d.insertions, d.deletions))
            .unwrap_or((0, 0))
    }
}
```

And add the corresponding fields to `ExecutorNode`:
```rust
pub spawned_at: std::time::SystemTime,
pub last_event_at: Option<std::time::SystemTime>,
pub tool_call_count: usize,
pub latest_tool_call: Option<String>,
pub files_touched_count: usize,
pub latest_diff_summary: Option<spur_acp::domain::events::DiffSummary>,
pub last_error: Option<String>,
```

Initialize `spawned_at: std::time::SystemTime::now()` in the `ExecutorSpawned` apply branch (use `event.occurred_at` instead of `now()` per the projection's "do not call now() in apply" rule). Initialize the rest as `0` / `None`.

In the projection's `apply` method, update fields when relevant events arrive:
- `ExecutorPhaseChanged` → bump `last_event_at = Some(event.occurred_at)`
- `ExecutorArtifact { artifact: Diff(d), .. }` → set `latest_diff_summary = Some(d.clone()); files_touched_count = d.files_changed`
- `AgentNotification` carrying a tool call → bump `tool_call_count += 1; latest_tool_call = Some(<tool_name>)` (find the existing tool-call extraction logic — likely already present for the dashboard)

If the existing projection doesn't track tool calls per-executor, this is a non-trivial addition; in that case **scope-limit Task 5** to render with the fields available (`elapsed_secs`, phase) and defer the `tool_call_count`/`latest_tool_call` fields to a follow-up task. For the plan as written, assume the projection can be extended.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui --test inline_executor_card_render && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): InlineExecutorCard component with state-driven density

R1 focus indicator (hint line on focus), R3 stale colors + spinner,
R7 attention-state taller cards, R14 per-state density (2-5 lines).
Pure render against ExecutorLineage; reactivity from projection.
Adds elapsed/last-event/tool-count fields to ExecutorNode."
```

---

## Task 6: First-use banner + ux-state.json (R2)

**Files:**
- Create: `crates/spur-tui/src/ux_state.rs`
- Create: `crates/spur-tui/src/components/first_use_banner.rs`
- Modify: `crates/spur-tui/src/lib.rs` (export)
- Test: `crates/spur-tui/tests/ux_state.rs`

Persistence at `~/.spur/ux-state.json` for one-shot tutorial banners.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/ux_state.rs`:
```rust
use spur_tui::ux_state::UxState;
use std::path::PathBuf;

fn temp_path() -> PathBuf {
    let dir = std::env::temp_dir();
    dir.join(format!("spur-ux-state-{}.json", uuid::Uuid::new_v4()))
}

#[test]
fn fresh_state_has_no_seen_flags() {
    let path = temp_path();
    let state = UxState::load_from(&path);
    assert!(!state.seen_loop_view());
}

#[test]
fn marking_seen_persists_across_load() {
    let path = temp_path();
    let mut state = UxState::load_from(&path);
    state.mark_seen_loop_view();
    state.save_to(&path).expect("save");

    let reloaded = UxState::load_from(&path);
    assert!(reloaded.seen_loop_view());

    std::fs::remove_file(&path).ok();
}

#[test]
fn corrupt_file_falls_back_to_fresh_state() {
    let path = temp_path();
    std::fs::write(&path, "not valid json").expect("write");
    let state = UxState::load_from(&path);
    assert!(!state.seen_loop_view(), "corrupt file → fresh state");
    std::fs::remove_file(&path).ok();
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p spur-tui --test ux_state`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement `UxState`**

Create `crates/spur-tui/src/ux_state.rs`:
```rust
//! One-shot UX flags persisted to `~/.spur/ux-state.json`.
//! Use for: first-use banners, "don't show again" prefs.
//! Corruption-tolerant: any read error falls back to defaults.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UxState {
    #[serde(default)]
    seen_loop_view: bool,
    #[serde(default)]
    reduced_motion: bool,
}

impl UxState {
    /// Load from the user's default path (`~/.spur/ux-state.json`).
    /// Falls back to `UxState::default()` on any error.
    pub fn load_default() -> Self {
        match default_path() {
            Some(p) => Self::load_from(&p),
            None => Self::default(),
        }
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_default(&self) -> std::io::Result<()> {
        let path = default_path().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "cannot resolve home dir")
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let s = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, s)
    }

    pub fn seen_loop_view(&self) -> bool {
        self.seen_loop_view
    }

    pub fn mark_seen_loop_view(&mut self) {
        self.seen_loop_view = true;
    }

    pub fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }
}

fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".spur").join("ux-state.json"))
}
```

- [ ] **Step 4: Implement the banner component**

Create `crates/spur-tui/src/components/first_use_banner.rs`:
```rust
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Render the one-shot tutorial banner above the first executor card.
/// Caller decides when to show it based on `UxState::seen_loop_view()`.
pub fn render_loop_view_banner() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "┌─ tip (shown once) ─────────────────────────────────────────────────",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "│ This is an executor card. Press Tab to focus, Enter (or >) to step",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "│ inside and watch live. Press < to come back. [Esc to dismiss]",
            Style::default().fg(Color::Cyan),
        )),
        Line::from(Span::styled(
            "└────────────────────────────────────────────────────────────────────",
            Style::default().fg(Color::Cyan),
        )),
    ]
}
```

- [ ] **Step 5: Register modules**

Modify `crates/spur-tui/src/lib.rs`:
```rust
pub mod ux_state;
```

Modify `crates/spur-tui/src/components/mod.rs`:
```rust
pub mod first_use_banner;
pub mod inline_executor_card;
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-tui --test ux_state && cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): UxState persistence + first-use banner (R2)

UxState reads/writes ~/.spur/ux-state.json for one-shot UX flags.
Corruption-tolerant: any error → default state. First-use banner
component renders the one-shot Loop view tutorial."
```

---

## Task 7: Embed inline cards in `session_detail` render

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/components/react_trace.rs` (render path for `Delegate`)
- Test: `crates/spur-tui/tests/session_detail_inline_cards.rs`

When session_detail renders a `Delegate` entry whose `executor_id` is `Some`, splice in `render_card(...)` lines.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/session_detail_inline_cards.rs`:
```rust
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use spur_acp::SessionId;
use spur_acp::domain::events::{LifecycleState, Role, SpurEvent, SpurEventBody};
use spur_core::lineage::projection::ExecutorLineage;
use spur_tui::views::session_detail::SessionDetailView;

#[test]
fn delegation_dispatched_then_phase_change_renders_inline_card() {
    let session = SessionId::new();
    let mut view = SessionDetailView::new(session.clone());
    let mut lineage = ExecutorLineage::new();

    let req_event = SpurEvent::now(SpurEventBody::DelegationRequested {
        from: session.clone(),
        to_agent: "claude-coder".into(),
        task: "do work".into(),
        request_id: "req-1".into(),
    });
    view.handle_spur_event(&req_event);

    let dispatch_event = SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: session.clone(),
        request_id: "req-1".into(),
        executor_id: "exec-1".into(),
    });
    view.handle_spur_event(&dispatch_event);

    let spawn_event = SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: None,
        session_id: session.clone(),
        agent: "claude-coder".into(),
        role: Role::Executor,
        task_spec: "do work".into(),
    });
    lineage.apply(&spawn_event);

    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| view.render_with_lineage(f, f.size(), &lineage))
        .expect("draw");

    let buf = terminal.backend().buffer().clone();
    let text: String = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| buf.get(x, y).symbol().to_string()))
        .collect();
    assert!(text.contains("exec/"), "inline card rendered: {}", &text[..text.len().min(500)]);
    assert!(text.contains("claude-coder"));
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p spur-tui --test session_detail_inline_cards`
Expected: FAIL — `render_with_lineage` doesn't exist or doesn't render the card.

- [ ] **Step 3: Add `render_with_lineage` to `SessionDetailView`**

In `crates/spur-tui/src/views/session_detail.rs`, add a new render method that takes a `&ExecutorLineage`:
```rust
pub fn render_with_lineage(
    &self,
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    lineage: &spur_core::lineage::projection::ExecutorLineage,
) {
    self.lineage_for_render = Some(lineage as *const _);
    self.render(frame, area);
    // Note: pointer use is read-only and same-frame; safe.
}
```

(Or — cleaner — pass `lineage` directly through `render` by extending the signature, but that breaks the `View` trait. Use a setter pattern instead: `set_lineage(&ExecutorLineage)` called before `render`.) Pick whichever is less invasive.

In the existing render path where `TraceKind::Delegate` is rendered (around `react_trace.rs:505`), check for `executor_id: Some(id)` and splice in the card lines:
```rust
TraceKind::Delegate { agent, task, status, executor_id, request_id: _ } => {
    let mut lines = vec![/* existing header line */];
    if let Some(eid) = executor_id {
        if let Some(lineage) = lineage_ref {
            lines.extend(spur_tui::components::inline_executor_card::render_card(
                lineage,
                &spur_core::ExecutorId(eid.clone()),
                /* focused = */ false,  // focus state passed through later
            ));
        }
    } else {
        // Pre-DelegationDispatched fallback: render bare line.
        lines.push(/* status line */);
    }
    // ... existing render
}
```

The exact lifetime/threading of `lineage_ref` into the trace render is the architectural decision here; the cleanest is to pass `lineage: Option<&ExecutorLineage>` as a parameter through the render call chain. Update `ReactTrace::render` and friends to thread it.

- [ ] **Step 4: Update existing render call sites**

In `crates/spur-tui/src/views/session_detail.rs::render`, when calling into `react_trace.render(...)`, pass `self.lineage_for_render` (or whichever mechanism was chosen).

The app.rs render path that owns both `SessionDetailView` and `ExecutorLineage` calls `view.render_with_lineage(frame, area, &self.lineage)` instead of `view.render(...)`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui --test session_detail_inline_cards && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): embed live inline executor cards in session_detail

Delegate trace entries with executor_id render the InlineExecutorCard
lines spliced into the conversation. Pre-dispatch entries fall back
to bare status. App.rs now passes &ExecutorLineage to render."
```

---

## Task 8: `loop_layout` helper

**Files:**
- Create: `crates/spur-tui/src/components/loop_layout.rs`
- Test: `crates/spur-tui/tests/loop_layout.rs`

Tiny helper for body+gutter rect splitting. Used by both Brain mode (session_detail) and Executor mode (new view in Task 9).

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/loop_layout.rs`:
```rust
use ratatui::layout::Rect;
use spur_tui::components::loop_layout::{split, GutterPosition};

#[test]
fn bottom_gutter_takes_3_lines() {
    let area = Rect::new(0, 0, 80, 30);
    let (body, gutter) = split(area, GutterPosition::Bottom, 3);
    assert_eq!(body, Rect::new(0, 0, 80, 27));
    assert_eq!(gutter, Rect::new(0, 27, 80, 3));
}

#[test]
fn top_gutter_at_top() {
    let area = Rect::new(0, 0, 80, 30);
    let (body, gutter) = split(area, GutterPosition::Top, 3);
    assert_eq!(gutter, Rect::new(0, 0, 80, 3));
    assert_eq!(body, Rect::new(0, 3, 80, 27));
}

#[test]
fn left_gutter_takes_columns() {
    let area = Rect::new(0, 0, 80, 30);
    let (body, gutter) = split(area, GutterPosition::Left, 30);
    assert_eq!(gutter, Rect::new(0, 0, 30, 30));
    assert_eq!(body, Rect::new(30, 0, 50, 30));
}

#[test]
fn gutter_clamped_to_area_size() {
    let area = Rect::new(0, 0, 80, 5);
    let (body, gutter) = split(area, GutterPosition::Bottom, 10);
    assert_eq!(gutter.height + body.height, 5);
    assert!(body.height >= 1, "body retains at least 1 line");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-tui --test loop_layout`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement**

Create `crates/spur-tui/src/components/loop_layout.rs`:
```rust
use ratatui::layout::Rect;

#[derive(Debug, Clone, Copy)]
pub enum GutterPosition {
    Top,
    Bottom,
    Left,
    Right,
}

/// Split `area` into `(body, gutter)` rects according to `position`
/// and the requested gutter `size` (rows for Top/Bottom, cols for
/// Left/Right). Gutter is clamped to leave at least 1 cell for body.
pub fn split(area: Rect, position: GutterPosition, size: u16) -> (Rect, Rect) {
    match position {
        GutterPosition::Bottom => {
            let g_height = size.min(area.height.saturating_sub(1));
            let body = Rect::new(area.x, area.y, area.width, area.height - g_height);
            let gutter = Rect::new(area.x, area.y + body.height, area.width, g_height);
            (body, gutter)
        }
        GutterPosition::Top => {
            let g_height = size.min(area.height.saturating_sub(1));
            let gutter = Rect::new(area.x, area.y, area.width, g_height);
            let body = Rect::new(area.x, area.y + g_height, area.width, area.height - g_height);
            (body, gutter)
        }
        GutterPosition::Left => {
            let g_width = size.min(area.width.saturating_sub(1));
            let gutter = Rect::new(area.x, area.y, g_width, area.height);
            let body = Rect::new(area.x + g_width, area.y, area.width - g_width, area.height);
            (body, gutter)
        }
        GutterPosition::Right => {
            let g_width = size.min(area.width.saturating_sub(1));
            let body = Rect::new(area.x, area.y, area.width - g_width, area.height);
            let gutter = Rect::new(area.x + body.width, area.y, g_width, area.height);
            (body, gutter)
        }
    }
}
```

- [ ] **Step 4: Register the module**

Modify `crates/spur-tui/src/components/mod.rs`:
```rust
pub mod loop_layout;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui --test loop_layout`
Expected: PASS — all 4 tests green.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): loop_layout body+gutter rect helper"
```

---

## Task 9: `ExecutorDetailView`

**Files:**
- Create: `crates/spur-tui/src/views/executor_detail.rs`
- Modify: `crates/spur-tui/src/views/mod.rs` (export)
- Modify: `crates/spur-tui/src/action.rs` (add `ViewId::ExecutorDetail`)
- Test: `crates/spur-tui/tests/executor_detail_render.rs`

Mirror of session_detail scoped to one executor. Body shows executor's events; gutter shows brain context (one block before delegation + call args + one block after resume) + breadcrumb.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/executor_detail_render.rs`:
```rust
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use spur_acp::SessionId;
use spur_acp::domain::events::{LifecycleState, Role, SpurEvent, SpurEventBody};
use spur_core::lineage::projection::ExecutorLineage;
use spur_tui::views::executor_detail::ExecutorDetailView;

#[test]
fn renders_executor_body_and_breadcrumb_gutter() {
    let session = SessionId::new();
    let mut lineage = ExecutorLineage::new();
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: None,
        session_id: session.clone(),
        agent: "claude-coder".into(),
        role: Role::Executor,
        task_spec: "refactor auth".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "exec-1".into(),
        phase: LifecycleState::Running,
    }));

    let view = ExecutorDetailView::new("exec-1".to_string(), session.clone());
    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|f| view.render(f, f.size(), &lineage, /* brain_context */ &[]))
        .expect("draw");

    let buf = terminal.backend().buffer().clone();
    let text: String = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| buf.get(x, y).symbol().to_string()))
        .collect();
    assert!(text.contains("EXEC"), "header indicates EXEC mode");
    assert!(text.contains("exec-1") || text.contains("exec/exec"), "exec id present");
    assert!(text.contains("you are here") || text.contains("brain >"), "breadcrumb present");
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p spur-tui --test executor_detail_render`
Expected: FAIL — `ExecutorDetailView` doesn't exist.

- [ ] **Step 3: Implement `ExecutorDetailView`**

Create `crates/spur-tui/src/views/executor_detail.rs`:
```rust
//! Executor mode of the Loop view. Mirrors session_detail in
//! structure but scoped to a single executor: body shows the
//! executor's events; gutter shows brain context (delegation call
//! args + surrounding reasoning) plus a breadcrumb.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use spur_acp::SessionId;
use spur_core::lineage::projection::ExecutorLineage;
use spur_core::ExecutorId;

use crate::components::loop_layout::{split, GutterPosition};

pub struct ExecutorDetailView {
    pub executor_id: String,
    pub brain_session_id: SessionId,
}

impl ExecutorDetailView {
    pub fn new(executor_id: String, brain_session_id: SessionId) -> Self {
        Self { executor_id, brain_session_id }
    }

    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        lineage: &ExecutorLineage,
        brain_context: &[Line<'static>],
    ) {
        let (body_area, gutter_area) = split(area, GutterPosition::Bottom, 1);

        // Header (top line of body): EXEC mode
        let node = lineage.node(&ExecutorId(self.executor_id.clone()));
        let header_text = match node {
            Some(n) => format!(
                " EXEC · {} · attempt {}/{} · {} ",
                short_id(&self.executor_id),
                n.attempt_n,
                n.max_attempts.unwrap_or(3),
                phase_label(n.phase),
            ),
            None => format!(" EXEC · {} · (unknown) ", short_id(&self.executor_id)),
        };
        let header_block = Block::default()
            .title(header_text)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue));

        let mut body_lines: Vec<Line<'static>> = Vec::new();

        // ⤴ brain context (dim)
        if !brain_context.is_empty() {
            body_lines.push(Line::from(Span::styled(
                "⤴ brain context:",
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            )));
            for line in brain_context {
                body_lines.push(line.clone());
            }
            body_lines.push(Line::from(""));
        }

        // ▼ executor body
        body_lines.push(Line::from(Span::styled(
            "▼ executor body",
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )));

        if let Some(node) = node {
            body_lines.push(Line::from(format!("Task: {}", node.task_spec)));
            body_lines.push(Line::from(format!(
                "Spawned · phase={:?} · {}s elapsed",
                node.phase,
                node.elapsed_secs(),
            )));
            // Future: render executor's tool-call stream here once
            // per-executor event log is wired into the projection.
            // For now, the inline card content is the body.
            body_lines.extend(crate::components::inline_executor_card::render_card(
                lineage,
                &ExecutorId(self.executor_id.clone()),
                false,
            ));
        } else {
            body_lines.push(Line::from(Span::styled(
                "(executor not found in lineage)",
                Style::default().fg(Color::Red),
            )));
        }

        let body_paragraph = Paragraph::new(body_lines).block(header_block);
        frame.render_widget(body_paragraph, body_area);

        // Gutter: breadcrumb
        let crumb = breadcrumb(lineage, &self.executor_id);
        let gutter_paragraph = Paragraph::new(Line::from(crumb));
        frame.render_widget(gutter_paragraph, gutter_area);
    }
}

fn phase_label(phase: spur_acp::domain::events::LifecycleState) -> &'static str {
    use spur_acp::domain::events::LifecycleState::*;
    match phase {
        Spawning => "spawning",
        Running => "running",
        AwaitingReview => "awaiting review",
        Resuming => "resuming",
        Succeeded => "succeeded",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn breadcrumb(lineage: &ExecutorLineage, current: &str) -> Vec<Span<'static>> {
    let mut chain: Vec<String> = Vec::new();
    let mut cur = Some(ExecutorId(current.to_string()));
    while let Some(id) = cur {
        chain.push(id.0.clone());
        cur = lineage
            .node(&id)
            .and_then(|n| n.parent_id.as_ref())
            .map(|p| ExecutorId(p.clone()));
    }
    chain.reverse();

    let mut spans = vec![Span::styled(
        "lineage: brain",
        Style::default().fg(Color::DarkGray),
    )];
    for (i, id) in chain.iter().enumerate() {
        let is_last = i == chain.len() - 1;
        spans.push(Span::raw(" > "));
        spans.push(Span::styled(
            format!("exec/{}", short_id(id)),
            if is_last {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ));
        if is_last {
            spans.push(Span::styled(
                " [you are here]",
                Style::default().fg(Color::Cyan),
            ));
        }
    }
    spans
}
```

This requires `ExecutorNode` to expose `attempt_n` and `parent_id`. `parent_id: Option<String>` already exists from `ExecutorSpawned`. For `attempt_n`: prefer reading from `node.pending_review.as_ref().map(|r| r.attempt_n).unwrap_or(1)` rather than adding a separate field — the projection already tracks attempt_n on the `ReviewRequest` and on `ExecutorRetryStarted` events. If neither is present (no review yet, no retry), default to 1. Update the executor_detail render to use that derivation:
```rust
let attempt_n = node.pending_review.as_ref().map(|r| r.attempt_n).unwrap_or(1);
let max_attempts = 3u32; // hard-coded v1 default; future: read from AgentReviewPolicy
```

- [ ] **Step 4: Register module**

Modify `crates/spur-tui/src/views/mod.rs`:
```rust
pub mod executor_detail;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui --test executor_detail_render && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): ExecutorDetailView for Executor mode of Loop view

Body shows scoped executor view (task, phase, inline card content);
gutter shows breadcrumb walking parent chain back to brain. Brain
context section accepts caller-provided lines (one pre-block + call
args + one post-block per spec)."
```

---

## Task 10: Mode-swap routing in `app.rs` (R8, R9, R10, R12, R13)

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Test: `crates/spur-tui/tests/mode_swap_routing.rs`

New actions: `DescendIntoExecutor`, `AscendFromExecutor`, `AscendToRoot`. Tab cycles inline cards in priority order. Peek banner for new content. Auto-follow-bottom.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/mode_swap_routing.rs`:
```rust
use spur_acp::SessionId;
use spur_acp::domain::events::{LifecycleState, Role, SpurEvent, SpurEventBody, ReviewKind, ReviewPayload};
use spur_core::lineage::projection::ExecutorLineage;
use spur_tui::action::Action;
use spur_tui::loop_focus::{tab_priority_order, TabContext};

fn lineage_with_three_states() -> ExecutorLineage {
    let mut l = ExecutorLineage::new();
    let s = SessionId::new();
    let mk_spawn = |id: &str| SpurEventBody::ExecutorSpawned {
        id: id.into(),
        parent_id: None,
        session_id: s.clone(),
        agent: "agent".into(),
        role: Role::Executor,
        task_spec: "task".into(),
    };
    for id in &["e-running", "e-awaiting", "e-failed"] {
        l.apply(&SpurEvent::now(mk_spawn(id)));
    }
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "e-running".into(),
        phase: LifecycleState::Running,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "e-awaiting".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "e-failed".into(),
        phase: LifecycleState::Failed,
    }));
    l
}

#[test]
fn tab_priority_lands_awaiting_review_first() {
    let lineage = lineage_with_three_states();
    let candidates = vec!["e-running".into(), "e-awaiting".into(), "e-failed".into()];
    let order = tab_priority_order(&lineage, &candidates);
    assert_eq!(order[0], "e-awaiting", "AwaitingReview first");
    assert_eq!(order[1], "e-failed", "Failed second");
    assert_eq!(order[2], "e-running", "Running third");
}

#[test]
fn ascend_one_level_action_exists() {
    // Compile-time check: variant exists.
    let _ = Action::AscendFromExecutor;
    let _ = Action::AscendToRoot;
    let _ = Action::DescendIntoExecutor { executor_id: "x".into() };
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test -p spur-tui --test mode_swap_routing`
Expected: FAIL — module + actions don't exist.

- [ ] **Step 3: Add the actions**

Modify `crates/spur-tui/src/action.rs`:
```rust
/// Descend into Executor mode for the given executor.
DescendIntoExecutor { executor_id: String },
/// Ascend one level (Executor → parent Executor or → Brain mode).
AscendFromExecutor,
/// Ascend all the way to root brain mode regardless of depth.
AscendToRoot,
```

Add `ViewId::ExecutorDetail(String)` variant if not present.

- [ ] **Step 4: Implement priority ordering helper**

Create `crates/spur-tui/src/loop_focus.rs`:
```rust
//! Helpers for Loop view focus management — tab priority order,
//! ascent path resolution.

use spur_core::lineage::projection::ExecutorLineage;
use spur_acp::domain::events::LifecycleState;

#[derive(Debug, Clone)]
pub struct TabContext;

/// Returns `candidates` reordered by attention priority:
/// AwaitingReview > Failed/Cancelled > Running > Spawning/Resuming > Succeeded.
/// Ties broken by insertion order in `candidates`.
pub fn tab_priority_order(lineage: &ExecutorLineage, candidates: &[String]) -> Vec<String> {
    let priority = |id: &str| -> u8 {
        let phase = lineage
            .node(&spur_core::ExecutorId(id.to_string()))
            .map(|n| n.phase);
        match phase {
            Some(LifecycleState::AwaitingReview) => 0,
            Some(LifecycleState::Failed) | Some(LifecycleState::Cancelled) => 1,
            Some(LifecycleState::Running) => 2,
            Some(LifecycleState::Spawning) | Some(LifecycleState::Resuming) => 3,
            Some(LifecycleState::Succeeded) => 4,
            None => 5,
        }
    };
    let mut indexed: Vec<(usize, &String)> = candidates.iter().enumerate().collect();
    indexed.sort_by_key(|(i, id)| (priority(id), *i));
    indexed.into_iter().map(|(_, id)| id.clone()).collect()
}
```

Modify `crates/spur-tui/src/lib.rs`:
```rust
pub mod loop_focus;
```

- [ ] **Step 5: Wire actions in app.rs**

In `crates/spur-tui/src/app.rs`, add handlers for the new actions:
```rust
Action::DescendIntoExecutor { executor_id } => {
    self.executor_detail = Some(spur_tui::views::executor_detail::ExecutorDetailView::new(
        executor_id.clone(),
        self.current_brain_session.clone(),
    ));
    self.active_view = ViewId::ExecutorDetail(executor_id);
    self.dirty = true;
}
Action::AscendFromExecutor => {
    if let ViewId::ExecutorDetail(ref id) = self.active_view {
        // Find parent: either parent executor or brain.
        let parent = self
            .lineage
            .node(&spur_core::ExecutorId(id.clone()))
            .and_then(|n| n.parent_id.clone());
        match parent {
            Some(p) => {
                self.executor_detail = Some(spur_tui::views::executor_detail::ExecutorDetailView::new(
                    p.clone(),
                    self.current_brain_session.clone(),
                ));
                self.active_view = ViewId::ExecutorDetail(p);
            }
            None => {
                self.executor_detail = None;
                self.active_view = ViewId::SessionDetail(self.current_brain_session.clone());
            }
        }
        self.dirty = true;
    }
}
Action::AscendToRoot => {
    self.executor_detail = None;
    self.active_view = ViewId::SessionDetail(self.current_brain_session.clone());
    self.dirty = true;
}
```

Wire keybindings: in the appropriate view's `handle_key`, map `>` and `Enter` (on focused inline card) to `DescendIntoExecutor`, `<` and `Esc` to `AscendFromExecutor`, `Ctrl+<` to `AscendToRoot`.

For Tab in session_detail: collect all `executor_id`s from `Delegate` entries, run through `loop_focus::tab_priority_order`, advance focus index.

- [ ] **Step 6: Implement peek banner + auto-follow (R12, R13)**

In session_detail.rs and executor_detail.rs add:
- `auto_follow_bottom: bool` field, default `true`.
- On scroll-up event, set `auto_follow_bottom = false`.
- On `G` (scroll-to-bottom) or scroll-to-bottom event, set `auto_follow_bottom = true`.
- On new event arrival, if not at bottom, render a peek banner at the bottom of the body: `↓ N new lines · <last event description> ─── G to jump`.
- If `auto_follow_bottom == true`, auto-scroll to bottom on any new event.

- [ ] **Step 7: Run tests**

Run: `cargo test -p spur-tui --test mode_swap_routing && cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Wire R10 interactive breadcrumb**

In `crates/spur-tui/src/views/executor_detail.rs::handle_key`, add:
```rust
KeyCode::BackTab => {
    // Shift+Tab focuses the breadcrumb gutter.
    self.breadcrumb_focused = true;
    return None;
}
KeyCode::Left if self.breadcrumb_focused => {
    // Walk one level up the parent chain.
    return Some(Action::AscendFromExecutor);
}
KeyCode::Right if self.breadcrumb_focused => {
    // Cannot descend (no children stored on breadcrumb); no-op.
    return None;
}
KeyCode::Esc if self.breadcrumb_focused => {
    self.breadcrumb_focused = false;
    return None;
}
```

When `breadcrumb_focused`, the breadcrumb render in `executor_detail.rs` (Task 9) highlights the focused crumb with a different color.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): mode-swap routing + Tab priority + ascent ergonomics

R8 Tab cycles in priority order (AwaitingReview > Failed > Running >
Spawning/Resuming > Succeeded). R9 single-level ascent + Ctrl+< to
root. R10 interactive breadcrumb (Shift+Tab focuses gutter, Left
walks ancestors). R12 peek banner for new content while scrolled
up. R13 auto-follow-bottom (default on, disables on scroll-up)."
```

---

## Task 11: `ReviewModal` + vim-modal input flow (R6d, R11)

**Files:**
- Create: `crates/spur-tui/src/components/review_modal.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs` (remove old `'a/d/m/R'` interception path)
- Modify: `crates/spur-tui/src/app.rs` (drive modal state)
- Test: `crates/spur-tui/tests/review_modal_input.rs`

Vim-modal: decision state (default, bare a/d/m/R fire) and edit state ('i' enters, Esc returns). Reject countdown when reason field empty (R11).

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/review_modal_input.rs`:
```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ReviewDecision;
use spur_tui::components::review_modal::{ReviewModal, ModalKeyOutcome, ModalState};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

#[test]
fn decision_state_a_fires_approve() {
    let mut modal = ReviewModal::new("exec-1".into(), 1, /* attempt_n */ 1);
    let out = modal.handle_key(key('a'));
    assert!(matches!(out, ModalKeyOutcome::Decided(ReviewDecision::Approve)));
}

#[test]
fn decision_state_d_with_empty_reason_starts_countdown() {
    let mut modal = ReviewModal::new("exec-1".into(), 1, 1);
    let out = modal.handle_key(key('d'));
    assert!(matches!(out, ModalKeyOutcome::CountdownStarted));
    assert_eq!(modal.state, ModalState::CountdownReject);
}

#[test]
fn decision_state_d_with_reason_dispatches_immediately() {
    let mut modal = ReviewModal::new("exec-1".into(), 1, 1);
    modal.set_reason("token scope check missing".into());
    let out = modal.handle_key(key('d'));
    match out {
        ModalKeyOutcome::Decided(ReviewDecision::Reject { reason }) => {
            assert_eq!(reason, "token scope check missing");
        }
        _ => panic!("expected immediate Reject, got {out:?}"),
    }
}

#[test]
fn i_enters_edit_state_and_chars_buffer() {
    let mut modal = ReviewModal::new("exec-1".into(), 1, 1);
    modal.handle_key(key('i'));
    assert_eq!(modal.state, ModalState::Edit);

    modal.handle_key(key('h'));
    modal.handle_key(key('i'));
    assert_eq!(modal.reason(), "hi");
}

#[test]
fn esc_in_edit_returns_to_decision_state() {
    let mut modal = ReviewModal::new("exec-1".into(), 1, 1);
    modal.handle_key(key('i'));
    modal.handle_key(key('a')); // would fire approve in decision state, but here buffers
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    modal.handle_key(esc);
    assert_eq!(modal.state, ModalState::Decision);
    assert_eq!(modal.reason(), "a");
}

#[test]
fn esc_during_countdown_cancels() {
    let mut modal = ReviewModal::new("exec-1".into(), 1, 1);
    modal.handle_key(key('d'));
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let out = modal.handle_key(esc);
    assert!(matches!(out, ModalKeyOutcome::CountdownCancelled));
    assert_eq!(modal.state, ModalState::Decision);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-tui --test review_modal_input`
Expected: FAIL — `ReviewModal` doesn't exist.

- [ ] **Step 3: Implement `ReviewModal`**

Create `crates/spur-tui/src/components/review_modal.rs`:
```rust
//! Review decision modal — vim-style modal editor (R6d) for the
//! reason/note/constraints field. Reject with empty reason triggers
//! a 3-second countdown (R11) so accidental rejects can be cancelled.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use spur_acp::ReviewDecision;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalState {
    Decision,
    Edit,
    CountdownReject,
}

#[derive(Debug)]
pub enum ModalKeyOutcome {
    /// No state change worth surfacing (e.g., printable in edit mode).
    None,
    /// Modal switched to a new state.
    StateChanged,
    /// User started the destructive-Reject countdown.
    CountdownStarted,
    /// User cancelled the countdown via Esc.
    CountdownCancelled,
    /// Modal should close with this decision.
    Decided(ReviewDecision),
    /// User pressed Esc with no countdown active — close modal.
    Closed,
    /// User pressed 'v' — open pager.
    OpenPager,
}

pub struct ReviewModal {
    pub executor_id: String,
    pub attempt_n: u32,
    pub max_attempts: u32,
    pub state: ModalState,
    reason: String,
    countdown_started_at: Option<std::time::Instant>,
}

impl ReviewModal {
    pub fn new(executor_id: String, attempt_n: u32, max_attempts: u32) -> Self {
        Self {
            executor_id,
            attempt_n,
            max_attempts,
            state: ModalState::Decision,
            reason: String::new(),
            countdown_started_at: None,
        }
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn set_reason(&mut self, r: String) {
        self.reason = r;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ModalKeyOutcome {
        match self.state {
            ModalState::Decision => self.handle_decision_key(key),
            ModalState::Edit => self.handle_edit_key(key),
            ModalState::CountdownReject => self.handle_countdown_key(key),
        }
    }

    fn handle_decision_key(&mut self, key: KeyEvent) -> ModalKeyOutcome {
        match key.code {
            KeyCode::Char('a') => ModalKeyOutcome::Decided(ReviewDecision::Approve),
            KeyCode::Char('d') => {
                if self.reason.is_empty() {
                    self.state = ModalState::CountdownReject;
                    self.countdown_started_at = Some(std::time::Instant::now());
                    ModalKeyOutcome::CountdownStarted
                } else {
                    ModalKeyOutcome::Decided(ReviewDecision::Reject {
                        reason: self.reason.clone(),
                    })
                }
            }
            KeyCode::Char('m') => ModalKeyOutcome::Decided(ReviewDecision::Modify {
                note: if self.reason.is_empty() {
                    "(no note)".into()
                } else {
                    self.reason.clone()
                },
            }),
            KeyCode::Char('R') => ModalKeyOutcome::Decided(ReviewDecision::Retry {
                new_constraints: if self.reason.is_empty() {
                    "(no additional constraints)".into()
                } else {
                    self.reason.clone()
                },
            }),
            KeyCode::Char('i') => {
                self.state = ModalState::Edit;
                ModalKeyOutcome::StateChanged
            }
            KeyCode::Char('v') => ModalKeyOutcome::OpenPager,
            KeyCode::Esc => ModalKeyOutcome::Closed,
            _ => ModalKeyOutcome::None,
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> ModalKeyOutcome {
        match key.code {
            KeyCode::Esc => {
                self.state = ModalState::Decision;
                ModalKeyOutcome::StateChanged
            }
            KeyCode::Enter => {
                self.reason.push('\n');
                ModalKeyOutcome::None
            }
            KeyCode::Backspace => {
                self.reason.pop();
                ModalKeyOutcome::None
            }
            KeyCode::Char(c) => {
                self.reason.push(c);
                ModalKeyOutcome::None
            }
            _ => ModalKeyOutcome::None,
        }
    }

    fn handle_countdown_key(&mut self, key: KeyEvent) -> ModalKeyOutcome {
        match key.code {
            KeyCode::Esc => {
                self.state = ModalState::Decision;
                self.countdown_started_at = None;
                ModalKeyOutcome::CountdownCancelled
            }
            _ => ModalKeyOutcome::None,
        }
    }

    /// Call from the app's tick loop; if the 3-second countdown has
    /// elapsed, returns the Reject decision.
    pub fn poll_countdown(&mut self) -> Option<ReviewDecision> {
        if self.state == ModalState::CountdownReject {
            if let Some(started) = self.countdown_started_at {
                if started.elapsed() >= std::time::Duration::from_secs(3) {
                    self.state = ModalState::Decision;
                    self.countdown_started_at = None;
                    return Some(ReviewDecision::Reject {
                        reason: "(no reason given)".into(),
                    });
                }
            }
        }
        None
    }

    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        agent: &str,
        task: &str,
        diff_summary: Option<&spur_acp::domain::events::DiffSummary>,
        worker_summary: &str,
    ) {
        let width = 70u16.min(area.width.saturating_sub(4));
        let height = 18u16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let popup = Rect::new(x, y, width, height);

        frame.render_widget(Clear, popup);

        let mode_hint = match self.state {
            ModalState::Decision => "[ a/d/m/R decide | i edit reason | v pager | Esc close ]",
            ModalState::Edit => "[ Esc done | Enter newline | Backspace delete ]",
            ModalState::CountdownReject => "[ Esc to cancel ]",
        };
        let title = format!(
            " REVIEW · exec/{} · attempt {}/{} ",
            short_id(&self.executor_id),
            self.attempt_n,
            self.max_attempts,
        );
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let mut lines = vec![
            Line::from(format!("  Agent: {}", agent)),
            Line::from(format!("  Task:  {}", truncate(task, 60))),
        ];
        if let Some(d) = diff_summary {
            lines.push(Line::from(format!(
                "  Diff:  {} files, +{}/-{}",
                d.files_changed, d.insertions, d.deletions
            )));
            for f in d.files.iter().take(8) {
                lines.push(Line::from(format!("    {}", f.display())));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "  Diff:  (not available — orchestrator did not populate)",
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::from(format!(
            "  Summary: \"{}\"",
            truncate(worker_summary, 60)
        )));
        lines.push(Line::from(""));

        match self.state {
            ModalState::CountdownReject => {
                let elapsed = self
                    .countdown_started_at
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0);
                let remaining = 3u64.saturating_sub(elapsed);
                lines.push(Line::from(Span::styled(
                    format!("  ⏳ REJECTING in {remaining}s (Esc to cancel)"),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )));
            }
            _ => {
                lines.push(Line::from(
                    "  [a] approve  [d] deny  [m] modify  [R] retry  [v] pager",
                ));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(format!("  reason: {}", self.reason)));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", mode_hint),
            Style::default().fg(Color::Cyan),
        )));

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, popup);
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}
```

- [ ] **Step 4: Wire modal into app.rs**

In `crates/spur-tui/src/app.rs`, add `review_modal: Option<ReviewModal>` field.
- On `Action::JumpToReview` (or `r` from any view), construct modal if there's a pending review on the focused executor.
- Route key events to modal first when `review_modal.is_some()`.
- On modal `Decided(decision)`: dispatch `Action::SubmitReview { executor_id, attempt_n, decision }`, close modal.
- On modal `Closed` / `CountdownCancelled`: close modal.
- On modal `OpenPager`: dispatch `Action::OpenDiffInPager { executor_id }` (Task 14).
- In tick handler, call `review_modal.poll_countdown()` and if Some, dispatch the Reject + close.
- Render modal as last step (overlay) when `Some`.

Remove the now-dead old review-card handling at `dashboard.rs:344` (the `'a' | 'd' | 'm' | 'R'` arm). The dashboard's Review tab also goes — handled in Task 12.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-tui --test review_modal_input && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): ReviewModal with vim-modal input + reject countdown

R6d: Decision state (bare a/d/m/R fire) vs Edit state ('i' enters,
Esc returns). Eliminates v1's broken 'buffered text + decision keys'
flow that fired Approve on every 'a' character in a typed reason.
R11: Reject with empty reason → 3-second countdown (Esc cancels);
non-empty reason fires immediately. Removes dead dashboard review
key path."
```

---

## Task 12: `r` JumpToReview reroutes through Loop view

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Test: `crates/spur-tui/tests/jump_to_review_via_loop.rs`

`r` from anywhere (brain mode, executor mode, dashboard) navigates to the Loop view of the brain session owning the next pending review and overlays the modal.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-tui/tests/jump_to_review_via_loop.rs`:
```rust
use spur_acp::SessionId;
use spur_acp::domain::events::{Role, SpurEvent, SpurEventBody, ReviewKind, ReviewPayload};
use spur_tui::action::{Action, ViewId};
use spur_tui::app::App;

#[test]
fn jump_to_review_navigates_to_loop_view_and_opens_modal() {
    let session = SessionId::new();
    let mut app = App::new_for_test(session.clone());

    // Spawn an executor with a pending review.
    app.apply_event(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-X".into(),
        parent_id: None,
        session_id: session.clone(),
        agent: "claude-coder".into(),
        role: Role::Executor,
        task_spec: "task".into(),
    }));
    app.apply_event(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "exec-X".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ready".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
    }));

    app.handle_action(Action::JumpToReview);

    assert!(matches!(
        app.active_view(),
        ViewId::ExecutorDetail(id) if id == "exec-X"
    ));
    assert!(app.has_review_modal(), "ReviewModal should be open");
}
```

The test relies on three new pub-test accessors on `App`: `new_for_test(session_id)`, `apply_event(&SpurEvent)`, `handle_action(Action)`, `active_view() -> &ViewId`, `has_review_modal() -> bool`. Add them as `#[cfg(any(test, feature = "test-helpers"))]` items on `App`. Keep them out of the production surface.

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-tui --test jump_to_review_via_loop`
Expected: FAIL — behavior not wired.

- [ ] **Step 3: Wire JumpToReview-via-Loop**

In `crates/spur-tui/src/app.rs::handle_action`, replace the existing `Action::JumpToReview` arm with:
```rust
Action::JumpToReview => {
    let pending = self.lineage.pending_reviews();
    let next = pending.iter().next().cloned();
    if let Some(executor_id) = next {
        // Resolve the brain session owning this executor.
        let brain_session = self
            .lineage
            .node(&executor_id)
            .map(|n| n.session_id.clone())
            .unwrap_or_else(|| self.current_brain_session.clone());
        self.current_brain_session = brain_session;

        // Open Loop view in Executor mode for this exec.
        self.active_view = ViewId::ExecutorDetail(executor_id.0.clone());
        self.executor_detail = Some(spur_tui::views::executor_detail::ExecutorDetailView::new(
            executor_id.0.clone(),
            self.current_brain_session.clone(),
        ));

        // Construct modal from pending review.
        if let Some(node) = self.lineage.node(&executor_id) {
            if let Some(req) = &node.pending_review {
                self.review_modal = Some(spur_tui::components::review_modal::ReviewModal::new(
                    executor_id.0.clone(),
                    req.attempt_n,
                    /* max_attempts */ 3,
                ));
            }
        }
        self.dirty = true;
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-tui --test jump_to_review_via_loop && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): r JumpToReview routes through Loop view + opens modal

r from any view: navigate to Executor mode for the next pending
review's executor, then overlay the ReviewModal. Replaces
dashboard's old Review-tab focus pattern."
```

---

## Task 13: Dashboard role refactor

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/components/detail_pane.rs`
- Test: existing dashboard tests should pass; no new tests required.

Remove Review tab + its rendering. Dashboard's detail pane simplifies to "recent terminals + open in Loop view." Agents tree retained.

- [ ] **Step 1: Remove Review tab from `DetailTab` enum**

In `crates/spur-tui/src/components/detail_pane.rs`, remove the `Review` variant from `DetailTab`. Remove all rendering code that handles `DetailTab::Review`.

- [ ] **Step 2: Remove review-key interception from dashboard**

In `crates/spur-tui/src/views/dashboard.rs:338-365`, delete the entire `if self.input_bar.text().len() == 1 && self.focused_node.is_some() && self.detail_pane.current_tab == DetailTab::Review { ... }` block. The ReviewModal handles all decisions now.

- [ ] **Step 3: Add "open in Loop view" hint to dashboard detail pane**

When focusing an executor in dashboard, the detail pane shows a hint:
```rust
"  ▶ Press 'o' to open in Loop view"
```

Add `Action::OpenInLoopView { executor_id }` and an `'o'` keybinding in dashboard's handle_key. The action navigates to `ViewId::ExecutorDetail(executor_id)`.

- [ ] **Step 4: Run tests**

Run: `cargo test --workspace`
Expected: existing tests pass; any tests that referenced `DetailTab::Review` are updated.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(spur-tui): dashboard sheds per-session inspector role

Remove DetailTab::Review (ReviewModal subsumes). Remove old review
key interception path. Dashboard becomes cross-session monitor +
session picker. Add 'o' to open focused executor in Loop view."
```

---

## Task 14: `v` open-in-pager from review modal

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (handle `Action::OpenDiffInPager`)
- Test: manual smoke (file-side test in CI is brittle for shellouts).

When operator hits `v` in review modal, write the diff to a temp file and shell out to `$PAGER` (default `less`).

- [ ] **Step 1: Add the action**

Modify `crates/spur-tui/src/action.rs`:
```rust
OpenDiffInPager { executor_id: String },
```

- [ ] **Step 2: Add `latest_diff_text` to `ExecutorNode`**

Modify `crates/spur-core/src/lineage/projection.rs` `ExecutorNode`:
```rust
/// Raw unified-diff text (for pager). Populated from
/// `ExecutorArtifact::Diff` events alongside `latest_diff_summary`.
pub latest_diff_text: Option<String>,
```

Today, `Artifact::Diff(DiffSummary)` carries no raw text. Add a sibling `Artifact::DiffText(String)` variant in `crates/spur-acp/src/domain/events.rs`, OR change `Artifact::Diff` to carry both: `Diff { summary: DiffSummary, text: Option<String> }`. Pick the latter (one variant, additive struct fields). Update the orchestrator's emit site to populate `text: Some(outcome.diff)`.

- [ ] **Step 3: Add `tempfile` dev-dependency**

Modify `crates/spur-tui/Cargo.toml`:
```toml
[dependencies]
tempfile = "3"
```

(Production dep, not dev — pager invocation is a runtime path.)

- [ ] **Step 4: Add `tui_suspend` / `tui_resume` helpers**

In `crates/spur-tui/src/lib.rs` (or wherever the ratatui terminal init lives — search for `enable_raw_mode`):
```rust
pub fn tui_suspend() -> std::io::Result<()> {
    use crossterm::execute;
    use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

pub fn tui_resume() -> std::io::Result<()> {
    use crossterm::execute;
    use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    Ok(())
}
```

If a similar pair already exists (e.g., for a help overlay shellout), reuse rather than duplicate.

- [ ] **Step 5: Implement the handler**

In `crates/spur-tui/src/app.rs`:
```rust
Action::OpenDiffInPager { executor_id } => {
    let diff = self
        .lineage
        .node(&spur_core::ExecutorId(executor_id.clone()))
        .and_then(|n| n.latest_diff_text.clone())
        .unwrap_or_default();
    if diff.is_empty() {
        tracing::warn!(executor_id = %executor_id, "no diff available to view");
        return;
    }
    if let Err(e) = open_in_pager(&diff) {
        tracing::warn!(error = %e, "failed to open pager");
    }
}
```

Add the helper:
```rust
fn open_in_pager(content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut tmp = tempfile::NamedTempFile::new()?;
    tmp.write_all(content.as_bytes())?;
    let path = tmp.path().to_path_buf();
    let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
    crate::tui_suspend()?;
    let status = std::process::Command::new(pager).arg(&path).status()?;
    crate::tui_resume()?;
    if !status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("pager exited with {status}"),
        ));
    }
    Ok(())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo build --workspace && cargo test --workspace`
Expected: build clean; existing tests pass.

- [ ] **Step 4: Manual smoke**

Run `spur watch` with a real worker that produces a diff; trigger a review; press `v`. `$PAGER` (or `less`) should open with the diff.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): v opens diff in \$PAGER from review modal

Suspends TUI, shells to \$PAGER (or less), resumes on exit.
Adds latest_diff_text field to ExecutorNode for full diff
retention beyond the DiffSummary stats."
```

---

## Task 15: End-to-end smoke test

**Files:**
- Create: `crates/spur-cli/tests/loop_view_e2e.rs` (or similar location for CLI integration)

Walk a real session: brain delegates → inline card appears → executor runs → review modal opens with diff → operator types reason via vim-modal → Reject dispatches → brain resumes.

This is largely a manual smoke; the automated piece verifies the event chain end-to-end.

- [ ] **Step 1: Write the e2e test**

Create `crates/spur-cli/tests/loop_view_e2e.rs`:
```rust
//! End-to-end smoke for the Close Feedback Loop UI:
//! - Spawn brain + worker
//! - Brain calls delegate_to_worker
//! - Verify DelegationDispatched fires with correct request_id correlation
//! - Verify ExecutorReviewRequested.payload.diff_summary is populated
//! - Verify projection's Delegate trace entry has executor_id attached
//! - Submit a Reject decision through the dispatcher
//! - Verify brain receives Rejected status

#[tokio::test(flavor = "multi_thread")]
async fn loop_view_end_to_end_review_reject() {
    // ... build orchestrator with mock brain + mock worker that
    // produces a known diff. Capture the event stream. Drive a
    // SubmitReview through review_dispatcher_loop. Assert.
    //
    // This is a substantial harness; if existing test infrastructure
    // (e.g., review_loopback_e2e.rs) exists, extend it rather than
    // building fresh.
}
```

If extending `crates/spur-core/tests/review_loopback_e2e.rs` is cleaner, do that instead — the review-loopback e2e already drives the orchestrator end-to-end.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: PASS — full test suite green.

- [ ] **Step 3: Manual smoke (mandatory before declaring done)**

Configure a test agent with `review_required = true`. Run `spur watch`. Issue a brain message that delegates a small task. Verify:
1. Inline card appears in brain conversation when delegation starts
2. Card updates live (tool count, elapsed)
3. When AwaitingReview, card grows + shows ⚠ ATTENTION header
4. `r` opens ReviewModal with diff stats visible
5. `i` enters edit mode; type "test reason"; Esc returns to decision mode
6. `d` immediately dispatches Reject (reason non-empty) — brain receives Rejected status
7. Try with empty reason: `d` shows 3-second countdown; Esc cancels
8. `>` from focused inline card descends to Executor mode
9. `<` ascends back to Brain mode
10. Parallel-N: trigger 3 delegations; verify Tab cycles in priority order (AwaitingReview first)
11. Long stare: leave a card running >30s; verify stale color turns yellow
12. First-time: delete `~/.spur/ux-state.json` and verify banner appears once

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(spur-cli): end-to-end smoke for Close Feedback Loop UI

Drives a brain delegation through the full Loop view: inline card
spawn, live updates, attention-state pop, ReviewModal with diff,
vim-modal reason input, Reject dispatch, brain resume."
```

---

## Notes for the implementer

- **Do not skip the existing test suite at any task boundary.** Each task's "Run all tests" step is mandatory; surfacing a regression three tasks late is exponentially more expensive to debug.
- **Match the spec's UX refinement numbers (R1-R14) in commit messages** so changelog readers can trace.
- **Where the plan asks for a code shape that conflicts with what you find in the existing code**, surface the conflict before writing code. The plan was authored against the spec; the existing codebase may have evolved.
- **Where projection fields don't exist yet** (e.g., `tool_call_count`, `latest_tool_call`, `latest_diff_summary`, `latest_diff_text`), Task 5 / Task 14 explicitly call them out as projection extensions. If the existing projection's `apply` branches don't yet handle the events that populate these (e.g., per-executor tool calls), you may need a small intermediate task before Task 5 to wire those up.
- **All existing audit findings (F4 attempt_n in card, F8 agent+task in card)** are folded into the InlineExecutorCard render in Task 5 — verify they're visible in the manual smoke.
- **F12 (brain-facing JSON framing)** is explicitly NOT in this plan. It's a parallel spur-mcp track. Don't attempt to combine.
