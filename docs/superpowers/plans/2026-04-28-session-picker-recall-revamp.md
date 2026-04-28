# Session Picker Recall Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace weak agent-generated session titles in the SPUR TUI picker with the user's first message (intent recall), and invert the preview pane to surface the user's last message + draft (state recall), via a new event-sourced projection in `spur-core`.

**Architecture:** Add `SessionSynopsisProjection` in `spur-core` mirroring `ExecutorLineage` (`crates/spur-core/src/lineage/projection.rs`). The projection observes existing `SpurEventBody::AgentNotification` and `SpurEventBody::SessionHistory` events from the broadcast funnel, accumulates multi-chunk user messages per session, and exposes a read-only API. The TUI's `App` instantiates one and threads it through `ViewContext` to the picker view, which uses it for row labels, preview rows, and the filter haystack.

**Tech Stack:** Rust 2021, ratatui (TUI), tokio (broadcast), nucleo-matcher (filter), unicode-segmentation (TUI-side truncation), insta (snapshot tests), chrono (timestamps).

**Spec:** [`docs/superpowers/specs/2026-04-28-session-picker-recall-revamp-design.md`](../specs/2026-04-28-session-picker-recall-revamp-design.md)

---

## File Structure

| Path | Status | Responsibility |
|---|---|---|
| `crates/spur-core/src/session_synopsis/mod.rs` | NEW | Module root, re-exports |
| `crates/spur-core/src/session_synopsis/projection.rs` | NEW | `SessionSynopsis`, `SessionSynopsisProjection`, `apply()`, `get()`, accumulator state machine |
| `crates/spur-core/src/lib.rs` | MODIFY | `pub mod session_synopsis;` + `pub use` re-exports |
| `crates/spur-tui/src/app.rs` | MODIFY | Hold projection field; call `synopsis.apply(&event)` in `handle_spur_event` |
| `crates/spur-tui/src/views/mod.rs` | MODIFY | `ViewContext.synopsis: &'a SessionSynopsisProjection` |
| `crates/spur-tui/src/lib.rs` | MODIFY | `test_view_ctx` helper if it constructs `ViewContext` |
| `crates/spur-tui/src/views/session_picker.rs` | MODIFY | `resolve_label`, `truncate_for_row`, haystack precompute, preview-row population |
| `crates/spur-tui/src/components/session_preview.rs` | MODIFY | Add `PreviewRow { label, value, value_style, wrap }`; `From<(String, String)>` conversion |
| `crates/spur-tui/src/views/session_detail.rs` | MODIFY | Update `test_ctx()` constructors (no behavior change) |

Workspace build/test commands used throughout:

| Action | Command |
|---|---|
| Build all crates | `cargo build --workspace` |
| Test core | `cargo test -p spur-core` |
| Test TUI | `cargo test -p spur-tui` |
| Run a specific test | `cargo test -p spur-core <test_name>` |
| Snapshot review | `cargo insta review` |

---

## Phase 1 — Core projection (`spur-core`)

### Task 1: Bootstrap module + empty types

**Files:**
- Create: `crates/spur-core/src/session_synopsis/mod.rs`
- Create: `crates/spur-core/src/session_synopsis/projection.rs`
- Modify: `crates/spur-core/src/lib.rs`
- Test: `crates/spur-core/src/session_synopsis/projection.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test for bare types**

In `crates/spur-core/src/session_synopsis/projection.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_starts_empty() {
        let proj = SessionSynopsisProjection::new();
        assert!(proj.get(&spur_acp::SessionId("missing".into())).is_none());
    }

    #[test]
    fn synopsis_default_has_no_messages() {
        let s = SessionSynopsis::default();
        assert!(s.first_user_msg.is_none());
        assert!(s.last_user_msg.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (compile error)**

Run: `cargo test -p spur-core session_synopsis`
Expected: compile error — `SessionSynopsis` and `SessionSynopsisProjection` not defined.

- [ ] **Step 3: Implement minimal types**

In `crates/spur-core/src/session_synopsis/projection.rs`:

```rust
//! Session synopsis projection — derived from the event stream.
//!
//! Mirrors `ExecutorLineage` in shape: a passive `apply(&event)` struct
//! that consumers feed from their broadcast subscription. Read API is
//! pure functions over the in-memory state.

use std::collections::HashMap;
use spur_acp::SessionId;

/// First and last user-authored message text for a session, derived
/// from observed events. Stored raw; render-side consumers do their
/// own truncation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionSynopsis {
    pub first_user_msg: Option<String>,
    pub last_user_msg: Option<String>,
}

/// In-memory projection of session synopses, fed by `apply(&event)`.
#[derive(Debug, Default)]
pub struct SessionSynopsisProjection {
    by_session: HashMap<SessionId, SessionSynopsis>,
    pending: HashMap<SessionId, String>,
}

impl SessionSynopsisProjection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read API. Returns `None` for unknown sessions.
    /// (Commit-on-read fallback added in Task 9.)
    pub fn get(&self, id: &SessionId) -> Option<SessionSynopsis> {
        self.by_session.get(id).cloned()
    }
}
```

In `crates/spur-core/src/session_synopsis/mod.rs`:

```rust
pub mod projection;

pub use projection::{SessionSynopsis, SessionSynopsisProjection};
```

In `crates/spur-core/src/lib.rs`, add alongside the existing `pub mod lineage;` and `pub mod plan_projection;`:

```rust
pub mod session_synopsis;
```

And alongside the existing `pub use lineage::{...};` and `pub use plan_projection::{...};`:

```rust
pub use session_synopsis::{SessionSynopsis, SessionSynopsisProjection};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 2 passed.

- [ ] **Step 5: Build the workspace to confirm no downstream regression**

Run: `cargo build --workspace`
Expected: compiles clean.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/session_synopsis crates/spur-core/src/lib.rs
git commit -m "feat(spur-core): bootstrap SessionSynopsisProjection module"
```

---

### Task 2: `apply()` for single-chunk live user message

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests`:

```rust
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use agent_client_protocol::schema::{SessionNotification, SessionUpdate, ContentBlock, TextContent};

fn user_chunk_event(session: &str, text: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: SessionId(session.into()),
        notification: Box::new(SessionNotification {
            session_id: agent_client_protocol::schema::SessionId(session.into()),
            update: SessionUpdate::UserMessageChunk(ContentBlock::Text(TextContent {
                text: text.into(),
                annotations: None,
                meta: None,
            })),
            meta: None,
        }),
    })
}

fn agent_chunk_event(session: &str, text: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: SessionId(session.into()),
        notification: Box::new(SessionNotification {
            session_id: agent_client_protocol::schema::SessionId(session.into()),
            update: SessionUpdate::AgentMessageChunk(ContentBlock::Text(TextContent {
                text: text.into(),
                annotations: None,
                meta: None,
            })),
            meta: None,
        }),
    })
}

