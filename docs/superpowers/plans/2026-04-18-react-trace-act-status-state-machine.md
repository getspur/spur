# ReactTrace Act Status State Machine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the fragile "adjacent `Observe` with `Some(payload)` stops the spinner" rule with an explicit `ActStatus` enum embedded on `TraceKind::Act`, driven by the ACP `ToolCallStatus` enum. Fixes the reported bug (spinner never stops) and the latent inverse bug (partial output stops spinner prematurely).

**Architecture:** Add a `pub use` for `ToolCallId` in `spur-acp`. Introduce `ActStatus {Pending, InProgress, Completed, Failed}` in `spur-tui`. Add two helpers on `ReactTrace`: `find_act_by_id_mut` (backward scan) and `merge_status` (free function). Push-side ToolCall creates Act with mapped initial status; push-side ToolCallUpdate mutates the existing Act in place. Three renderer paths + the tick animator stop using adjacency and read `status` directly.

**Tech Stack:** Rust 2024 edition, ratatui, `agent-client-protocol` schema crate, cargo test, `cargo check --features markdown` and `--no-default-features` for both feature gates.

**Spec:** `docs/superpowers/specs/2026-04-18-react-trace-act-status-state-machine-design.md`

---

## File Structure

**Created (none).**

**Modified:**
- `crates/spur-acp/src/lib.rs` — re-export `ToolCallId`.
- `crates/spur-tui/src/components/react_trace/types.rs` — add `ActStatus` enum; extend `TraceKind::Act`.
- `crates/spur-tui/src/components/react_trace/mod.rs` — add `find_act_by_id_mut` and `merge_status`; rewrite `first_active_spinner`; rewrite `render_to_strings` Act/Observe branches; update 8 `Act { .. }` match sites including tests.
- `crates/spur-tui/src/components/react_trace/builder.rs` — rewrite 7 `Act { .. }` match sites: the collapsed & expanded `build_display_lines` paths and the collapsed & expanded `build_virtual_rows` paths.
- `crates/spur-tui/src/components/react_trace/streaming_tests.rs` — add 9 new tests and update any existing `Act` literals.
- `crates/spur-tui/src/views/session_detail.rs` — rewire `SessionUpdate::ToolCall` (keep `tool_depth` side-effect) and `SessionUpdate::ToolCallUpdate` (mutate-in-place).
- `crates/spur-tui/tests/render_golden.rs` — update `Act` literal; regenerate snapshot after renderer rewrite.
- `crates/spur-tui/benches/*` and `crates/spur-tui/examples/react_trace_bench_sim.rs` (uncommitted) — update `Act` literals if any.

**Not touched:**
- `spur-core`, `spur-mcp`, any orchestrator code.
- `render.rs` (cache machinery; consumes `entries` but never matches on `Act`).
- `replay_history` (produces `UserMessage`/`AgentMessage` only).

---

## Task 1: Re-export `ToolCallId` from `spur-acp`

**Files:**
- Modify: `crates/spur-acp/src/lib.rs:46`

- [ ] **Step 1.1: Add `ToolCallId` to the existing `pub use agent_client_protocol::{...}` block.**

Edit `crates/spur-acp/src/lib.rs` at line 46. The current line reads:
```rust
    ToolCall as AcpToolCall, ToolCallContent, ToolCallLocation, ToolCallStatus,
```
Change it to:
```rust
    ToolCall as AcpToolCall, ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus,
```

- [ ] **Step 1.2: Verify the re-export compiles.**

Run: `cargo check -p spur-acp`
Expected: PASS.

- [ ] **Step 1.3: Verify TUI can reach it.**

Run: `cargo check -p spur-tui --features markdown`
Expected: PASS (no new code yet uses it, so this is a smoke check).

- [ ] **Step 1.4: Commit.**

```bash
git add crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): re-export ToolCallId for downstream use"
```

---

## Task 2: Introduce `ActStatus` enum and extend `TraceKind::Act`

The build goes red on this task. Subsequent tasks progressively bring it green by migrating every `Act { .. }` match arm. Do not skip ahead — finish this task's migration sweep before touching logic.

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/types.rs`

- [ ] **Step 2.1: Add the `ActStatus` enum and extend `TraceKind::Act`.**

Open `crates/spur-tui/src/components/react_trace/types.rs`. Replace the existing
`TraceKind` enum (lines 5-37) with:

```rust
use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
use spur_acp::ToolCallId;

use ratatui::text::Line;

/// Terminal/non-terminal state of a tool call.
///
/// Mirrors `agent_client_protocol::ToolCallStatus` but embeds the outcome
/// payload directly so a single `TraceEntry` represents the full lifecycle
/// of one tool call. Non-terminal variants keep the spinner animating;
/// terminal variants render the outcome glyph.
#[derive(Debug, Clone)]
pub enum ActStatus {
    Pending,
    InProgress {
        /// Streamed partial output. Stored but NOT rendered in Phase 1.
        partial: Option<ObservePayload>,
    },
    Completed(Option<ObservePayload>),
    Failed(Option<ObservePayload>),
}

impl ActStatus {
    /// True when the spinner should keep animating.
    pub fn is_active(&self) -> bool {
        matches!(self, ActStatus::Pending | ActStatus::InProgress { .. })
    }
}

/// What kind of ReAct trace step this entry represents.
#[derive(Debug, Clone)]
pub enum TraceKind {
    Think,
    AgentMessage {
        agent: String,
    },
    Act {
        tool: String,
        family: ToolFamily,
        input: ToolInputDisplay,
        /// ACP-originated calls carry their protocol id; synthetic or
        /// test-generated Acts may use `None`.
        tool_call_id: Option<ToolCallId>,
        /// Drives spinner vs. outcome rendering.
        status: ActStatus,
    },
    /// Informational notes only (system, brain events). Tool-call lifecycle
    /// lives on `Act.status` — do not use `Observe` for tool outcomes.
    Observe {
        payload: Option<ObservePayload>,
    },
    Delegate {
        agent: String,
        task: String,
        status: String,
        request_id: Option<String>,
        executor_id: Option<String>,
    },
    UserMessage,
    Permission {
        description: String,
        pending: bool,
        countdown: u8,
    },
}
```

- [ ] **Step 2.2: Confirm the build is red with the expected error shape.**

Run: `cargo check -p spur-tui --features markdown 2>&1 | head -40`
Expected: a small cluster of errors of the form
`error[E0063]: missing fields 'tool_call_id' and 'status' in initializer of 'TraceKind::Act'`
at `session_detail.rs:1215`, `streaming_tests.rs` (if present), `mod.rs:1195` and `mod.rs:1407`, `render_golden.rs:26`, and the `builder.rs` construction-side (if any). Read the errors — they are the migration worklist for Step 2.3.

- [ ] **Step 2.3: Migrate every `TraceKind::Act { ... }` construction to include the two new fields.**

In this step you ONLY ADD the two new fields with defaults. You do NOT change spinner/render logic or the ToolCallUpdate handler yet. Target shape for each constructor:

```rust
TraceKind::Act {
    tool,
    family,
    input,
    tool_call_id: None,              // default — overwritten by Task 5 for the real push site
    status: ActStatus::Pending,      // default — overwritten by Task 5 / Task 11
}
```

Apply this to every site the compiler flagged in Step 2.2. Import `ActStatus` at the top of each file (e.g. `use crate::components::react_trace::types::{ActStatus, TraceKind};` or `use super::ActStatus;` inside the `react_trace` module).

- [ ] **Step 2.4: Update every `match` arm on `TraceKind::Act { ... }` to ignore the two new fields via `..` rest pattern.**

The compiler will flag these as non-exhaustive patterns. For each of the 14 sites listed in the spec file-and-line table, change the arm to:

```rust
TraceKind::Act { tool, family, input, .. } => { /* existing body unchanged */ }
```

This lets Task 2 land as a pure schema widening with no behavioural change. The renderer still reads adjacency because that code is untouched in this task.

- [ ] **Step 2.5: Verify the build is green again.**

Run: `cargo check -p spur-tui --features markdown`
Expected: PASS.
Run: `cargo check -p spur-tui --no-default-features`
Expected: PASS.

- [ ] **Step 2.6: Run the existing tests to confirm no regression.**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: all tests pass. Behaviour is unchanged because the new fields default to `None`/`Pending` and no match arm reads them yet.

- [ ] **Step 2.7: Commit.**

```bash
git add crates/spur-tui/src/
git commit -m "refactor(spur-tui): add ActStatus and widen TraceKind::Act schema

