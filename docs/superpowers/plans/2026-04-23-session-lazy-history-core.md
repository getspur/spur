# Session Lazy History Core Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-04-23-session-lazy-loading-design.md`
**Design epic:** not recorded in beads; spec was approved directly in the design thread

**Goal:** Land a truthful lazy older-history path for disk-backed session replay without changing the current eager live `load_session` replay behavior.

**Architecture:** The first slice adds an explicit lazy-history contract, retains only disk-backed older replay chunks inside the orchestrator, and teaches `SessionDetailView` plus `ReactTrace` how to prepend older entries without breaking scroll anchors. Live replay remains eager in this slice and gets non-regression coverage instead of speculative buffering.

**Tech Stack:** Rust 2021, `tokio`, `serde`, `ratatui`, `crossterm`, `cargo test`

---

## Scope check

The approved spec spans three independent subsystems:

1. lazy history contract + detail view behavior
2. live `load_session` replay buffering/pagination
3. picker state cleanup for future pagination

This plan intentionally covers only subsystem 1 plus explicit live-replay non-regression tests. Subsystems 2 and 3 should remain separate follow-up plans.

## File map

- `crates/spur-acp/src/domain/events.rs`: add the new history-chunk event contract
- `crates/spur-acp/tests/executor_events_roundtrip.rs`: round-trip serde coverage for the new event
- `crates/spur-tui/src/action.rs`: add `Action::LoadOlderHistory`
- `crates/spur-tui/src/app.rs`: queue the new user input and consume chunk events
- `crates/spur-cli/src/main.rs`: bridge TUI input to interactive orchestrator input
- `crates/spur-core/src/orchestrator.rs`: retain disk history chunks and emit `SessionHistoryChunk`
- `crates/spur-tui/src/views/session_detail.rs`: own partial-history state and request older chunks on `PageUp`
- `crates/spur-tui/src/components/react_trace/mod.rs`: prepend older entries while preserving anchors
- `crates/spur-tui/src/components/react_trace/streaming_tests.rs`: anchor and cache regressions for prepend
- `crates/spur-tui/tests/session_update_handling.rs`: prove eager live replay still works

## DAG summary

- `lazy-history-event-contract`: root
- `load-older-history-request-chain`: root
- `react-trace-prepend`: root
- `disk-history-chunk-provider`: depends on `lazy-history-event-contract`, `load-older-history-request-chain`
- `detail-view-history-consumer`: depends on `lazy-history-event-contract`, `load-older-history-request-chain`, `react-trace-prepend`, `disk-history-chunk-provider`
- `live-replay-nonregression`: depends on `detail-view-history-consumer`

---

### Task 1: Add the ACP history-chunk contract

**Task ID:** `lazy-history-event-contract`

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/tests/executor_events_roundtrip.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `SessionHistoryChunkKind` exists with `InitialWindow` and `OlderPrepend`
- [ ] `SpurEventBody::SessionHistoryChunk` exists with `session`, `kind`, `entries`, and `older_remaining`
- [ ] ACP round-trip serde coverage proves the new variant survives encode/decode

**Suggested Worker:** `codex`

**Scope Boundary:**
- IN scope: the ACP event type and its round-trip test
- OUT of scope: TUI actions, orchestrator input routing, session-detail rendering
- If additional crates are needed to compile, stop and emit `scope_drift`

**Implementation:**
- [ ] **Step 1: Add the failing round-trip test**

```rust
#[test]
fn session_history_chunk_roundtrips() {
    use spur_acp::{HistoryEntry, SessionHistoryChunkKind, SessionId, SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::SessionHistoryChunk {
        session: SessionId("brain-1".into()),
        kind: SessionHistoryChunkKind::InitialWindow,
        entries: vec![HistoryEntry {
            role: "user".into(),
            text: "hello".into(),
        }],
        older_remaining: 12,
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();

    assert!(matches!(
        round.body,
        SpurEventBody::SessionHistoryChunk { .. }
    ));
    assert!(json.contains("SessionHistoryChunk"));
    assert!(json.contains("InitialWindow"));
}
```

- [ ] **Step 2: Verify the test fails before implementation**

```bash
cargo test -p spur-acp session_history_chunk_roundtrips -- --nocapture
```