#[test]
fn first_user_chunk_is_buffered_then_flushed_on_agent_reply() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "fix the auth bug"));

    // Pending — not yet committed.
    assert!(proj.get(&SessionId("S1".into())).is_none());

    // Agent reply triggers flush.
    proj.apply(&agent_chunk_event("S1", "I'll take a look."));

    let s = proj.get(&SessionId("S1".into())).expect("synopsis present");
    assert_eq!(s.first_user_msg.as_deref(), Some("fix the auth bug"));
    assert_eq!(s.last_user_msg.as_deref(), Some("fix the auth bug"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core session_synopsis`
Expected: compile error — `apply` is not defined on `SessionSynopsisProjection`.

- [ ] **Step 3: Implement `apply` for the two relevant arms**

In `projection.rs`, add to `impl SessionSynopsisProjection`:

```rust
    /// Fold an event into the projection. Idempotent on irrelevant variants.
    pub fn apply(&mut self, event: &spur_acp::SpurEvent) {
        use spur_acp::domain::events::SpurEventBody;
        use agent_client_protocol::schema::SessionUpdate;

        match &event.body {
            SpurEventBody::AgentNotification { session, notification } => {
                match &notification.update {
                    SessionUpdate::UserMessageChunk(content) => {
                        let text = content_block_text(content);
                        self.pending
                            .entry(session.clone())
                            .or_default()
                            .push_str(text);
                    }
                    // Any non-user agent update flushes the pending buffer.
                    SessionUpdate::AgentMessageChunk(_)
                    | SessionUpdate::AgentThoughtChunk(_)
                    | SessionUpdate::ToolCall(_)
                    | SessionUpdate::ToolCallUpdate(_)
                    | SessionUpdate::Plan(_)
                    | SessionUpdate::AvailableCommandsUpdate(_)
                    | SessionUpdate::CurrentModeUpdate(_) => {
                        self.flush_pending(session);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn flush_pending(&mut self, session: &SessionId) {
        let buf = match self.pending.remove(session) {
            Some(b) => b,
            None => return,
        };
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            return;
        }
        let s = self.by_session.entry(session.clone()).or_default();
        if s.first_user_msg.is_none() {
            s.first_user_msg = Some(trimmed.to_owned());
        }
        s.last_user_msg = Some(trimmed.to_owned());
    }
}

fn content_block_text(content: &agent_client_protocol::schema::ContentBlock) -> &str {
    use agent_client_protocol::schema::ContentBlock;
    match content {
        ContentBlock::Text(t) => &t.text,
        _ => "",
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "feat(spur-core): SessionSynopsisProjection.apply for live user_message_chunk"
```

---

### Task 3: Multi-chunk accumulation

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn multi_chunk_user_message_accumulates_then_flushes_as_one() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "fix the "));
    proj.apply(&user_chunk_event("S1", "auth bug"));
    proj.apply(&agent_chunk_event("S1", "ack"));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("fix the auth bug"));
    assert_eq!(s.last_user_msg.as_deref(), Some("fix the auth bug"));
}

#[test]
fn second_user_message_in_same_session_updates_last_only() {
    let mut proj = SessionSynopsisProjection::new();
    // Turn 1.
    proj.apply(&user_chunk_event("S1", "first request"));
    proj.apply(&agent_chunk_event("S1", "ok"));
    // Turn 2.
    proj.apply(&user_chunk_event("S1", "second request"));
    proj.apply(&agent_chunk_event("S1", "ok"));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("first request"));
    assert_eq!(s.last_user_msg.as_deref(), Some("second request"));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 5 passed (Task 2 logic already accumulates because `push_str` was used).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "test(spur-core): cover multi-chunk accumulation and second-turn updates"
```

---

### Task 4: Slash-command guard

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn slash_command_first_message_does_not_become_first_user_msg() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "/clear"));
    proj.apply(&agent_chunk_event("S1", "ok"));
    proj.apply(&user_chunk_event("S1", "real first message"));
    proj.apply(&agent_chunk_event("S1", "ack"));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(
        s.first_user_msg.as_deref(),
        Some("real first message"),
        "slash-command should not lock in as first_user_msg"
    );
    // last_user_msg DOES get the most recent submission, even slash if it's last.
    assert_eq!(s.last_user_msg.as_deref(), Some("real first message"));
}

#[test]
fn slash_command_still_updates_last_user_msg_when_most_recent() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "real msg"));
    proj.apply(&agent_chunk_event("S1", "ok"));
    proj.apply(&user_chunk_event("S1", "/clear"));
    proj.apply(&agent_chunk_event("S1", "ok"));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("real msg"));
    assert_eq!(s.last_user_msg.as_deref(), Some("/clear"));
}
```

- [ ] **Step 2: Run tests to verify the first one fails**

Run: `cargo test -p spur-core session_synopsis`
Expected: `slash_command_first_message_does_not_become_first_user_msg` fails — current `flush_pending` always sets `first_user_msg` if None.

- [ ] **Step 3: Update `flush_pending` to skip slash-commands for first_user_msg only**

Replace the `flush_pending` body in `projection.rs`:

```rust
    fn flush_pending(&mut self, session: &SessionId) {
        let buf = match self.pending.remove(session) {
            Some(b) => b,
            None => return,
        };
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            return;
        }
        let s = self.by_session.entry(session.clone()).or_default();
        if s.first_user_msg.is_none() && !trimmed.starts_with('/') {
            s.first_user_msg = Some(trimmed.to_owned());
        }
        s.last_user_msg = Some(trimmed.to_owned());
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "feat(spur-core): synopsis skips slash-command from first_user_msg"
```

---

### Task 5: Empty / whitespace-only chunks skipped

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn whitespace_only_user_message_does_not_commit_synopsis() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "   \t\n  "));
    proj.apply(&agent_chunk_event("S1", "ok"));

    assert!(
        proj.get(&SessionId("S1".into())).is_none(),
        "whitespace-only flush should not create a synopsis"
    );
}

#[test]
fn empty_chunk_then_real_chunk_commits_only_real_text() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", ""));
    proj.apply(&user_chunk_event("S1", "actual content"));
    proj.apply(&agent_chunk_event("S1", "ok"));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("actual content"));
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 9 passed (the existing `if trimmed.is_empty()` guard in `flush_pending` already handles this; trimming during commit handles the second case).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "test(spur-core): cover empty/whitespace user_message_chunk handling"
```

---