No behavioural change. Every existing Act constructor defaults tool_call_id
to None and status to ActStatus::Pending; every match arm ignores the new
fields via .. rest pattern. Subsequent tasks rewire push-side and
renderer to use the new state machine."
```

---

## Task 3: Add `find_act_by_id_mut` helper on `ReactTrace`

TDD. The helper is used by Task 6 to locate Acts by their ACP id on incoming ToolCallUpdate events.

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`
- Test: `crates/spur-tui/src/components/react_trace/mod.rs` (new test in the existing `#[cfg(test)] mod tests` block near the bottom).

- [ ] **Step 3.1: Write the failing test.**

Append to the `#[cfg(test)] mod tests { ... }` block at the bottom of `crates/spur-tui/src/components/react_trace/mod.rs`:

```rust
    #[test]
    fn find_act_by_id_mut_returns_newest_matching_act() {
        use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
        use spur_acp::ToolCallId;
        use std::sync::Arc;

        let mut trace = ReactTrace::new();
        let id_a: ToolCallId = ToolCallId(Arc::from("call-A"));
        let id_b: ToolCallId = ToolCallId(Arc::from("call-B"));
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "first".into(),
                family: ToolFamily::Unknown,
                input: ToolInputDisplay::Empty,
                tool_call_id: Some(id_a.clone()),
                status: ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "t0".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "second".into(),
                family: ToolFamily::Unknown,
                input: ToolInputDisplay::Empty,
                tool_call_id: Some(id_b.clone()),
                status: ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "t1".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });

        let found = trace.find_act_by_id_mut(&id_a);
        assert!(found.is_some(), "should find act by id");
        let (idx, entry) = found.unwrap();
        assert_eq!(idx, 0, "should return the matching entry's absolute index");
        assert!(
            matches!(&entry.kind, TraceKind::Act { tool, .. } if tool == "first"),
            "should return a mutable reference to the matching entry"
        );

        let id_missing: ToolCallId = ToolCallId(Arc::from("nope"));
        assert!(trace.find_act_by_id_mut(&id_missing).is_none());
    }
```

- [ ] **Step 3.2: Run the test to verify it fails.**

Run: `cargo test -p spur-tui --features markdown --lib find_act_by_id_mut_returns_newest_matching_act`
Expected: FAIL with `no method named 'find_act_by_id_mut' found`.

- [ ] **Step 3.3: Implement the helper.**

In `crates/spur-tui/src/components/react_trace/mod.rs`, add at the top near the other `use` statements:

```rust
use spur_acp::ToolCallId;
```

Then, inside the existing `impl ReactTrace { ... }` block (the one that contains `attach_executor_id`), add:

```rust
    /// Locate the newest `TraceKind::Act` entry whose `tool_call_id` matches.
    /// Returns the absolute entry index and a mutable reference, or `None`.
    ///
    /// Compares the inner `Arc<str>` content rather than `Arc` identity, so
    /// ids produced by separate protocol round trips still compare equal.
    pub(crate) fn find_act_by_id_mut(
        &mut self,
        id: &ToolCallId,
    ) -> Option<(usize, &mut TraceEntry)> {
        let needle: &str = id.0.as_ref();
        for (idx, entry) in self.entries.iter_mut().enumerate().rev() {
            if let TraceKind::Act {
                tool_call_id: Some(existing),
                ..
            } = &entry.kind
            {
                if existing.0.as_ref() == needle {
                    return Some((idx, entry));
                }
            }
        }
        None
    }
```

- [ ] **Step 3.4: Run the test to verify it passes.**

Run: `cargo test -p spur-tui --features markdown --lib find_act_by_id_mut_returns_newest_matching_act`
Expected: PASS.

- [ ] **Step 3.5: Also run under no-default-features to confirm the helper compiles without markdown.**

Run: `cargo test -p spur-tui --no-default-features --lib find_act_by_id_mut_returns_newest_matching_act`
Expected: PASS.

- [ ] **Step 3.6: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui): add ReactTrace::find_act_by_id_mut helper"
```

---

## Task 4: Add `merge_status` free function

TDD. Pure function, easy to test in isolation. Used by Task 6.

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`

- [ ] **Step 4.1: Write three failing tests covering the core transitions.**

Append to the same `#[cfg(test)] mod tests { ... }` block:

```rust
    #[test]
    fn merge_status_pending_to_completed_with_payload() {
        use spur_acp::adapter::ObservePayload;
        use spur_acp::{AgentKind, ToolCallStatus};
        let payload_json = serde_json::json!({"text": "ok"});
        let new = super::merge_status(
            &ActStatus::Pending,
            Some(ToolCallStatus::Completed),
            Some(&payload_json),
            AgentKind::Generic,
        );
        match new {
            ActStatus::Completed(Some(ObservePayload::Text { .. })) => {}
            other => panic!("expected Completed(Some(Text)), got {:?}", other),
        }
    }

    #[test]
    fn merge_status_completed_is_terminal_ignores_late_in_progress() {
        use spur_acp::adapter::ObservePayload;
        use spur_acp::{AgentKind, ToolCallStatus};
        let prev = ActStatus::Completed(Some(ObservePayload::Text {
            body: "done".into(),
        }));
        let new = super::merge_status(
            &prev,
            Some(ToolCallStatus::InProgress),
            None,
            AgentKind::Generic,
        );
        // Terminal state must not be reopened.
        assert!(
            matches!(new, ActStatus::Completed(Some(_))),
            "terminal Completed must not regress to InProgress, got {:?}",
            new
        );
    }

    #[test]
    fn merge_status_none_incoming_status_preserves_variant() {
        use spur_acp::AgentKind;
        let prev = ActStatus::Pending;
        let new = super::merge_status(&prev, None, None, AgentKind::Generic);
        assert!(matches!(new, ActStatus::Pending));
    }
```

- [ ] **Step 4.2: Run the tests to verify they fail.**

Run: `cargo test -p spur-tui --features markdown --lib merge_status_`
Expected: FAIL with `cannot find function 'merge_status'`.

- [ ] **Step 4.3: Implement `merge_status`.**

In `crates/spur-tui/src/components/react_trace/mod.rs`, below the existing `row_to_anchor` helper (around line 76) and above the `impl ReactTrace` block, add:

