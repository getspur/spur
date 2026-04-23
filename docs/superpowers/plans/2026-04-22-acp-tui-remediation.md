# ACP/TUI Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the validated ACP/TUI remediation spec so the session trace preserves active-work visibility, terminal delegation outcomes, ACP content fidelity, and scroll/state integrity without reopening the disproven F11 race.

**Architecture:** Keep the fix set additive and local to `spur-tui`. The work falls into three seams: session lifecycle state in `SessionDetailView`, ACP-to-trace projection in `react_trace::dispatch`, and trace/cache integrity in `ReactTrace` + non-markdown render code. Follow the spec's priority order exactly: land reachable control/fidelity fixes first, then liveness and representation parity, then the low-confidence depth hardening.

**Tech Stack:** Rust workspace (`spur-tui`, `spur-acp`, `spur-core`), `ratatui`, ACP schema types re-exported by `spur-acp`, targeted `cargo test -p spur-tui` runs, and one new `similar = { version = "2.7", default-features = false }` dependency in `crates/spur-tui/Cargo.toml` for line-aware diff output.

---

## Decisions Locked By This Plan

These were open or implicit in the design spec; this plan resolves them so workers do not invent policy mid-implementation.

1. `F3` trace notes are mandatory for non-success terminal delegation outcomes plus `Modified`. Plain `Success` does **not** emit a main-trace note in this pass; that keeps failures visible without making every successful delegation noisy.
2. `F14` arms `stream_in_flight` on `ToolCall`, `ToolCallUpdate`, and `Plan`, but not on `UsageUpdate` or `CurrentModeUpdate`.
3. `F11` uses a fixed local timeout of **60 seconds** and clears both `stream_in_flight` and `cancelling_in_flight`.
4. `F2` updates the production helper in `react_trace/dispatch.rs` and the mirrored test-only helper in `session_detail.rs` so tests keep matching the real renderer.
5. `F5` is implemented by introducing per-entry grouping for the non-markdown render path instead of trying to reverse-engineer `entry_row_starts` from already-flattened wrapped lines.

## File Structure

Files touched by this plan:

- `crates/spur-tui/src/views/session_detail.rs`
  - Owns session-scoped lifecycle state (`stream_in_flight`, `cancelling_in_flight`, `tool_depth`), delegation event handling, and the per-view `tick()` hook.
  - Tasks P1, P2, P6, and P9 touch this file.
- `crates/spur-tui/src/components/react_trace/dispatch.rs`
  - ACP projection layer from `SessionUpdate` into `TraceEntry`.
  - Tasks P3, P5, and P7 touch this file.
- `crates/spur-tui/src/components/react_trace/mod.rs`
  - Owns `ReactTrace`, entry mutation rules, dirty-cache tracking, and test helpers.
  - Tasks P4 and P9 touch this file; P8 may add a small helper if grouping lives here instead of `builder.rs`.
- `crates/spur-tui/src/components/react_trace/builder.rs`
  - Owns flat display-line construction from trace entries.
  - Task P8 touches this file to introduce per-entry grouping for non-markdown scroll metadata.
- `crates/spur-tui/src/components/react_trace/render.rs`
  - Owns `LineCacheEntry` / `VirtualRowCacheEntry` and full-render cache population.
  - Task P8 touches this file.
- `crates/spur-tui/Cargo.toml`
  - Only Task P7 touches this file to add `similar`.

Existing test surfaces to extend instead of inventing new harnesses:

- `crates/spur-tui/src/views/session_detail.rs` already has focused unit tests and helper constructors around line `2164`.
- `crates/spur-tui/src/components/react_trace/mod.rs` already has markdown/state tests and row/anchor helpers.
- `crates/spur-tui/tests/delegation_status_rendering.rs` already exercises delegation terminal wording in the dashboard.
- `crates/spur-tui/tests/stream_tab_parity.rs` already exercises tool-call lifecycle parity and can stay as a regression backstop after P5/P7.

Out of scope for this plan:

- Rich-media trace rendering beyond fail-visible placeholders.
- ACP protocol changes (`DelegationCompleted` still has no `request_id`).
- Architectural follow-ons from Phase 3 of the spec.

## Execution Order / Constraints

1. Run tasks in order. `session_detail.rs` and `dispatch.rs` are both hot files; parallel edits will fight.
2. Keep TDD strict: failing test commit first, then fix commit.
3. Use narrow test filters during red/green, then run `cargo test -p spur-tui` after the last task in each phase.
4. For P8, use `--no-default-features` because the bug only exists in the non-markdown render path.

---

### Task P1: Arm `stream_in_flight` for tool-first and plan-first turns (`F14`)

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:1357-1371`
- Modify: `crates/spur-tui/src/views/session_detail.rs:2164-2268`

- [ ] **Step 1: Write the failing tests**

Add session-event helpers plus three red tests in the existing `#[cfg(test)]` block:

```rust
fn tool_call_event(session: &spur_acp::SessionId) -> SpurEvent {
    let update = spur_acp::SessionUpdate::ToolCall(spur_acp::AcpToolCall::new(
        spur_acp::ToolCallId::new("tc-prefix"),
        "read_file",
    ));
    let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: session.clone(),
        notification: Box::new(notification),
    })
}

fn plan_event(session: &spur_acp::SessionId) -> SpurEvent {
    let plan = spur_acp::Plan::new(vec![spur_acp::PlanEntry::new(
        "Scan the workspace",
        spur_acp::PlanEntryPriority::High,
        spur_acp::PlanEntryStatus::InProgress,
    )]);
    let notification = spur_acp::SessionNotification::new(
        session.0.clone(),
        spur_acp::SessionUpdate::Plan(plan),
    );
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: session.clone(),
        notification: Box::new(notification),
    })
}

#[test]
fn tool_call_sets_stream_in_flight() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.handle_spur_event(&tool_call_event(&sid), &test_ctx());
    assert!(v.stream_in_flight);
}

#[test]
fn plan_update_sets_stream_in_flight() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.handle_spur_event(&plan_event(&sid), &test_ctx());
    assert!(v.stream_in_flight);
}

#[test]
fn esc_after_tool_first_update_emits_cancel_stream() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
    v.handle_spur_event(&tool_call_event(&sid), &test_ctx());

    let action = <SessionDetailView as crate::views::View>::handle_key(
        &mut v,
        press(KeyCode::Esc),
        &test_ctx(),
    );

    assert!(matches!(action, Some(Action::CancelStream { .. })));
    assert!(v.cancelling_in_flight);
}
```

- [ ] **Step 2: Run the red tests**

Run:

```bash
cargo test -p spur-tui tool_call_sets_stream_in_flight
cargo test -p spur-tui plan_update_sets_stream_in_flight
cargo test -p spur-tui esc_after_tool_first_update_emits_cancel_stream
```

Expected: the first two fail because `stream_in_flight` is only armed on message/thought chunks; the third fails for the same reason.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "test(spur-tui): P1 cover tool-plan stream arming"
```

- [ ] **Step 4: Implement the minimal fix**

Expand the arming match in `handle_spur_event`:

```rust
match &notification.update {
    spur_acp::SessionUpdate::AgentThoughtChunk(_)
    | spur_acp::SessionUpdate::AgentMessageChunk(_)
    | spur_acp::SessionUpdate::ToolCall(_)
    | spur_acp::SessionUpdate::ToolCallUpdate(_)
    | spur_acp::SessionUpdate::Plan(_) => {
        self.stream_in_flight = true;
    }
    _ => {}
}
```

Keep `UsageUpdate` and `CurrentModeUpdate` excluded.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui tool_call_sets_stream_in_flight
cargo test -p spur-tui plan_update_sets_stream_in_flight
cargo test -p spur-tui esc_after_tool_first_update_emits_cancel_stream
cargo test -p spur-tui chunk_sets_stream_in_flight
```

Expected: all four pass.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "fix(spur-tui): P1 arm stream on tool and plan updates"
```

---

### Task P2: Surface non-success delegation terminals in the main trace (`F3`)

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:1389-1450`
- Modify: `crates/spur-tui/src/views/session_detail.rs:2164-2268`

- [ ] **Step 1: Write the failing tests**

Add delegation helpers and three red tests in `session_detail.rs`:

```rust
fn delegation_requested_event(session: &spur_acp::SessionId) -> SpurEvent {
    SpurEvent::now(SpurEventBody::DelegationRequested {
        from: session.clone(),
        to_agent: "claude-code".to_string(),
        task: "inspect the diff".to_string(),
        request_id: "req-1".to_string(),
        delegation_plan: None,
        issue_id: None,
    })
}

fn delegation_dispatched_event(session: &spur_acp::SessionId) -> SpurEvent {
    SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: session.clone(),
        request_id: "req-1".to_string(),
        executor_id: "worker-1".to_string(),
    })
}

fn delegation_completed_event(status: spur_acp::DelegationStatus) -> SpurEvent {
    SpurEvent::now(SpurEventBody::DelegationCompleted {
        worker_session: spur_acp::SessionId("worker-1".to_string()),
        status,
    })
}

#[test]
fn delegation_failed_appends_main_trace_note() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.handle_spur_event(&delegation_requested_event(&sid), &test_ctx());
    v.handle_spur_event(
        &delegation_completed_event(spur_acp::DelegationStatus::Failed {
            error: "worker crashed".to_string(),
        }),
        &test_ctx(),
    );

    let last = v.react_trace().entries().last().expect("trace entry");
    assert!(matches!(last.kind, crate::components::react_trace::TraceKind::Observe { .. }));
    assert!(last.text.contains("worker crashed"));
}

#[test]
fn delegation_completed_uses_executor_correlation_when_present() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.handle_spur_event(&delegation_requested_event(&sid), &test_ctx());
    v.handle_spur_event(&delegation_dispatched_event(&sid), &test_ctx());
    v.handle_spur_event(
        &delegation_completed_event(spur_acp::DelegationStatus::Rejected {
            reason: "scope too large".to_string(),
        }),
        &test_ctx(),
    );

    let last = v.react_trace().entries().last().expect("trace entry");
    assert!(last.text.contains("claude-code"));
    assert!(last.text.contains("scope too large"));
}

#[test]
fn delegation_completed_without_dispatch_still_emits_session_note() {
    let mut v = make_view();
    let sid = v.session_id().clone();
    v.handle_spur_event(&delegation_requested_event(&sid), &test_ctx());
    v.handle_spur_event(
        &SpurEvent::now(SpurEventBody::DelegationCompleted {
            worker_session: spur_acp::SessionId("pre-spawn".to_string()),
            status: spur_acp::DelegationStatus::TimedOut {
                waited_for: std::time::Duration::from_secs(30),
                fallback: spur_acp::TimeoutFallback::Abandon,
            },
        }),
        &test_ctx(),
    );

    let last = v.react_trace().entries().last().expect("trace entry");
    assert!(last.text.contains("timed out"));
}
```