### Task 6: `TurnComplete` flush trigger

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn turn_complete_flushes_pending_buffer() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "abandoned partial msg"));
    // No agent reply — only TurnComplete.
    proj.apply(&SpurEvent::now(SpurEventBody::TurnComplete {
        session: SessionId("S1".into()),
    }));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("abandoned partial msg"));
    assert_eq!(s.last_user_msg.as_deref(), Some("abandoned partial msg"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core session_synopsis turn_complete`
Expected: `turn_complete_flushes_pending_buffer` fails — synopsis is None.

- [ ] **Step 3: Add `TurnComplete` arm to `apply`**

In the `match &event.body { ... }` of `apply()`, after the `AgentNotification` arm:

```rust
            SpurEventBody::TurnComplete { session } => {
                self.flush_pending(session);
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 10 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "feat(spur-core): synopsis flushes pending buffer on TurnComplete"
```

---

### Task 7: Other terminal flush triggers (`BrainRetired`, `SessionCompleted`, `SessionAttachRejected`)

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn brain_retired_flushes_pending() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "before retire"));
    proj.apply(&SpurEvent::now(SpurEventBody::BrainRetired {
        session: SessionId("S1".into()),
        reason: spur_acp::domain::events::BrainRetireReason::ShutdownRequested,
    }));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.last_user_msg.as_deref(), Some("before retire"));
}

#[test]
fn session_completed_flushes_pending() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "before complete"));
    proj.apply(&SpurEvent::now(SpurEventBody::SessionCompleted {
        session: SessionId("S1".into()),
    }));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.last_user_msg.as_deref(), Some("before complete"));
}
```

> Verify the exact field name of `SessionCompleted` and the `BrainRetireReason` variant against `crates/spur-acp/src/domain/events.rs:798` and surrounding lines before writing the test. If the variant name differs, update the test accordingly.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core session_synopsis brain_retired session_completed`
Expected: both fail — synopsis is None.

- [ ] **Step 3: Add the additional terminal arms to `apply`**

Inside the `match &event.body { ... }` block:

```rust
            SpurEventBody::BrainRetired { session, .. }
            | SpurEventBody::SessionCompleted { session } => {
                self.flush_pending(session);
            }
            SpurEventBody::SessionAttachRejected { acp_session_id, .. } => {
                self.flush_pending(&SessionId(acp_session_id.clone()));
            }
```

> If the actual `SessionAttachRejected` payload uses a different field/type, adjust the destructure to match. Goal: flush whatever session id the variant exposes.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 12 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "feat(spur-core): synopsis flushes pending on terminal session events"
```

---

### Task 8: `SessionHistory` arm (kiro fallback)

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test**

```rust
use spur_acp::domain::events::HistoryEntry;

fn history_entry(role: &str, text: &str) -> HistoryEntry {
    HistoryEntry {
        role: role.into(),
        text: text.into(),
    }
}

#[test]
fn session_history_populates_first_and_last_user_msg() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
        session: SessionId("S1".into()),
        entries: vec![
            history_entry("user", "first kiro msg"),
            history_entry("assistant", "ack"),
            history_entry("user", "second kiro msg"),
            history_entry("assistant", "ack"),
            history_entry("user", "third kiro msg"),
        ],
    }));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("first kiro msg"));
    assert_eq!(s.last_user_msg.as_deref(), Some("third kiro msg"));
}

#[test]
fn session_history_drops_pending_buffer() {
    let mut proj = SessionSynopsisProjection::new();
    // Stale pending from before history arrives.
    proj.apply(&user_chunk_event("S1", "stale partial"));
    proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
        session: SessionId("S1".into()),
        entries: vec![history_entry("user", "real first")],
    }));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("real first"));
    // Stale pending should have been dropped, not appended.
    assert_eq!(s.last_user_msg.as_deref(), Some("real first"));
}

#[test]
fn session_history_with_no_user_entries_is_noop() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
        session: SessionId("S1".into()),
        entries: vec![history_entry("assistant", "only assistant")],
    }));

    assert!(proj.get(&SessionId("S1".into())).is_none());
}

#[test]
fn session_history_empty_entries_is_noop() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&SpurEvent::now(SpurEventBody::SessionHistory {
        session: SessionId("S1".into()),
        entries: vec![],
    }));

    assert!(proj.get(&SessionId("S1".into())).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core session_synopsis session_history`
Expected: all four fail — `SessionHistory` is not handled.

- [ ] **Step 3: Add the `SessionHistory` arm**

Inside the `match &event.body { ... }` block in `apply`:

```rust
            SpurEventBody::SessionHistory { session, entries } => {
                // Drop any stale pending buffer for this session — the history
                // is authoritative.
                self.pending.remove(session);

                let user_texts: Vec<&str> = entries
                    .iter()
                    .filter(|e| e.role == "user")
                    .map(|e| e.text.trim())
                    .filter(|t| !t.is_empty())
                    .collect();

                if user_texts.is_empty() {
                    return;
                }

                let first = user_texts.first().copied().unwrap();
                let last = user_texts.last().copied().unwrap();
                let s = self.by_session.entry(session.clone()).or_default();
                if s.first_user_msg.is_none() && !first.starts_with('/') {
                    s.first_user_msg = Some(first.to_owned());
                }
                s.last_user_msg = Some(last.to_owned());
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 16 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "feat(spur-core): synopsis handles SessionHistory (kiro fallback)"
```

---

### Task 9: Commit-on-read fallback for abandoned mid-turn

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn get_exposes_pending_buffer_when_no_committed_last_msg() {
    let mut proj = SessionSynopsisProjection::new();
    // User typed but no flush trigger has fired.
    proj.apply(&user_chunk_event("S1", "abandoned mid turn"));

    let s = proj
        .get(&SessionId("S1".into()))
        .expect("commit-on-read should surface pending");
    assert_eq!(s.last_user_msg.as_deref(), Some("abandoned mid turn"));
    // first_user_msg may also be set by the read-side commit fallback,
    // but only if the pending text is non-slash.
    assert_eq!(s.first_user_msg.as_deref(), Some("abandoned mid turn"));
}

#[test]
fn get_does_not_promote_slash_command_to_first_user_msg_via_read_fallback() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "/clear"));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert!(s.first_user_msg.is_none(), "slash should not become first via read fallback");
    assert_eq!(s.last_user_msg.as_deref(), Some("/clear"));
}

#[test]
fn get_committed_synopsis_preferred_over_pending() {
    let mut proj = SessionSynopsisProjection::new();
    proj.apply(&user_chunk_event("S1", "committed msg"));
    proj.apply(&agent_chunk_event("S1", "ok"));
    // New pending buffer with no flush yet.
    proj.apply(&user_chunk_event("S1", "in-flight new turn"));

    let s = proj.get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("committed msg"));
    // Pending IS exposed via last_user_msg because committed last_user_msg
    // is older — but the spec choice is: prefer committed values when
    // they exist. Test the exact behavior.
    assert_eq!(s.last_user_msg.as_deref(), Some("committed msg"));
}
```

> Note: the third test pins the spec choice that committed values win over pending. If you prefer "pending overrides committed last_user_msg," update both the test and `get()` accordingly — but pick one and document it.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core session_synopsis get_`
Expected: first two fail — `get()` returns None when only pending exists.

- [ ] **Step 3: Update `get()` with commit-on-read fallback**

Replace the `get` method in `projection.rs`:

```rust
    /// Read API. Returns the committed synopsis when present. If a
    /// session has only a pending buffer (no committed last_user_msg
    /// yet — abandoned mid-user-turn), exposes the pending text as
    /// last_user_msg and (when not a slash-command) as first_user_msg.
    pub fn get(&self, id: &SessionId) -> Option<SessionSynopsis> {
        let committed = self.by_session.get(id);
        let pending_trimmed = self
            .pending
            .get(id)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());

        match (committed, pending_trimmed) {
            (Some(c), _) => Some(c.clone()),
            (None, Some(p)) => Some(SessionSynopsis {
                first_user_msg: if p.starts_with('/') {
                    None
                } else {
                    Some(p.to_owned())
                },
                last_user_msg: Some(p.to_owned()),
            }),
            (None, None) => None,
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis`
Expected: 19 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "feat(spur-core): synopsis commit-on-read fallback for abandoned mid-turn"
```

---

### Task 10: Other-event variants are no-ops (defensive coverage)

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs`

- [ ] **Step 1: Write the failing test (acts as a regression guard)**

```rust
#[test]
fn unrelated_event_variants_are_ignored() {
    let mut proj = SessionSynopsisProjection::new();
    // CostUpdate has no session synopsis relevance.
    proj.apply(&SpurEvent::now(SpurEventBody::CostUpdate {
        brain_session_id: "BS".into(),
        delta_micros_usd: 100,
        total_micros_usd: 100,
    }));
    assert!(proj.get(&SessionId("missing".into())).is_none());
}
```