```rust
/// Merge an incoming `ToolCallUpdate.fields` into the previous `ActStatus`.
///
/// Rules:
///   - Terminal `prev` (Completed / Failed): `debug_assert!` that the
///     incoming status, if present, matches; return `prev.clone()` unchanged.
///     Prevents a late `InProgress` update from reopening a closed tool call.
///   - `incoming_status == None`: keep `prev` variant; refresh
///     `InProgress.partial` only when `prev` is `InProgress` AND
///     `incoming_raw_output` is `Some(v)`.
///   - `incoming_status == Some(s)`: map `(s, incoming_raw_output)` to a
///     new `ActStatus`. An incoming terminal always replaces non-terminal.
///   - Any future `ToolCallStatus` variant not listed here (the enum may
///     become `#[non_exhaustive]` upstream) is absorbed: log via
///     `tracing::debug!` and return `prev.clone()`.
pub(super) fn merge_status(
    prev: &super::react_trace::types::ActStatus,
    incoming_status: Option<spur_acp::ToolCallStatus>,
    incoming_raw_output: Option<&serde_json::Value>,
    kind: spur_acp::AgentKind,
) -> super::react_trace::types::ActStatus {
    use spur_acp::adapter::extract_observe;
    use spur_acp::ToolCallStatus;
    use types::ActStatus;

    let parse = |v: &serde_json::Value| extract_observe(v, kind);

    // Terminal prev wins.
    if matches!(prev, ActStatus::Completed(_) | ActStatus::Failed(_)) {
        if let Some(s) = incoming_status {
            let prev_is_completed = matches!(prev, ActStatus::Completed(_));
            let prev_is_failed = matches!(prev, ActStatus::Failed(_));
            let ok = (prev_is_completed && matches!(s, ToolCallStatus::Completed))
                || (prev_is_failed && matches!(s, ToolCallStatus::Failed));
            debug_assert!(
                ok,
                "late ToolCallUpdate tried to change terminal state: prev={:?} incoming={:?}",
                prev, s
            );
            if !ok {
                tracing::debug!(
                    ?prev,
                    incoming = ?s,
                    "ignoring late ToolCallUpdate on terminal ActStatus"
                );
            }
        }
        return prev.clone();
    }

    let Some(s) = incoming_status else {
        // No status change. Possibly refresh partial on InProgress.
        return match (prev, incoming_raw_output) {
            (ActStatus::InProgress { .. }, Some(v)) => ActStatus::InProgress {
                partial: Some(parse(v)),
            },
            _ => prev.clone(),
        };
    };

    match s {
        ToolCallStatus::Pending => ActStatus::Pending,
        ToolCallStatus::InProgress => ActStatus::InProgress {
            partial: incoming_raw_output.map(parse),
        },
        ToolCallStatus::Completed => ActStatus::Completed(incoming_raw_output.map(parse)),
        ToolCallStatus::Failed => ActStatus::Failed(incoming_raw_output.map(parse)),
        _ => {
            tracing::debug!(
                ?prev,
                incoming = ?s,
                "unknown ToolCallStatus variant; preserving prev"
            );
            prev.clone()
        }
    }
}
```

Add `use types::ActStatus;` near the top of `mod.rs` if it's not already in scope, or reference `crate::components::react_trace::types::ActStatus` in the signature.

- [ ] **Step 4.4: Run the tests to verify they pass.**

Run: `cargo test -p spur-tui --features markdown --lib merge_status_`
Expected: all 3 tests PASS.

- [ ] **Step 4.5: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui): add merge_status for ActStatus transitions"
```

---

## Task 5: Add `map_initial_status` helper for the ToolCall push-side

TDD. Used by Task 6.

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`

- [ ] **Step 5.1: Write the failing test.**

Append to the test block:

```rust
    #[test]
    fn map_initial_status_pending_yields_pending() {
        use spur_acp::{AgentKind, ToolCallStatus};
        let got = super::map_initial_status(ToolCallStatus::Pending, None, AgentKind::Generic);
        assert!(matches!(got, ActStatus::Pending));
    }

    #[test]
    fn map_initial_status_completed_with_output_yields_completed_some() {
        use spur_acp::{AgentKind, ToolCallStatus};
        let out = serde_json::json!({"text": "hi"});
        let got = super::map_initial_status(
            ToolCallStatus::Completed,
            Some(&out),
            AgentKind::Generic,
        );
        assert!(matches!(got, ActStatus::Completed(Some(_))));
    }
```

- [ ] **Step 5.2: Run the tests to verify they fail.**

Run: `cargo test -p spur-tui --features markdown --lib map_initial_status_`
Expected: FAIL with `cannot find function 'map_initial_status'`.

- [ ] **Step 5.3: Implement `map_initial_status`.**

Below `merge_status` in `mod.rs`:

```rust
/// Map an ACP `ToolCallStatus` + optional `raw_output` to an `ActStatus`
/// for a newly-created Act entry. Honours the incoming status — an agent
/// may stream an already-completed tool call on the first event.
pub(super) fn map_initial_status(
    status: spur_acp::ToolCallStatus,
    raw_output: Option<&serde_json::Value>,
    kind: spur_acp::AgentKind,
) -> types::ActStatus {
    use spur_acp::adapter::extract_observe;
    use spur_acp::ToolCallStatus;
    use types::ActStatus;

    let parse = |v: &serde_json::Value| extract_observe(v, kind);
    match status {
        ToolCallStatus::Pending => ActStatus::Pending,
        ToolCallStatus::InProgress => ActStatus::InProgress {
            partial: raw_output.map(parse),
        },
        ToolCallStatus::Completed => ActStatus::Completed(raw_output.map(parse)),
        ToolCallStatus::Failed => ActStatus::Failed(raw_output.map(parse)),
        _ => ActStatus::Pending,
    }
}
```

- [ ] **Step 5.4: Run the tests to verify they pass.**

Run: `cargo test -p spur-tui --features markdown --lib map_initial_status_`
Expected: PASS.

- [ ] **Step 5.5: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui): add map_initial_status for Act creation"
```

---

## Task 6: Rewire `SessionUpdate::ToolCall` and `ToolCallUpdate` handlers

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:1189-1247`

- [ ] **Step 6.1: Rewrite the `ToolCall` arm to populate `tool_call_id` and `status`.**

Replace `session_detail.rs:1189-1225` with the following. **Keep the `self.tool_depth.insert(...)` line at its existing position** — it must survive the refactor:

```rust
                    spur_acp::SessionUpdate::ToolCall(tc) => {
                        use spur_acp::adapter::{self, ToolInputDisplay};
                        let kind = self.agent_kind();
                        let meta = spur_acp::adapter::extract_tool_meta(tc, kind);
                        let display_name =
                            meta.tool_name.as_deref().unwrap_or(tc.title.as_str());
                        let depth = meta
                            .parent_tool_use_id
                            .as_ref()
                            .and_then(|pid| self.tool_depth.get(pid).copied())
                            .map(|d| d.saturating_add(1).min(8))
                            .unwrap_or(0);
                        self.tool_depth
                            .insert(tc.tool_call_id.0.to_string(), depth);
                        let indent = "  ".repeat(depth as usize);
                        let tool = format!("{}{}", indent, display_name);
                        let family = adapter::classify_tool(tc, kind);
                        let input = tc
                            .raw_input
                            .as_ref()
                            .map(|v| adapter::format_input(v, kind))
                            .unwrap_or(ToolInputDisplay::Empty);
                        let fallback_text = extract_tool_call_text(&tc.content)
                            .or_else(|| tc.raw_input.as_ref().map(format_tool_args))
                            .unwrap_or_default();
                        let status = crate::components::react_trace::map_initial_status(
                            tc.status,
                            tc.raw_output.as_ref(),
                            kind,
                        );
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Act {
                                tool,
                                family,
                                input,
                                tool_call_id: Some(tc.tool_call_id.clone()),
                                status,
                            },
                            text: fallback_text,
                            timestamp: Self::now_stamp(),
                            #[cfg(feature = "markdown")]
                            markdown: None,
                        });
                    }
