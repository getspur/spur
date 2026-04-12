# ACP SessionEvent Pass-Through Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the lossy `SessionEvent` translation layer and pass the SDK's `SessionNotification` through `SpurEvent` verbatim, giving the TUI full access to ACP protocol data.

**Architecture:** Replace `SpurEvent::AgentOutput { session, event: SessionEvent }` with `SpurEvent::AgentNotification { session, notification: SessionNotification }`. Delete `SessionEvent`, `AgentStatus`, and the `notification_to_session_event()` mapping function. Update all consumers to match on `SessionUpdate` variants directly.

**Tech Stack:** Rust, agent-client-protocol SDK 0.10.4, ratatui

---

### Task 1: Add SDK re-exports to spur-acp

**Files:**
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Add re-exports**

Add after the existing re-exports in `crates/spur-acp/src/lib.rs`:

```rust
// Re-export ACP SDK types for consumer crates (TUI, orchestrator).
// This avoids adding agent-client-protocol to each consumer's Cargo.toml.
pub use agent_client_protocol::{
    ContentBlock, ContentChunk, TextContent,
    SessionNotification, SessionUpdate,
    ToolCall as AcpToolCall, ToolCallUpdate as AcpToolCallUpdate,
    ToolCallStatus, ToolKind, ToolCallContent, ToolCallLocation,
    Plan, PlanEntry, PlanEntryStatus, PlanEntryPriority,
};
```

Note: `ToolCall` and `ToolCallUpdate` are aliased with `Acp` prefix to avoid confusion with spur-internal names if any exist.

- [ ] **Step 2: Verify build**

Run: `cargo check --package spur-acp`
Expected: PASS — re-exports are additive.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): re-export ACP SDK types for consumer crates"
```

---

### Task 2: Update SpurEvent to carry SessionNotification

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`

- [ ] **Step 1: Add SessionNotification import and rename variant**

In `crates/spur-acp/src/domain/events.rs`, add the import and change the `AgentOutput` variant:

```rust
use crate::types::SessionId;
use crate::domain::delegation::DelegationStatus;
use agent_client_protocol::SessionNotification;
```

Replace:
```rust
AgentOutput { session: SessionId, event: SessionEvent },
```
With:
```rust
AgentNotification { session: SessionId, notification: SessionNotification },
```

- [ ] **Step 2: Verify build fails (expected — consumers still reference AgentOutput)**

Run: `cargo check --package spur-acp 2>&1 | head -5`
Expected: Compile errors in spur-core and spur-tui referencing `AgentOutput` and `SessionEvent`. This confirms the change propagates.

- [ ] **Step 3: Commit (breaking change, will be fixed in Tasks 3-5)**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "feat(spur-acp): replace AgentOutput with AgentNotification carrying SDK SessionNotification"
```

---

### Task 3: Delete SessionEvent and AgentStatus from types.rs

**Files:**
- Modify: `crates/spur-acp/src/types.rs`

- [ ] **Step 1: Delete SessionEvent and AgentStatus enums**

Remove the `AgentStatus` enum (lines 50-58) and the `SessionEvent` enum (lines 62-102) from `crates/spur-acp/src/types.rs`. These are:

```rust
// DELETE this entire block:
pub enum AgentStatus {
    Thinking,
    Working,
    Idle,
    Done,
    Error,
}