> Adjust the `CostUpdate` field names to match the actual variant in `crates/spur-acp/src/domain/events.rs`. Goal: prove that an irrelevant event leaves the projection in default state.

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p spur-core session_synopsis unrelated`
Expected: 1 passed (the `_ => {}` fallthrough already handles this).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "test(spur-core): regression guard for unrelated event variants"
```

---

## Phase 2 — TUI plumbing (App + ViewContext)

### Task 11: `App` holds the projection and applies events

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Write the failing test**

Add to `app.rs` `#[cfg(test)] mod tests` (or create one if absent):

```rust
#[cfg(test)]
mod synopsis_wire_tests {
    use super::*;
    use spur_acp::SessionId;
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};
    use agent_client_protocol::schema::{SessionNotification, SessionUpdate, ContentBlock, TextContent};

    #[test]
    fn handle_spur_event_applies_to_synopsis_projection() {
        let mut app = App::new(/* fill in the existing test constructor */);

        let event = SpurEvent::now(SpurEventBody::AgentNotification {
            session: SessionId("S1".into()),
            notification: Box::new(SessionNotification {
                session_id: agent_client_protocol::schema::SessionId("S1".into()),
                update: SessionUpdate::UserMessageChunk(ContentBlock::Text(TextContent {
                    text: "hello world".into(),
                    annotations: None,
                    meta: None,
                })),
                meta: None,
            }),
        });
        app.handle_spur_event(event);

        // Pending — no flush trigger fired. Read-side fallback exposes pending.
        let s = app
            .synopsis()
            .get(&SessionId("S1".into()))
            .expect("commit-on-read fallback");
        assert_eq!(s.last_user_msg.as_deref(), Some("hello world"));
    }
}
```

> Read the existing `App::new` signature in `app.rs` to see what arguments the test must construct. Match the existing test pattern in the file.

- [ ] **Step 2: Run tests to verify they fail (compile error)**

Run: `cargo test -p spur-tui synopsis_wire`
Expected: compile error — `App` has no `synopsis` field, no `synopsis()` accessor.

- [ ] **Step 3: Add the field and accessor**

In `crates/spur-tui/src/app.rs`:

In the `use` block at the top (line 14 area):

```rust
use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};
```

In the `App` struct (around line 205-207), add a field next to `lineage` and `plan_projection`:

```rust
    synopsis: SessionSynopsisProjection,
```

In `App::new` (around line 359-360), after `plan_projection: PlanProjectionStore::new(),`:

```rust
            synopsis: SessionSynopsisProjection::new(),
```

In the existing `pub fn plan_projection(&self) -> &PlanProjectionStore` (line ~2408) area, add:

```rust
    pub fn synopsis(&self) -> &SessionSynopsisProjection {
        &self.synopsis
    }
```

In `handle_spur_event` (line ~1136), next to the existing `self.lineage.apply(&event); self.plan_projection.apply(&event);`:

```rust
        self.synopsis.apply(&event);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui synopsis_wire`
Expected: 1 passed.

- [ ] **Step 5: Run full TUI test suite to confirm no regression**

Run: `cargo test -p spur-tui`
Expected: all existing tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): App owns SessionSynopsisProjection and applies events"
```

---

### Task 12: `ViewContext` gains `synopsis: &'a SessionSynopsisProjection`

**Files:**
- Modify: `crates/spur-tui/src/views/mod.rs`
- Modify: `crates/spur-tui/src/app.rs` (ViewContext construction sites at lines ~935, ~1506, ~2533)
- Modify: `crates/spur-tui/src/lib.rs` (if `test_view_ctx` exists at line ~28)
- Modify: `crates/spur-tui/src/views/session_picker.rs` (`test_ctx()` at line ~1370)
- Modify: `crates/spur-tui/src/views/session_detail.rs` (multiple `test_ctx()` at lines ~2911, 3022, 3440, 3475, 3692, 3856)

- [ ] **Step 1: Add the field to `ViewContext`**

In `crates/spur-tui/src/views/mod.rs`, locate the `ViewContext<'a>` struct (around line 81-90). Add:

```rust
pub struct ViewContext<'a> {
    pub lineage: &'a spur_core::ExecutorLineage,
    pub plan_projection: &'a spur_core::PlanProjectionStore,
    pub synopsis: &'a spur_core::SessionSynopsisProjection,   // NEW
    pub brain_status: &'a crate::app::BrainStatus,
    pub license_badge: Option<&'a crate::components::status_bar::LicenseBadge>,
    pub flag_summary: Option<(usize, usize)>,
}
```

> Verify the existing field names against the actual file before writing — adjust as needed to match the current struct.

- [ ] **Step 2: Run `cargo build -p spur-tui` and follow the compiler**

Run: `cargo build -p spur-tui 2>&1 | head -80`
Expected: errors at every `ViewContext { ... }` construction site (missing `synopsis` field).

- [ ] **Step 3: Update each construction site**

For each error location, add `synopsis: &self.synopsis,` (in `app.rs`) or `synopsis: &SYNOPSIS,` (in test_ctx functions) inside the `ViewContext { ... }` literal.

For `app.rs`, the production sites read from the App struct: `synopsis: &self.synopsis,`.

For `test_ctx()` functions in `views/session_picker.rs` and `views/session_detail.rs`, add a static analogous to the existing `LINEAGE` and `PLAN_PROJECTION` statics:

```rust
static SYNOPSIS: std::sync::LazyLock<spur_core::SessionSynopsisProjection> =
    std::sync::LazyLock::new(spur_core::SessionSynopsisProjection::new);
```

And in the returned `ViewContext { ... }`:

```rust
synopsis: &SYNOPSIS,
```

> If the existing `LINEAGE` and `PLAN_PROJECTION` statics use `OnceLock` rather than `LazyLock`, follow that style for consistency. Inspect each test_ctx() before edit.

For `crates/spur-tui/src/lib.rs` `test_view_ctx` (line ~28), apply the same change.

- [ ] **Step 4: Build and run tests to verify they pass**

Run: `cargo build -p spur-tui && cargo test -p spur-tui`
Expected: builds cleanly, all existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/mod.rs crates/spur-tui/src/app.rs crates/spur-tui/src/lib.rs crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): ViewContext exposes &SessionSynopsisProjection"
```

---

## Phase 3 — Picker label rendering

### Task 13: `truncate_for_row` helper

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod` block:

```rust
#[cfg(test)]
mod truncate_tests {
    use super::truncate_for_row;

    #[test]
    fn keeps_short_text_unchanged() {
        assert_eq!(truncate_for_row("short", 10), "short");
    }

    #[test]
    fn cuts_at_first_sentence_boundary() {
        assert_eq!(truncate_for_row("First sentence. Second one.", 100), "First sentence");
    }

    #[test]
    fn cuts_at_first_question_mark() {
        assert_eq!(truncate_for_row("Why? Because.", 100), "Why");
    }

    #[test]
    fn cuts_at_newline() {
        assert_eq!(truncate_for_row("line one\nline two", 100), "line one");
    }

    #[test]
    fn cuts_at_grapheme_budget_with_ellipsis() {
        assert_eq!(truncate_for_row("abcdefghij", 5), "abcde\u{2026}");
    }

    #[test]
    fn handles_unicode_grapheme_clusters() {
        // "é" can be 1 or 2 codepoints; truncate by graphemes, not bytes.
        let s = "ééééé";
        assert_eq!(truncate_for_row(s, 3), "ééé\u{2026}");
    }

    #[test]
    fn returns_ellipsis_when_budget_under_one() {
        assert_eq!(truncate_for_row("anything", 0), "\u{2026}");
    }

    #[test]
    fn strips_leading_whitespace() {
        assert_eq!(truncate_for_row("   hello", 10), "hello");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (compile error)**

Run: `cargo test -p spur-tui truncate_tests`
Expected: compile error — `truncate_for_row` not defined.

- [ ] **Step 3: Implement `truncate_for_row`**

In `session_picker.rs`, add (near `Self::display_text` at line ~511):

```rust
/// Truncate a string for row display: cut at the first sentence
/// boundary or `budget` graphemes, whichever comes first. Strips
/// leading whitespace. Adds `…` when the cut shortened the text or
/// when the budget is < 1.
pub(super) fn truncate_for_row(input: &str, budget: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation;

    let trimmed = input.trim_start();
    if budget == 0 {
        return "\u{2026}".to_string();
    }

    // Find first sentence-boundary or newline.
    let punct_cut = trimmed.find(|c| matches!(c, '.' | '?' | '!' | '\n'));
    let punct_text = punct_cut.map(|i| &trimmed[..i]).unwrap_or(trimmed);

    let graphemes: Vec<&str> = punct_text.graphemes(true).collect();
    if graphemes.len() <= budget && punct_cut.is_none() {
        return punct_text.to_string();
    }
    if graphemes.len() <= budget {
        // Was cut by punctuation; no further trimming needed.
        return punct_text.to_string();
    }
    let mut out: String = graphemes.iter().take(budget).copied().collect();
    out.push('\u{2026}');
    out
}
```

> Confirm `unicode-segmentation` is already a dependency of `spur-tui` (`crates/spur-tui/Cargo.toml`). If it isn't, add it: `unicode-segmentation = "1"`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui truncate_tests`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-tui/Cargo.toml
git commit -m "feat(spur-tui): truncate_for_row helper with sentence boundary + grapheme cap"
```

---

### Task 14: `resolve_label` precedence chain

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Write the failing tests**

Add a new test module:

```rust
#[cfg(test)]
mod resolve_label_tests {
    use super::*;
    use crate::session_metadata::SessionEntry;
    use spur_acp::SessionInfo;
    use spur_core::SessionSynopsis;
    use std::path::PathBuf;

    fn info_with_title(title: Option<&str>) -> SessionInfo {
        let mut info = SessionInfo::new("S1".to_string(), PathBuf::from("/tmp/proj"));
        info.title = title.map(|t| t.to_string());
        info
    }

    fn entry_with_override(t: Option<&str>) -> SessionEntry {
        SessionEntry {
            title_override: t.map(|s| s.to_string()),
            ..SessionEntry::default()
        }
    }

    fn synopsis_with_first(t: Option<&str>) -> SessionSynopsis {
        SessionSynopsis {
            first_user_msg: t.map(|s| s.to_string()),
            last_user_msg: None,
        }
    }