```

If `map_initial_status` is not re-exported at `crate::components::react_trace::`, adjust the call path to match the module where it lives. A clean option is to add `pub(crate) use map_initial_status;` inside `crates/spur-tui/src/components/react_trace/mod.rs` at the top-level (outside any `impl`), which makes the function reachable as `crate::components::react_trace::map_initial_status`.

- [ ] **Step 6.2: Rewrite the `ToolCallUpdate` arm to mutate in place.**

Replace `session_detail.rs:1226-1247` with:

```rust
                    spur_acp::SessionUpdate::ToolCallUpdate(tcu) => {
                        use crate::components::react_trace::types::{ActStatus, TraceKind};
                        let kind = self.agent_kind();
                        if let Some((idx, act_entry)) =
                            self.react_trace.find_act_by_id_mut(&tcu.tool_call_id)
                        {
                            let new_status = if let TraceKind::Act { status, .. } =
                                &act_entry.kind
                            {
                                crate::components::react_trace::merge_status(
                                    status,
                                    tcu.fields.status,
                                    tcu.fields.raw_output.as_ref(),
                                    kind,
                                )
                            } else {
                                unreachable!("find_act_by_id_mut only returns Act entries")
                            };
                            if let TraceKind::Act { status, .. } = &mut act_entry.kind {
                                *status = new_status;
                            }
                            self.react_trace.mark_dirty_from_for_update(idx);
                        } else if tcu.fields.title.is_some() || tcu.fields.kind.is_some() {
                            // Out-of-order update arriving before ToolCall — synthesize.
                            tracing::debug!(
                                id = ?tcu.tool_call_id,
                                "ToolCallUpdate before ToolCall; synthesizing Act"
                            );
                            let tool = tcu
                                .fields
                                .title
                                .clone()
                                .unwrap_or_else(|| "unknown".into());
                            let family = spur_acp::adapter::ToolFamily::Unknown;
                            let input = spur_acp::adapter::ToolInputDisplay::Empty;
                            let status = crate::components::react_trace::map_initial_status(
                                tcu.fields.status.unwrap_or(spur_acp::ToolCallStatus::Pending),
                                tcu.fields.raw_output.as_ref(),
                                kind,
                            );
                            self.react_trace.push(TraceEntry {
                                kind: TraceKind::Act {
                                    tool,
                                    family,
                                    input,
                                    tool_call_id: Some(tcu.tool_call_id.clone()),
                                    status,
                                },
                                text: String::new(),
                                timestamp: Self::now_stamp(),
                                #[cfg(feature = "markdown")]
                                markdown: None,
                            });
                        } else {
                            tracing::debug!(
                                id = ?tcu.tool_call_id,
                                "dropping ToolCallUpdate with no matching Act and no title/kind"
                            );
                        }
                    }
```

- [ ] **Step 6.3: Expose `mark_dirty_from` to the view.**

The existing `mark_dirty_from` is `fn` (private) on `impl ReactTrace` (`mod.rs:163`). Rename it to `pub(crate) fn mark_dirty_from_for_update` OR add a public shim. Cleanest: add a separate `pub(crate) fn mark_dirty_from_for_update(&mut self, idx: usize) { self.mark_dirty_from(idx); }` on `impl ReactTrace` so the name is unambiguous when grep'd from outside the module. Add it directly below the existing `mark_dirty_from`.

```rust
    /// Public wrapper for external callers that legitimately mutate an
    /// entry in-place (e.g. `SessionUpdate::ToolCallUpdate` merging into
    /// an existing `Act`). Bumps generation + marks cache dirty from idx.
    pub(crate) fn mark_dirty_from_for_update(&mut self, idx: usize) {
        self.mark_dirty_from(idx);
    }
```

- [ ] **Step 6.4: Re-export `map_initial_status` and `merge_status` at the `react_trace` module root for call-site convenience.**

In `crates/spur-tui/src/components/react_trace/mod.rs`, at the top-level (after the existing `pub use` statements), add:

```rust
pub(crate) use self::types::ActStatus;
```

Confirm `map_initial_status` and `merge_status` are `pub(super)` or `pub(crate)` — if the handler cannot see them as `crate::components::react_trace::map_initial_status`, widen the visibility to `pub(crate)`.

- [ ] **Step 6.5: Confirm the TUI crate builds and all existing tests pass.**

Run: `cargo check -p spur-tui --features markdown`
Expected: PASS.
Run: `cargo test -p spur-tui --features markdown --lib`
Expected: existing tests PASS. Some render-shape tests for collapsed Act+Observe pairs may still pass because the renderer hasn't been rewritten yet — that is fine. Render rewrite lands in Task 8.

- [ ] **Step 6.6: Commit.**

```bash
git add crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "feat(spur-tui): mutate Act.status in place on ToolCallUpdate

Replace the push-a-new-Observe pattern with find_act_by_id_mut +
merge_status. Preserves tool_depth side-effect for sub-agent indentation.
Handles out-of-order ToolCallUpdate by synthesizing an Act when title or
kind is present; drops silently with a debug log otherwise."
```

---

## Task 7: Rewrite `first_active_spinner`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:469-486`

- [ ] **Step 7.1: Add a failing test.**

Append to the test block:

```rust
    #[test]
    fn first_active_spinner_returns_pending_act_index() {
        use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
        let mut trace = ReactTrace::new();
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "t".into(),
                family: ToolFamily::Unknown,
                input: ToolInputDisplay::Empty,
                tool_call_id: None,
                status: ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "t0".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        assert_eq!(trace.first_active_spinner(), Some(0));

        // Transition to Completed: spinner should stop.
        if let TraceKind::Act { status, .. } = &mut trace.entries[0].kind {
            *status = ActStatus::Completed(None);
        }
        assert_eq!(trace.first_active_spinner(), None);
    }
```

Note: `first_active_spinner` is currently private. Either make it `pub(crate)` or expose a test helper. Make it `pub(crate)`:

- [ ] **Step 7.2: Run the test to verify it fails.**

Run: `cargo test -p spur-tui --features markdown --lib first_active_spinner_returns_pending_act_index`
Expected: FAIL — either "method is private" or assertion failure depending on prior visibility.

- [ ] **Step 7.3: Rewrite `first_active_spinner`.**

Replace the existing `first_active_spinner` (`mod.rs:469-486`) with:

```rust
    /// Returns the index of the first entry whose tool call is still
    /// animating (Pending or InProgress). Caller uses this to drive cache
    /// invalidation in `tick`.
    pub(crate) fn first_active_spinner(&self) -> Option<usize> {
        self.entries.iter().position(|e| {
            matches!(
                &e.kind,
                TraceKind::Act { status, .. } if status.is_active()
            )
        })
    }
```

- [ ] **Step 7.4: Run the test.**

Run: `cargo test -p spur-tui --features markdown --lib first_active_spinner_returns_pending_act_index`
Expected: PASS.

- [ ] **Step 7.5: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "refactor(spur-tui): first_active_spinner reads ActStatus.is_active"
```

---

## Task 8: Rewrite collapsed plain-text renderer in `render_to_strings`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:650-848` (the `render_to_strings` function).

- [ ] **Step 8.1: Write the failing test.**

Append to the test block:

```rust
    #[test]
    fn render_to_strings_completed_act_shows_outcome_glyph_not_spinner() {
        use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
        let mut trace = ReactTrace::new();
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "echo hi".into(),
                    cwd: None,
                },
                tool_call_id: None,
                status: ActStatus::Completed(Some(ObservePayload::CommandOutput {
                    exit_code: Some(0),
                    stdout: "hi".into(),
                    stderr: String::new(),
                })),
            },
            text: String::new(),
            timestamp: "10:00".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        let lines = trace.render_to_strings().join("\n");
        assert!(
            lines.contains("✓"),
            "expected success glyph in collapsed render, got:\n{lines}"
        );
        for frame in SPINNER_FRAMES {
            assert!(
                !lines.contains(frame),
                "completed Act must not render a spinner frame ({frame}) in:\n{lines}"
            );
        }
    }

    #[test]
    fn render_to_strings_pending_act_shows_spinner_placeholder() {
        use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
        let mut trace = ReactTrace::new();
        trace.push(TraceEntry {
            kind: TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "sleep 5".into(),
                    cwd: None,
                },
                tool_call_id: None,
                status: ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "10:00".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
        let joined = trace.render_to_strings().join("\n");
        // render_to_strings emits the Unicode ellipsis placeholder for the
        // plain-text path's spinner slot (not a real animated frame).
        assert!(
            joined.contains("\u{2026}") || SPINNER_FRAMES.iter().any(|f| joined.contains(f)),
            "pending Act must render a spinner placeholder, got:\n{joined}"
        );
    }
```

