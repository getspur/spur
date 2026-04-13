# Realtime Streaming Diagnosis — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instrument the kiro-cli → TUI streaming pipeline with six probe sites so a single streaming turn produces enough log data to disambiguate five hypotheses (H1, H1′, H2, H4, H5) about why messages are "not realtime" and "break at the end."

**Architecture:** Add `tracing::debug!`/`tracing::warn!` sites at ACP-callback, tx-swap, orchestrator-emit, TUI-append, broadcast-lag, and per-frame-drain boundaries. Each entry carries `streaming_probe = true` so `rg streaming_probe` yields a clean capture. Zero behavior change — only observation.

**Tech Stack:** Rust + `tracing` crate (already present in workspace). No new dependencies.

**Spec:** [`docs/superpowers/specs/2026-04-13-realtime-streaming-diagnosis-design.md`](../specs/2026-04-13-realtime-streaming-diagnosis-design.md)

**TDD note:** Logging changes have no meaningful unit-test surface. The acceptance check is a runtime smoke test (Task 8): build, run one turn, confirm expected probe rows appear in the log.

---

## File Map

| File | Change | Responsibility |
|---|---|---|
| `crates/spur-tui/src/components/react_trace.rs` | Modify | Add `last_entry_kind_name()` accessor for probe (D) |
| `crates/spur-acp/src/connection/native.rs` | Modify | Probes (A) and (B): session_notification log + dead-tx swap log |
| `crates/spur-core/src/orchestrator.rs` | Modify | Probe (C): emit log with `since_prompt_ms` |
| `crates/spur-tui/src/views/session_detail.rs` | Modify | Probe (D): pre-`append_message` log |
| `crates/spur-tui/src/app.rs` | Modify | Probe (E): lag warn + Probe (F): per-frame drain count |

Helper additions kept minimal. No new modules, no new types.

---

### Task 1: Add `last_entry_kind_name()` helper on `ReactTrace`

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs` (near other public methods on `ReactTrace`)

This method is consumed only by probe (D) to log which trace-entry kind was last, so we can detect when a non-message entry is about to force a new `AgentMessage` block (H2).

- [ ] **Step 1: Add the accessor**

Add this method to the `impl ReactTrace { … }` block, placed immediately before the existing `pub fn append_think` definition (so related helpers stay together):

```rust
/// Return a short kind name for the most recent entry, or `None` if empty.
///
/// Used by diagnostic logging to detect when a trace entry of a different
/// kind sits between successive `AgentMessageChunk`s — which forces
/// `append_message` to push a new block instead of continuing the previous
/// one.
pub fn last_entry_kind_name(&self) -> Option<&'static str> {
    self.entries.last().map(|e| match &e.kind {
        TraceKind::Think => "think",
        TraceKind::AgentMessage { .. } => "agent_message",
        TraceKind::Act { .. } => "act",
        TraceKind::Observe => "observe",
        TraceKind::Delegate { .. } => "delegate",
        TraceKind::UserMessage => "user_message",
        TraceKind::Permission { .. } => "permission",
    })
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p spur-tui`
Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/react_trace.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): add ReactTrace::last_entry_kind_name for diagnostics

Exposes the discriminant of the most recent trace entry as a short static
string. Consumed by the streaming-diagnosis probe in session_detail to
detect interleave-induced message-entry splits.
EOF
)"
```

---

### Task 2: Probe (A) — log ACP `session_notification` arrivals with `send_result`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs` (`SpurAcpClientDynamic::session_notification`, lines ~775–781)

Current code:

```rust
async fn session_notification(
    &self,
    args: SessionNotification,
) -> agent_client_protocol::Result<()> {
    let _ = self.notification_tx.borrow().send(args);
    Ok(())
}
```

This silently drops send errors — a key H5 signal. Replace with a version that logs variant, text length, and whether the send succeeded.

- [ ] **Step 1: Add the variant-to-string + text-len helpers at module scope**

In `crates/spur-acp/src/connection/native.rs`, near the bottom of the file (after `impl Client for SpurAcpClientDynamic`), add:

```rust
/// Short static name for each SessionUpdate discriminant.
/// Used by diagnostic logging only; keep lowercase snake_case.
fn session_update_variant_name(u: &agent_client_protocol::SessionUpdate) -> &'static str {
    use agent_client_protocol::SessionUpdate::*;
    match u {
        AgentThoughtChunk(_) => "agent_thought_chunk",
        AgentMessageChunk(_) => "agent_message_chunk",
        UserMessageChunk(_) => "user_message_chunk",
        ToolCall(_) => "tool_call",
        ToolCallUpdate(_) => "tool_call_update",
        Plan(_) => "plan",
        AvailableCommandsUpdate(_) => "available_commands_update",
        CurrentModeUpdate(_) => "current_mode_update",
        _ => "other",
    }
}

/// Return the text length of a content chunk, or 0 if non-text.
fn content_chunk_text_len(chunk: &agent_client_protocol::ContentChunk) -> usize {
    match &chunk.content {
        agent_client_protocol::ContentBlock::Text(tc) => tc.text.len(),
        _ => 0,
    }
}
```

> **Note:** `_ => "other"` is required because `SessionUpdate` is non-exhaustive
> in the ACP crate. If the compiler complains that any listed variant doesn't
> exist, delete that arm — the `_` catches it.

- [ ] **Step 2: Replace the `session_notification` body**

Locate the existing method (around line 775) and replace its body:

```rust
async fn session_notification(
    &self,
    args: SessionNotification,
) -> agent_client_protocol::Result<()> {
    let variant = session_update_variant_name(&args.update);
    let text_len = match &args.update {
        agent_client_protocol::SessionUpdate::AgentMessageChunk(c)
        | agent_client_protocol::SessionUpdate::AgentThoughtChunk(c)
        | agent_client_protocol::SessionUpdate::UserMessageChunk(c) => {
            content_chunk_text_len(c)
        }
        _ => 0,
    };
    let session = args.session_id.to_string();
    let send_result = self.notification_tx.borrow().send(args);
    let send_result_str = if send_result.is_ok() { "ok" } else { "err" };
    tracing::debug!(
        streaming_probe = true,
        site = "A_session_notification",
        variant = variant,
        text_len = text_len,
        session = %session,
        send_result = send_result_str,
        "ACP session_notification"
    );
    Ok(())
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p spur-acp`
Expected: compiles cleanly. If the `SessionUpdate` match complains about a missing variant, delete the extra arm (the `_ => "other"` catches it).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "$(cat <<'EOF'
debug(spur-acp): log session_notification arrivals with send_result

Probe (A) of the streaming diagnosis. Replaces the swallowed-error forward
at native.rs:779 with a tracing::debug call that records variant, text
length, session id, and whether the send to the notification mpsc
succeeded. A send_result="err" row is the direct signal for H5 (dead-tx
race losing trailing notifications).
EOF
)"
```

---

### Task 3: Probe (B) — log `dead_tx` swap sites

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs` (~lines 628–633 and 689–691)

Two sites replace the per-command notification sender with a dead one: the end of a prompt, and the end of a `load_session` call. Both are racy relative to trailing `session_notification` callbacks. Log them so probe (A) arrivals can be timestamp-compared against the swap.

- [ ] **Step 1: Log the prompt-end swap**

Locate the existing lines at ~632-633:

```rust
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

Replace with:

```rust
tracing::debug!(
    streaming_probe = true,
    site = "B_dead_tx_swap",
    which = "prompt_end",
    agent = %agent_name_prompt,
    "notification_tx → dead_tx (prompt returned)"
);
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

- [ ] **Step 2: Log the load_session-end swap**

Locate the existing lines at ~690-691:

```rust
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

Replace with:

```rust
tracing::debug!(
    streaming_probe = true,
    site = "B_dead_tx_swap",
    which = "load_session_end",
    agent = %agent_name_load,
    "notification_tx → dead_tx (load_session returned)"
);
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

- [ ] **Step 3: Build**

Run: `cargo build -p spur-acp`
Expected: compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "$(cat <<'EOF'
debug(spur-acp): log dead_tx swap at prompt and load_session ends