- [ ] **Step 2: Run the red tests**

Run:

```bash
cargo test -p spur-tui delegation_failed_appends_main_trace_note
cargo test -p spur-tui delegation_completed_uses_executor_correlation_when_present
cargo test -p spur-tui delegation_completed_without_dispatch_still_emits_session_note
```

Expected: all fail because `DelegationCompleted` is still a no-op.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "test(spur-tui): P2 cover delegation terminal notes"
```

- [ ] **Step 4: Implement the minimal fix**

Add a small formatter plus correlated/fallback note emission:

```rust
fn delegation_terminal_note(
    trace: &crate::components::react_trace::ReactTrace,
    worker_session: &spur_acp::SessionId,
    status: &spur_acp::DelegationStatus,
) -> Option<String> {
    let correlated_agent = trace.entries().iter().rev().find_map(|entry| {
        match &entry.kind {
            crate::components::react_trace::TraceKind::Delegate {
                agent,
                executor_id: Some(id),
                ..
            } if id == &worker_session.0 => Some(agent.clone()),
            _ => None,
        }
    });

    let prefix = correlated_agent
        .map(|agent| format!("Delegation to {agent}"))
        .unwrap_or_else(|| "Delegation".to_string());

    match status {
        spur_acp::DelegationStatus::Conflict { files } => {
            Some(format!("{prefix} conflicted: {} file(s)", files.len()))
        }
        spur_acp::DelegationStatus::Modified { reviewer_note } => {
            Some(format!("{prefix} modified: {reviewer_note}"))
        }
        spur_acp::DelegationStatus::Rejected { reason } => {
            Some(format!("{prefix} rejected: {reason}"))
        }
        spur_acp::DelegationStatus::Failed { error } => {
            Some(format!("{prefix} failed: {error}"))
        }
        spur_acp::DelegationStatus::Timeout => {
            Some(format!("{prefix} timed out"))
        }
        spur_acp::DelegationStatus::TimedOut { waited_for, .. } => {
            Some(format!("{prefix} timed out after {}s", waited_for.as_secs()))
        }
        spur_acp::DelegationStatus::Cancelled { reason } => {
            Some(format!("{prefix} cancelled: {reason}"))
        }
        spur_acp::DelegationStatus::Success => None,
        _ => None,
    }
}
```

In the `DelegationCompleted` match arm, append a `TraceEntry { kind: TraceKind::Observe { payload: None }, text: note, ... }` when `delegation_terminal_note(...)` returns `Some`.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui delegation_failed_appends_main_trace_note
cargo test -p spur-tui delegation_completed_uses_executor_correlation_when_present
cargo test -p spur-tui delegation_completed_without_dispatch_still_emits_session_note
cargo test -p spur-tui dashboard_rejected_status_renders_with_reason
```

Expected: the three new session-detail tests pass, and the existing dashboard regression still passes.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "fix(spur-tui): P2 show delegation terminals in trace"
```

---

### Task P3: Render ACP non-text content as placeholders instead of dropping it (`F1` / `F13`)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs:47-255`

- [ ] **Step 1: Write the failing tests**

Add a new `#[cfg(test)] mod tests` to `dispatch.rs` with a local dispatch helper:

```rust
fn dispatch_one(update: spur_acp::SessionUpdate) -> crate::components::react_trace::ReactTrace {
    let mut trace = crate::components::react_trace::ReactTrace::new();
    let mut tool_depth = std::collections::HashMap::new();
    let mut ctx = super::DispatchCtx {
        agent_name: "claude",
        agent_kind: spur_acp::AgentKind::Generic,
        now_stamp: || "10:00".to_string(),
        tool_depth: &mut tool_depth,
    };
    super::dispatch_session_update(&mut trace, &update, &mut ctx);
    trace
}

#[test]
fn user_message_resource_link_becomes_placeholder() {
    let trace = dispatch_one(spur_acp::SessionUpdate::UserMessageChunk(
        spur_acp::ContentChunk::new(spur_acp::ContentBlock::ResourceLink(
            spur_acp::ResourceLink::new("claude-code", "worker://claude-code"),
        )),
    ));
    assert_eq!(trace.entries()[0].text, "[mention: claude-code]");
}

#[test]
fn agent_image_becomes_placeholder() {
    let trace = dispatch_one(spur_acp::SessionUpdate::AgentMessageChunk(
        spur_acp::ContentChunk::new(spur_acp::ContentBlock::Image(
            agent_client_protocol::ImageContent::new("Zm9v", "image/png"),
        )),
    ));
    assert!(trace.render_to_strings().join("\n").contains("[image omitted]"));
}

#[test]
fn agent_audio_becomes_placeholder() {
    let trace = dispatch_one(spur_acp::SessionUpdate::AgentMessageChunk(
        spur_acp::ContentChunk::new(spur_acp::ContentBlock::Audio(
            agent_client_protocol::AudioContent::new("YmFy", "audio/wav"),
        )),
    ));
    assert!(trace.render_to_strings().join("\n").contains("[audio omitted]"));
}

#[test]
fn agent_embedded_resource_becomes_placeholder() {
    let resource = agent_client_protocol::EmbeddedResource::new(
        agent_client_protocol::EmbeddedResourceResource::TextResourceContents(
            agent_client_protocol::TextResourceContents::new(
                "let x = 1;",
                "file:///tmp/x.rs",
            ),
        ),
    );
    let trace = dispatch_one(spur_acp::SessionUpdate::AgentMessageChunk(
        spur_acp::ContentChunk::new(spur_acp::ContentBlock::Resource(resource)),
    ));
    assert!(trace.render_to_strings().join("\n").contains("[resource omitted]"));
}
```

- [ ] **Step 2: Run the red tests**

Run:

```bash
cargo test -p spur-tui user_message_resource_link_becomes_placeholder
cargo test -p spur-tui agent_image_becomes_placeholder
cargo test -p spur-tui agent_audio_becomes_placeholder
cargo test -p spur-tui agent_embedded_resource_becomes_placeholder
```

Expected: all fail because `extract_text()` and `UserMessageChunk` only handle `Text`.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs
git commit -m "test(spur-tui): P3 cover ACP content placeholders"
```

- [ ] **Step 4: Implement the minimal fix**

Introduce one shared renderer and use it in message, user-message, and tool-call content paths:

```rust
fn render_content_block(block: &spur_acp::ContentBlock) -> Option<String> {
    match block {
        spur_acp::ContentBlock::Text(tc) => Some(tc.text.clone()),
        spur_acp::ContentBlock::ResourceLink(link) => {
            Some(format!("[mention: {}]", link.name))
        }
        spur_acp::ContentBlock::Image(_) => Some("[image omitted]".to_string()),
        spur_acp::ContentBlock::Audio(_) => Some("[audio omitted]".to_string()),
        spur_acp::ContentBlock::Resource(_) => Some("[resource omitted]".to_string()),
        _ => None,
    }
}

fn extract_text(chunk: &ContentChunk) -> Option<String> {
    render_content_block(&chunk.content)
}
```

Then update:

```rust
if let Some(text) = extract_text(chunk) {
    trace.append_message(&text, ctx.agent_name, (ctx.now_stamp)());
}

if let Some(text) = render_content_block(&chunk.content) {
    trace.append_user_message(&text, (ctx.now_stamp)());
}
```

And in `extract_tool_call_text`, replace the `Content(cb)` arm with `if let Some(text) = render_content_block(cb) { out.push_str(&text); }`.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui user_message_resource_link_becomes_placeholder
cargo test -p spur-tui agent_image_becomes_placeholder
cargo test -p spur-tui agent_audio_becomes_placeholder
cargo test -p spur-tui agent_embedded_resource_becomes_placeholder
cargo test -p spur-tui worker_mention_send_path
```

Expected: all new tests pass, and the outbound mention send-path tests still pass.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs
git commit -m "fix(spur-tui): P3 preserve ACP non-text placeholders"
```

---

### Task P4: Keep markdown `entry.text` in sync with the stream raw text (`F9`)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:389-429`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:1232-1417`

- [ ] **Step 1: Write the failing test**

Add a markdown-path regression next to the existing append-message tests:

```rust
#[cfg(feature = "markdown")]
#[test]
fn append_message_keeps_text_mirror_in_sync() {
    let mut trace = ReactTrace::new();
    trace.append_message("hello", "claude", "10:00:01".to_string());
    assert_eq!(trace.entries_for_test()[0].text, "hello");

    trace.append_message(" world", "claude", "10:00:02".to_string());
    assert_eq!(trace.entries_for_test()[0].text, "hello world");
}
```

- [ ] **Step 2: Run the red test**

Run:

```bash
cargo test -p spur-tui append_message_keeps_text_mirror_in_sync
```

Expected: fail on the first assertion because the markdown path currently leaves `text` empty.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "test(spur-tui): P4 cover markdown text mirror"
```

- [ ] **Step 4: Implement the minimal fix**

Update the markdown append path in `append_message`:

```rust
if let Some(stream) = entry.markdown.as_mut() {
    stream.append(text);
    entry.text = stream.raw_text().to_string();
    self.mark_dirty_from(idx);
    return;
}
```