- [ ] **Step 8.2: Run the tests to verify they fail.**

Run: `cargo test -p spur-tui --features markdown --lib render_to_strings_`
Expected: FAIL (the existing renderer still reads adjacency; for a single-entry trace with no Observe neighbour it emits the ellipsis today, so `pending` test may PASS incidentally; `completed` test will FAIL because there is no adjacent Observe to provide the glyph).

- [ ] **Step 8.3: Rewrite the collapsed Act branch in `render_to_strings`.**

Locate the `while i < self.entries.len() { ... }` loop at `mod.rs:650-848`. Replace the current `if collapsed { if let TraceKind::Act { ... } ... }` block (roughly lines 654-684) with:

```rust
            if collapsed {
                if let TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } = &entry.kind
                {
                    let (act_glyph, _) = family_glyph(*family);
                    let id_str = input_summary(input, tool);
                    let tail = match status {
                        ActStatus::Pending | ActStatus::InProgress { .. } => {
                            "\u{2026}".to_string()
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (glyph, _, stats) =
                                super::trace_format::observe_compact(p);
                            if stats.is_empty() {
                                glyph.to_string()
                            } else {
                                format!("{} {}", glyph, stats)
                            }
                        }
                        ActStatus::Completed(None) => "✓".to_string(),
                        ActStatus::Failed(_) => "✗".to_string(),
                    };
                    lines.push(format!(
                        "{} {} {}  {}",
                        entry.timestamp, act_glyph, id_str, tail
                    ));
                    lines.push(String::new());
                    i += 1;
                    continue;
                }
            }
```

- [ ] **Step 8.4: Rewrite the non-collapsed Act branch further down in the same function.**

Replace the existing `TraceKind::Act { tool, family, input }` arm (roughly lines 753-771) with:

```rust
                TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } => {
                    let (glyph, _) = family_glyph(*family);
                    lines.push(format!("{} {} {}", entry.timestamp, glyph, tool));
                    if matches!(input, ToolInputDisplay::Empty) {
                        for text_line in entry.text.lines() {
                            lines.push(format!("   {}", text_line));
                        }
                    } else {
                        for l in input_display_lines(input) {
                            let joined: String =
                                l.spans.iter().map(|s| s.content.as_ref()).collect();
                            lines.push(joined);
                        }
                    }
                    // Terminal states in expanded mode also render the outcome
                    // body inline from `status` (there is no paired Observe).
                    match status {
                        ActStatus::Completed(Some(p)) | ActStatus::Failed(Some(p)) => {
                            let verb = super::trace_format::observe_verb(p);
                            let (glyph, _) = match status {
                                ActStatus::Failed(_) => ("✗", ratatui::style::Color::Red),
                                _ => super::trace_format::outcome_glyph(p),
                            };
                            lines.push(format!(
                                "{} {} {}",
                                entry.timestamp, glyph, verb
                            ));
                            for l in
                                super::trace_format::observe_payload_lines(p, self.observe_collapsed)
                            {
                                let joined: String =
                                    l.spans.iter().map(|s| s.content.as_ref()).collect();
                                lines.push(joined);
                            }
                        }
                        ActStatus::Completed(None) => {
                            lines.push(format!("{} ✓ done", entry.timestamp));
                        }
                        ActStatus::Failed(None) => {
                            lines.push(format!("{} ✗ failed", entry.timestamp));
                        }
                        ActStatus::Pending | ActStatus::InProgress { .. } => {}
                    }
                }
```

- [ ] **Step 8.5: Remove the `skip_blank` adjacency suppression in the same function.**

The block near `mod.rs:836-843`:
```rust
let skip_blank = matches!(&entry.kind, TraceKind::Act { .. })
    && matches!(
        self.entries.get(i + 1).map(|e| &e.kind),
        Some(TraceKind::Observe { payload: Some(_) })
    );
if !skip_blank {
    lines.push(String::new());
}
```
Replace with an unconditional:
```rust
lines.push(String::new());
```
Rationale: in the new model there is no adjacent Observe pair, so the conditional always evaluates to `false` anyway. Dropping it clarifies the invariant.

- [ ] **Step 8.6: Run the new tests.**

Run: `cargo test -p spur-tui --features markdown --lib render_to_strings_`
Expected: PASS.

- [ ] **Step 8.7: Run all existing tests to catch regressions.**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: all PASS. If any existing render assertion breaks because it expected an adjacent `Observe`, update the test fixture to use `ActStatus::Completed(Some(...))` on the Act instead of pushing a separate Observe.

- [ ] **Step 8.8: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "refactor(spur-tui): render_to_strings reads ActStatus for spinner vs outcome"
```

---

## Task 9: Rewrite collapsed `build_display_lines` (markdown build) renderer

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs:37-78`

- [ ] **Step 9.1: Write a failing test that drives the markdown-build display-line output.**

Append to `streaming_tests.rs` (or to `mod.rs` test module if `streaming_tests.rs` is gated):

```rust
    #[cfg(feature = "markdown")]
    #[test]
    fn build_display_lines_completed_shows_outcome_glyph() {
        use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
        let mut trace = super::ReactTrace::new();
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "echo".into(),
                    cwd: None,
                },
                tool_call_id: None,
                status: super::ActStatus::Completed(Some(ObservePayload::CommandOutput {
                    exit_code: Some(0),
                    stdout: "hi".into(),
                    stderr: String::new(),
                })),
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        let lines = trace.build_display_lines_for_tests(super::SPINNER_FRAMES[0], None);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect();
        assert!(joined.contains("✓"), "expected success glyph: {joined}");
        for f in super::SPINNER_FRAMES {
            assert!(
                !joined.contains(f),
                "must not render spinner frame {f} for completed Act: {joined}"
            );
        }
    }
```

- [ ] **Step 9.2: Run the test to verify it fails.**

Run: `cargo test -p spur-tui --features markdown --lib build_display_lines_completed_shows_outcome_glyph`
Expected: FAIL — the existing collapsed renderer looks at `entries.get(i+1)` and does not see a paired Observe, so it emits the spinner frame.

- [ ] **Step 9.3: Rewrite the collapsed Act branch in `build_display_lines`.**

In `builder.rs`, replace the `if collapsed { if let TraceKind::Act { ... } }` block (lines 37-78) with:

```rust
            if collapsed {
                if let TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } = &entry.kind
                {
                    let (act_glyph, act_color) = family_glyph(*family);
                    let id_str = input_summary(input, tool);
                    let mut spans = vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("{} {}", act_glyph, id_str),
                            Style::default().fg(act_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                    ];
                    match status {
                        ActStatus::Pending | ActStatus::InProgress { .. } => {
                            spans.push(Span::styled(
                                spinner_frame.to_string(),
                                Style::default().fg(Color::Yellow),
                            ));
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (obs_glyph, obs_color, stats) = observe_compact(p);
                            spans.push(Span::styled(
                                obs_glyph.to_string(),
                                Style::default()
                                    .fg(obs_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            if !stats.is_empty() {
                                spans.push(Span::raw(" "));
                                spans.push(Span::styled(
                                    stats,
                                    Style::default().fg(Color::DarkGray),
                                ));
                            }
                        }
                        ActStatus::Completed(None) => {
                            spans.push(Span::styled(
                                "✓".to_string(),
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        ActStatus::Failed(_) => {
                            spans.push(Span::styled(
                                "✗".to_string(),
                                Style::default()
                                    .fg(Color::Red)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    lines.push(Line::from(spans));
                    lines.push(Line::from(""));
                    i += 1;
                    continue;
                }
            }
```