// DELETE this entire block:
pub enum SessionEvent {
    TextDelta(String),
    MessageDelta(String),
    ToolCallStart { id: String, name: String, input: serde_json::Value },
    ToolCallResult { id: String, output: serde_json::Value },
    StatusUpdate(AgentStatus),
    RateLimitHit { retry_after: Option<Duration> },
    Error { code: i32, message: String },
    Complete { session_id: SessionId },
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/spur-acp/src/types.rs
git commit -m "feat(spur-acp): delete SessionEvent and AgentStatus enums"
```

---

### Task 4: Update orchestrator to emit AgentNotification directly

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Delete notification_to_session_event() and helpers**

Remove the `notification_to_session_event()` function (around line 1095-1141) from `crates/spur-core/src/orchestrator.rs`.

- [ ] **Step 2: Update run_adhoc() brain streaming loop (around line 254)**

Replace:
```rust
while let Some(notification) = stream.next().await {
    let event = notification_to_session_event(&notification);

    match &event {
        SessionEvent::TextDelta(text) | SessionEvent::MessageDelta(text) => {
            print!("{text}");
        }
        SessionEvent::ToolCallStart { name, .. } => {
            debug!(tool = %name, "Brain calling tool");
        }
        SessionEvent::Error { code, message } => {
            error!(code, message = %message, "Brain agent error");
            success = false;
        }
        SessionEvent::RateLimitHit { retry_after } => {
            warn!(retry_after = ?retry_after, "Brain hit rate limit");
            self.emit(SpurEvent::RateLimitDetected {
                agent: brain_name.clone(),
                retry_after: *retry_after,
            });
        }
        SessionEvent::Complete { .. } => {
            info!(brain = %brain_name, "Brain session completed");
        }
        _ => {}
    }

    self.emit(SpurEvent::AgentOutput {
        session: session_id.clone(),
        event,
    });
}
```

With:
```rust
while let Some(notification) = stream.next().await {
    match &notification.update {
        SessionUpdate::AgentThoughtChunk(chunk) | SessionUpdate::AgentMessageChunk(chunk) => {
            if let ContentBlock::Text(tc) = &chunk.content {
                print!("{}", tc.text);
            }
        }
        SessionUpdate::ToolCall(tc) => {
            debug!(tool = %tc.title, "Brain calling tool");
        }
        _ => {}
    }

    self.emit(SpurEvent::AgentNotification {
        session: session_id.clone(),
        notification,
    });
}
```

Note: `RateLimitHit`, `Error`, and `Complete` events were being emitted as both `SessionEvent` variants AND separate `SpurEvent` variants — double-emission. The orchestrator already emits `SpurEvent::RateLimitDetected` and handles errors at the stream-end level. The redundant `SessionEvent` matching is removed.

- [ ] **Step 3: Update worker delegation streaming loop (around line 1022)**

Replace:
```rust
while let Some(notification) = stream.next().await {
    let event = notification_to_session_event(&notification);
    match event {
        SessionEvent::TextDelta(text)
        | SessionEvent::MessageDelta(text) => {
            output_text.push_str(&text);
        }
        SessionEvent::Error { message, .. } => {
            worker_success = false;
            output_text.push_str(&format!("\nError: {message}"));
        }
        _ => {}
    }
}
```

With:
```rust
while let Some(notification) = stream.next().await {
    match &notification.update {
        SessionUpdate::AgentThoughtChunk(chunk)
        | SessionUpdate::AgentMessageChunk(chunk) => {
            if let ContentBlock::Text(tc) = &chunk.content {
                output_text.push_str(&tc.text);
            }
        }
        _ => {}
    }
}
```

Note: Worker errors are already handled at the `connection.prompt()` result level. The inline `SessionEvent::Error` match was defensive — the SDK signals errors via the prompt result, not via notifications.

- [ ] **Step 4: Update run_single_agent() streaming loop (around line 500)**

Replace:
```rust
while let Some(notification) = stream.next().await {
    let event = notification_to_session_event(&notification);
    match &event {
        SessionEvent::TextDelta(text) | SessionEvent::MessageDelta(text) => {
            print!("{text}");
        }
        SessionEvent::Error { message, .. } => {
            error!(message = %message, "Agent error");
            success = false;
        }
        _ => {}
    }
}
```

With:
```rust
while let Some(notification) = stream.next().await {
    match &notification.update {
        SessionUpdate::AgentThoughtChunk(chunk)
        | SessionUpdate::AgentMessageChunk(chunk) => {
            if let ContentBlock::Text(tc) = &chunk.content {
                print!("{}", tc.text);
            }
        }
        _ => {}
    }
}
```

- [ ] **Step 5: Update interactive loop (around line 396)**

Replace:
```rust
Some(notification) => {
    let event = notification_to_session_event(&notification);
    self.emit(SpurEvent::AgentOutput {
        session: b.spur_session_id.clone(),
        event,
    });
}
```

With:
```rust
Some(notification) => {
    self.emit(SpurEvent::AgentNotification {
        session: b.spur_session_id.clone(),
        notification,
    });
}
```

- [ ] **Step 6: Update imports at top of orchestrator.rs**

Remove any `use spur_acp::SessionEvent` or `use spur_acp::AgentStatus` imports. Add:

```rust
use spur_acp::{SessionUpdate, ContentBlock};
```

(These may already be imported via `spur_acp::*` or similar.)

- [ ] **Step 7: Verify spur-core compiles**

Run: `cargo check --package spur-core`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): emit AgentNotification with SDK types, delete mapping function"
```

---

### Task 5: Update TUI session_detail to match on SessionUpdate

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Add extract_text helper**

Add near the other formatting helpers at the bottom of `session_detail.rs`:

```rust
/// Extract text from an ACP ContentChunk, returning None for non-text content.
fn extract_text(chunk: &spur_acp::ContentChunk) -> Option<&str> {
    match &chunk.content {
        spur_acp::ContentBlock::Text(tc) => Some(&tc.text),
        _ => None,
    }
}
```

- [ ] **Step 2: Update handle_spur_event match arm**

In `handle_spur_event`, replace the `SpurEvent::AgentOutput` match arm. The current code matches on `SessionEvent` variants inside `AgentOutput`. Replace the entire arm:

```rust
SpurEvent::AgentOutput {
    session,
    event: se,
} => {
    if session.0 != self.session_id.0 {
        return;
    }
    match se {
        SessionEvent::TextDelta(text) => { ... }
        SessionEvent::MessageDelta(text) => { ... }
        SessionEvent::ToolCallStart { name, input, .. } => { ... }
        SessionEvent::ToolCallResult { output, .. } => { ... }
        SessionEvent::Error { message, .. } => { ... }
        SessionEvent::Complete { .. } => { ... }
        _ => {}
    }
}
```

With:

```rust
SpurEvent::AgentNotification {
    session,
    notification,
} => {
    if session.0 != self.session_id.0 {
        return;
    }

    match &notification.update {
        spur_acp::SessionUpdate::AgentThoughtChunk(chunk) => {
            if let Some(text) = extract_text(chunk) {
                if !text.is_empty() {
                    self.react_trace.append_think(text, Self::now_stamp());
                }
            }
        }
        spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
            if let Some(text) = extract_text(chunk) {
                if !text.is_empty() {
                    self.react_trace.append_message(
                        text,
                        &self.agent_name,
                        Self::now_stamp(),
                    );
                }
            }
        }
        spur_acp::SessionUpdate::ToolCall(tc) => {
            let args = format_tool_args(
                &tc.raw_input.clone().unwrap_or(serde_json::Value::Null),
            );
            self.react_trace.push(TraceEntry {
                kind: TraceKind::Act {
                    tool: tc.title.clone(),
                    args,
                },
                text: String::new(),
                timestamp: Self::now_stamp(),
            });
        }
        spur_acp::SessionUpdate::ToolCallUpdate(tcu) => {
            let output = tcu
                .fields
                .raw_output
                .clone()
                .unwrap_or(serde_json::Value::Null);
            let text = format_observe_output(&output);
            self.react_trace.push(TraceEntry {
                kind: TraceKind::Observe,
                text,
                timestamp: Self::now_stamp(),
            });
        }
        spur_acp::SessionUpdate::Plan(plan) => {
            let text = plan
                .entries
                .iter()
                .map(|e| {
                    let marker = match &e.status {
                        spur_acp::PlanEntryStatus::Completed => "[x]",
                        spur_acp::PlanEntryStatus::InProgress => "[~]",
                        spur_acp::PlanEntryStatus::Pending => "[ ]",
                        _ => "[ ]",
                    };
                    format!("{} {}", marker, e.content)
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.react_trace.push(TraceEntry {
                kind: TraceKind::Think,
                text,
                timestamp: Self::now_stamp(),
            });
        }
        _ => {}
    }
}
```

- [ ] **Step 3: Remove old SessionEvent imports**

Remove `use spur_acp::SessionEvent;` and `use spur_acp::AgentStatus;` if present. The `spur_acp::SessionUpdate` and `spur_acp::ContentBlock` are used qualified in the match arms above.

- [ ] **Step 4: Verify spur-tui compiles**

Run: `cargo check --package spur-tui`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): match on SDK SessionUpdate variants in session detail view"
```

---

### Task 6: Update TUI dashboard to match on SessionUpdate

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Update handle_spur_event match arm**

Replace the `SpurEvent::AgentOutput` match arm in `DashboardView::handle_spur_event`. The current code matches on `SessionEvent` variants. Replace with `SessionUpdate` matches:

```rust
SpurEvent::AgentNotification {
    session,
    notification,
} => {
    let prefix = self.prefix_for_session(&session.0);

    match &notification.update {
        spur_acp::SessionUpdate::AgentThoughtChunk(chunk) => {
            if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
                let trimmed = tc.text.trim();
                if !trimmed.is_empty() {
                    // Accumulate in text_batch for batched flushing
                    let entry = self
                        .text_batch
                        .entry(session.0.clone())
                        .or_insert_with(|| (String::new(), Instant::now()));
                    entry.0.push_str(trimmed);
                    if entry.0.len() > 200 {
                        let mut start = entry.0.len() - 200;
                        while !entry.0.is_char_boundary(start) {
                            start += 1;
                        }
                        entry.0 = entry.0[start..].to_string();
                    }
                    entry.1 = Instant::now();
                }
            }
        }
        spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
            if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
                let trimmed = tc.text.trim();
                if !trimmed.is_empty() {
                    let entry = self
                        .text_batch
                        .entry(session.0.clone())
                        .or_insert_with(|| (String::new(), Instant::now()));
                    entry.0.push_str(trimmed);
                    if entry.0.len() > 200 {
                        let mut start = entry.0.len() - 200;
                        while !entry.0.is_char_boundary(start) {
                            start += 1;
                        }
                        entry.0 = entry.0[start..].to_string();
                    }
                    entry.1 = Instant::now();
                }
            }
        }
        spur_acp::SessionUpdate::ToolCall(tc) => {
            self.activity_log.push(LogEntry {
                timestamp: Self::now_stamp(),
                prefix,
                message: format!("\u{1f527} Tool: {}", tc.title),
                kind: LogEntryKind::Act,
            });
        }
        spur_acp::SessionUpdate::ToolCallUpdate(_) => {
            // Not logged in dashboard (condensed view)
        }
        _ => {
            // StatusUpdate, Plan, etc. — derive status from variant
            self.set_agent_status_for_session(&session.0, "working");
        }
    }
}
```

Note: The old `SessionEvent::StatusUpdate`, `Error`, `RateLimitHit`, `Complete` arms are removed. These are already handled by separate `SpurEvent` variants (`BrainError`, `RateLimitDetected`, `SessionCompleted`) which the dashboard already processes.

- [ ] **Step 2: Remove old SessionEvent imports**

Remove any `use spur_acp::{SessionEvent, AgentStatus};` imports.

- [ ] **Step 3: Verify full workspace compiles**

Run: `cargo check`
Expected: PASS — all consumers updated.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): match on SDK SessionUpdate variants in dashboard view"
```

---

### Task 7: Final verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo check`
Expected: PASS with no warnings about unused imports.

- [ ] **Step 2: Grep for any remaining SessionEvent references**

Run: `grep -r "SessionEvent" crates/ --include="*.rs"`
Expected: No matches (the type is deleted).

- [ ] **Step 3: Grep for any remaining AgentOutput references**

Run: `grep -r "AgentOutput" crates/ --include="*.rs"`
Expected: No matches (renamed to AgentNotification).

- [ ] **Step 4: Grep for notification_to_session_event references**

Run: `grep -r "notification_to_session_event" crates/ --include="*.rs"`
Expected: No matches (function deleted).

- [ ] **Step 5: Commit any cleanup**

If any stray references were found and fixed:
```bash
git add -A
git commit -m "chore: clean up remaining SessionEvent references"
```