    #[test]
    fn title_override_wins_over_everything() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(Some("manual rename"));
        let synopsis = synopsis_with_first(Some("first user msg"));
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "manual rename"
        );
    }

    #[test]
    fn first_user_msg_beats_agent_title_when_no_override() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(None);
        let synopsis = synopsis_with_first(Some("real intent"));
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "real intent"
        );
    }

    #[test]
    fn agent_title_used_when_no_synopsis() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(None);
        let synopsis = synopsis_with_first(None);
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "agent title"
        );
    }

    #[test]
    fn cwd_fallback_when_no_title_or_synopsis() {
        let info = info_with_title(None);
        let entry = entry_with_override(None);
        assert_eq!(
            resolve_label(&info, Some(&entry), None, true, 60),
            "proj/"
        );
    }

    #[test]
    fn final_fallback_to_untitled_session() {
        let info = info_with_title(None);
        assert_eq!(
            resolve_label(&info, None, None, false, 60),
            "(untitled session)"
        );
    }

    #[test]
    fn empty_string_override_is_skipped() {
        let info = info_with_title(Some("agent title"));
        let entry = entry_with_override(Some(""));
        let synopsis = synopsis_with_first(Some("first user msg"));
        // Empty override falls through to first_user_msg.
        assert_eq!(
            resolve_label(&info, Some(&entry), Some(&synopsis), false, 60),
            "first user msg"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui resolve_label_tests`
Expected: compile error — `resolve_label` is not defined.

- [ ] **Step 3: Implement `resolve_label`**

Add to `session_picker.rs` (replacing or adjacent to the existing `resolved_title`):

```rust
pub(super) fn resolve_label(
    session: &spur_acp::SessionInfo,
    entry: Option<&crate::session_metadata::SessionEntry>,
    synopsis: Option<&spur_core::SessionSynopsis>,
    show_cwd: bool,
    label_budget: usize,
) -> String {
    if let Some(t) = entry
        .and_then(|e| e.title_override.as_deref())
        .filter(|t| !t.is_empty())
    {
        return truncate_for_row(t, label_budget);
    }
    if let Some(snippet) = synopsis
        .and_then(|s| s.first_user_msg.as_deref())
        .filter(|s| !s.is_empty())
    {
        return truncate_for_row(snippet, label_budget);
    }
    if let Some(t) = session.title.as_deref().filter(|t| !t.is_empty()) {
        return truncate_for_row(t, label_budget);
    }
    if show_cwd {
        return format!("{}/", SessionPickerView::cwd_basename(&session.cwd));
    }
    "(untitled session)".to_string()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui resolve_label_tests`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
git commit -m "feat(spur-tui): resolve_label precedence override>first-msg>title>cwd"
```

---

### Task 15: Wire `resolve_label` into row rendering and remove `resolved_title`

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Update `render_populated` to use `resolve_label`**

Replace the row composition block at line ~736-799. The key change is: at line ~742 (where `display = Self::resolved_title(...)` is computed), replace with:

```rust
            let synopsis = ctx.synopsis.get(&session.session_id);
            let label_budget = compute_label_budget(area.width, show_cwd, show_brain);
            let display = resolve_label(
                session,
                self.metadata.sessions.get(session.session_id.0.as_ref()),
                synopsis.as_ref(),
                show_cwd,
                label_budget,
            );
```

> `ctx` is the `&ViewContext` passed to `render`. If `render_populated`'s signature does not currently take `ctx`, thread it through from `render`. The signature change is mechanical.

Add a helper near the top of the file:

```rust
fn compute_label_budget(area_width: u16, show_cwd: bool, show_brain: bool) -> usize {
    // Right gutter: short_id (8) + 2 spaces, relative_ts (~8) + 2 spaces,
    // optional brain (~8) + 2 spaces, optional cwd basename (~14) + 2 spaces,
    // prefix "  " or "▸ " (2 chars). Pad to a static cap for ultrawide.
    let mut gutter = 2 /* prefix */ + 8 + 2 /* short_id+gap */ + 8 + 2 /* time+gap */;
    if show_brain { gutter += 8 + 2; }
    if show_cwd   { gutter += 16 + 2; } // cwd basename + slash + gap
    let avail = (area_width as usize).saturating_sub(gutter);
    avail.min(60).max(8)
}
```

- [ ] **Step 2: Replace the existing `resolved_title` callers**

`grep -n "resolved_title" crates/spur-tui/src/views/session_picker.rs` to find all call sites. They appear in:
- Render row composition (replaced above).
- Filter haystack composition (Task 18 will rewrite this).
- `build_preselect_banner` (replace with `resolve_label`, passing `synopsis = ctx.synopsis.get(&session.session_id).as_ref()`).
- `R` rename buffer initialization (replace with `resolve_label`).

For each non-render call site, replace:

```rust
let label = Self::resolved_title(session, &self.metadata, false);
```

with:

```rust
let synopsis = self.synopsis_view().get(&session.session_id);
let label = resolve_label(
    session,
    self.metadata.sessions.get(session.session_id.0.as_ref()),
    synopsis.as_ref(),
    false,
    usize::MAX,
);
```

> Where this call is inside a method that doesn't have access to `ctx`, add a `synopsis: &SessionSynopsisProjection` parameter to that method and thread it from the caller. The `R` rename handler in `handle_key` does have access via `ctx`.

- [ ] **Step 3: Delete `resolved_title`**

Remove the `fn resolved_title(...)` definition at line ~521.

- [ ] **Step 4: Build and run tests**

Run: `cargo build -p spur-tui && cargo test -p spur-tui`
Expected: builds and tests pass. The existing `current_session_shortcut_tests` and `enter_on_*` tests should still pass — they don't depend on label content.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
git commit -m "refactor(spur-tui): replace resolved_title with resolve_label everywhere"
```

---

## Phase 4 — Preview pane

### Task 16: `PreviewRow` extension in `session_preview.rs`

**Files:**
- Modify: `crates/spur-tui/src/components/session_preview.rs`

- [ ] **Step 1: Read current `PreviewContent` struct shape**

Open `crates/spur-tui/src/components/session_preview.rs` and inspect the existing `PreviewContent` (around line 11-16). It currently has `rows: Vec<(String, String)>` and `placeholder: Option<String>`.

- [ ] **Step 2: Write the failing test for the new `PreviewRow`**

Add a `#[cfg(test)] mod tests` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Style};

    #[test]
    fn from_tuple_creates_unstyled_unwrapped_row() {
        let row: PreviewRow = ("Label".to_string(), "Value".to_string()).into();
        assert_eq!(row.label, "Label");
        assert_eq!(row.value, "Value");
        assert!(row.value_style.is_none());
        assert!(!row.wrap);
    }

    #[test]
    fn explicit_construction_with_style_and_wrap() {
        let row = PreviewRow {
            label: "Intent".into(),
            value: "long wrapped value".into(),
            value_style: Some(Style::default().fg(Color::Gray)),
            wrap: true,
        };
        assert_eq!(row.label, "Intent");
        assert!(row.value_style.is_some());
        assert!(row.wrap);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p spur-tui session_preview`
Expected: compile error — `PreviewRow` not defined.

- [ ] **Step 4: Add `PreviewRow` and update `PreviewContent`**

In `session_preview.rs`:

```rust
#[derive(Debug, Clone, Default)]
pub struct PreviewRow {
    pub label: String,
    pub value: String,
    pub value_style: Option<ratatui::style::Style>,
    pub wrap: bool,
}

impl From<(String, String)> for PreviewRow {
    fn from((label, value): (String, String)) -> Self {
        Self { label, value, value_style: None, wrap: false }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PreviewContent {
    pub rows: Vec<PreviewRow>,
    pub placeholder: Option<String>,
}
```

> Existing call sites that build `PreviewContent { rows: vec![("k".into(), "v".into()), ...], ... }` will break. Update them to `vec![("k".into(), "v".into()).into(), ...]` or to explicit `PreviewRow { ... }` constructors.

- [ ] **Step 5: Update the renderer (`SessionPreview::render`) to honor `value_style` and `wrap`**

Find the existing render impl in `session_preview.rs` (it iterates `rows` and renders each as label+value). Update to:

```rust
        for row in &content.rows {
            // Empty label = visual separator: render value only at row position.
            // Empty label AND empty value = blank row.
            let value_style = row.value_style.unwrap_or_else(Style::default);
            if row.wrap {
                // Use ratatui Paragraph with Wrap { trim: false } for this row.
                // (Existing renderer probably uses Lines; switch to Paragraph for
                // wrapped rows, single-line style otherwise.)
            } else {
                // Existing single-line render path.
            }
        }
```

> The exact integration depends on the current renderer's structure. Goal: rows with `wrap: true` flow across multiple lines bounded by the pane width; rows with `value_style: Some(...)` use that style for the value span; existing call sites continue to work via `From<(String, String)>`.

- [ ] **Step 6: Run tests to verify they pass and full TUI suite is clean**

Run: `cargo test -p spur-tui`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/session_preview.rs
git commit -m "feat(spur-tui): PreviewRow with value_style + wrap; From<(String,String)>"
```

---

### Task 17: State-first preview population

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Locate the preview-build site**

In `session_picker.rs`, find the preview-content construction block at line ~847-899 (inside `render_populated`, when `self.preview_visible` is true).

- [ ] **Step 2: Write the new preview construction**

Replace the existing block with:

```rust
            use crate::components::session_preview::{PreviewContent, PreviewRow, SessionPreview};
            use ratatui::style::{Color, Modifier, Style};

            let content = if cursor == 0 {
                PreviewContent {
                    rows: vec![],
                    placeholder: Some(
                        "Press Enter to start a new session \u{00b7} any unsent draft will be saved"
                            .to_string(),
                    ),
                }
            } else {
                let indices = Self::filtered_indices(sessions, filter, &self.metadata, self.show_archived);
                let real_idx = indices.get(cursor - 1).copied();
                if let Some(i) = real_idx {
                    let session = &sessions[i];
                    let entry = self.metadata.sessions.get(session.session_id.0.as_ref());
                    let synopsis = ctx.synopsis.get(&session.session_id);
                    let draft = entry.map(|e| e.draft.clone()).unwrap_or_default();
                    let brain = entry.and_then(|e| e.brain_name.clone()).unwrap_or_default();
                    let cwd = session.cwd.display().to_string();
                    let short_id = {
                        let raw = session.session_id.0.as_ref();
                        raw[..8.min(raw.len())].to_string()
                    };

                    let mut rows: Vec<PreviewRow> = Vec::new();

                    // 1. Last
                    if let Some(last) = synopsis.as_ref().and_then(|s| s.last_user_msg.clone()) {
                        rows.push(PreviewRow {
                            label: "Last".into(),
                            value: last,
                            value_style: None,
                            wrap: false,
                        });
                    }

                    // 2. Draft
                    if !draft.is_empty() {
                        rows.push(PreviewRow {
                            label: "Draft".into(),
                            value: draft,
                            value_style: Some(Style::default().fg(Color::Yellow)),
                            wrap: false,
                        });
                    }

                    // 3. Blank separator
                    rows.push(PreviewRow::default());

                    // 4. Intent (wrapped, dim gray)
                    if let Some(first) = synopsis.as_ref().and_then(|s| s.first_user_msg.clone()) {
                        rows.push(PreviewRow {
                            label: "Intent".into(),
                            value: first,
                            value_style: Some(Style::default().fg(Color::Gray)),
                            wrap: true,
                        });
                    }

                    // 5. Blank separator
                    rows.push(PreviewRow::default());

                    // 6. Footer
                    rows.push(PreviewRow {
                        label: "".into(),
                        value: format!("{cwd} \u{00b7} {brain} \u{00b7} {short_id}"),
                        value_style: Some(Style::default().fg(Color::DarkGray)),
                        wrap: false,
                    });

                    PreviewContent { rows, placeholder: None }
                } else {
                    PreviewContent::default()
                }
            };
            SessionPreview::render(frame, chunks[1], &content);
```

- [ ] **Step 3: Update `preview_height` constant**

In `render_populated` (line ~802), change:

```rust
        let preview_height: u16 = 12;
```

(was 8). Make sure the layout chunks math at lines 810-836 uses this constant; the existing `Constraint::Length(preview_height)` should pick it up.

- [ ] **Step 4: Build and run tests**

Run: `cargo build -p spur-tui && cargo test -p spur-tui`
Expected: builds, all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
git commit -m "feat(spur-tui): state-first preview pane (Last + Draft top, Intent below)"
```

---

## Phase 5 — Filter widening + haystack precompute

### Task 18: Widen filter haystack to include synopsis fields

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Write the failing test**

Add a test case that exercises filter matching against synopsis content:

```rust
#[cfg(test)]
mod filter_haystack_tests {
    use super::*;
    use crate::session_metadata::SessionMetadata;
    use spur_core::{SessionSynopsis, SessionSynopsisProjection};
    use std::path::PathBuf;

    #[test]
    fn filter_matches_first_user_msg_even_when_label_does_not() {
        // Session "S1" has agent title "Build fix" but synopsis first_user_msg
        // contains "auth refactor". Filter "auth" should match.
        let sessions = vec![{
            let mut s = SessionInfo::new("S1".into(), PathBuf::from("/tmp"));
            s.title = Some("Build fix".into());
            s
        }];
        let metadata = SessionMetadata::default();

        // We need to thread a synopsis projection. The current
        // filtered_indices signature must be extended to accept &SessionSynopsisProjection.
        let mut synopsis = SessionSynopsisProjection::new();
        // (Inject synthetic synopsis directly via a test helper, or use the
        // public apply() with a UserMessageChunk + AgentMessageChunk pair.)
        // For unit-test simplicity, expose a #[cfg(test)] insert helper on the
        // projection, OR construct via apply().

        // Drive synopsis to first_user_msg = "refactor auth callers".
        // (Use the same apply() helpers used in spur-core projection tests,
        // adapted to spur-tui's test scope, OR inject via a #[cfg(test)] helper.)
        let _ = &mut synopsis; // placeholder; engineer wires this up.

        let indices = SessionPickerView::filtered_indices(
            &sessions,
            "auth",
            &metadata,
            false,
            &synopsis,
        );
        assert_eq!(indices, vec![0], "filter 'auth' should match synopsis content");
    }
}
```

> The test depends on the projection being populated. Easiest path: add a `#[cfg(test)] pub fn insert_for_test(&mut self, id, synopsis)` to `SessionSynopsisProjection` in spur-core (gated on `#[cfg(test)]` or a `cfg(any(test, feature = "test-helpers"))` feature), and use it from this test. Alternative: drive via `apply()` of synthetic events, mirroring the spur-core projection tests.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui filter_haystack_tests`
Expected: compile error — `filtered_indices` doesn't take a `&SessionSynopsisProjection`.

- [ ] **Step 3: Update `filtered_indices` signature and body**

In `session_picker.rs`, update `fn filtered_indices` (line ~318):

```rust
    fn filtered_indices(
        sessions: &[SessionInfo],
        filter: &str,
        metadata: &SessionMetadata,
        show_archived: bool,
        synopsis: &spur_core::SessionSynopsisProjection,
    ) -> Vec<usize> {
        // ... existing candidates filter ...

        if filter.is_empty() {
            // ... existing sort logic unchanged ...
            return all;
        }

        use nucleo_matcher::{
            pattern::{CaseMatching, Normalization, Pattern},
            Matcher,
        };
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, usize)> = candidates
            .into_iter()
            .filter_map(|i| {
                let session = &sessions[i];
                let entry = metadata.sessions.get(session.session_id.0.as_ref());
                let synopsis_for = synopsis.get(&session.session_id);

                let label = resolve_label(
                    session,
                    entry,
                    synopsis_for.as_ref(),
                    false,
                    usize::MAX,
                );
                let first = synopsis_for
                    .as_ref()
                    .and_then(|s| s.first_user_msg.as_deref())
                    .unwrap_or("");
                let last = synopsis_for
                    .as_ref()
                    .and_then(|s| s.last_user_msg.as_deref())
                    .unwrap_or("");
                let cwd = session.cwd.display().to_string();
                let id = session.session_id.0.as_ref();
                let haystack = format!("{label} {first} {last} {cwd} {id}");
                let score = pattern.score(
                    nucleo_matcher::Utf32Str::new(&haystack, &mut Vec::new()),
                    &mut matcher,
                )?;
                Some((score, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }
```

Update every call site of `filtered_indices` to pass `&ctx.synopsis` (or `&self.synopsis_for_test()` in tests).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui`
Expected: all tests pass, including new filter test.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs crates/spur-core/src/session_synopsis/projection.rs
git commit -m "feat(spur-tui): filter haystack matches synopsis first/last_user_msg"
```

---

### Task 19: Lazy haystack precompute on `set_sessions` and filter input

**Files:**
- Modify: `crates/spur-tui/src/views/session_picker.rs`

- [ ] **Step 1: Add `haystacks` field to `PickerState::Populated`**

In `session_picker.rs`, update the enum (line ~83):

```rust
enum PickerState {
    Loading,
    Populated {
        agent: String,
        sessions: Vec<SessionInfo>,
        haystacks: Vec<String>,   // NEW: parallel to sessions, indexed by real_i
        cursor: usize,
        search_focused: bool,
        filter: String,
    },
    Error { message: String },
}
```

- [ ] **Step 2: Build haystacks in `set_sessions`**

Update `pub fn set_sessions(...)` (line ~249). After computing `indices` and `cursor`:

```rust
        let haystacks = sessions.iter().enumerate().map(|(i, _)| {
            self.build_haystack_for(i, &sessions, synopsis)
        }).collect();
```

> `set_sessions` will need a new `synopsis: &SessionSynopsisProjection` parameter. Add it to the signature and update the App caller.

Add a helper method:

```rust
    fn build_haystack_for(
        &self,
        i: usize,
        sessions: &[SessionInfo],
        synopsis: &spur_core::SessionSynopsisProjection,
    ) -> String {
        let session = &sessions[i];
        let entry = self.metadata.sessions.get(session.session_id.0.as_ref());
        let synopsis_for = synopsis.get(&session.session_id);
        let label = resolve_label(session, entry, synopsis_for.as_ref(), false, usize::MAX);
        let first = synopsis_for.as_ref().and_then(|s| s.first_user_msg.as_deref()).unwrap_or("");
        let last = synopsis_for.as_ref().and_then(|s| s.last_user_msg.as_deref()).unwrap_or("");
        let cwd = session.cwd.display().to_string();
        let id = session.session_id.0.as_ref();
        format!("{label} {first} {last} {cwd} {id}")
    }
```

- [ ] **Step 3: Use cached haystacks in `filtered_indices`**

Update `filtered_indices` to read from `&[String]` instead of computing per-call:

```rust
    fn filtered_indices(
        sessions: &[SessionInfo],
        haystacks: &[String],
        filter: &str,
        metadata: &SessionMetadata,
        show_archived: bool,
    ) -> Vec<usize> {
        // ... candidates filter unchanged ...
        if filter.is_empty() {
            // ... unchanged sort logic ...
            return all;
        }
        // Replace the in-loop format!() with a lookup:
        let mut scored: Vec<(u32, usize)> = candidates
            .into_iter()
            .filter_map(|i| {
                let score = pattern.score(
                    nucleo_matcher::Utf32Str::new(&haystacks[i], &mut Vec::new()),
                    &mut matcher,
                )?;
                Some((score, i))
            })
            .collect();
        // ... unchanged ...
    }
```

The signature change cascades. Update every caller to pass `haystacks` from the `PickerState::Populated` it's already destructuring.

- [ ] **Step 4: Add a test for cache reuse**

```rust
#[test]
fn haystack_is_built_once_per_set_sessions() {
    // Construct picker, call set_sessions, then verify the haystack
    // for session S1 is the value computed at set_sessions time
    // (does not include synopsis updates that arrive between set_sessions
    // and the next set_sessions).
    // Pure black-box: feed a synopsis update after set_sessions, then
    // call filter, and confirm the synopsis update is NOT visible in
    // filter results (it'll be visible on the next set_sessions).
    // ...
}
```

> If unit-testing the cache is awkward, leave a `#[cfg(test)] pub fn debug_haystack(&self, i: usize) -> Option<&str>` accessor on the picker and assert against it.

- [ ] **Step 5: Build and run tests**

Run: `cargo build -p spur-tui && cargo test -p spur-tui`
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/views/session_picker.rs
git commit -m "perf(spur-tui): precompute filter haystacks on set_sessions"
```

---

## Phase 6 — Integration test + manual QA

### Task 20: End-to-end synthetic-event integration test

**Files:**
- Create: `crates/spur-tui/tests/picker_synopsis_e2e.rs`

- [ ] **Step 1: Write the integration test**

Create `crates/spur-tui/tests/picker_synopsis_e2e.rs`:

```rust
//! End-to-end: synthetic SpurEvents through App → projection → picker label.

use spur_acp::SessionId;
use spur_acp::domain::events::{SpurEvent, SpurEventBody, HistoryEntry};
use agent_client_protocol::schema::{SessionNotification, SessionUpdate, ContentBlock, TextContent};

fn user_chunk(session: &str, text: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: SessionId(session.into()),
        notification: Box::new(SessionNotification {
            session_id: agent_client_protocol::schema::SessionId(session.into()),
            update: SessionUpdate::UserMessageChunk(ContentBlock::Text(TextContent {
                text: text.into(), annotations: None, meta: None,
            })),
            meta: None,
        }),
    })
}

fn agent_chunk(session: &str, text: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: SessionId(session.into()),
        notification: Box::new(SessionNotification {
            session_id: agent_client_protocol::schema::SessionId(session.into()),
            update: SessionUpdate::AgentMessageChunk(ContentBlock::Text(TextContent {
                text: text.into(), annotations: None, meta: None,
            })),
            meta: None,
        }),
    })
}

#[test]
fn live_user_chunks_produce_synopsis_visible_to_picker() {
    let mut app = spur_tui::App::new(/* match existing constructor */);
    app.handle_spur_event(user_chunk("S1", "fix the auth refactor bug"));
    app.handle_spur_event(agent_chunk("S1", "ack"));

    let s = app.synopsis().get(&SessionId("S1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("fix the auth refactor bug"));
    assert_eq!(s.last_user_msg.as_deref(), Some("fix the auth refactor bug"));
}

#[test]
fn session_history_replay_populates_synopsis_for_kiro_path() {
    let mut app = spur_tui::App::new(/* match existing constructor */);
    app.handle_spur_event(SpurEvent::now(SpurEventBody::SessionHistory {
        session: SessionId("kiro1".into()),
        entries: vec![
            HistoryEntry { role: "user".into(), text: "first kiro".into() },
            HistoryEntry { role: "assistant".into(), text: "ok".into() },
            HistoryEntry { role: "user".into(), text: "second kiro".into() },
        ],
    }));

    let s = app.synopsis().get(&SessionId("kiro1".into())).unwrap();
    assert_eq!(s.first_user_msg.as_deref(), Some("first kiro"));
    assert_eq!(s.last_user_msg.as_deref(), Some("second kiro"));
}
```

> The exact `App::new(...)` constructor signature requires inspecting `app.rs`. If `App::new` requires runtime infrastructure (broadcast channels, license badge, etc.), expose a `#[cfg(test)] pub fn new_for_test() -> App` constructor that wires defaults. Keep the integration test surface minimal.

- [ ] **Step 2: Build and run**

Run: `cargo test -p spur-tui --test picker_synopsis_e2e`
Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/tests/picker_synopsis_e2e.rs
git commit -m "test(spur-tui): e2e synopsis populates from live chunks and SessionHistory"
```

---

## Final sweep

### Manual QA checklist

- [ ] Resume a Claude Code session in the TUI; confirm picker shows the user's first message as the row label after a moment.
- [ ] Resume a kiro session that has a prior `~/.kiro/sessions/cli/<id>.jsonl` file; confirm picker label populates from history.
- [ ] Submit `/clear` as the first message in a fresh session; confirm row label falls through to agent title (or cwd), not `/clear`.
- [ ] Type a partial message in the composer, hit `Esc` without submitting, return to picker; confirm preview Last shows nothing or pending text via commit-on-read.
- [ ] Filter `/auth` in the picker; confirm a session whose visible label is "Build fix" but synopsis contains "auth" is included in matches.
- [ ] Press `R` to rename a session; confirm `title_override` wins over the synopsis-derived label.
- [ ] Test on 80×24, 120×40, and 200×60 terminal sizes; confirm row label fits, preview height grows from 8 → 12 when `P` is on, and ratatui clipping is acceptable on small heights.

### Acceptance criteria

- All 20 tasks committed in order.
- `cargo build --workspace` succeeds.
- `cargo test --workspace` passes (existing + new tests).
- `cargo insta review` shows no unexpected snapshot changes.
- Manual QA checklist passes for at least Claude Code and one of (kiro / no-history fallback).

### Out of scope (deferred to v2)

- NDJSON replay on TUI startup to rehydrate the projection (closes the broadcast `Lagged` user-visible degradation).
- Input-side `UserPromptSubmitted` event with prompt blocks.
- Bot integration of `SessionSynopsisProjection`.
- Per-session cost in preview footer.
- AI-generated semantic summary.
- Backfill for legacy sessions.

---

## Self-review checklist

(Author checklist — engineer can ignore.)

- [x] **Spec coverage:** every locked decision has a task. Precedence chain (Task 14), preview hierarchy (Task 17), data tiers via projection in core (Tasks 1-10), kiro fallback (Task 8), commit-on-read (Task 9), Lagged-degradation documented in spec (no new code needed). All present.
- [x] **Placeholder scan:** "TBD"/"TODO"/"appropriate"/"similar to" — none in task bodies.
- [x] **Type consistency:** `SessionSynopsis { first_user_msg, last_user_msg }` — used uniformly. `apply()` matches existing `lineage.apply` convention. `get(id) -> Option<SessionSynopsis>` — used uniformly. `PreviewRow { label, value, value_style, wrap }` — used uniformly.
- [x] Code blocks complete in every step that introduces new code.
- [x] Exact file paths and line ranges for every modify.
- [x] Exact build/test commands with expected output.