- [ ] **Step 3: Add the new contract types**

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionHistoryChunkKind {
    InitialWindow,
    OlderPrepend,
}
```

```rust
    SessionHistoryChunk {
        session: SessionId,
        kind: SessionHistoryChunkKind,
        entries: Vec<HistoryEntry>,
        older_remaining: usize,
    },
```

- [ ] **Step 4: Re-run the focused ACP test**

```bash
cargo test -p spur-acp session_history_chunk_roundtrips -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs \
        crates/spur-acp/tests/executor_events_roundtrip.rs
git commit -m "feat(spur-acp): S1.a add session history chunk event"
```

---

### Task 2: Add the `LoadOlderHistory` request chain

**Task ID:** `load-older-history-request-chain`

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `Action::LoadOlderHistory { session }` exists
- [ ] `App::process_action(...)` queues `UserInput::LoadOlderHistory { session }`
- [ ] `tui_input_to_interactive(...)` maps to `InteractiveInput::LoadOlderHistory { session }`
- [ ] The orchestrator input enum accepts the new command even before chunk loading is wired

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: input plumbing only
- OUT of scope: history splitting, event emission, `SessionDetailView` rendering, `ReactTrace`
- If you need to modify disk replay logic in `ResumeSession`, stop and emit `scope_drift`

**Implementation:**
- [ ] **Step 1: Add the failing TUI action-queue test in `crates/spur-tui/src/app.rs`**

```rust
#[test]
fn load_older_history_action_queues_user_input() {
    let (mut app, mut rx) = app_with_user_input_tx();
    let sid = SessionId("brain-1".into());

    app.process_action(Action::LoadOlderHistory {
        session: sid.clone(),
    });

    match rx.try_recv() {
        Ok(UserInput::LoadOlderHistory { session }) => assert_eq!(session, sid),
        Ok(other) => panic!("expected LoadOlderHistory, got {other:?}"),
        Err(err) => panic!("expected queued user input, got {err:?}"),
    }
}
```

- [ ] **Step 2: Add the failing CLI bridge test in `crates/spur-cli/src/main.rs`**

```rust
#[cfg(test)]
mod tui_input_to_interactive_tests {
    use super::tui_input_to_interactive;

    #[test]
    fn load_older_history_maps_one_to_one() {
        let sid = spur_acp::SessionId("brain-1".into());
        let mapped =
            tui_input_to_interactive(spur_tui::UserInput::LoadOlderHistory {
                session: sid.clone(),
            });

        assert!(matches!(
            mapped,
            spur_core::InteractiveInput::LoadOlderHistory { session } if session == sid
        ));
    }
}
```

- [ ] **Step 3: Verify both tests fail before implementation**

```bash
cargo test -p spur-tui load_older_history_action_queues_user_input -- --nocapture
cargo test -p spur-cli load_older_history_maps_one_to_one -- --nocapture
```

- [ ] **Step 4: Add the new input variants**

```rust
// crates/spur-tui/src/action.rs
LoadOlderHistory { session: SessionId },
```

```rust
// crates/spur-tui/src/app.rs
LoadOlderHistory { session: SessionId },
```

```rust
// crates/spur-core/src/orchestrator.rs
LoadOlderHistory { session: SessionId },
```

- [ ] **Step 5: Wire the bridges**

```rust
// crates/spur-tui/src/app.rs
Action::LoadOlderHistory { session } => {
    if let Some(ref tx) = self.user_input_tx {
        let _ = tx.try_send(UserInput::LoadOlderHistory { session });
    }
}
```

```rust
// crates/spur-cli/src/main.rs
spur_tui::UserInput::LoadOlderHistory { session } => {
    spur_core::InteractiveInput::LoadOlderHistory { session }
}
```

```rust
// crates/spur-core/src/orchestrator.rs
InteractiveInput::LoadOlderHistory { .. } => {
    continue;
}
```

- [ ] **Step 6: Re-run the focused routing tests**

```bash
cargo test -p spur-tui load_older_history_action_queues_user_input -- --nocapture
cargo test -p spur-cli load_older_history_maps_one_to_one -- --nocapture
```

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/action.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-cli/src/main.rs \
        crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-tui): S1.b add load older history request chain"
```