Add `use super::ActStatus;` or `use super::types::ActStatus;` at the top of `builder.rs` if not already imported.

- [ ] **Step 9.4: Run the test.**

Run: `cargo test -p spur-tui --features markdown --lib build_display_lines_completed_shows_outcome_glyph`
Expected: PASS.

- [ ] **Step 9.5: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "refactor(spur-tui): build_display_lines collapsed Act reads ActStatus"
```

---

## Task 10: Rewrite expanded `build_display_lines` Act branch

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs:149-177`

- [ ] **Step 10.1: Write a failing test for expanded mode.**

Append to `streaming_tests.rs`:

```rust
    #[cfg(feature = "markdown")]
    #[test]
    fn build_display_lines_expanded_completed_renders_outcome_body() {
        use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
        let mut trace = super::ReactTrace::new();
        trace.toggle_observe_collapsed(); // expanded
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "echo hi".into(),
                    cwd: None,
                },
                tool_call_id: None,
                status: super::ActStatus::Completed(Some(ObservePayload::CommandOutput {
                    exit_code: Some(0),
                    stdout: "hi".into(),
                    stderr: String::new(),
                })),
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        let lines = trace.build_display_lines_for_tests(super::SPINNER_FRAMES[0], None);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect();
        assert!(joined.contains("hi"), "expected stdout body: {joined}");
        assert!(joined.contains("✓"), "expected success glyph: {joined}");
    }
```

- [ ] **Step 10.2: Run the test to verify it fails.**

Run: `cargo test -p spur-tui --features markdown --lib build_display_lines_expanded_completed_renders_outcome_body`
Expected: FAIL — expanded renderer currently reads body from a neighbour Observe that doesn't exist.

- [ ] **Step 10.3: Rewrite the expanded Act arm.**

In `builder.rs`, replace the `TraceKind::Act { tool, family, input }` arm (lines 149-177) with:

```rust
                TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } => {
                    let (glyph, glyph_color) = family_glyph(*family);
                    lines.push(Line::from(vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("{} {}", glyph, tool),
                            Style::default()
                                .fg(glyph_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                    if matches!(input, spur_acp::adapter::ToolInputDisplay::Empty) {
                        for text_line in entry.text.lines() {
                            lines.push(Line::from(vec![
                                Span::raw("   "),
                                Span::styled(
                                    text_line.to_string(),
                                    Style::default().fg(glyph_color),
                                ),
                            ]));
                        }
                    } else {
                        lines.extend(input_display_lines(input));
                    }
                    // Render outcome body inline from `status` — no paired
                    // Observe entry exists in the new model.
                    match status {
                        ActStatus::Completed(Some(p)) => {
                            let (og, oc) = outcome_glyph(p);
                            let verb = observe_verb(p);
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    format!("{} {}", og, verb),
                                    Style::default()
                                        .fg(oc)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                            lines.extend(observe_payload_lines(p, collapsed));
                        }
                        ActStatus::Failed(Some(p)) => {
                            let verb = observe_verb(p);
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    format!("✗ {}", verb),
                                    Style::default()
                                        .fg(Color::Red)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                            lines.extend(observe_payload_lines(p, collapsed));
                        }
                        ActStatus::Completed(None) => {
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "✓ done".to_string(),
                                    Style::default()
                                        .fg(Color::Green)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        }
                        ActStatus::Failed(None) => {
                            lines.push(Line::from(vec![
                                ts_span.clone(),
                                Span::styled(
                                    "✗ failed".to_string(),
                                    Style::default()
                                        .fg(Color::Red)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                        }
                        ActStatus::Pending | ActStatus::InProgress { .. } => {}
                    }
                }
```

- [ ] **Step 10.4: Run the test.**

Run: `cargo test -p spur-tui --features markdown --lib build_display_lines_expanded_completed_renders_outcome_body`
Expected: PASS.

- [ ] **Step 10.5: Also update the `skip_blank` logic at `builder.rs:323-330`.**

Replace:
```rust
let skip_blank = matches!(&entry.kind, TraceKind::Act { .. })
    && matches!(
        self.entries.get(i + 1).map(|e| &e.kind),
        Some(TraceKind::Observe { payload: Some(_) })
    );
if !skip_blank {
    lines.push(Line::from(""));
}
```
With:
```rust
lines.push(Line::from(""));
```

- [ ] **Step 10.6: Run all markdown-build lib tests.**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: PASS. Fix any test that still assumes an adjacent Observe entry by reshaping the fixture to use `ActStatus::Completed(Some(...))`.

- [ ] **Step 10.7: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "refactor(spur-tui): expanded build_display_lines renders outcome from ActStatus"
```

---

## Task 11: Rewrite collapsed `build_virtual_rows` Act branch

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs:477-552`

- [ ] **Step 11.1: Write a failing virtual-row test.**

Append to `streaming_tests.rs`:

```rust
    #[cfg(feature = "markdown")]
    #[test]
    fn virtual_rows_collapsed_completed_act_shows_outcome_no_spinner() {
        use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
        let mut trace = super::ReactTrace::new();
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "echo".into(),
                    cwd: None,
                },
                tool_call_id: None,
                status: super::ActStatus::Completed(Some(ObservePayload::CommandOutput {
                    exit_code: Some(0),
                    stdout: "hi".into(),
                    stderr: String::new(),
                })),
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        let (rows, _, _) =
            trace.build_virtual_rows(0, 80, &std::collections::HashMap::new(), None);
        let txt: String = rows
            .iter()
            .filter_map(|r| match r {
                super::VirtualRow::Text(l) => Some(
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(txt.contains("✓"), "virtual rows must contain outcome: {txt}");
    }
```

- [ ] **Step 11.2: Run — verify it fails.**

Run: `cargo test -p spur-tui --features markdown --lib virtual_rows_collapsed_completed_act_shows_outcome_no_spinner`
Expected: FAIL.

- [ ] **Step 11.3: Rewrite the collapsed Act branch of `build_virtual_rows`.**

Replace lines 477-552 (the `if collapsed { if let TraceKind::Act { ... } }` block plus the prior `if collapsed && i > 0 { skip an Observe... }` guard) with:

```rust
        // The previous "skip an Observe consumed by the preceding Act"
        // compensator is no longer needed — Acts own their terminal state.
        let _ = from; // no-op: kept so the binding is still used elsewhere.

        while i < self.entries.len() {
            entry_row_starts[i - from] = rows.len();
            let entry = &self.entries[i];
            let ts_span = Span::styled(
                format!("{} ", entry.timestamp),
                Style::default().fg(Color::DarkGray),
            );

            // Collapsed mode: render Act as a one-line summary.
            if collapsed {
                if let TraceKind::Act {
                    tool,
                    family,
                    input,
                    status,
                    ..
                } = &entry.kind
                {
                    let (act_glyph, act_color) = family_glyph(*family);
                    let id_str = input_summary(input, tool);
                    let mut spans = vec![
                        ts_span.clone(),
                        Span::styled(
                            format!("{} {}", act_glyph, id_str),
                            Style::default().fg(act_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                    ];
                    match status {
                        ActStatus::Pending | ActStatus::InProgress { .. } => {
                            spans.push(Span::styled(
                                spinner_frame.to_string(),
                                Style::default().fg(Color::Yellow),
                            ));
                        }
                        ActStatus::Completed(Some(p)) => {
                            let (obs_glyph, obs_color, stats) = observe_compact(p);
                            spans.push(Span::styled(
                                obs_glyph.to_string(),
                                Style::default()
                                    .fg(obs_color)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            if !stats.is_empty() {
                                spans.push(Span::raw(" "));
                                spans.push(Span::styled(
                                    stats,
                                    Style::default().fg(Color::DarkGray),
                                ));
                            }
                        }
                        ActStatus::Completed(None) => {
                            spans.push(Span::styled(
                                "✓".to_string(),
                                Style::default()
                                    .fg(Color::Green)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        ActStatus::Failed(_) => {
                            spans.push(Span::styled(
                                "✗".to_string(),
                                Style::default()
                                    .fg(Color::Red)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    }
                    push_wrapped(
                        &mut rows,
                        &mut byte_ranges,
                        Some(0..entry.text.len()),
                        Line::from(spans),
                    );
                    push_wrapped(&mut rows, &mut byte_ranges, None, Line::from(""));
                    i += 1;
                    continue;
                }
            }
```