Probe (B) of the streaming diagnosis. Emits a tracing::debug line at both
sites where the per-command notification sender is replaced by a dead one.
Comparing these timestamps against probe (A) arrivals detects
session_notification callbacks that land on the dead channel (H5).
EOF
)"
```

---

### Task 4: Probe (C) — log orchestrator emit with `since_prompt_ms`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (streaming loop ~line 497–527)

The orchestrator pulls notifications from `stream` and emits `SpurEvent::AgentNotification`. We want per-emit cadence relative to prompt submission time.

- [ ] **Step 1: Capture prompt submission time and emit with elapsed**

Find the block around line 497 that starts:

```rust
                    let mut stream = match b.connection.prompt(prompt_request).await {
                        Ok(s) => s,
                        Err(e) => {
```

Immediately **above** the `let mut stream = …` line, insert the timestamp anchor:

```rust
                    let prompt_started_at = std::time::Instant::now();
```

Then replace the existing inner emit block (currently lines 517–527):

```rust
                            item = stream.next() => {
                                match item {
                                    Some(notification) => {
                                        self.emit(SpurEvent::AgentNotification {
                                            session: b.spur_session_id.clone(),
                                            notification,
                                        });
                                    }
                                    None => break, // Turn complete
                                }
                            }
```

with this version that logs variant + text_len + since_prompt_ms before emitting:

```rust
                            item = stream.next() => {
                                match item {
                                    Some(notification) => {
                                        let variant = match &notification.update {
                                            spur_acp::SessionUpdate::AgentThoughtChunk(_) => "agent_thought_chunk",
                                            spur_acp::SessionUpdate::AgentMessageChunk(_) => "agent_message_chunk",
                                            spur_acp::SessionUpdate::UserMessageChunk(_) => "user_message_chunk",
                                            spur_acp::SessionUpdate::ToolCall(_) => "tool_call",
                                            spur_acp::SessionUpdate::ToolCallUpdate(_) => "tool_call_update",
                                            spur_acp::SessionUpdate::Plan(_) => "plan",
                                            _ => "other",
                                        };
                                        let text_len = match &notification.update {
                                            spur_acp::SessionUpdate::AgentMessageChunk(c)
                                            | spur_acp::SessionUpdate::AgentThoughtChunk(c)
                                            | spur_acp::SessionUpdate::UserMessageChunk(c) => {
                                                match &c.content {
                                                    spur_acp::ContentBlock::Text(tc) => tc.text.len(),
                                                    _ => 0,
                                                }
                                            }
                                            _ => 0,
                                        };
                                        tracing::debug!(
                                            streaming_probe = true,
                                            site = "C_orchestrator_emit",
                                            variant = variant,
                                            text_len = text_len,
                                            since_prompt_ms = prompt_started_at.elapsed().as_millis() as u64,
                                            "orchestrator emitting AgentNotification"
                                        );
                                        self.emit(SpurEvent::AgentNotification {
                                            session: b.spur_session_id.clone(),
                                            notification,
                                        });
                                    }
                                    None => break, // Turn complete
                                }
                            }
```

- [ ] **Step 2: Build**

Run: `cargo build -p spur-core`
Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "$(cat <<'EOF'
debug(spur-core): log per-emit cadence relative to prompt start

Probe (C) of the streaming diagnosis. Captures prompt_started_at before
the streaming loop and logs variant, text_len, and since_prompt_ms for
each emitted AgentNotification. Inter-arrival deltas derived from this
field answer the H1 "coarse kiro cadence" question.
EOF
)"
```

---

### Task 5: Probe (D) — log trace append with previous-entry-kind

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs` (`AgentMessageChunk` branch, lines ~293–299)

- [ ] **Step 1: Replace the `AgentMessageChunk` branch body**

Locate the existing block at lines 293–299:

```rust
                    spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let Some(text) = extract_text(chunk) {
                            if !text.is_empty() {
                                self.react_trace.append_message(text, &self.agent_name, Self::now_stamp());
                            }
                        }
                    }
```

Replace with:

```rust
                    spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let Some(text) = extract_text(chunk) {
                            if !text.is_empty() {
                                let prev_kind = self
                                    .react_trace
                                    .last_entry_kind_name()
                                    .unwrap_or("none");
                                let will_continue = prev_kind == "agent_message";
                                tracing::debug!(
                                    streaming_probe = true,
                                    site = "D_trace_append",
                                    text_len = text.len(),
                                    prev_entry_kind = prev_kind,
                                    will_continue = will_continue,
                                    "about to append_message"
                                );
                                self.react_trace.append_message(text, &self.agent_name, Self::now_stamp());
                            }
                        }
                    }