---

### Task 3: Add prepend-aware `ReactTrace`

**Task ID:** `react-trace-prepend`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs`
- Modify: `crates/spur-tui/src/components/react_trace/streaming_tests.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ReactTrace::prepend_entries(...)` exists
- [ ] Row anchors shift forward by the prepended entry count
- [ ] `Following` remains `Following`
- [ ] Cache invalidation still happens after a prepend

**Suggested Worker:** `codex`

**Scope Boundary:**
- IN scope: prepend mechanics and tests for `ReactTrace`
- OUT of scope: session-detail history state, orchestrator disk replay, app metadata behavior
- If you need to change `SessionDetailView`, stop and emit `scope_drift`

**Implementation:**
- [ ] **Step 1: Add the failing prepend tests in `crates/spur-tui/src/components/react_trace/streaming_tests.rs`**

```rust
#[test]
fn prepend_entries_shifts_row_anchor_forward_by_prepended_entry_count() {
    use crate::components::react_trace::types::ScrollAnchor;

    let mut trace = ReactTrace::new_for_tests();
    trace.push(TraceEntry {
        kind: TraceKind::UserMessage,
        text: "first".into(),
        timestamp: "10:00:00".into(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
    trace.push(TraceEntry {
        kind: TraceKind::AgentMessage {
            agent: "claude".into(),
        },
        text: "second".into(),
        timestamp: "10:00:01".into(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
    trace.scroll_to_top();

    trace.prepend_entries(vec![
        TraceEntry {
            kind: TraceKind::Think,
            text: "older-a".into(),
            timestamp: String::new(),
            #[cfg(feature = "markdown")]
            markdown: None,
        },
        TraceEntry {
            kind: TraceKind::Think,
            text: "older-b".into(),
            timestamp: String::new(),
            #[cfg(feature = "markdown")]
            markdown: None,
        },
    ]);

    assert!(matches!(
        trace.anchor_for_tests(),
        ScrollAnchor::Row {
            entry_idx: 2,
            row_within_entry: 0
        }
    ));
}

#[test]
fn prepend_entries_preserves_following_anchor() {
    let mut trace = ReactTrace::new_for_tests();
    trace.push(TraceEntry {
        kind: TraceKind::UserMessage,
        text: "tail".into(),
        timestamp: "10:00:00".into(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
    trace.scroll_to_bottom();

    trace.prepend_entries(vec![TraceEntry {
        kind: TraceKind::Think,
        text: "older".into(),
        timestamp: String::new(),
        #[cfg(feature = "markdown")]
        markdown: None,
    }]);

    assert!(matches!(
        trace.anchor_for_tests(),
        crate::components::react_trace::types::ScrollAnchor::Following
    ));
}
```

- [ ] **Step 2: Add one cache invalidation assertion**

```rust
#[test]
fn prepend_entries_invalidates_layout_cache() {
    let mut trace = ReactTrace::new_for_tests();
    trace.push(TraceEntry {
        kind: TraceKind::UserMessage,
        text: "tail".into(),
        timestamp: "10:00:00".into(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
    trace.seed_line_cache_for_tests(80, &std::collections::HashMap::new());

    trace.prepend_entries(vec![TraceEntry {
        kind: TraceKind::Think,
        text: "older".into(),
        timestamp: String::new(),
        #[cfg(feature = "markdown")]
        markdown: None,
    }]);

    assert!(trace.dirty_from_for_tests().is_none());
    assert!(trace.render_lines_for_test(80).iter().any(|line| line.contains("older")));
}
```

- [ ] **Step 3: Verify the prepend tests fail before implementation**

```bash
cargo test -p spur-tui prepend_entries_ -- --nocapture
```

- [ ] **Step 4: Implement `prepend_entries(...)`**

```rust
pub fn prepend_entries(&mut self, mut incoming: Vec<TraceEntry>) {
    if incoming.is_empty() {
        return;
    }

    let added = incoming.len();
    incoming.append(&mut self.entries);
    self.entries = incoming;

    match self.anchor {
        crate::components::react_trace::types::ScrollAnchor::Row {
            entry_idx,
            row_within_entry,
        } => {
            self.anchor = crate::components::react_trace::types::ScrollAnchor::Row {
                entry_idx: entry_idx + added,
                row_within_entry,
            };
        }
        crate::components::react_trace::types::ScrollAnchor::Following => {}
    }

    self.invalidate_cache();
}
```

- [ ] **Step 5: Re-run the prepend tests**

```bash
cargo test -p spur-tui prepend_entries_ -- --nocapture
```

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs \
        crates/spur-tui/src/components/react_trace/streaming_tests.rs
git commit -m "feat(spur-tui): S1.c add prepend-aware react trace"
```

---

### Task 4: Retain disk replay chunks in the orchestrator

**Task ID:** `disk-history-chunk-provider`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

**Depends on:** `lazy-history-event-contract`, `load-older-history-request-chain`

**Acceptance Criteria:**
- [ ] Disk fallback emits `SessionHistoryChunkKind::InitialWindow` instead of the one-shot `SessionHistory`
- [ ] Older disk replay stays retained in memory as chunked history for the focused resumed session
- [ ] `InteractiveInput::LoadOlderHistory` emits `OlderPrepend` chunks until exhausted
- [ ] Retained disk history is cleared on session swap/new resume boundaries

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: `InteractiveInput::ResumeSession`, disk replay fallback, helper state local to orchestrator
- OUT of scope: picker behavior, `ReactTrace`, detail-view rendering
- If live `load_session` history streaming needs redesign in this task, stop and emit `scope_drift`

**Scope Drift Checkpoint:**
- If the retained-state design spills outside `orchestrator.rs`, emit `scope_drift`
- If the chunk policy needs persisted metadata, emit `risk`

**Implementation:**
- [ ] **Step 1: Add focused helper tests in `crates/spur-core/src/orchestrator.rs`**

```rust
#[cfg(test)]
mod lazy_history_tests {
    use super::*;

    fn entry(n: usize) -> spur_acp::HistoryEntry {
        spur_acp::HistoryEntry {
            role: "user".into(),
            text: format!("m{n}"),
        }
    }

    #[test]
    fn split_history_tail_window_keeps_recent_tail_as_initial_window() {
        let entries: Vec<_> = (0..5).map(entry).collect();
        let (initial, mut older) = split_history_tail_window(entries, 2);

        assert_eq!(
            initial.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["m3", "m4"]
        );
        let nearest_older = older.pop_back().expect("nearest older chunk");
        assert_eq!(
            nearest_older.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m2"]
        );
    }

    #[test]
    fn split_history_tail_window_preserves_oldest_remainder() {
        let entries: Vec<_> = (0..5).map(entry).collect();
        let (_initial, older) = split_history_tail_window(entries, 2);

        assert_eq!(
            older.front().unwrap().iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["m0"]
        );
    }
}
```

- [ ] **Step 2: Verify the helper tests fail before implementation**

```bash
cargo test -p spur-core split_history_tail_window -- --nocapture
```

- [ ] **Step 3: Add the retained-history helper types**

```rust
const DISK_HISTORY_CHUNK_SIZE: usize = 200;

#[derive(Debug, Clone)]
struct RetainedDiskHistory {
    session: SessionId,
    older_chunks: std::collections::VecDeque<Vec<spur_acp::HistoryEntry>>,
}
```

```rust
fn split_history_tail_window(
    entries: Vec<spur_acp::HistoryEntry>,
    chunk_size: usize,
) -> (
    Vec<spur_acp::HistoryEntry>,
    std::collections::VecDeque<Vec<spur_acp::HistoryEntry>>,
) {
    if entries.len() <= chunk_size {
        return (entries, std::collections::VecDeque::new());
    }

    let split_at = entries.len() - chunk_size;
    let mut older = std::collections::VecDeque::new();
    for chunk in entries[..split_at].chunks(chunk_size) {
        older.push_back(chunk.to_vec());
    }

    (entries[split_at..].to_vec(), older)
}
```

- [ ] **Step 4: Replace the one-shot disk fallback with chunk emission**

```rust
let entries = Self::read_session_history_from_disk(&original_session_id);
if !entries.is_empty() {
    let (initial, older_chunks) =
        split_history_tail_window(entries, DISK_HISTORY_CHUNK_SIZE);
    let older_remaining = older_chunks.iter().map(|chunk| chunk.len()).sum();

    self.emit(SpurEvent::now(SpurEventBody::SessionHistoryChunk {
        session: spur_id.clone(),
        kind: spur_acp::SessionHistoryChunkKind::InitialWindow,
        entries: initial,
        older_remaining,
    }));

    retained_disk_history = Some(RetainedDiskHistory {
        session: spur_id.clone(),
        older_chunks,
    });
} else {
    retained_disk_history = None;
}
```

- [ ] **Step 5: Add the `LoadOlderHistory` interactive arm**

```rust
InteractiveInput::LoadOlderHistory { session } => {
    let Some(state) = retained_disk_history.as_mut() else {
        continue;
    };
    if state.session != session {
        continue;
    }
    let Some(entries) = state.older_chunks.pop_back() else {
        continue;
    };
    let older_remaining = state.older_chunks.iter().map(|chunk| chunk.len()).sum();
    self.emit(SpurEvent::now(SpurEventBody::SessionHistoryChunk {
        session: session.clone(),
        kind: spur_acp::SessionHistoryChunkKind::OlderPrepend,
        entries,
        older_remaining,
    }));
}
```

- [ ] **Step 6: Re-run the helper tests**

```bash
cargo test -p spur-core split_history_tail_window -- --nocapture
```

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): S1.d retain disk replay chunks"
```

---

### Task 5: Consume history chunks in the detail view and app

**Task ID:** `detail-view-history-consumer`

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/app.rs`

**Depends on:** `lazy-history-event-contract`, `load-older-history-request-chain`, `react-trace-prepend`, `disk-history-chunk-provider`

**Acceptance Criteria:**
- [ ] `SessionDetailView` tracks whether older history remains
- [ ] `PageUp` at trace top emits `Action::LoadOlderHistory { session }` when older history exists
- [ ] `SessionHistoryChunkKind::InitialWindow` replaces the current one-shot `replay_history(...)` path for disk-backed lazy history
- [ ] `OlderPrepend` prepends entries without resetting the trace anchor
- [ ] User-input history backfill still dedupes and persists through `App`

**Suggested Worker:** `claude-code`

**Scope Boundary:**
- IN scope: `SessionDetailView`, `App`, and test coverage in those same files
- OUT of scope: orchestrator chunk splitting, picker behavior, live replay buffering
- If the work needs new persisted metadata fields, stop and emit `risk`

**Scope Drift Checkpoint:**
- If scroll correctness cannot be preserved with `ReactTrace::prepend_entries`, emit `risk`
- If app metadata semantics need a new store shape, emit `scope_drift`

**Implementation:**
- [ ] **Step 1: Add the failing `SessionDetailView` request test**

```rust
#[test]
fn pageup_at_trace_top_with_partial_history_requests_load_older_history() {
    use crate::action::Action;
    use crate::views::View;

    let mut view = make_view();
    view.apply_history_initial_window(
        &[spur_acp::HistoryEntry {
            role: "user".into(),
            text: "hello".into(),
        }],
        3,
    );
    view.react_trace.scroll_to_top();

    let action = <SessionDetailView as View>::handle_key(
        &mut view,
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        ),
        &test_ctx(),
    );

    assert!(matches!(
        action,
        Some(Action::LoadOlderHistory { session }) if session == spur_acp::SessionId("s".into())
    ));
}
```

- [ ] **Step 2: Add the failing app chunk-consumption test**

```rust
#[test]
fn session_history_chunk_initial_window_updates_trace_and_input_history() {
    let mut app = App::new_for_tests();
    let sid = SessionId("brain-1".into());

    app.handle_spur_event(wrap(SpurEventBody::BrainSpawned {
        agent: "claude".into(),
        session: sid.clone(),
    }));

    app.handle_spur_event(wrap(SpurEventBody::SessionHistoryChunk {
        session: sid.clone(),
        kind: spur_acp::SessionHistoryChunkKind::InitialWindow,
        entries: vec![
            spur_acp::HistoryEntry {
                role: "user".into(),
                text: "older prompt".into(),
            },
            spur_acp::HistoryEntry {
                role: "assistant".into(),
                text: "older answer".into(),
            },
        ],
        older_remaining: 4,
    }));

    let detail = app.session_detail.as_ref().expect("detail exists");
    assert!(detail.trace_entry_count() >= 2);
    assert_eq!(
        app.metadata_store
            .metadata()
            .input_history
            .last()
            .expect("history backfilled")
            .snapshot
            .text,
        "older prompt"
    );
}
```

- [ ] **Step 3: Verify both tests fail before implementation**

```bash
cargo test -p spur-tui pageup_at_trace_top_with_partial_history_requests_load_older_history -- --nocapture
cargo test -p spur-tui session_history_chunk_initial_window_updates_trace_and_input_history -- --nocapture
```

- [ ] **Step 4: Add `HistoryLoadState` and chunk helpers to `SessionDetailView`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryLoadState {
    Complete,
    Partial { older_remaining: usize },
}
```

```rust
pub fn apply_history_initial_window(
    &mut self,
    entries: &[spur_acp::HistoryEntry],
    older_remaining: usize,
) {
    self.react_trace.clear();
    let converted = Self::history_entries_to_trace_entries(entries, &self.agent_name);
    self.react_trace.prepend_entries(converted);
    self.history_load_state = if older_remaining == 0 {
        HistoryLoadState::Complete
    } else {
        HistoryLoadState::Partial { older_remaining }
    };
}

pub fn prepend_history_chunk(
    &mut self,
    entries: &[spur_acp::HistoryEntry],
    older_remaining: usize,
) {
    let converted = Self::history_entries_to_trace_entries(entries, &self.agent_name);
    self.react_trace.prepend_entries(converted);
    self.history_load_state = if older_remaining == 0 {
        HistoryLoadState::Complete
    } else {
        HistoryLoadState::Partial { older_remaining }
    };
}
```

- [ ] **Step 5: Gate `PageUp` on partial history and top anchor**

```rust
KeyCode::PageUp => {
    let can_load_more =
        matches!(self.history_load_state, HistoryLoadState::Partial { .. })
            && self.react_trace.is_at_top();
    if can_load_more {
        return Some(Action::LoadOlderHistory {
            session: self.session_id.clone(),
        });
    }
    self.react_trace.page_up();
    return Some(Action::ScrollUp);
}
```

- [ ] **Step 6: Add the `SessionHistoryChunk` app handler**

```rust
SpurEventBody::SessionHistoryChunk {
    session,
    kind,
    entries,
    older_remaining,
} => {
    if let Some(ref mut detail) = self.session_detail {
        if detail.session_id() == session {
            match kind {
                spur_acp::SessionHistoryChunkKind::InitialWindow => {
                    detail.apply_history_initial_window(entries, *older_remaining);
                }
                spur_acp::SessionHistoryChunkKind::OlderPrepend => {
                    detail.prepend_history_chunk(entries, *older_remaining);
                }
            }
        }
    }

    let mut changed = false;
    {
        let hist = &mut self.metadata_store.metadata_mut().input_history;
        for entry in entries {
            if entry.role == "user" {
                let history_entry = InputHistoryEntry::from_text(entry.text.clone());
                changed |= Self::merge_input_history_entry(hist, history_entry);
            }
        }
    }
    if changed {
        if let Err(e) = self.metadata_store.save() {
            tracing::warn!(error = %e, "failed to persist lazy-history input backfill");
        }
        self.sync_input_history();
    }

    return;
}
```

- [ ] **Step 7: Re-run the focused TUI tests**

```bash
cargo test -p spur-tui pageup_at_trace_top_with_partial_history_requests_load_older_history -- --nocapture
cargo test -p spur-tui session_history_chunk_initial_window_updates_trace_and_input_history -- --nocapture
```

- [ ] **Step 8: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): S1.e wire lazy history chunks into detail view"
```

---

### Task 6: Lock live replay behavior with non-regression coverage

**Task ID:** `live-replay-nonregression`

**Files:**
- Modify: `crates/spur-tui/tests/session_update_handling.rs`

**Depends on:** `detail-view-history-consumer`

**Acceptance Criteria:**
- [ ] Live `AgentNotification` replay still reaches the detail trace without any `SessionHistoryChunk`
- [ ] Focused lazy-history tests still pass together
- [ ] `spur-acp`, `spur-core`, and `spur-tui` crate test suites pass before approval

**Suggested Worker:** `codex`

**Scope Boundary:**
- IN scope: integration coverage and verification commands
- OUT of scope: new product behavior, picker cleanup, live replay buffering redesign
- If a failing safety-net test points to broader architectural regression, emit `risk`

**Implementation:**
- [ ] **Step 1: Add the non-regression test**

```rust
#[test]
fn agent_notification_replay_path_stays_eager_and_does_not_require_history_chunks() {
    use spur_acp::{ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, SpurEvent, SpurEventBody, TextContent};

    let mut app = spur_tui::test_support::new_app();
    let sid = SessionId("resume-live".into());
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "claude".into(),
            session: sid.clone(),
        }),
    );

    let notif = SessionNotification::new(
        sid.0.clone(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
            TextContent::new("live replay text"),
        ))),
    );
    spur_tui::test_support::push_event(
        &mut app,
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: sid.clone(),
            notification: Box::new(notif),
        }),
    );

    let detail = spur_tui::test_support::session_detail(&app).expect("has detail");
    let snapshot = detail.trace_snapshot_for_test();
    assert!(
        snapshot.iter().any(|line| line.contains("live replay text")),
        "live replay path must still work without SessionHistoryChunk; snapshot={snapshot:?}"
    );
}
```

- [ ] **Step 2: Run the focused non-regression test**

```bash
cargo test -p spur-tui agent_notification_replay_path_stays_eager_and_does_not_require_history_chunks -- --nocapture
```

- [ ] **Step 3: Run the focused verification sweep**

```bash
cargo test -p spur-acp session_history_chunk_roundtrips -- --nocapture
cargo test -p spur-core split_history_tail_window -- --nocapture
cargo test -p spur-tui prepend_entries_ -- --nocapture
cargo test -p spur-tui pageup_at_trace_top_with_partial_history_requests_load_older_history -- --nocapture
cargo test -p spur-tui session_history_chunk_initial_window_updates_trace_and_input_history -- --nocapture
cargo test -p spur-tui agent_notification_replay_path_stays_eager_and_does_not_require_history_chunks -- --nocapture
```

- [ ] **Step 4: Run the crate-level safety net**

```bash
cargo test -p spur-acp -- --nocapture
cargo test -p spur-core -- --nocapture
cargo test -p spur-tui -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/tests/session_update_handling.rs
git commit -m "test(spur-tui): S1.f lock live replay non-regression"
```

---

## Follow-up plans

1. **Live replay buffering / pagination**
   Replace the still-eager `load_session` replay path with a retained or paged provider that can honor bounded initial windows without draining the full stream first.

2. **Picker state cleanup**
   Separate canonical session storage, filtered/sorted projection, and viewport window so future backend pagination can land cleanly.

## Self-review

### Spec coverage

- Explicit request/response chain: covered by Tasks 1, 2, 4
- Disk-backed initial window plus older prepend chunks: covered by Task 4
- `SessionDetailView` ownership and truthful sentinel behavior: covered by Task 5
- `ReactTrace` prepend surface and anchor preservation: covered by Task 3
- Input-history side effects under chunking: covered by Task 5
- Live replay path remains eager and tested: covered by Task 6
- Picker cleanup and live replay buffering are intentionally deferred to follow-up plans

### Placeholder scan

Search terms checked: `TODO`, `TBD`, `implement later`, `fill in details`, `appropriate error handling`, `similar to`, `write tests for the above`

Result: none present.

### DAG validation

- No cycles
- Three root tasks maximize safe parallelism
- `orchestrator.rs` and `app.rs` edits are serialized to avoid overlapping write scopes

### beads compatibility

- Every task has a unique task ID
- Every dependency is explicit
- Every task has acceptance criteria, file boundaries, and suggested worker routing

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-23-session-lazy-history-core.md`.

Two options:

1. **Submit to Orchestrator (recommended)** — call `submit_plan(persist_as_epic=true)` and let SPUR create the epic plus child issues.
2. **Review First** — keep this as the approved plan doc and submit only after one more review pass.