Note: remove the `if consumed == 2 && i + 1 >= from { entry_row_starts[i + 1 - from] = rows.len(); }` bookkeeping — it only existed to compensate for the now-gone two-entry pair.

- [ ] **Step 11.4: Rewrite the non-collapsed Act arm in `build_virtual_rows` (lines 666-706).**

Apply the same inline-body pattern as Task 10 Step 10.3, but using `push_wrapped(...)` instead of pushing to `lines`. Concretely, after the existing input-or-fallback rendering, append:

```rust
                    match status {
                        ActStatus::Completed(Some(p)) => {
                            let (og, oc) = outcome_glyph(p);
                            let verb = observe_verb(p);
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        format!("{} {}", og, verb),
                                        Style::default()
                                            .fg(oc)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                            for l in observe_payload_lines(p, collapsed) {
                                push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), l);
                            }
                        }
                        ActStatus::Failed(Some(p)) => {
                            let verb = observe_verb(p);
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        format!("✗ {}", verb),
                                        Style::default()
                                            .fg(Color::Red)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                            for l in observe_payload_lines(p, collapsed) {
                                push_wrapped(&mut rows, &mut byte_ranges, content_range.clone(), l);
                            }
                        }
                        ActStatus::Completed(None) => {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        "✓ done".to_string(),
                                        Style::default()
                                            .fg(Color::Green)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                        }
                        ActStatus::Failed(None) => {
                            push_wrapped(
                                &mut rows,
                                &mut byte_ranges,
                                content_range.clone(),
                                Line::from(vec![
                                    ts_span.clone(),
                                    Span::styled(
                                        "✗ failed".to_string(),
                                        Style::default()
                                            .fg(Color::Red)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]),
                            );
                        }
                        ActStatus::Pending | ActStatus::InProgress { .. } => {}
                    }
```

- [ ] **Step 11.5: Run the test.**

Run: `cargo test -p spur-tui --features markdown --lib virtual_rows_collapsed_completed_act_shows_outcome_no_spinner`
Expected: PASS.

- [ ] **Step 11.6: Run the full lib test suite.**

Run: `cargo test -p spur-tui --features markdown --lib`
Expected: PASS.

- [ ] **Step 11.7: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "refactor(spur-tui): build_virtual_rows reads ActStatus in both modes"
```

---

## Task 12: Lifecycle tests — the bug the user reported + its inverse

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs`

- [ ] **Step 12.1: Add the user-symptom test.**

```rust
    #[cfg(feature = "markdown")]
    #[test]
    fn completed_status_with_no_raw_output_stops_spinner() {
        use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
        use spur_acp::ToolCallId;
        use std::sync::Arc;
        let mut trace = super::ReactTrace::new();
        let id = ToolCallId(Arc::from("call-1"));
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "true".into(),
                    cwd: None,
                },
                tool_call_id: Some(id.clone()),
                status: super::ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        // Simulate ToolCallUpdate { status: Completed, raw_output: None }.
        let new = super::merge_status(
            match &trace.entries()[0].kind {
                super::TraceKind::Act { status, .. } => status,
                _ => unreachable!(),
            },
            Some(spur_acp::ToolCallStatus::Completed),
            None,
            spur_acp::AgentKind::Generic,
        );
        if let super::TraceKind::Act { status, .. } = &mut trace.entries_mut_for_test()[0].kind {
            *status = new;
        }
        assert_eq!(trace.first_active_spinner(), None);
        assert!(matches!(
            trace.entries()[0].kind,
            super::TraceKind::Act {
                status: super::ActStatus::Completed(None),
                ..
            }
        ));
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn in_progress_with_partial_raw_output_keeps_spinner() {
        use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
        use spur_acp::ToolCallId;
        use std::sync::Arc;
        let mut trace = super::ReactTrace::new();
        let id = ToolCallId(Arc::from("call-2"));
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "long".into(),
                    cwd: None,
                },
                tool_call_id: Some(id.clone()),
                status: super::ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        let partial = serde_json::json!({"text": "partial output"});
        let new = super::merge_status(
            &super::ActStatus::Pending,
            Some(spur_acp::ToolCallStatus::InProgress),
            Some(&partial),
            spur_acp::AgentKind::Generic,
        );
        if let super::TraceKind::Act { status, .. } = &mut trace.entries_mut_for_test()[0].kind {
            *status = new;
        }
        assert_eq!(trace.first_active_spinner(), Some(0));
    }
```

The test uses `entries_mut_for_test` — add that helper to `mod.rs` below the existing `entries_for_test`:

```rust
    /// Test-only mutable accessor.
    #[doc(hidden)]
    #[cfg(test)]
    pub(crate) fn entries_mut_for_test(&mut self) -> &mut Vec<TraceEntry> {
        &mut self.entries
    }
```

- [ ] **Step 12.2: Run the tests.**

Run: `cargo test -p spur-tui --features markdown --lib completed_status_with_no_raw_output_stops_spinner in_progress_with_partial_raw_output_keeps_spinner`
Expected: PASS.

- [ ] **Step 12.3: Add the interleaved-note and multiple-updates tests.**

```rust
    #[cfg(feature = "markdown")]
    #[test]
    fn multiple_updates_mutate_in_place_keep_entries_len_stable() {
        use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
        use spur_acp::ToolCallId;
        use std::sync::Arc;
        let mut trace = super::ReactTrace::new();
        let id = ToolCallId(Arc::from("call-3"));
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Command {
                    cmd: "c".into(),
                    cwd: None,
                },
                tool_call_id: Some(id.clone()),
                status: super::ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        assert_eq!(trace.entry_count(), 1);
        // In-progress update.
        if let Some((idx, entry)) = trace.find_act_by_id_mut(&id) {
            if let super::TraceKind::Act { status, .. } = &mut entry.kind {
                *status = super::merge_status(
                    status,
                    Some(spur_acp::ToolCallStatus::InProgress),
                    None,
                    spur_acp::AgentKind::Generic,
                );
            }
            let _ = idx;
        }
        assert_eq!(trace.entry_count(), 1);
        // Completion.
        let out = serde_json::json!({"text": "done"});
        if let Some((_, entry)) = trace.find_act_by_id_mut(&id) {
            if let super::TraceKind::Act { status, .. } = &mut entry.kind {
                *status = super::merge_status(
                    status,
                    Some(spur_acp::ToolCallStatus::Completed),
                    Some(&out),
                    spur_acp::AgentKind::Generic,
                );
            }
        }
        assert_eq!(trace.entry_count(), 1);
        assert!(matches!(
            trace.entries()[0].kind,
            super::TraceKind::Act {
                status: super::ActStatus::Completed(Some(ObservePayload::Text { .. })),
                ..
            }
        ));
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn interleaved_observe_note_does_not_break_lookup() {
        use spur_acp::adapter::{ToolFamily, ToolInputDisplay};
        use spur_acp::ToolCallId;
        use std::sync::Arc;
        let mut trace = super::ReactTrace::new();
        let id = ToolCallId(Arc::from("call-4"));
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Empty,
                tool_call_id: Some(id.clone()),
                status: super::ActStatus::Pending,
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        // Informational note pushed as Observe{None}.
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Observe { payload: None },
            text: "system note".into(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        // Terminal update still finds the original Act.
        let found = trace.find_act_by_id_mut(&id);
        assert!(found.is_some(), "note interleaving must not break id lookup");
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn failed_status_renders_failure_glyph_even_with_non_error_payload() {
        use spur_acp::adapter::{ObservePayload, ToolFamily, ToolInputDisplay};
        let mut trace = super::ReactTrace::new();
        trace.push(super::TraceEntry {
            kind: super::TraceKind::Act {
                tool: "shell".into(),
                family: ToolFamily::Execute,
                input: ToolInputDisplay::Empty,
                tool_call_id: None,
                // Failed with a Text payload (NOT Error variant) — must still
                // render as failure.
                status: super::ActStatus::Failed(Some(ObservePayload::Text {
                    body: "meh".into(),
                })),
            },
            text: String::new(),
            timestamp: "10:00".into(),
            markdown: None,
        });
        let joined = trace.render_to_strings().join("\n");
        assert!(joined.contains("✗"), "Failed must use ✗ regardless of payload shape: {joined}");
    }
```