Keep the non-markdown path unchanged.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui append_message_keeps_text_mirror_in_sync
cargo test -p spur-tui append_message_merges_consecutive_chunks_from_same_agent
cargo test -p spur-tui first_chunk_renders_body_before_debounce_flush
```

Expected: all pass.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "fix(spur-tui): P4 mirror markdown raw text"
```

---

### Task P5: Make synthesized tool calls idempotent when the canonical `ToolCall` arrives (`F12`)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs:69-152`

- [ ] **Step 1: Write the failing test**

Extend the `dispatch.rs` unit-test module from P3:

```rust
#[test]
fn canonical_tool_call_reuses_synthesized_act() {
    let mut trace = crate::components::react_trace::ReactTrace::new();
    let mut tool_depth = std::collections::HashMap::new();
    let mut ctx = super::DispatchCtx {
        agent_name: "claude",
        agent_kind: spur_acp::AgentKind::Generic,
        now_stamp: || "10:00".to_string(),
        tool_depth: &mut tool_depth,
    };

    let id = spur_acp::ToolCallId::new("tc-merge");
    let update = spur_acp::AcpToolCallUpdate::new(
        id.clone(),
        agent_client_protocol::ToolCallUpdateFields::new()
            .title("read_file")
            .status(spur_acp::ToolCallStatus::InProgress),
    );
    let call = spur_acp::AcpToolCall::new(id.clone(), "read_file");
    let done = spur_acp::AcpToolCallUpdate::new(
        id.clone(),
        agent_client_protocol::ToolCallUpdateFields::new()
            .status(spur_acp::ToolCallStatus::Completed),
    );

    super::dispatch_session_update(&mut trace, &spur_acp::SessionUpdate::ToolCallUpdate(update), &mut ctx);
    super::dispatch_session_update(&mut trace, &spur_acp::SessionUpdate::ToolCall(call), &mut ctx);
    super::dispatch_session_update(&mut trace, &spur_acp::SessionUpdate::ToolCallUpdate(done), &mut ctx);

    let acts: Vec<_> = trace.entries().iter().filter(|entry| {
        matches!(
            &entry.kind,
            crate::components::react_trace::TraceKind::Act {
                tool_call_id: Some(existing),
                ..
            } if existing.0.as_ref() == "tc-merge"
        )
    }).collect();

    assert_eq!(acts.len(), 1, "one tool_call_id must map to one Act");
}
```

- [ ] **Step 2: Run the red test**

Run:

```bash
cargo test -p spur-tui canonical_tool_call_reuses_synthesized_act
```

Expected: fail with `acts.len() == 2`.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs
git commit -m "test(spur-tui): P5 cover synth tool-call dedupe"
```

- [ ] **Step 4: Implement the minimal fix**

Change the `SessionUpdate::ToolCall(tc)` arm to merge into an existing `Act` first:

```rust
if let Some((idx, existing)) = trace.find_act_by_id_mut(&tc.tool_call_id) {
    if let TraceKind::Act {
        tool,
        family,
        input,
        status,
        ..
    } = &mut existing.kind
    {
        *tool = tool_name;
        *family = family_kind;
        *input = input_display;
        *status = merge_status(
            status,
            Some(tc.status),
            tc.raw_output.as_ref(),
            ctx.agent_kind,
        );
    }
    if existing.text.is_empty() {
        existing.text = fallback_text;
    }
    trace.mark_dirty_from_for_update(idx);
} else {
    trace.push(/* existing Act creation */);
}
```

Preserve the existing timestamp and `tool_call_id`; do not push a second entry.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui canonical_tool_call_reuses_synthesized_act
cargo test -p spur-tui new_path_covers_old_kinds_and_adds_lifecycle
```

Expected: both pass.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs
git commit -m "fix(spur-tui): P5 dedupe synthesized tool calls"
```

---

### Task P6: Repair stale local streaming state after 60s without progress (`F11`)

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:74-116`
- Modify: `crates/spur-tui/src/views/session_detail.rs:1357-1465`
- Modify: `crates/spur-tui/src/views/session_detail.rs:1616-1640`
- Modify: `crates/spur-tui/src/views/session_detail.rs:2200-2765`

- [ ] **Step 1: Write the failing tests**

Add state/time helpers plus two tests:

```rust
#[cfg(test)]
fn test_set_last_stream_activity(&mut self, at: std::time::Instant) {
    self.last_stream_activity_at = Some(at);
}

#[test]
fn stalled_stream_tick_clears_both_flags_and_adds_note() {
    let mut v = make_view();
    v.stream_in_flight = true;
    v.cancelling_in_flight = true;
    v.test_set_last_stream_activity(
        std::time::Instant::now() - std::time::Duration::from_secs(61),
    );

    v.tick();

    assert!(!v.stream_in_flight);
    assert!(!v.cancelling_in_flight);
    assert!(v.react_trace().last_text().unwrap_or_default().contains("stalled"));
}

#[test]
fn fresh_stream_tick_preserves_flags() {
    let mut v = make_view();
    v.stream_in_flight = true;
    v.cancelling_in_flight = true;
    v.test_set_last_stream_activity(std::time::Instant::now());

    v.tick();

    assert!(v.stream_in_flight);
    assert!(v.cancelling_in_flight);
}
```