```

- [ ] **Step 2: Build**

Run: `cargo build -p spur-tui`
Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "$(cat <<'EOF'
debug(spur-tui): log pre-append_message with prev entry kind

Probe (D) of the streaming diagnosis. Before each append_message call for
an AgentMessageChunk, logs the text length, the kind of the previous
trace entry, and will_continue (whether append will concatenate vs push).
Any will_continue=false row after the first chunk of a turn confirms H2
(interleave-induced entry splits → visible "break at end").
EOF
)"
```

---

### Task 6: Probe (E) — warn on broadcast `Lagged`

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (main loop, ~line 532)

- [ ] **Step 1: Replace the silent-swallow arm**

Locate the existing lines in the `tokio::select!` block of `run_tui`:

```rust
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
```

Replace with:

```rust
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            streaming_probe = true,
                            site = "E_broadcast_lag",
                            lagged_n = n,
                            "TUI broadcast receiver lagged — events dropped"
                        );
                    }
```

- [ ] **Step 2: Build**

Run: `cargo build -p spur-tui`
Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "$(cat <<'EOF'
debug(spur-tui): warn on broadcast receiver Lagged

Probe (E) of the streaming diagnosis. The TUI's broadcast receiver
previously swallowed Lagged(n) events silently, which would hide event
loss under burst load. Emits a tracing::warn line so H4 is visible in a
single streaming capture.
EOF
)"
```

---

### Task 7: Probe (F) — log per-frame drain count

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (main loop, lines ~550–573)

This probe detects H1′ (drain-then-render coalescing of chunk bursts).

- [ ] **Step 1: Count drained events in phases 2 and 3**

In `run_tui`, locate the current Phase 2 (crossterm drain), Phase 3 (spur event drain), and Phase 4 (render) block. The current code:

```rust
        // Phase 2: Drain all remaining crossterm events (non-blocking).
        // This collapses bursts of mouse scroll events into one render pass.
        loop {
            match timeout(Duration::ZERO, event_stream.next()).await {
                Ok(Some(Ok(ev))) => app.handle_crossterm_event(ev),
                _ => break,
            }
        }

        // Phase 3: Drain all remaining spur events (non-blocking).
        loop {
            match event_rx.try_recv() {
                Ok(spur_event) => app.handle_spur_event(spur_event),
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }

        // Phase 4: Single render pass.
        if app.dirty {
            terminal.draw(|f| app.render(f))?;
            app.dirty = false;
        }
```

Replace with a counted version. The `Phase 1` select arm above it already consumed one event; count it as the initial `1`. Drain counts accumulate into `spur_drained`:

```rust
        // Count how many events feed into each render. H1' detection.
        let mut spur_drained: u32 = 0;
        let mut crossterm_drained: u32 = 0;

        // Phase 2: Drain all remaining crossterm events (non-blocking).
        loop {
            match timeout(Duration::ZERO, event_stream.next()).await {
                Ok(Some(Ok(ev))) => {
                    crossterm_drained += 1;
                    app.handle_crossterm_event(ev);
                }
                _ => break,
            }
        }

        // Phase 3: Drain all remaining spur events (non-blocking).
        loop {
            match event_rx.try_recv() {
                Ok(spur_event) => {
                    spur_drained += 1;
                    app.handle_spur_event(spur_event);
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!(
                        streaming_probe = true,
                        site = "E_broadcast_lag",
                        lagged_n = n,
                        "TUI broadcast receiver lagged (drain phase) — events dropped"
                    );
                    continue;
                }
                Err(_) => break,
            }
        }

        // Phase 4: Single render pass.
        if app.dirty {
            if spur_drained > 0 || crossterm_drained > 0 {
                tracing::debug!(
                    streaming_probe = true,
                    site = "F_frame_drain",
                    spur_drained = spur_drained,
                    crossterm_drained = crossterm_drained,
                    "rendering frame"
                );
            }
            terminal.draw(|f| app.render(f))?;
            app.dirty = false;
        }
```

> **Note:** We only log when at least one event was drained so idle-tick renders don't spam the log. The Phase-3 `Lagged` arm was previously `continue` silently; replacing it here mirrors the warn added in Task 6 so drain-phase lag is also visible.

- [ ] **Step 2: Build**

Run: `cargo build -p spur-tui`
Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "$(cat <<'EOF'
debug(spur-tui): log per-frame drain count for H1' detection

Probe (F) of the streaming diagnosis. Counts crossterm and spur events
drained in phases 2-3 of the main loop; logs the counts when any frame
renders with non-zero drain. Regular spur_drained >5 during streaming
confirms H1' (drain-then-render coalescing makes token bursts look
chunky). Also upgrades the in-drain Lagged arm from silent continue to
warn.
EOF
)"
```

---

### Task 8: Full workspace build + runtime smoke test

**Files:** none (verification only)

This task is the acceptance gate per the spec.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: compiles cleanly, no new warnings introduced by the probe code.

- [ ] **Step 2: Launch spur-cli and send one message to a kiro session**

Run (terminal 1):

```bash
SPUR_LOG=debug cargo run -p spur-cli -- run 2> /tmp/spur-probe.log
```

Then in the TUI, send a single prompt to the kiro brain (e.g., "Say hello in three sentences."). Wait for the full reply to render. Press `q` or `Ctrl+C` to exit cleanly.

- [ ] **Step 3: Filter the log**

Run:

```bash
rg 'streaming_probe' /tmp/spur-probe.log | head -80
```

Expected: at minimum, these rows appear for one streaming turn:
- Multiple `site="A_session_notification"` rows — one per notification kiro sent, each with `send_result="ok"` or `"err"`.
- Exactly one `site="B_dead_tx_swap"` row with `which="prompt_end"` after the last `A` row of the turn.
- Matching `site="C_orchestrator_emit"` rows, each with a monotonically increasing `since_prompt_ms`.
- `site="D_trace_append"` rows, each with `prev_entry_kind` and `will_continue`.
- `site="F_frame_drain"` rows with `spur_drained > 0`.
- Optionally `site="E_broadcast_lag"` (only if burst load occurred).

- [ ] **Step 4: Confirm no behavior regression**

In the TUI during step 2, confirm:
- The assistant message renders as before (same text, same formatting).
- No visible stuttering, no new scroll issues.

- [ ] **Step 5: Final commit of the log-filter runbook**

Nothing new to commit; all probe commits already landed in Tasks 1-7. If the workspace build in Step 1 produced any automatic `Cargo.lock` update, include it in a small follow-up:

```bash
git status
# If only Cargo.lock changed:
git add Cargo.lock && git commit -m "chore: refresh Cargo.lock after diagnosis probes" || true
```

- [ ] **Step 6: Hand off**

Paste the filtered output of Step 3 (up to 80 lines) back into the parent conversation. The decision-tree table in the spec maps each observation to exactly one follow-up fix; that decision is out of scope for this plan.

---

## Self-Review Notes

- **Spec coverage:** Probes A–F in the spec map 1:1 to Tasks 2, 3, 4, 5, 6, 7. The `last_entry_kind_name` helper (Task 1) is an implementation dependency of probe (D). Task 8 is the acceptance gate from the spec.
- **Placeholder scan:** No TBDs. Every step shows the actual code or command.
- **Type consistency:** `session_update_variant_name` and `content_chunk_text_len` are defined in Task 2 and referenced only within `native.rs`. The orchestrator's inline match in Task 4 duplicates the shape rather than importing those helpers — deliberate, because they live in a different crate and are tiny. `last_entry_kind_name` returns `Option<&'static str>` in Task 1 and is consumed via `.unwrap_or("none")` in Task 5 — consistent.
- **Non-exhaustive match:** The two variant matches (Tasks 2 and 4) both have `_ => "other"` / `_ => 0` fallbacks to survive future ACP crate additions without breaking the build.