- [ ] **Step 12.4: Run the new tests.**

Run: `cargo test -p spur-tui --features markdown --lib multiple_updates_mutate_in_place interleaved_observe_note failed_status_renders_failure_glyph`
Expected: PASS.

- [ ] **Step 12.5: Commit.**

```bash
git add crates/spur-tui/src/components/react_trace/streaming_tests.rs crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "test(spur-tui): lifecycle tests for ActStatus state machine"
```

---

## Task 13: Regenerate render-golden snapshot

**Files:**
- Inspect: `crates/spur-tui/tests/render_golden.rs`
- Possibly modify: `crates/spur-tui/tests/render_golden.rs` and any associated golden fixture file.

- [ ] **Step 13.1: Inspect the existing golden test.**

Run: `cargo test -p spur-tui --test render_golden -- --nocapture 2>&1 | tail -60`
Expected: Either PASS (if the test doesn't exercise the completed-Act path) or FAIL with a snapshot diff. Read the diff carefully.

- [ ] **Step 13.2: If the test fails, visually inspect the diff.**

The refactor preserves the visual shape for terminal Act + Observe pairs: one header line, one outcome line, one blank. If the diff shows EXACTLY the same text bytes with a single blank removed or added (from the `skip_blank` drop), update the snapshot. If the diff shows DIFFERENT text content, stop and investigate — the renderer rewrite has a bug.

- [ ] **Step 13.3: Regenerate the snapshot if and only if the diff is blank-line-only.**

If the golden is stored as an inline string literal in `render_golden.rs`, update the literal to match the new output. If it uses `insta` or similar, run `cargo insta review` and accept only the blank-line-only changes.

- [ ] **Step 13.4: Re-run.**

Run: `cargo test -p spur-tui --test render_golden`
Expected: PASS.

- [ ] **Step 13.5: Commit.**

```bash
git add crates/spur-tui/tests/render_golden.rs
git commit -m "test(spur-tui): update render_golden snapshot for ActStatus renderer"
```

---

## Task 14: Sweep uncommitted benches/examples

**Files:**
- Inspect: `crates/spur-tui/benches/` (all files).
- Inspect: `crates/spur-tui/examples/react_trace_bench_sim.rs`.

- [ ] **Step 14.1: Locate all `TraceKind::Act` constructions in bench/example files.**

Run: (use the Grep tool for this in the harness)
Pattern: `TraceKind::Act`
Path: `crates/spur-tui/benches crates/spur-tui/examples`

- [ ] **Step 14.2: For each hit, add the new fields with defaults.**

Exactly the migration pattern from Task 2 Step 2.3:
```rust
TraceKind::Act {
    tool,
    family,
    input,
    tool_call_id: None,
    status: ActStatus::Pending,
}
```
Plus the required `use` for `ActStatus`.

- [ ] **Step 14.3: Build benches.**

Run: `cargo bench -p spur-tui --features markdown --no-run`
Expected: PASS.

- [ ] **Step 14.4: Build examples.**

Run: `cargo build -p spur-tui --examples --features markdown`
Expected: PASS.

- [ ] **Step 14.5: Commit (only if anything changed).**

```bash
git add crates/spur-tui/benches crates/spur-tui/examples
git commit -m "chore(spur-tui): migrate bench/example Act literals to new schema"
```

---

## Task 15: End-to-end verification

- [ ] **Step 15.1: Full test suite, markdown.**

Run: `cargo test -p spur-tui --features markdown`
Expected: PASS.

- [ ] **Step 15.2: Full test suite, no-default-features.**

Run: `cargo test -p spur-tui --no-default-features`
Expected: PASS.

- [ ] **Step 15.3: Workspace build.**

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 15.4: Clippy.**

Run: `cargo clippy -p spur-tui --features markdown -- -D warnings`
Expected: PASS. Fix any warnings inline (prefer `&ActStatus` borrows over `status.clone()` in match arms, etc.).

- [ ] **Step 15.5: Manual smoke test (recommended).**

Launch the TUI against a real brain that makes multiple tool calls. Observe: spinners animate during execution, stop cleanly on completion, no leftover "forever-spinning" rows. Verify: sub-agent indentation still works (this confirms Task 6's `tool_depth.insert` preservation).

- [ ] **Step 15.6: Final commit if anything lingering.**

```bash
git status
# If clean, nothing to do.
```

---

## Self-Review Checklist

**Spec coverage:**

- [x] `pub use agent_client_protocol::ToolCallId` in `spur-acp` — Task 1.
- [x] `ActStatus` enum with all four variants — Task 2.
- [x] `tool_call_id: Option<ToolCallId>` on `TraceKind::Act` — Task 2.
- [x] `map_initial_status` honours incoming `tc.status` — Task 5.
- [x] `merge_status` with terminal-wins and non-exhaustive fallback — Task 4.
- [x] `find_act_by_id_mut` with `.0.as_ref()` comparison — Task 3.
- [x] Preserve `self.tool_depth.insert` side-effect — Task 6 Step 6.1.
- [x] Out-of-order synthesis when `title` or `kind` present — Task 6 Step 6.2.
- [x] `first_active_spinner` reads `status.is_active()` — Task 7.
- [x] All three renderer paths rewritten (collapsed plain, expanded plain, collapsed virtual, expanded virtual) — Tasks 8, 9, 10, 11.
- [x] `Failed(_)` uses fixed failure glyph regardless of payload — Task 8 Step 8.4, Task 9 Step 9.3, Task 10 Step 10.3, Task 11 Step 11.3; test in Task 12 Step 12.3.
- [x] `InProgress.partial` stored but NOT rendered in Phase 1 — Task 4/8/9/10/11 render paths only check variant, not `partial` field.
- [x] Blank-line invariant: one blank after every Act in both modes — Task 8 Step 8.5, Task 10 Step 10.5, Task 11 Step 11.3.
- [x] 14 Act match sites migrated — Task 2.
- [x] render_golden snapshot regenerated — Task 13.
- [x] Bench/example files swept — Task 14.
- [x] 9 lifecycle tests — Tasks 3, 5, 7, 9, 10, 11, 12.

**Placeholder scan:** none found.

**Type consistency:** `find_act_by_id_mut` takes `&ToolCallId`; callers pass `&tcu.tool_call_id` (consistent). `merge_status` takes `&ActStatus`; callers destructure through `&mut act_entry.kind` and pass the shared ref (consistent). `ActStatus::is_active` defined in Task 2 and used in Task 7; same name throughout.

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-18-react-trace-act-status-state-machine.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints.

**Which approach?**