- [ ] **Step 2: Run the red tests**

Run:

```bash
cargo test -p spur-tui stalled_stream_tick_clears_both_flags_and_adds_note
cargo test -p spur-tui fresh_stream_tick_preserves_flags
```

Expected: fail because there is no timeout state or timestamp.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "test(spur-tui): P6 cover stale stream timeout repair"
```

- [ ] **Step 4: Implement the minimal fix**

Add a field + constant:

```rust
const STREAM_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

last_stream_activity_at: Option<std::time::Instant>,
```

Initialize/reset it in constructors and `reset_for_clear`, refresh it when the P1 arming match fires, clear it on `TurnComplete`, and repair it inside `tick()`:

```rust
if self.stream_in_flight {
    if let Some(last) = self.last_stream_activity_at {
        if last.elapsed() >= STREAM_STALL_TIMEOUT {
            self.stream_in_flight = false;
            self.cancelling_in_flight = false;
            self.last_stream_activity_at = None;
            self.push_system_note("Stream stalled; cleared local in-flight state".to_string());
        }
    }
}
```

Do **not** synthesize `TurnComplete`.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui stalled_stream_tick_clears_both_flags_and_adds_note
cargo test -p spur-tui fresh_stream_tick_preserves_flags
cargo test -p spur-tui turn_complete_clears_both_flags
```

Expected: all pass.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "fix(spur-tui): P6 clear stalled local stream state"
```

---

### Task P7: Replace fake delete-all/add-all diffs with real line-aware diffs (`F2`)

**Files:**
- Modify: `crates/spur-tui/Cargo.toml:13-40`
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs:186-255`
- Modify: `crates/spur-tui/src/views/session_detail.rs:1893-1975`

- [ ] **Step 1: Write the failing regression tests**

Extend the existing `extract_tool_call_text_tests` in `session_detail.rs` and the `dispatch.rs` test module:

```rust
#[test]
fn extract_tool_call_text_keeps_unchanged_context_lines() {
    use agent_client_protocol::{Diff, ToolCallContent};

    let diff = Diff::new(
        "src/lib.rs",
        "fn keep() {}\nfn new_name() {}\nfn tail() {}\n",
    )
    .old_text("fn keep() {}\nfn old_name() {}\nfn tail() {}\n".to_string());

    let out = extract_tool_call_text(&[ToolCallContent::Diff(diff)]).expect("diff text");

    assert!(out.contains(" fn keep() {}"), "context line missing: {out}");
    assert!(out.contains("-fn old_name() {}"), "delete line missing: {out}");
    assert!(out.contains("+fn new_name() {}"), "insert line missing: {out}");
    assert!(out.contains("@@"), "hunk header missing: {out}");
}
```

This should fail on the current implementation because there are no context lines or hunk headers.

- [ ] **Step 2: Run the red test**

Run:

```bash
cargo test -p spur-tui extract_tool_call_text_keeps_unchanged_context_lines
```

Expected: fail because the current formatter only emits `-old` / `+new`.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/components/react_trace/dispatch.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "test(spur-tui): P7 cover real diff context output"
```

- [ ] **Step 4: Implement the minimal fix**

Add the dependency:

```toml
[dependencies]
similar = { version = "2.7", default-features = false }
```

Then replace the formatter with `similar::TextDiff` in both the production helper and the test-only mirror:

```rust
fn format_diff_truncated(path: &str, old: Option<&str>, new_: &str) -> String {
    let diff = similar::TextDiff::from_lines(old.unwrap_or(""), new_);
    let mut bytes = Vec::new();
    diff.unified_diff()
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_writer(&mut bytes)
        .expect("write unified diff");
    let out = String::from_utf8(bytes).expect("utf8 diff");

    let mut lines: Vec<&str> = out.lines().collect();
    if lines.len() > DIFF_MAX_LINES {
        let truncated = lines.len() - DIFF_MAX_LINES;
        lines.truncate(DIFF_MAX_LINES);
        let trailer = format!("... ({truncated} more lines)");
        return format!("{}\n{}\n", lines.join("\n"), trailer);
    }
    format!("{}\n", lines.join("\n"))
}
```

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui extract_tool_call_text_keeps_unchanged_context_lines
cargo test -p spur-tui extract_tool_call_text_renders_diff_content
cargo test -p spur-tui extract_tool_call_text_truncates_long_diffs
```

Expected: all pass, with the old assertions still satisfied and the new context assertion now green.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/Cargo.toml crates/spur-tui/src/components/react_trace/dispatch.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "fix(spur-tui): P7 render line-aware tool diffs"
```

---

### Task P8: Add per-entry scroll metadata to the non-markdown full-render path (`F5`)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/builder.rs:14-220`
- Modify: `crates/spur-tui/src/components/react_trace/render.rs:21-40,295-340`
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:576-640`

- [ ] **Step 1: Write the failing non-markdown tests**

Add `#[cfg(not(feature = "markdown"))]` tests in `react_trace/mod.rs`:

```rust
#[cfg(not(feature = "markdown"))]
#[test]
fn layout_for_scroll_returns_metadata_for_full_non_markdown() {
    let mut trace = ReactTrace::new();
    trace.append_message("alpha beta gamma delta", "claude", "10:00".into());
    trace.append_message("second entry", "claude", "10:01".into());

    let backend = ratatui::backend::TestBackend::new(24, 8);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| trace.render(frame, frame.area(), None))
        .unwrap();

    let layout = trace.layout_for_scroll();
    assert!(layout.is_some(), "full non-markdown layout must exist");
}

#[cfg(not(feature = "markdown"))]
#[test]
fn page_down_changes_anchor_after_full_non_markdown_render() {
    let mut trace = ReactTrace::new();
    for idx in 0..8 {
        trace.append_message(&format!("entry-{idx} wraps wraps wraps"), "claude", "10:00".into());
    }

    let backend = ratatui::backend::TestBackend::new(20, 6);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| trace.render(frame, frame.area(), None))
        .unwrap();

    let before = trace.anchor_for_tests();
    trace.page_down();
    let after = trace.anchor_for_tests();
    assert_ne!(before, after, "page_down must stop no-oping");
}
```

- [ ] **Step 2: Run the red tests in the non-markdown build**

Run:

```bash
cargo test -p spur-tui --no-default-features layout_for_scroll_returns_metadata_for_full_non_markdown
cargo test -p spur-tui --no-default-features page_down_changes_anchor_after_full_non_markdown_render
```

Expected: fail because `layout_for_scroll()` returns `None` for `Surface::Full` without markdown.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "test(spur-tui): P8 cover non-markdown scroll metadata"
```

- [ ] **Step 4: Implement the minimal fix**

First, refactor the builder so non-markdown full render can keep entry boundaries:

```rust
pub(super) fn build_display_line_groups(
    &self,
    spinner_frame: &str,
    lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
) -> Vec<Vec<Line<'static>>> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i < self.entries.len() {
        let entry = &self.entries[i];
        let mut entry_lines = Vec::new();
        // Copy the existing match arms from build_display_lines, but push
        // into entry_lines instead of the shared lines Vec.
        // At the end of each entry arm:
        groups.push(entry_lines);
        i += 1;
    }
    groups
}

pub(super) fn build_display_lines(...) -> Vec<Line<'static>> {
    self.build_display_line_groups(spinner_frame, lineage)
        .into_iter()
        .flatten()
        .collect()
}
```

Then update the non-markdown cache:

```rust
pub(in crate::components) struct LineCacheEntry {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) entry_row_starts: Vec<usize>,
    pub(super) width: u16,
    pub(super) generation: u64,
}
```

And populate it by wrapping per-entry groups:

```rust
let groups = self.build_display_line_groups(spinner_frame, lineage);
let mut built = Vec::new();
let mut entry_row_starts = Vec::with_capacity(groups.len());
for group in groups {
    entry_row_starts.push(built.len());
    for line in group {
        built.extend(wrap_line_to_width(&line, effective_width));
    }
}
self.line_cache = Some(LineCacheEntry {
    lines: built,
    entry_row_starts,
    width: effective_width,
    generation: self.generation,
});
```

Finally, change `layout_for_scroll()` to return `entry_row_starts` and `lines.len()` for the non-markdown full path.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui --no-default-features layout_for_scroll_returns_metadata_for_full_non_markdown
cargo test -p spur-tui --no-default-features page_down_changes_anchor_after_full_non_markdown_render
cargo test -p spur-tui --no-default-features
```

Expected: the two new tests pass and the non-markdown unit suite stays green.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/components/react_trace/builder.rs crates/spur-tui/src/components/react_trace/render.rs crates/spur-tui/src/components/react_trace/mod.rs
git commit -m "fix(spur-tui): P8 restore non-markdown full scroll metadata"
```

---

### Task P9: Stop blanket-clearing `tool_depth` and keep a bounded recent map (`F4`)

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace/mod.rs:760-860`
- Modify: `crates/spur-tui/src/views/session_detail.rs:1455-1460,2348-2740`

- [ ] **Step 1: Write the failing tests**

Replace the current blanket-clear assertions with bounded-retention assertions:

```rust
#[test]
fn turn_complete_retains_recent_tool_depth_ids() {
    let mut view = make_view();
    view.tool_depth.insert("parent".into(), 0);
    view.tool_depth.insert("child".into(), 1);

    view.react_trace_mut_for_test().push(crate::components::react_trace::TraceEntry {
        kind: crate::components::react_trace::TraceKind::Act {
            tool: "read_file".into(),
            family: spur_acp::adapter::ToolFamily::Unknown,
            input: spur_acp::adapter::ToolInputDisplay::Empty,
            tool_call_id: Some(spur_acp::ToolCallId::new("parent")),
            status: crate::components::react_trace::ActStatus::Completed(None),
        },
        text: String::new(),
        timestamp: "10:00".into(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });

    view.handle_spur_event(&turn_complete_event(&view.session_id().clone()), &test_ctx());

    assert!(view.tool_depth.contains_key("parent"));
}

#[test]
fn turn_complete_caps_tool_depth_to_128_recent_entries() {
    let mut view = make_view();
    for idx in 0..140 {
        let id = format!("tc-{idx}");
        view.tool_depth.insert(id.clone(), 1);
        view.react_trace_mut_for_test().push(crate::components::react_trace::TraceEntry {
            kind: crate::components::react_trace::TraceKind::Act {
                tool: format!("tool-{idx}"),
                family: spur_acp::adapter::ToolFamily::Unknown,
                input: spur_acp::adapter::ToolInputDisplay::Empty,
                tool_call_id: Some(spur_acp::ToolCallId::new(id)),
                status: crate::components::react_trace::ActStatus::Completed(None),
            },
            text: String::new(),
            timestamp: "10:00".into(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    view.handle_spur_event(&turn_complete_event(&view.session_id().clone()), &test_ctx());
    assert!(view.tool_depth.len() <= 128);
}
```

Add the accessor in `session_detail.rs` up front:

```rust
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn react_trace_mut_for_test(
    &mut self,
) -> &mut crate::components::react_trace::ReactTrace {
    &mut self.react_trace
}
```

- [ ] **Step 2: Run the red tests**

Run:

```bash
cargo test -p spur-tui turn_complete_retains_recent_tool_depth_ids
cargo test -p spur-tui turn_complete_caps_tool_depth_to_128_recent_entries
```

Expected: fail because `TurnComplete` currently calls `self.tool_depth.clear()`.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "test(spur-tui): P9 cover bounded tool-depth retention"
```

- [ ] **Step 4: Implement the minimal fix**

Add a recent-id helper to `ReactTrace`:

```rust
pub(crate) fn recent_tool_call_ids(&self, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entry in self.entries.iter().rev() {
        if let TraceKind::Act {
            tool_call_id: Some(id),
            ..
        } = &entry.kind
        {
            if seen.insert(id.0.as_ref()) {
                out.push(id.0.to_string());
            }
        }
        if out.len() == limit {
            break;
        }
    }
    out.reverse();
    out
}
```

Then replace blanket clear on `TurnComplete` with bounded retain:

```rust
let keep: std::collections::HashSet<String> = self
    .react_trace
    .recent_tool_call_ids(128)
    .into_iter()
    .collect();
self.tool_depth.retain(|id, _| keep.contains(id));
```

This is intentionally recency-based, not lifecycle-perfect.

- [ ] **Step 5: Verify green**

Run:

```bash
cargo test -p spur-tui turn_complete_retains_recent_tool_depth_ids
cargo test -p spur-tui turn_complete_caps_tool_depth_to_128_recent_entries
cargo test -p spur-tui tool_depth_nested_two_levels
```

Expected: both new tests pass and the existing in-turn nesting tests remain green.

- [ ] **Step 6: Commit the fix**

```bash
git add crates/spur-tui/src/components/react_trace/mod.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "fix(spur-tui): P9 retain bounded recent tool depth"
```

---

## Phase Verification

After Tasks P1-P5:

```bash
cargo test -p spur-tui tool_call_sets_stream_in_flight
cargo test -p spur-tui delegation_failed_appends_main_trace_note
cargo test -p spur-tui user_message_resource_link_becomes_placeholder
cargo test -p spur-tui append_message_keeps_text_mirror_in_sync
cargo test -p spur-tui canonical_tool_call_reuses_synthesized_act
```

After Tasks P6-P9:

```bash
cargo test -p spur-tui stalled_stream_tick_clears_both_flags_and_adds_note
cargo test -p spur-tui extract_tool_call_text_keeps_unchanged_context_lines
cargo test -p spur-tui --no-default-features layout_for_scroll_returns_metadata_for_full_non_markdown
cargo test -p spur-tui turn_complete_caps_tool_depth_to_128_recent_entries
```

Final full verification:

```bash
cargo fmt --all
cargo test -p spur-tui
cargo test -p spur-tui --no-default-features
```

If `cargo test -p spur-tui --no-default-features` is too broad for the worker budget, at minimum run the two new P8 non-markdown tests under `--no-default-features`.

## Self-Review Checklist

Before handing this plan to an implementation worker:

1. Confirm every spec finding `F14`, `F3`, `F1/F13`, `F9`, `F12`, `F11`, `F2`, `F5`, and `F4` maps to one task above.
2. Search this file for banned placeholders:

```bash
rg -n "TODO|TBD|implement later|appropriate error handling|Write tests for the above|Similar to Task" docs/superpowers/plans/2026-04-22-acp-tui-remediation.md
```

Expected: no matches.

3. Confirm the non-markdown task (P8) still uses `--no-default-features`.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-22-acp-tui-remediation.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using `executing-plans`, batch execution with checkpoints

Which approach?
