# SpurEvent Stream Backbone Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden the orchestrator's event stream: fix four live-streaming pathologies (S1), add a unified emit funnel with monotonic sequence numbers (S2), add a durable JSONL sink for replay and external observers (S3), and define a `_spur/*` ACP ExtNotification vocabulary so workers can emit structured events beyond raw text (S5).

**Architecture:** Single-publisher/many-subscriber fanout. The orchestrator remains the sole publisher. All emissions serialize through a singleton emitter task that stamps seq + timestamp. Three in-process subscribers (TUI, Lineage, JSONL sink); external subscribers read the JSONL file via `tail -f` / `notify`. No broker, no daemon, zero new dependencies.

**Tech Stack:** Rust, tokio (`broadcast`, `mpsc`, `sync::AtomicU64`), serde_json, ACP (agent_client_protocol crate), chrono (existing).

**Spec:** `docs/superpowers/specs/2026-04-14-spurevent-stream-backbone-design.md`
**Companion architecture:** `docs/spur/brain-worker-architecture.md` (§1 Channels, §2 Delegation Lifecycle, §6.1 Event Variants)

---

## Revision log

**2026-04-14 rev 3 — apply arch doc §5.0 adjustments (post-Phase-1 code reality):**

After Phase 1 landed (commits `85415c3..edd94f3`) and `docs/spur/brain-worker-architecture.md` §5.0 published three adjustments the stream spec must respect, revise:

1. **Task 10 — keep `Orchestrator::emit(event: SpurEvent)` signature unchanged.** The ~22 method-scope `self.emit(SpurEvent::now(body))` sites stay as-is. `emit` internally delegates to the funnel by destructuring the event's body and discarding the caller's occurred_at (funnel restamps). No caller churn; seq still monotonically stamped at the funnel.
2. **Task 11 — migrate ONLY the ~13 free-function `event_tx.send(SpurEvent::now(body))` sites** (lines 1113, 1258, 1821, 1832, 1852, 1884, 1913, 1942, 2028, 2084, 2133, 2242, 2409, 2444, 2451 in current code). These are in free helper functions (`handle_delegations`, `execute_delegation`, `run_one_worker_attempt`, `finalize`, review-gate helpers) that receive `event_tx` as a parameter. Change their parameter to `funnel: FunnelHandle` and emit via `funnel.emit(body)`. This is where the funnel's value actually lands — the method-scope sites gain nothing from churn.
3. **Task 14 — add `brain_session_id: SessionId` to `WorkerHeartbeat` / `WorkerProgress` / `WorkerFileTouched` variants.** Phase 1 threads brain_session_id through `run_one_worker_attempt(brain_session_id: &SessionId, ..)` (confirmed at `orchestrator.rs:2392-2394`), so it's in scope anywhere these new variants are emitted. Including it in the event makes external stream queries filter-not-join (subscribers don't need to maintain an executor_id → brain_session_id lookup table).
4. **Task 15 — `interpret` function signature gains `brain_session_id: SessionId` parameter** alongside `executor_id`.
5. **Task 16 — per-worker consumer task must capture `brain_session_id`** from the `run_one_worker_attempt` scope and pass to `interpret`.
6. **Task 17 — file_touched synthesizer must include brain_session_id** in the emitted `WorkerFileTouched` variant.

Total changes: ~5 LoC added to variants; ~13 function signatures change to `FunnelHandle`; interpreter signature gains one parameter. No other structural change to the plan.

**2026-04-14 rev 2 — MCTS cross-check against `docs/spur/brain-worker-architecture.md`:**

1. **Removed Task 17** (extend `report_progress` MCP tool). The arch doc §1.1 lists MCP tools on Channel A as **brain-facing only** — workers do not connect to the MCP callback server. `report_progress` is called by the brain, not by workers; it cannot emit `WorkerProgress { executor_id, .. }` because the brain isn't an executor. **Worker-progress-via-MCP requires first adding a worker-side MCP path**, which is a Phase 2 capability not in scope. `_spur/*` vocabulary remains defined in Task 14 as a wire format for future SPUR-aware agents; only `_spur/file_touched` is exercised in v1 via the server-side ToolCall synthesis path (Task 17 — renumbered from 18).
2. **Scoped Task 14 vocabulary** to be explicit: v1 emitters for `WorkerHeartbeat` and `WorkerProgress` = NONE. Only `WorkerFileTouched` is produced (by server-side synthesis). The other two variants are forward-compatible placeholders.
3. **Task 12 (JSONL sink) trade-off documented**: v1 uses direct broadcast subscribe (best-effort durability — events can be lost if sink lags). The spec's mpsc-bridge-with-backpressure pattern is a Phase 2 upgrade path, not a v1 blocker.
4. **Added arch-doc invariant notes** at Task 11 (S2 funnel preserves Channel D's `register_gate BEFORE emit` ordering invariant from §1.4) and at Task 2 (grace window latency respects §2 await-point budgets — adds <1% overhead to worker spawn time).

Total tasks: 19 (was 20). No change to phase structure or checkpoint tags.

---

## File structure

### Files to create

| Path | Responsibility |
|---|---|
| `crates/spur-core/src/event_sink.rs` | JSONL durable sink task — subscribes to broadcast, appends to `~/.spur/events/{pid}-{ts}.ndjson`, rotates on size. |
| `crates/spur-core/src/event_funnel.rs` | Singleton emitter task — receives `SpurEventBody` over mpsc, stamps seq/ts, sends on broadcast. |
| `crates/spur-core/src/spur_ext_interp.rs` | `_spur/*` ExtNotification interpreter — translates ExtNotificationPayload into SpurEventBody variants. |

### Files to modify

| Path | Changes |
|---|---|
| `crates/spur-acp/src/domain/events.rs` | Add `seq: u64` to `SpurEvent`; add `WorkerHeartbeat`/`WorkerProgress`/`WorkerFileTouched` variants to `SpurEventBody`. |
| `crates/spur-acp/src/connection/native.rs` | S1.a grace-window pattern for trailing notifications (replaces immediate `dead_tx` swap). |
| `crates/spur-core/src/orchestrator.rs` | S1.d bump broadcast size 256→4096; S2 refactor all emit sites to use funnel; wire sink + interpreter into startup. |
| `crates/spur-core/src/lib.rs` | Register `event_sink`, `event_funnel`, `spur_ext_interp` modules. |
| `crates/spur-mcp/src/tools.rs` | S5 — extend `report_progress` params to optionally include `name` and `pct`. |
| `crates/spur-tui/src/components/react_trace.rs` | S1.b — `append_message` walks backwards past non-message entries before creating a new block. |
| `crates/spur-tui/src/app.rs` | S1.c drain cap (max 8 events per paint loop iteration); S1.d `Lagged` arm → WARN log. |

### Directories created at runtime

- `.spur/events/` — JSONL log files, parallel to existing `.spur/logs/`

---

## Preflight — verify state

- [ ] **Step P1: Verify instrumentation is in place**

The diagnosis spec (`2026-04-13-realtime-streaming-diagnosis-design.md`) ships debug instrumentation. Verify:

Run: `grep -n 'streaming_probe = true' crates/spur-acp/src/connection/native.rs`
Expected: at least 4 hits (A_session_notification, B_dead_tx_swap, etc.)

Run: `grep -n 'streaming_probe = true' crates/spur-core/src/orchestrator.rs`
Expected: at least 1 hit (C_streaming_emit or similar).

If missing, stop and escalate — the instrumentation is a prerequisite.

- [ ] **Step P2: Build baseline**

Run: `cargo build --workspace`
Expected: successful build, no compile errors.

Run: `cargo test --workspace --no-run`
Expected: test binaries compile.

---

## Phase S1 — Streaming pathology fixes

Each of S1.a-d is independent. Land them in any order; the tasks below order by severity (S1.a first — silent data loss).

### Task 1: S1.a grace-window pattern — write failing test

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs` (add test module if absent)
- Test: inline `#[cfg(test)] mod tests { ... }` block at bottom of file

The spec's preferred "buffer pattern" has a session-mixing issue (stragglers from turn N would replay into turn N+1's receiver). We implement the spec's stated alternative: a grace window. After `connection.prompt()` returns, keep the live `notification_tx` for 250ms of idle (no new `session_notification`) before swapping to `dead_tx`. The orchestrator's existing drain loop naturally receives stragglers during the window.

We cannot unit-test the ACP thread directly (it's `!Send`, spawned via `LocalSet`). Instead, add an integration-style test that constructs a mock agent via `StdioConnection` emitting a delayed notification and verifies the orchestrator receives it.

Skip the unit-test attempt — too much plumbing. Instead, add a regression test that exercises the real `NativeAcpConnection` via its public API with a deterministic mock agent binary.

**Pragma:** create a smaller, targeted test fixture. In `crates/spur-acp/tests/native_trailing_notification.rs`:

- [ ] **Step 1.1: Create test fixture helper (mock agent script)**

```bash
mkdir -p crates/spur-acp/tests/fixtures
```

Create `crates/spur-acp/tests/fixtures/agent_trailing_notification.sh`:

```bash
#!/bin/bash
# Mock ACP agent: responds to initialize + prompt, emits a trailing
# session/update 200ms AFTER the prompt_response has been sent.
# Used to reproduce the H5 dead-tx race.

set -e
while IFS= read -r line; do
    # Echo method extraction: we only respond to initialize, new_session, prompt.
    method=$(echo "$line" | sed -n 's/.*"method":"\([^"]*\)".*/\1/p')
    id=$(echo "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')

    case "$method" in
        initialize)
            echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":1,"agentCapabilities":{}}}'
            ;;
        session/new)
            echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"sessionId":"test-session"}}'
            ;;
        session/prompt)
            # Emit one chunk synchronously, reply, then emit a trailing chunk.
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"test-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"first"}}}}'
            echo '{"jsonrpc":"2.0","id":'"$id"',"result":{"stopReason":"end_turn"}}'
            # Sleep 200ms (in seconds for bash), then emit a trailing chunk.
            sleep 0.2
            echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"test-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"second"}}}}'
            ;;
    esac
done
```

Make executable:

```bash
chmod +x crates/spur-acp/tests/fixtures/agent_trailing_notification.sh
```

- [ ] **Step 1.2: Write the failing integration test**

Create `crates/spur-acp/tests/native_trailing_notification.rs`:

```rust
//! Regression test for H5 — the dead_tx race that drops trailing
//! `session_notification` chunks arriving after `prompt()` returns.

use spur_acp::connection::native::NativeAcpConnection;
use spur_acp::connection::AgentConnection;
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn trailing_notification_reaches_caller() {
    let script = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/agent_trailing_notification.sh");
    assert!(script.exists(), "fixture missing at {}", script.display());

    let mut conn = NativeAcpConnection::new(
        "mock",
        script.to_string_lossy(),
        vec![],
        None,
    );

    // Initialize + new_session (elided — see other integration tests in
    // crates/spur-acp/tests/ for the pattern). Then call prompt and drain
    // the stream for up to 1s. We expect TWO chunks: "first" and "second".

    let init_caps = conn.initialize().await.expect("initialize");
    let _session_id = conn.new_session(".").await.expect("new_session");

    let mut stream = conn.prompt("test-session", "any-prompt").await
        .expect("prompt");

    let mut chunks = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(1000);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() { break; }
        match tokio::time::timeout(remaining, stream.recv()).await {
            Ok(Some(notif)) => chunks.push(format!("{notif:?}")),
            Ok(None) => break,  // stream closed
            Err(_) => break,    // overall deadline
        }
    }

    conn.shutdown().await.ok();

    assert!(
        chunks.iter().any(|s| s.contains("first")),
        "expected first chunk, got: {chunks:?}"
    );
    assert!(
        chunks.iter().any(|s| s.contains("second")),
        "expected trailing chunk, got: {chunks:?} — H5 regressed"
    );
}
```

- [ ] **Step 1.3: Run test and verify it FAILS**

Run: `cargo test -p spur-acp --test native_trailing_notification -- --nocapture`
Expected: FAIL with `expected trailing chunk` — confirms H5 bug is reproduced.

If the assertion "expected first chunk" also fails, the fixture / API shape doesn't match the actual `NativeAcpConnection` signatures. Inspect existing integration tests in `crates/spur-acp/tests/` for the correct call pattern (esp. `initialize` vs `new` semantics) and update the test before proceeding.

### Task 2: S1.a implement the grace window

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs:860-903` (Prompt handler block)
- Modify: `crates/spur-acp/src/connection/native.rs:980-998` (LoadSession handler block)

**Latency impact (arch doc §2 cross-check):** the grace window can delay the ACP thread's command loop by up to 1s. The orchestrator's retry loop respawns a new worker on a fresh `NativeAcpConnection` (separate thread), so retry spawn (T5b=1-3s per arch doc §2) is NOT serialized against the old worker's grace window. The serial cost is only on the old worker's teardown — <1% of typical T5c (worker execution). Within budget.

Strategy: after `connection.prompt()` returns, instead of immediately swapping to `dead_tx`, enter a polling loop that:
1. Tracks `last_notification_at` (monotonic Instant, updated by `session_notification`)
2. Loops `tokio::time::sleep(50ms)` until `now() - last_notification_at >= 250ms` OR a 1s absolute deadline elapses
3. Then swaps to `dead_tx`

`last_notification_at` needs to live on the `SpurAcpClientDynamic` client (lines 797-803) as `Rc<RefCell<Instant>>`. The `session_notification` handler (line 1143) updates it on every call.

- [ ] **Step 2.1: Add `last_notification_at` field to SpurAcpClientDynamic**

Modify `crates/spur-acp/src/connection/native.rs` — add field to the struct near line 1072:

```rust
struct SpurAcpClientDynamic {
    // ... existing fields ...
    notification_tx: std::rc::Rc<std::cell::RefCell<mpsc::UnboundedSender<SessionNotification>>>,
    cwd: std::rc::Rc<std::cell::RefCell<PathBuf>>,
    permission_tx: Option<mpsc::UnboundedSender<crate::types::PermissionRequest>>,
    terminals: std::rc::Rc<std::cell::RefCell<HashMap<String, TerminalState>>>,
    ext_notification_tx: mpsc::UnboundedSender<ExtNotificationPayload>,
    // NEW: grace-window support for H5 fix.
    last_notification_at: std::rc::Rc<std::cell::RefCell<std::time::Instant>>,
}
```

- [ ] **Step 2.2: Initialize the field at construction (line ~797)**

In the block starting `let spur_client = SpurAcpClientDynamic {` (around line 797), add:

```rust
let last_notification_at = std::rc::Rc::new(std::cell::RefCell::new(
    std::time::Instant::now()
));
let last_notification_at_for_client = last_notification_at.clone();

let spur_client = SpurAcpClientDynamic {
    notification_tx: notification_tx_for_client,
    cwd: std::rc::Rc::new(std::cell::RefCell::new(PathBuf::from("."))),
    permission_tx,
    terminals: std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())),
    ext_notification_tx: ext_notification_tx.clone(),
    last_notification_at: last_notification_at_for_client,
};
```

And keep a clone accessible to the command loop (for the Prompt handler's grace check):

```rust
let last_notification_at_for_thread = last_notification_at.clone();
```

- [ ] **Step 2.3: Update `session_notification` to stamp the timestamp**

At `crates/spur-acp/src/connection/native.rs:1143` (the line with `let send_result = self.notification_tx.borrow().send(args);`), add immediately before the send:

```rust
*self.last_notification_at.borrow_mut() = std::time::Instant::now();
let send_result = self.notification_tx.borrow().send(args);
```

- [ ] **Step 2.4: Replace immediate `dead_tx` swap with grace-window loop (Prompt handler, ~line 894-903)**

Replace the block at lines 894-903:

```rust
// When prompt() returns, the notification channel sender
// gets replaced on the next prompt call (or dropped on
// shutdown), which will close the stream for the consumer.
// We explicitly drop the current sender to signal completion.
tracing::debug!(
    streaming_probe = true,
    site = "B_dead_tx_swap",
    which = "prompt_end",
    agent = %agent_name_prompt,
    session = %session_id_for_probe,
    "notification_tx -> dead_tx (prompt returned)"
);
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

with:

```rust
// S1.a — grace window for trailing session_notification chunks
// (fixes H5 dead-tx race). Wait for 250ms of idle OR 1s absolute
// deadline, whichever comes first, before swapping to dead_tx.
// During this window, any trailing notification lands on the live
// tx and is drained by the caller's still-active receiver.
let grace_start = std::time::Instant::now();
let idle_threshold = std::time::Duration::from_millis(250);
let absolute_cap = std::time::Duration::from_secs(1);
loop {
    let since_last = last_notification_at_for_thread
        .borrow()
        .elapsed();
    let total_wait = grace_start.elapsed();
    if since_last >= idle_threshold || total_wait >= absolute_cap {
        break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
tracing::debug!(
    streaming_probe = true,
    site = "B_dead_tx_swap",
    which = "prompt_end",
    agent = %agent_name_prompt,
    session = %session_id_for_probe,
    grace_elapsed_ms = grace_start.elapsed().as_millis() as u64,
    "notification_tx -> dead_tx (post-grace)"
);
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

- [ ] **Step 2.5: Mirror the grace window in the LoadSession handler (~line 995-998)**

Same fix at lines 995-998 of the LoadSession block. Replace:

```rust
// Swap notification_tx to a dead channel regardless of
// outcome — history streaming is over.
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

with:

```rust
// S1.a — grace window for trailing history notifications.
let grace_start = std::time::Instant::now();
let idle_threshold = std::time::Duration::from_millis(250);
let absolute_cap = std::time::Duration::from_secs(1);
loop {
    let since_last = last_notification_at_for_thread
        .borrow()
        .elapsed();
    let total_wait = grace_start.elapsed();
    if since_last >= idle_threshold || total_wait >= absolute_cap {
        break;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
let (dead_tx, _) = mpsc::unbounded_channel::<SessionNotification>();
*notification_tx.borrow_mut() = dead_tx;
```

- [ ] **Step 2.6: Compile + run the test**

Run: `cargo build -p spur-acp`
Expected: successful build.

Run: `cargo test -p spur-acp --test native_trailing_notification -- --nocapture`
Expected: PASS — both "first" and "second" chunks received.

- [ ] **Step 2.7: Commit**

```bash
git add crates/spur-acp/src/connection/native.rs \
        crates/spur-acp/tests/native_trailing_notification.rs \
        crates/spur-acp/tests/fixtures/agent_trailing_notification.sh
git commit -m "$(cat <<'EOF'
fix(spur-acp): S1.a grace window for trailing notifications (H5)

After connection.prompt() returns, wait for 250ms of idle (or 1s
absolute cap) before swapping notification_tx to dead_tx. During
the grace window, trailing session_notifications reach the caller
via the still-live tx. Fixes the "message breaks at end" symptom.

Mirrors the fix in LoadSession handler too.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 3: S1.b interleave-split fix — write failing test

**Files:**
- Test: `crates/spur-tui/src/components/react_trace.rs` (inline `#[cfg(test)] mod tests` at bottom — already present, extend)

When a tool-call entry (`TraceKind::Act`) or other non-AgentMessage kind interrupts streaming chunks, the NEXT `append_message` creates a new AgentMessage block instead of continuing the previous one. Fix: walk backwards past non-AgentMessage entries within a reasonable window to find the most recent AgentMessage from the same agent.

- [ ] **Step 3.1: Add failing test in `react_trace.rs`**

At the bottom of `crates/spur-tui/src/components/react_trace.rs` inside the existing `#[cfg(test)] mod tests` block (after line ~1570ish where other `append_message` tests live), add:

```rust
#[test]
fn append_message_continues_existing_after_tool_call_interleave() {
    let mut trace = ReactTrace::new();
    trace.append_message("first chunk. ", "claude", "10:00:01".to_string());
    // Simulate a tool call landing between chunks (from session/update:
    // ToolCall or ToolCallUpdate variants). ReactTrace tracks tool calls as
    // TraceKind::Act.
    trace.push(TraceEntry {
        kind: TraceKind::Act { tool_name: "read_file".to_string(), args: String::new() },
        text: "read_file(path=...)".to_string(),
        timestamp: "10:00:02".to_string(),
        #[cfg(feature = "markdown")]
        markdown: None,
    });
    trace.append_message("second chunk.", "claude", "10:00:03".to_string());

    // Count AgentMessage entries with agent="claude" — must be ONE merged.
    let agent_message_count = trace.entries.iter().filter(|e| {
        matches!(&e.kind, TraceKind::AgentMessage { agent, .. } if agent == "claude")
    }).count();
    assert_eq!(
        agent_message_count, 1,
        "interleaved tool call split AgentMessage — H2 regressed"
    );

    // The single AgentMessage should contain both chunks.
    let msg = trace.entries.iter().find_map(|e| match &e.kind {
        TraceKind::AgentMessage { .. } => Some(&e.text),
        _ => None,
    }).expect("expected AgentMessage entry");
    assert!(msg.contains("first chunk"), "missing first chunk");
    assert!(msg.contains("second chunk"), "missing second chunk");
}
```

- [ ] **Step 3.2: Run test and verify it FAILS**

Run: `cargo test -p spur-tui append_message_continues_existing_after_tool_call_interleave -- --nocapture`
Expected: FAIL — current `append_message` creates a second AgentMessage entry when the previous entry is not itself an AgentMessage.

### Task 4: S1.b implement the walkback

**Files:**
- Modify: `crates/spur-tui/src/components/react_trace.rs:305-340` (the `append_message` method)

- [ ] **Step 4.1: Replace `append_message` body with walkback logic**

Locate the current `append_message` (starts at line 305). Current code checks ONLY `self.entries.last_mut()`. Replace the body (preserve method signature and markdown-cfg blocks) so it walks backwards through at most 10 entries to find the most recent `AgentMessage` for the same agent, appending there; otherwise creates a new block.

Replacement body:

```rust
pub fn append_message(&mut self, text: &str, agent: &str, timestamp: String) {
    // Walk backwards up to a small bounded window looking for the most
    // recent AgentMessage for THIS agent, skipping non-message entries
    // (tool calls, observations, etc.). This prevents interleaved tool
    // calls from splitting one logical message into fragments (S1.b fix).
    const WALKBACK_LIMIT: usize = 10;
    let mut target_idx: Option<usize> = None;
    for (offset, entry) in self.entries.iter().rev().take(WALKBACK_LIMIT).enumerate() {
        match &entry.kind {
            TraceKind::AgentMessage { agent: entry_agent } if entry_agent == agent => {
                target_idx = Some(self.entries.len() - 1 - offset);
                break;
            }
            // Don't walk past user turns or other agents' messages.
            TraceKind::UserMessage => break,
            TraceKind::AgentMessage { .. } => break,  // different agent
            _ => continue,                             // tool call / think / etc.
        }
    }

    #[cfg(feature = "markdown")]
    {
        if let Some(idx) = target_idx {
            if let Some(entry) = self.entries.get_mut(idx) {
                if let Some(stream) = entry.markdown.as_mut() {
                    stream.append(text);
                }
                if self.is_following {
                    self.scroll_to_bottom();
                }
                return;
            }
        }
    }

    #[cfg(not(feature = "markdown"))]
    {
        if let Some(idx) = target_idx {
            if let Some(entry) = self.entries.get_mut(idx) {
                if !entry.text.is_empty() {
                    entry.text.push_str(text);
                } else {
                    entry.text = text.to_string();
                }
                if self.is_following {
                    self.scroll_to_bottom();
                }
                return;
            }
        }
    }

    // No eligible previous AgentMessage within window — new entry.
    self.push(TraceEntry {
        kind: TraceKind::AgentMessage { agent: agent.to_string() },
        text: text.to_string(),
        timestamp,
        #[cfg(feature = "markdown")]
        markdown: Some(crate::components::markdown_stream::MarkdownStream::new(text)),
    });
}
```

Note: inspect the existing method's final `push(TraceEntry {...})` block to copy the EXACT `TraceKind::AgentMessage` variant shape (field names, `markdown` construction) — the snippet above is accurate to today's code but be precise to avoid compile errors. If the current `markdown` construction uses a different initializer, mirror that.

- [ ] **Step 4.2: Run the test and verify it PASSES**

Run: `cargo test -p spur-tui append_message_continues_existing_after_tool_call_interleave -- --nocapture`
Expected: PASS.

- [ ] **Step 4.3: Run full react_trace test suite to check no regressions**

Run: `cargo test -p spur-tui --lib components::react_trace`
Expected: all pre-existing `append_message*` tests still PASS.

- [ ] **Step 4.4: Commit**

```bash
git add crates/spur-tui/src/components/react_trace.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): S1.b walkback in append_message (H2 interleave)

When tool calls / observations land between streaming chunks,
append_message walks backwards up to 10 entries to continue the
existing AgentMessage for the same agent instead of creating a
new block. Stops at UserMessage or a different agent's message.

Fixes the "message fragments into pieces" symptom.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 5: S1.c drain-coalescing cap

**Files:**
- Modify: `crates/spur-tui/src/app.rs` — main loop event drain

No dedicated test: drain-coalescing is a perceived-smoothness fix best validated by manual smoke test. Instead, we verify via code inspection that the drain cap is honored.

- [ ] **Step 5.1: Locate the drain loop**

Run: `grep -n 'events_drained\|drain.*event\|while.*try_recv' crates/spur-tui/src/app.rs`

Expected: at least one hit showing the drain loop (per the diagnosis spec it's around lines 521-573).

- [ ] **Step 5.2: Cap the drain at 8 events**

Locate the drain loop. It likely looks like:

```rust
while let Ok(event) = event_rx.try_recv() {
    // process event
    dirty = true;
}
```

Replace with a counted version:

```rust
// S1.c — cap per-iteration drain to avoid coalescing many chunks
// into a single paint frame. After 8 events, yield to the render;
// leftover events drain on the next iteration.
const DRAIN_CAP_PER_FRAME: usize = 8;
let mut drained = 0;
while drained < DRAIN_CAP_PER_FRAME {
    match event_rx.try_recv() {
        Ok(event) => {
            // existing per-event processing (unchanged)
            dirty = true;
            drained += 1;
        }
        Err(broadcast::error::TryRecvError::Empty) => break,
        Err(broadcast::error::TryRecvError::Lagged(n)) => {
            tracing::warn!(
                streaming_probe = true,
                lagged_n = n,
                "TUI broadcast lagged"
            );
            // (S1.d included here — see Task 7.)
            drained += 1;
        }
        Err(broadcast::error::TryRecvError::Closed) => break,
    }
}
```

The exact placement depends on the current loop structure — preserve all existing per-event processing logic; only wrap the loop with a counter and the early-break.

- [ ] **Step 5.3: Build + run existing TUI tests**

Run: `cargo build -p spur-tui`
Expected: build succeeds.

Run: `cargo test -p spur-tui --lib`
Expected: all pre-existing tests still PASS.

- [ ] **Step 5.4: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "$(cat <<'EOF'
fix(spur-tui): S1.c cap per-frame event drain at 8 (H1')

Main loop no longer drains all pending events before each paint.
Capping at 8 per iteration restores progressive rendering for
streaming chunks; leftover events drain on the next iteration.
Preserves existing render cadence for bursty sessions.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 6: S1.d broadcast buffer bump

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:139`

- [ ] **Step 6.1: Bump the buffer size**

Change `crates/spur-core/src/orchestrator.rs:139` from:

```rust
let (event_tx, _) = broadcast::channel(256);
```

to:

```rust
// S1.d — 4096 supports ~2.5s of events at 1600 evt/s peak
// (20 workers × 80 evt/s). Subscribers that still lag get
// RecvError::Lagged (logged at WARN; see Task 7).
let (event_tx, _) = broadcast::channel(4096);
```

### Task 7: S1.d surface Lagged errors

**Files:**
- Modify: `crates/spur-tui/src/app.rs` — convert silent `{}` on `Lagged` into WARN
- Modify: `crates/spur-tui/src/**` any other silent arms flagged by grep

- [ ] **Step 7.1: Grep for silent Lagged arms**

Run: `grep -rn 'RecvError::Lagged' crates/spur-tui/src/`
Expected: several hits. Every arm whose body is `{}` or `()` needs a WARN log.

Also check: `grep -rn 'RecvError::Lagged' crates/spur-core/src/`

- [ ] **Step 7.2: Convert every silent arm**

For each hit where the current body is `{}` (swallowed), replace with:

```rust
Err(broadcast::error::RecvError::Lagged(n)) => {
    tracing::warn!(
        streaming_probe = true,
        lagged_n = n,
        source = file!(),
        line = line!(),
        "broadcast subscriber lagged"
    );
}
```

If the Lagged arm is inside a match on `try_recv`, use `TryRecvError::Lagged` instead. Task 5 already did this for the main loop in app.rs; extend to any other call sites.

- [ ] **Step 7.3: Build**

Run: `cargo build -p spur-tui -p spur-core`
Expected: success.

Run: `cargo test --workspace --no-run`
Expected: success.

- [ ] **Step 7.4: Commit S1.d (bump + Lagged logging together)**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-tui/src/
git commit -m "$(cat <<'EOF'
fix(core,tui): S1.d broadcast buffer 256→4096 + WARN on Lagged

Bumps the event bus capacity for bursty parallel-delegation loads
and makes subscriber lag observable. Silent `{}` arms on
RecvError::Lagged now log at WARN with the skipped count, file,
and line so we can detect when a subscriber is falling behind.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase S2 — Unified emit funnel + monotonic seq

### Task 8: Add seq field to SpurEvent

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`

- [ ] **Step 8.1: Add `seq: u64` to the struct**

At `crates/spur-acp/src/domain/events.rs:80` change:

```rust
pub struct SpurEvent {
    pub occurred_at: SystemTime,
    pub body: SpurEventBody,
}
```

to:

```rust
pub struct SpurEvent {
    pub occurred_at: SystemTime,
    /// Monotonic sequence number assigned by the orchestrator's emit
    /// funnel (S2). Direct constructors set this to 0; the funnel
    /// overwrites. Subscribers can detect gaps and order chronologically.
    pub seq: u64,
    pub body: SpurEventBody,
}
```

- [ ] **Step 8.2: Update `SpurEvent::now` to set seq=0**

At the `impl SpurEvent` block (around line 85):

```rust
impl SpurEvent {
    /// Convenience constructor. Use at emission sites. Do NOT call inside
    /// `apply` / projection code — timestamps there must come from the
    /// arriving event.
    ///
    /// Note: `seq` defaults to 0; the orchestrator's emit funnel (S2)
    /// overwrites with a real monotonic value before broadcast.
    pub fn now(body: SpurEventBody) -> Self {
        Self { occurred_at: SystemTime::now(), seq: 0, body }
    }
}
```

- [ ] **Step 8.3: Build the workspace to find downstream breakage**

Run: `cargo build --workspace 2>&1 | head -80`
Expected: either success, OR missing-field errors wherever `SpurEvent { occurred_at, body }` is pattern-matched or constructed directly.

If there are match/construction sites that break, fix each to include `seq` (projection code can match `{ seq: _, occurred_at, body }` since projections don't care about seq directly). Keep these fixes minimal — just satisfy the compiler.

- [ ] **Step 8.4: Run tests**

Run: `cargo test --workspace --no-run`
Expected: builds.

Run: `cargo test --workspace --lib 2>&1 | tail -30`
Expected: all pre-existing tests pass (since default seq=0 doesn't change serde-compat for any new events).

- [ ] **Step 8.5: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git add -u  # any pattern-match fixes
git commit -m "$(cat <<'EOF'
feat(spur-acp): S2 add seq field to SpurEvent envelope

Additive — defaults to 0 at construction; the orchestrator's emit
funnel (next task) will stamp the real monotonic value. Subscribers
can use seq to detect gaps and order events chronologically.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 9: Create the event funnel module

**Files:**
- Create: `crates/spur-core/src/event_funnel.rs`
- Modify: `crates/spur-core/src/lib.rs` (register module)

- [ ] **Step 9.1: Create `event_funnel.rs`**

```rust
//! Singleton event emitter task (Phase S2).
//!
//! All `SpurEvent` emission inside the orchestrator must flow through
//! this funnel. Each emit call sends a `SpurEventBody` over an
//! unbounded mpsc; a dedicated task reads the mpsc, stamps a monotonic
//! `seq` and `occurred_at`, and forwards on the broadcast channel.
//!
//! This guarantees strict seq ordering (Pitfall P1 in the design
//! spec): subscribers observe events in exactly the order the funnel
//! stamped them.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use tokio::sync::{broadcast, mpsc};

use spur_acp::domain::events::{SpurEvent, SpurEventBody};

/// Handle returned by `spawn_funnel`. Clone cheaply; call `emit`.
#[derive(Clone)]
pub struct FunnelHandle {
    tx: mpsc::UnboundedSender<SpurEventBody>,
}

impl FunnelHandle {
    /// Enqueue a body for stamping + broadcast. Non-blocking.
    /// Silently drops if the funnel task has terminated (treated as
    /// orchestrator shutdown).
    pub fn emit(&self, body: SpurEventBody) {
        let _ = self.tx.send(body);
    }
}

/// Spawn the singleton funnel task. The returned `FunnelHandle` is
/// given to every emitter inside the orchestrator.
pub fn spawn_funnel(
    broadcast_tx: broadcast::Sender<SpurEvent>,
    seq: Arc<AtomicU64>,
) -> FunnelHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<SpurEventBody>();

    tokio::spawn(async move {
        while let Some(body) = rx.recv().await {
            let s = seq.fetch_add(1, Ordering::Relaxed);
            let event = SpurEvent {
                occurred_at: SystemTime::now(),
                seq: s,
                body,
            };
            let _ = broadcast_tx.send(event);
        }
    });

    FunnelHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn funnel_stamps_monotonic_seq() {
        let (bcast_tx, mut bcast_rx) = broadcast::channel(256);
        let seq = Arc::new(AtomicU64::new(0));
        let handle = spawn_funnel(bcast_tx, seq);

        // Emit 5 events serially.
        for _ in 0..5 {
            handle.emit(SpurEventBody::TurnComplete {
                session: spur_acp::types::SessionId::from("s"),
            });
        }

        let mut seen = Vec::new();
        for _ in 0..5 {
            let ev = bcast_rx.recv().await.expect("recv");
            seen.push(ev.seq);
        }
        assert_eq!(seen, vec![0, 1, 2, 3, 4], "seq must be monotonic and start at 0");
    }

    #[tokio::test]
    async fn funnel_orders_concurrent_emits() {
        // Spawn 8 tasks, each emitting 100 events. After all done,
        // we should observe seq 0..800 in order on the broadcast.
        let (bcast_tx, mut bcast_rx) = broadcast::channel(4096);
        let seq = Arc::new(AtomicU64::new(0));
        let handle = spawn_funnel(bcast_tx, seq);

        let mut joins = Vec::new();
        for _ in 0..8 {
            let h = handle.clone();
            joins.push(tokio::spawn(async move {
                for _ in 0..100 {
                    h.emit(SpurEventBody::TurnComplete {
                        session: spur_acp::types::SessionId::from("s"),
                    });
                }
            }));
        }
        for j in joins { j.await.unwrap(); }

        let mut seen = Vec::new();
        for _ in 0..800 {
            let ev = bcast_rx.recv().await.expect("recv");
            seen.push(ev.seq);
        }
        let mut expected: Vec<u64> = (0..800).collect();
        seen.sort();
        expected.sort();
        assert_eq!(seen, expected, "every seq 0..800 must appear exactly once");
    }
}
```

- [ ] **Step 9.2: Register the module in `crates/spur-core/src/lib.rs`**

Add to `lib.rs`:

```rust
pub mod event_funnel;
```

- [ ] **Step 9.3: Build + test**

Run: `cargo test -p spur-core --lib event_funnel -- --nocapture`
Expected: both funnel tests PASS.

If `SpurEventBody::TurnComplete` shape differs in the actual code (for example it may take a different type for `session`), adapt the test bodies to use whatever the simplest variant is (check `crates/spur-acp/src/domain/events.rs` for a zero-field variant or use one with a trivial session param).

- [ ] **Step 9.4: Commit**

```bash
git add crates/spur-core/src/event_funnel.rs crates/spur-core/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): S2 event funnel module + tests

Singleton emitter task that receives SpurEventBody over unbounded
mpsc, stamps monotonic seq + timestamp, forwards on broadcast.
Guarantees strict order (seq N observed before seq N+1 at every
subscriber). Two tests cover serial and concurrent emission.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 10: Wire the funnel into Orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — struct, constructor, `emit()` helper

- [ ] **Step 10.1: Add `event_seq` and `funnel` fields**

Locate the `Orchestrator` struct (the `Ok(Self { registry, config, ... })` at line 142 gives the shape). Add:

```rust
pub struct Orchestrator {
    // ... existing fields ...
    event_tx: broadcast::Sender<SpurEvent>,
    // NEW:
    event_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    funnel: crate::event_funnel::FunnelHandle,
    // ... rest ...
}
```

- [ ] **Step 10.2: Initialize in `new()` (around line 139)**

Replace the broadcast-channel line with:

```rust
let (event_tx, _) = broadcast::channel(4096);
let event_seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
let funnel = crate::event_funnel::spawn_funnel(event_tx.clone(), event_seq.clone());
let review_sink = ReviewSink::new();

Ok(Self {
    registry,
    config,
    worktrees,
    cost_tracker,
    event_tx,
    event_seq,
    funnel,
    review_sink,
    repo_root,
})
```

- [ ] **Step 10.3: Refactor `Orchestrator::emit` (line ~1510) — preserve signature (rev 3)**

Per arch doc §5.0 adjustment (b), keep the `fn emit(&self, event: SpurEvent)` signature so the ~22 existing `self.emit(SpurEvent::now(body))` call sites compile unchanged. Route internally through the funnel by destructuring the event.

Replace:

```rust
fn emit(&self, event: SpurEvent) {
    let _ = self.event_tx.send(event);
}
```

with:

```rust
/// Emit an event through the S2 funnel. The funnel stamps seq + timestamp,
/// so the caller's `event.occurred_at` is discarded — funnel's is more
/// accurate (it's the wall-clock at send-to-broadcast moment).
fn emit(&self, event: SpurEvent) {
    self.funnel.emit(event.body);
}
```

- [ ] **Step 10.4: Verify method-scope call sites compile unchanged**

Run: `cargo build -p spur-core 2>&1 | grep -E 'error|warning' | head -40`
Expected: success (no changes to `self.emit(SpurEvent::now(body))` callers — they still pass `SpurEvent`).

The free-function `event_tx.send(SpurEvent::now(body))` sites are NOT touched by this task; they're migrated in Task 11.

### Task 11: Migrate free-function `event_tx.send(SpurEvent::now(body))` sites (rev 3 — scope narrowed)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (~13 free-function call sites)

Per arch doc §5.0 adjustment (b): **do NOT** churn the ~22 method-scope `self.emit(SpurEvent::now(body))` sites. They already route through `Orchestrator::emit`, which (after Task 10) funnels internally. Only the free-function sites — which receive `event_tx: broadcast::Sender<SpurEvent>` as a parameter — need the refactor.

**Arch doc invariants preserved (§1.4 and §6.1):**
- **Channel D ordering** — `register_gate()` MUST be called BEFORE emitting `ExecutorReviewRequested`. Orchestrator code already does this synchronously at the call site. The funnel's mpsc→singleton-task→broadcast path adds only latency; it cannot reorder. By the time `funnel.emit(ExecutorReviewRequested)` is called, `register_gate()` has already executed.
- **"Exactly one DelegationCompleted" per delegation** — `finalize()` remains a single call site per terminal arm. The funnel changes HOW emit happens, not how many times.
- **Broadcast send order preserved for lineage** — the singleton emitter task reads mpsc FIFO and calls `broadcast.send` serially. Events reach subscribers in emitter-task order.
- **brain_session_id threading preserved** — Phase 1 added `brain_session_id: &SessionId` to `run_one_worker_attempt` (`orchestrator.rs:2392-2394`). The refactor changes `event_tx → funnel` alongside this parameter; do not remove or reorder brain_session_id.

- [ ] **Step 11.1: Find the free-function sites**

Run: `grep -n 'event_tx\.send(SpurEvent::now' crates/spur-core/src/orchestrator.rs`

Expected: ~13 hits (at lines 1113, 1258, 1821, 1832, 1852, 1884, 1913, 1942, 2028, 2084, 2133, 2242, 2409, 2444, 2451 — minus any that have moved since the plan was written).

Example function signature change (at `handle_delegations`, line ~1519 onward — inspect the actual signature):

```rust
async fn handle_delegations(
    mut channel: DelegationChannel,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    max_concurrent: usize,
    event_tx: broadcast::Sender<SpurEvent>,   // ← change to funnel
    // ...
)
```

becomes:

```rust
async fn handle_delegations(
    mut channel: DelegationChannel,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    max_concurrent: usize,
    funnel: crate::event_funnel::FunnelHandle,
    // ...
)
```

And inside the body:

```rust
let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorPhaseChanged { .. }));
```

becomes:

```rust
funnel.emit(SpurEventBody::ExecutorPhaseChanged { .. });
```

Propagate the signature change to every downstream function that takes `event_tx` (probably `execute_delegation`, `run_one_worker_attempt`, `finalize`, and review-gate helpers). Pass `funnel.clone()` where a clone was previously `event_tx.clone()`.

Call site (in `Orchestrator::run_interactive` or wherever `handle_delegations` is spawned): pass `self.funnel.clone()` instead of `self.event_tx.clone()`.

- [ ] **Step 11.4: Build**

Run: `cargo build -p spur-core 2>&1 | tail -30`
Expected: success.

- [ ] **Step 11.5: Run the full test suite**

Run: `cargo test --workspace --lib 2>&1 | tail -20`
Expected: all pre-existing tests pass. The funnel is a transparent refactor — subscribers still receive identical SpurEvent values (now with seq > 0 for events emitted through the orchestrator).

- [ ] **Step 11.6: Verify free-function sites are fully migrated**

Run: `grep -n 'event_tx\.send(SpurEvent::now' crates/spur-core/src/orchestrator.rs`
Expected: zero hits.

Run: `grep -n 'self\.emit(SpurEvent::now' crates/spur-core/src/orchestrator.rs`
Expected: ~22 hits — these are the method-scope sites we INTENTIONALLY preserve per rev 3 adjustment (b). Task 10's internal delegation funnels them transparently.

- [ ] **Step 11.7: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "$(cat <<'EOF'
refactor(spur-core): S2 route free-function emits through funnel

All ~13 free-function `event_tx.send(SpurEvent::now(body))` call
sites (handle_delegations, execute_delegation, run_one_worker_attempt,
finalize, review-gate helpers) now take `funnel: FunnelHandle` and
emit via `funnel.emit(body)`. Per arch doc §5.0 adjustment (b), the
~22 method-scope `self.emit(SpurEvent::now(body))` sites are
preserved; Task 10's internal `Orchestrator::emit` delegation funnels
them transparently.

Phase 1's `brain_session_id: &SessionId` parameter on
`run_one_worker_attempt` preserved. No behavior change for existing
subscribers — events carry new seq > 0 (was always 0 before).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase S3 — JSONL durable sink

### Task 12: Create `event_sink.rs` scaffolding + first test

**Files:**
- Create: `crates/spur-core/src/event_sink.rs`
- Modify: `crates/spur-core/src/lib.rs`

**Backpressure trade-off (MCTS rev 2):** the spec's §"Durability vs. lag" prescribes a hot-path mpsc bridge with a threshold-based emit-funnel block to near-guarantee durability. V1 ships the simpler design below — the sink directly subscribes to the broadcast. Consequences:
- Under typical load (streaming peaks ~100 evt/s), sink keeps up comfortably.
- Under pathological load (>4096 events queued while the sink is blocked on a slow disk write), the sink receives `RecvError::Lagged(n)` and some events are LOST from the JSONL log. They still reach TUI and lineage via their own broadcast subscriptions — only the durable log has gaps.
- The WARN log from Lagged arms (Task 7) makes any gap observable.

**Phase 2 upgrade path** (when a concrete durability SLA exists): insert an unbounded mpsc between the broadcast subscriber and the sink file writer. A hot-path task subscribes to broadcast and forwards to the mpsc with minimal work. The sink drains mpsc FIFO. If mpsc queue depth exceeds a threshold, log WARN and optionally block the emit funnel momentarily to apply backpressure. ~40 additional LoC.

- [ ] **Step 12.1: Create the module**

```rust
//! JSONL durable event sink (Phase S3).
//!
//! Subscribes to the orchestrator's broadcast channel and appends every
//! `SpurEvent` as one line of JSON to `~/.spur/events/{pid}-{ts}.ndjson`.
//! Size-based rotation; log-and-drop on write error (never crashes the
//! orchestrator on disk-full).

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use tokio::sync::broadcast;

use spur_acp::domain::events::SpurEvent;

/// Maximum file size before rotation. Override with
/// `SPUR_EVENT_LOG_MAX_BYTES`.
const DEFAULT_MAX_BYTES: u64 = 128 * 1024 * 1024; // 128 MB
const FLUSH_BYTES: usize = 64 * 1024;              // 64 KB buffer threshold
const FLUSH_INTERVAL_MS: u64 = 100;

/// Spawn the sink task. Returns immediately; the task runs until the
/// broadcast channel closes (orchestrator shutdown).
pub fn spawn_sink(mut rx: broadcast::Receiver<SpurEvent>) {
    let events_dir = events_dir();
    if let Err(e) = fs::create_dir_all(&events_dir) {
        tracing::error!(error = %e, dir = %events_dir.display(),
            "event_sink: failed to create events dir; sink disabled");
        return;
    }

    tokio::spawn(async move {
        let mut state = match SinkState::open(&events_dir) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e,
                    "event_sink: failed to open first log file; sink disabled");
                return;
            }
        };

        let mut flush_timer = tokio::time::interval(
            std::time::Duration::from_millis(FLUSH_INTERVAL_MS),
        );

        loop {
            tokio::select! {
                res = rx.recv() => {
                    match res {
                        Ok(event) => {
                            if let Err(e) = state.write_event(&event) {
                                tracing::warn!(error = %e,
                                    "event_sink: write failed; dropping event");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(lagged_n = n,
                                "event_sink: broadcast lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = flush_timer.tick() => {
                    let _ = state.flush();
                }
            }
        }

        let _ = state.flush();
    });
}

struct SinkState {
    dir: PathBuf,
    writer: BufWriter<File>,
    current_path: PathBuf,
    bytes_in_file: u64,
    max_bytes: u64,
}

impl SinkState {
    fn open(dir: &PathBuf) -> std::io::Result<Self> {
        let max_bytes = std::env::var("SPUR_EVENT_LOG_MAX_BYTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_BYTES);
        let path = rotated_path(dir);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir: dir.clone(),
            writer: BufWriter::with_capacity(FLUSH_BYTES, file),
            current_path: path,
            bytes_in_file: bytes,
            max_bytes,
        })
    }

    fn write_event(&mut self, event: &SpurEvent) -> std::io::Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.bytes_in_file += line.len() as u64 + 1;
        if self.bytes_in_file >= self.max_bytes {
            self.rotate()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;
        let new_path = rotated_path(&self.dir);
        let file = OpenOptions::new().create(true).append(true).open(&new_path)?;
        self.writer = BufWriter::with_capacity(FLUSH_BYTES, file);
        self.current_path = new_path;
        self.bytes_in_file = 0;
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn events_dir() -> PathBuf {
    PathBuf::from(".spur/events")
}

fn rotated_path(dir: &PathBuf) -> PathBuf {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    dir.join(format!("{pid}-{ts}.ndjson"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::events::{SpurEvent, SpurEventBody};
    use spur_acp::types::SessionId;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn writes_events_to_file() {
        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path().join("events");
        fs::create_dir_all(&dir).unwrap();

        let mut state = SinkState::open(&dir).unwrap();

        let event = SpurEvent {
            occurred_at: SystemTime::UNIX_EPOCH,
            seq: 42,
            body: SpurEventBody::TurnComplete { session: SessionId::from("s1") },
        };
        state.write_event(&event).unwrap();
        state.flush().unwrap();

        let contents = fs::read_to_string(&state.current_path).unwrap();
        let line = contents.lines().next().unwrap();
        let back: SpurEvent = serde_json::from_str(line).unwrap();
        assert_eq!(back.seq, 42);
    }

    #[tokio::test]
    async fn rotates_on_size_threshold() {
        let tmpdir = tempfile::tempdir().unwrap();
        let dir = tmpdir.path().join("events");
        fs::create_dir_all(&dir).unwrap();

        // Force a tiny max_bytes so one event triggers rotation.
        std::env::set_var("SPUR_EVENT_LOG_MAX_BYTES", "10");
        let mut state = SinkState::open(&dir).unwrap();

        let event = SpurEvent {
            occurred_at: SystemTime::UNIX_EPOCH,
            seq: 1,
            body: SpurEventBody::TurnComplete { session: SessionId::from("s1") },
        };
        state.write_event(&event).unwrap();
        state.write_event(&event).unwrap();
        state.flush().unwrap();

        let files: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert!(files.len() >= 2, "expected rotation; got {} file(s)", files.len());

        std::env::remove_var("SPUR_EVENT_LOG_MAX_BYTES");
    }
}
```

- [ ] **Step 12.2: Register module + add `tempfile` as dev-dep**

Edit `crates/spur-core/src/lib.rs`:

```rust
pub mod event_sink;
```

Edit `crates/spur-core/Cargo.toml` (under `[dev-dependencies]`):

```toml
tempfile = "3"
```

(If `tempfile` is already a workspace dep, use `tempfile = { workspace = true }`.)

- [ ] **Step 12.3: Run tests**

Run: `cargo test -p spur-core --lib event_sink -- --nocapture`
Expected: both tests PASS.

- [ ] **Step 12.4: Commit**

```bash
git add crates/spur-core/src/event_sink.rs \
        crates/spur-core/src/lib.rs \
        crates/spur-core/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(spur-core): S3 JSONL event sink module

Subscribes to broadcast, appends every SpurEvent as one line of JSON
to ~/.spur/events/{pid}-{ts}.ndjson. Flush on 64KB buffer or 100ms
tick. Rotates at SPUR_EVENT_LOG_MAX_BYTES (default 128MB). Log and
drop on write error — never crashes orchestrator on disk-full.

Two tests: write round-trip and rotation-on-size.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 13: Wire the sink into orchestrator startup

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — after broadcast channel creation

- [ ] **Step 13.1: Spawn the sink in `Orchestrator::new`**

In `orchestrator.rs:139` (where the channel is created), after the funnel spawn from Task 10:

```rust
let (event_tx, _) = broadcast::channel(4096);
let event_seq = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
let funnel = crate::event_funnel::spawn_funnel(event_tx.clone(), event_seq.clone());
// S3 — durable JSONL sink subscribes to the same broadcast.
crate::event_sink::spawn_sink(event_tx.subscribe());
let review_sink = ReviewSink::new();
```

- [ ] **Step 13.2: Build + run workspace tests**

Run: `cargo build --workspace && cargo test --workspace --lib 2>&1 | tail -10`
Expected: success.

- [ ] **Step 13.3: Smoke test — start spur, confirm sink creates a file**

This is a manual test, not a checkbox gate, but worth doing:

```bash
cargo run -p spur-cli -- watch  # or whatever the current entrypoint is
# In another terminal:
ls -la .spur/events/
```

Expected: at least one `{pid}-{ts}.ndjson` file exists and grows as events fire.

- [ ] **Step 13.4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): S3 spawn JSONL sink on orchestrator startup

Sink subscribes to broadcast and persists every SpurEvent to
.spur/events/{pid}-{ts}.ndjson. Enables post-hoc debugging,
deterministic replay, and external observability via file-tail
(PostHog forwarders, analyzers, future 2nd TUI).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase S5 — `_spur/*` ACP ExtNotification vocabulary

**Vocabulary scope for v1 (MCTS rev 2):**

| Variant | Wire format (ACP ExtNotification) | V1 emitter |
|---|---|---|
| `WorkerFileTouched` | `_spur/file_touched` | **Server-side synthesis from ToolCall** (Task 17). Explicit ACP emission also supported via Task 15 interpreter. |
| `WorkerProgress` | `_spur/progress_milestone` | **None in v1.** Wire format ready; no current agent emits it. Requires SPUR-aware worker OR future worker-side MCP. |
| `WorkerHeartbeat` | `_spur/heartbeat` | **None in v1.** Wire format ready; no current agent emits it. |

Arch doc §1.1 clarifies that MCP tools (including `report_progress`) are **brain-facing** — workers have no MCP connection. V1 therefore cannot bridge brain-called tools to worker-scoped events. See the "~~Task 17~~ removed" entry in the revision log for details.

**Phase 1 refinement compatibility:** All three variants use `executor_id` as the correlation key. Brain correlation is derivable by joining against `DelegationRequested` / `DelegationDispatched` events (which will carry `brain_session_id` after the refinement spec ships at `docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md`). Lineage already performs this join.

### Task 14: Add SpurEventBody variants

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`

- [ ] **Step 14.1: Locate `SpurEventBody` enum**

Opens at `crates/spur-acp/src/domain/events.rs:96`. Existing variants are BrainSpawned, AgentSessionReady, ..., DelegationCompleted, etc.

- [ ] **Step 14.2: Add three new variants**

Add at the end of the enum body (before the closing brace). Per arch doc §5.0 adjustment (c), each variant carries `brain_session_id` so subscribers can filter by session without joining against `DelegationRequested`:

```rust
/// Worker emitted `_spur/heartbeat` — periodic alive signal.
/// The TUI uses this to detect stalled workers.
WorkerHeartbeat {
    brain_session_id: SessionId,
    executor_id: String,
    /// Wall-clock at the worker; informational only.
    worker_ts: Option<String>,
},

/// Worker emitted `_spur/progress_milestone` — named checkpoint.
/// The TUI shows this in the executor card.
WorkerProgress {
    brain_session_id: SessionId,
    executor_id: String,
    name: String,
    /// Optional 0..=100 percentage.
    pct: Option<u8>,
},

/// Worker read or wrote a file. Either emitted explicitly by the
/// worker via `_spur/file_touched`, or synthesized by the
/// orchestrator from observed ToolCall events with a 200ms
/// de-duplication window.
WorkerFileTouched {
    brain_session_id: SessionId,
    executor_id: String,
    path: std::path::PathBuf,
    kind: FileTouchKind,
},
```

And add the `FileTouchKind` enum near the top (after `ReviewKind` around line 12):

```rust
/// Whether a file was read or written.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileTouchKind {
    Read,
    Write,
}
```

- [ ] **Step 14.3: Build**

Run: `cargo build --workspace 2>&1 | head -40`
Expected: success. If any exhaustive matches on `SpurEventBody` now complain, add `_ => ...` arms or explicit no-op arms for the new variants. Prefer explicit arms (for lineage, at minimum, decide: do these new events persist into lineage? For v1, no — add no-op arms).

- [ ] **Step 14.4: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git add -u
git commit -m "$(cat <<'EOF'
feat(spur-acp): S5 add WorkerHeartbeat/Progress/FileTouched variants

Three new SpurEventBody variants for the _spur/* ACP ExtNotification
vocabulary. FileTouchKind enum (Read/Write) for the path-kind field.
Added no-op match arms in exhaustive consumers — these events don't
persist to lineage in v1.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 15: Create the `_spur/*` interpreter module

**Files:**
- Create: `crates/spur-core/src/spur_ext_interp.rs`
- Modify: `crates/spur-core/src/lib.rs`

- [ ] **Step 15.1: Create the module**

```rust
//! `_spur/*` ExtNotification interpreter (Phase S5).
//!
//! Consumes `ExtNotificationPayload` from `NativeAcpConnection`'s
//! ext_notification channel, parses the method (e.g.
//! `_spur/progress_milestone`) and params JSON, and emits the
//! corresponding `SpurEventBody` variant through the event funnel.

use spur_acp::connection::ExtNotificationPayload;
use spur_acp::domain::events::{FileTouchKind, SpurEventBody};

use crate::event_funnel::FunnelHandle;

/// Consume an ExtNotificationPayload and, if it's a known `_spur/*`
/// method, synthesize + emit the matching SpurEventBody.
///
/// Caller supplies `brain_session_id` and `executor_id` from the worker's
/// delegation context — both are in-scope inside `run_one_worker_attempt`
/// where the per-worker consumer task is spawned.
pub fn interpret(
    payload: ExtNotificationPayload,
    brain_session_id: spur_acp::types::SessionId,
    executor_id: String,
    funnel: &FunnelHandle,
) {
    match payload.method.as_str() {
        "_spur/heartbeat" => {
            let worker_ts = payload
                .params
                .get("ts")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            funnel.emit(SpurEventBody::WorkerHeartbeat {
                brain_session_id,
                executor_id,
                worker_ts,
            });
        }
        "_spur/progress_milestone" => {
            let name = payload
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                tracing::warn!(
                    method = %payload.method,
                    "_spur/*: missing or empty 'name' param"
                );
                return;
            }
            let pct = payload
                .params
                .get("pct")
                .and_then(|v| v.as_u64())
                .and_then(|u| u8::try_from(u).ok());
            funnel.emit(SpurEventBody::WorkerProgress {
                brain_session_id,
                executor_id,
                name,
                pct,
            });
        }
        "_spur/file_touched" => {
            let path = match payload.params.get("path").and_then(|v| v.as_str()) {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    tracing::warn!("_spur/file_touched: missing 'path' param");
                    return;
                }
            };
            let kind = match payload.params.get("kind").and_then(|v| v.as_str()) {
                Some("read") => FileTouchKind::Read,
                Some("write") => FileTouchKind::Write,
                other => {
                    tracing::warn!(kind = ?other,
                        "_spur/file_touched: unknown 'kind'");
                    return;
                }
            };
            funnel.emit(SpurEventBody::WorkerFileTouched {
                brain_session_id,
                executor_id,
                path,
                kind,
            });
        }
        other => {
            tracing::debug!(method = other, "ignoring unknown _spur/* method");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use tokio::sync::broadcast;

    fn harness() -> (FunnelHandle, broadcast::Receiver<spur_acp::domain::events::SpurEvent>) {
        let (tx, rx) = broadcast::channel(64);
        let seq = Arc::new(AtomicU64::new(0));
        let h = crate::event_funnel::spawn_funnel(tx, seq);
        (h, rx)
    }

    fn test_brain() -> spur_acp::types::SessionId {
        spur_acp::types::SessionId::from("brain-1")
    }

    #[tokio::test]
    async fn progress_milestone_synthesizes_event() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/progress_milestone".into(),
                params: json!({"name": "tests_starting", "pct": 60}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        let event = rx.recv().await.unwrap();
        match event.body {
            SpurEventBody::WorkerProgress { brain_session_id, executor_id, name, pct } => {
                assert_eq!(brain_session_id, test_brain());
                assert_eq!(executor_id, "exec-1");
                assert_eq!(name, "tests_starting");
                assert_eq!(pct, Some(60));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn file_touched_parses_kind() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/file_touched".into(),
                params: json!({"path": "src/foo.rs", "kind": "write"}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        let event = rx.recv().await.unwrap();
        match event.body {
            SpurEventBody::WorkerFileTouched { brain_session_id, executor_id, path, kind } => {
                assert_eq!(brain_session_id, test_brain());
                assert_eq!(executor_id, "exec-1");
                assert_eq!(path, std::path::PathBuf::from("src/foo.rs"));
                assert_eq!(kind, FileTouchKind::Write);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_method_does_not_emit() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/no-such-thing".into(),
                params: json!({}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        // Nothing should appear on the broadcast. Wait a moment, then try
        // a non-blocking recv.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "unknown method should not emit");
    }
}
```

- [ ] **Step 15.2: Register in `lib.rs`**

```rust
pub mod spur_ext_interp;
```

- [ ] **Step 15.3: Run tests**

Run: `cargo test -p spur-core --lib spur_ext_interp -- --nocapture`
Expected: three tests PASS.

- [ ] **Step 15.4: Commit**

```bash
git add crates/spur-core/src/spur_ext_interp.rs crates/spur-core/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): S5 _spur/* ExtNotification interpreter

Consumes ExtNotificationPayload from spur-acp and translates known
_spur/* methods into SpurEventBody variants via the event funnel.
Supports _spur/heartbeat, _spur/progress_milestone, and
_spur/file_touched with bad-params warning and unknown-method debug.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 16: Wire interpreter into Orchestrator per-worker

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — worker spawn path

Each worker's `NativeAcpConnection` exposes `take_ext_notification_rx()` (verified at native.rs:637). After spawning the worker, the orchestrator should start a task that consumes the rx and calls `spur_ext_interp::interpret` for each payload, with the worker's `executor_id`.

- [ ] **Step 16.1: Find the worker-spawn site**

Run: `grep -n 'take_ext_notification_rx\|NativeAcpConnection::new\|fn spawn.*worker\|fn run_one_worker_attempt' crates/spur-core/src/orchestrator.rs`

Expected: at least one hit for `take_ext_notification_rx` (check if orchestrator already consumes it in another context — if so, reuse that loop; if not, add new).

- [ ] **Step 16.2: Add the consumer task**

Inside `run_one_worker_attempt` (spec says this function builds the worker connection), after the `NativeAcpConnection` is constructed and before it's handed to the ACP client, take the ext rx and spawn a consumer:

```rust
// S5 — consume _spur/* ExtNotifications from this worker and
// translate into SpurEvent variants via the funnel.
// brain_session_id is in scope in run_one_worker_attempt per Phase 1.
if let Some(mut ext_rx) = connection.take_ext_notification_rx() {
    let funnel_for_ext = funnel.clone();
    let executor_id_for_ext = executor_id.clone();
    let brain_session_for_ext = brain_session_id.clone();
    tokio::spawn(async move {
        while let Some(payload) = ext_rx.recv().await {
            crate::spur_ext_interp::interpret(
                payload,
                brain_session_for_ext.clone(),
                executor_id_for_ext.clone(),
                &funnel_for_ext,
            );
        }
    });
}
```

Exact placement depends on the current code shape. Key constraints:
1. must happen AFTER connection construction and BEFORE the connection is moved into subsequent code (otherwise `take_ext_notification_rx` — which takes `&mut self` — can't be called).
2. `brain_session_id` parameter of `run_one_worker_attempt` (Phase 1, `orchestrator.rs:2392-2394`) is a `&SessionId`; clone before moving into the spawned task.

- [ ] **Step 16.3: Build**

Run: `cargo build -p spur-core 2>&1 | tail -20`
Expected: success. If `connection` is moved before we can take the rx, restructure to take it earlier (e.g. right after `NativeAcpConnection::new`).

- [ ] **Step 16.4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): S5 consume _spur/* ExtNotifications per worker

After spawning a worker's NativeAcpConnection, take its
ext_notification_rx and run a consumer task that calls
spur_ext_interp::interpret with the worker's executor_id. Worker
progress, heartbeats, and file-touches now surface as SpurEvents
through the existing funnel → broadcast path.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

### ~~Task 17: Extend `report_progress` MCP tool~~ — REMOVED in rev 2

**Reason for removal** (MCTS cross-check against arch doc §1.1): the `report_progress` MCP tool is brain-facing. Workers do not connect to the MCP callback server; they communicate with the orchestrator only via ACP stdio. Extending `report_progress` to emit `WorkerProgress { executor_id, .. }` conflates two semantic layers — the brain is not an executor, so `executor_id` has no valid value at the call site.

**Implication for v1:** the `_spur/*` vocabulary is defined (Task 14) and interpreted (Task 15) as forward-compatible wire format, but the only v1 emitter is the server-side ToolCall synthesis path for `WorkerFileTouched` (Task 17, renumbered from Task 18).

**Future work** (new spec, not this plan):
- Brain-progress via `report_progress` → new variant `SpurEventBody::BrainProgress { session, .. }` (not `WorkerProgress`).
- Worker-progress via ACP — requires SPUR-aware agents that emit `_spur/progress_milestone` ExtNotifications directly.
- Worker-side MCP access — requires extending the MCP callback server to accept worker connections in addition to the brain.

### Task 17: Server-side file_touched synthesis from ToolCall events

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — AgentNotification handler

When the orchestrator receives an `AgentNotification` whose body is a `ToolCall` with name matching common file operations (`read_file`, `write_file`, `edit_file`, `Read`, `Edit`, `Write`), synthesize a `WorkerFileTouched` event. De-dupe with a 200ms window on `(executor_id, path, kind)`.

- [ ] **Step 17.1: Define de-dup helper (module-local)**

Near the orchestrator AgentNotification handling code, add:

```rust
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

/// De-dup key. TTL is 200ms per the spec.
#[derive(Hash, Eq, PartialEq, Clone)]
struct FileTouchKey {
    executor_id: String,
    path: std::path::PathBuf,
    kind: spur_acp::domain::events::FileTouchKind,
}

struct FileTouchDedup {
    last_seen: StdMutex<HashMap<FileTouchKey, std::time::Instant>>,
    ttl: std::time::Duration,
}

impl FileTouchDedup {
    fn new() -> Self {
        Self {
            last_seen: StdMutex::new(HashMap::new()),
            ttl: std::time::Duration::from_millis(200),
        }
    }
    /// Returns true if this (executor, path, kind) is fresh and should
    /// be emitted. Updates the last-seen map.
    fn should_emit(&self, key: &FileTouchKey) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.last_seen.lock().unwrap();
        // Garbage collect stale entries opportunistically.
        map.retain(|_, t| now.duration_since(*t) < self.ttl * 5);
        match map.get(key) {
            Some(last) if now.duration_since(*last) < self.ttl => false,
            _ => {
                map.insert(key.clone(), now);
                true
            }
        }
    }
}
```

Attach an instance to the `Orchestrator`:

```rust
pub struct Orchestrator {
    // ... existing fields ...
    file_touch_dedup: std::sync::Arc<FileTouchDedup>,
}
```

Initialize in `new()`:

```rust
file_touch_dedup: std::sync::Arc::new(FileTouchDedup::new()),
```

- [ ] **Step 17.2: Hook into AgentNotification emit path**

Find where the orchestrator receives a `SessionNotification` and emits `SpurEventBody::AgentNotification` (grep confirms this happens at orchestrator.rs:337, 500, 725). For EACH of these sites, BEFORE the `funnel.emit(SpurEventBody::AgentNotification { .. })`, inspect the notification for a file-op ToolCall and synthesize if present.

Add a helper:

```rust
/// If `notification` is a ToolCall matching a known file-op tool name,
/// synthesize a WorkerFileTouched event (subject to dedup).
/// `brain_session_id` comes from the per-worker scope via the same
/// bookkeeping that feeds the S5 interpreter (Task 16).
fn maybe_synthesize_file_touch(
    notification: &agent_client_protocol::SessionNotification,
    brain_session_id: &spur_acp::types::SessionId,
    executor_id: &str,
    dedup: &FileTouchDedup,
    funnel: &crate::event_funnel::FunnelHandle,
) {
    use agent_client_protocol::SessionUpdate;
    let (tool_name, path_opt) = match &notification.update {
        SessionUpdate::ToolCall { name, input, .. } => {
            let path = input.get("path")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from)
                .or_else(|| input.get("file_path")
                    .and_then(|v| v.as_str())
                    .map(std::path::PathBuf::from));
            (name.clone(), path)
        }
        _ => return,
    };
    let Some(path) = path_opt else { return };
    let kind = match tool_name.as_str() {
        "read_file" | "Read" => spur_acp::domain::events::FileTouchKind::Read,
        "write_file" | "Write" | "edit_file" | "Edit" => {
            spur_acp::domain::events::FileTouchKind::Write
        }
        _ => return,
    };
    let key = FileTouchKey {
        executor_id: executor_id.to_string(),
        path: path.clone(),
        kind: kind.clone(),
    };
    if dedup.should_emit(&key) {
        funnel.emit(spur_acp::domain::events::SpurEventBody::WorkerFileTouched {
            brain_session_id: brain_session_id.clone(),
            executor_id: executor_id.to_string(),
            path,
            kind,
        });
    }
}
```

The exact SessionUpdate field names (`name`, `input`, `path`, `file_path`) must match the `agent_client_protocol` crate's current shape. Confirm via `grep -rn 'SessionUpdate::ToolCall' crates/` or by reading the ACP crate's source.

- [ ] **Step 17.3: Call the synthesizer at every AgentNotification emit site**

Before each `funnel.emit(SpurEventBody::AgentNotification { session, notification })`, insert (at sites inside `run_one_worker_attempt` where `brain_session_id` is in scope):

```rust
if let Some(executor_id) = self.session_to_executor(&session) {
    maybe_synthesize_file_touch(
        &notification,
        brain_session_id,
        &executor_id,
        &self.file_touch_dedup,
        &self.funnel,
    );
}
funnel.emit(SpurEventBody::AgentNotification { session, notification });
```

`session_to_executor` is a placeholder: the orchestrator must already have a lookup from session id to executor id (used for correlation elsewhere). Find and reuse it; if no equivalent exists, skip synthesis for the brain's own session (only worker sessions map to executor ids).

For sites OUTSIDE `run_one_worker_attempt` (e.g., the two brain-session AgentNotification emits at orchestrator.rs:337 and :500), skip file-touch synthesis — the brain isn't a worker and has no executor_id. Only the worker-session site at :725 needs the synthesizer hook.

- [ ] **Step 17.4: Build + test**

Run: `cargo build -p spur-core && cargo test -p spur-core --lib 2>&1 | tail -10`
Expected: success.

- [ ] **Step 17.5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): S5 synthesize WorkerFileTouched from ToolCalls

Orchestrator inspects every AgentNotification for a ToolCall whose
name matches a known file-op tool (read_file/Read/write_file/Write/
edit_file/Edit). Extracts the path, emits WorkerFileTouched via the
funnel. 200ms de-dup window prevents double-count when a worker
emits _spur/file_touched explicitly alongside the tool call.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase S2+S3+S5 integration test

### Task 18: End-to-end integration test

**Files:**
- Create: `crates/spur-core/tests/event_stream_e2e.rs`

Verify the full pipeline: emit via funnel → see event on broadcast + in JSONL file, with correct seq monotonicity and content.

- [ ] **Step 18.1: Write the test**

```rust
//! End-to-end test: emit events via the funnel, verify broadcast
//! subscribers receive them in order AND they land in the JSONL sink.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::types::SessionId;
use spur_core::event_funnel::spawn_funnel;
use spur_core::event_sink::spawn_sink;
use tokio::sync::broadcast;

#[tokio::test(flavor = "current_thread")]
async fn funnel_plus_sink_round_trip() {
    // Isolate events dir.
    let tmpdir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(tmpdir.path()).unwrap();
    fs::create_dir_all(".spur/events").unwrap();

    let (bcast_tx, mut bcast_rx) = broadcast::channel(256);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx.clone(), seq);
    spawn_sink(bcast_tx.subscribe());

    for i in 0..10 {
        funnel.emit(SpurEventBody::TurnComplete {
            session: SessionId::from(format!("s-{i}")),
        });
    }

    // Drain broadcast.
    let mut seen_seqs = Vec::new();
    for _ in 0..10 {
        let ev = bcast_rx.recv().await.expect("recv");
        seen_seqs.push(ev.seq);
    }
    assert_eq!(seen_seqs, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

    // Give the sink time to flush (100ms flush interval).
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Find the JSONL file.
    let files: Vec<_> = fs::read_dir(".spur/events").unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("ndjson"))
        .collect();
    assert_eq!(files.len(), 1, "expected one JSONL file");

    let contents = fs::read_to_string(files[0].path()).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 10, "expected 10 lines in JSONL");

    let parsed: Vec<SpurEvent> = lines.iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let persisted_seqs: Vec<u64> = parsed.iter().map(|e| e.seq).collect();
    assert_eq!(persisted_seqs, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}
```

- [ ] **Step 18.2: Run the test**

Run: `cargo test -p spur-core --test event_stream_e2e -- --nocapture`
Expected: PASS. Both the broadcast subscriber and the JSONL file contain seq 0..10.

If it fails with "current dir change" issues (tests run in parallel), either serialize with a Mutex / use `--test-threads=1`, or set a custom events dir via a config option (requires a tiny refactor to `event_sink::events_dir` to read from an env var — acceptable extension).

- [ ] **Step 18.3: Commit**

```bash
git add crates/spur-core/tests/event_stream_e2e.rs
git commit -m "$(cat <<'EOF'
test(spur-core): S2+S3 end-to-end funnel + sink integration

Emits 10 events through the funnel, verifies:
(a) broadcast subscriber receives seq 0..10 in order,
(b) JSONL sink persists the same 10 events to disk,
(c) parsed-back seq matches after serde round-trip.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Post-implementation verification

### Task 19: Manual smoke test

- [ ] **Step 19.1: Run a real session with streaming diagnostics**

```bash
SPUR_LOG=debug cargo run -p spur-cli -- watch 2> /tmp/spur-stream.log
# Run a brain session that generates streaming output (e.g., ask kiro a question
# with a long answer). Let it complete.
```

- [ ] **Step 19.2: Verify streaming probes show healthy behavior**

```bash
rg 'streaming_probe' /tmp/spur-stream.log | tail -40
```

Expected observations:
- `A_session_notification` rows with `send_result = "ok"` for every chunk (no `"err"` post-fix).
- `B_dead_tx_swap` rows show `grace_elapsed_ms` field (S1.a fix is live).
- No `TUI broadcast lagged` warnings under light load. If any appear with `n > 0`, the subscriber is lagging — investigate.

- [ ] **Step 19.3: Verify JSONL sink produced a file**

```bash
ls -la .spur/events/
head -3 .spur/events/*.ndjson
```

Expected: at least one `.ndjson` file exists; first 3 lines parse as valid JSON with `seq`, `occurred_at`, `body` fields.

- [ ] **Step 19.4: Verify seq monotonicity in the file**

```bash
jq -c '.seq' .spur/events/*.ndjson | awk 'BEGIN{prev=-1} {if ($1 != prev+1) print "GAP at", NR, "prev", prev, "curr", $1; prev=$1}' | head -20
```

Expected: zero "GAP" lines. Every seq = previous + 1.

- [ ] **Step 19.5: Verify WorkerFileTouched events fire when worker reads/writes**

Run a delegation that writes a file. Then:

```bash
jq -c 'select(.body.WorkerFileTouched)' .spur/events/*.ndjson
```

Expected: one or more lines per write. Each with `executor_id`, `path`, `kind: "write"` (or `"read"` for reads).

---

## Files by task summary

| Task | File(s) touched |
|---|---|
| 1–2 | `crates/spur-acp/src/connection/native.rs` + tests/fixtures |
| 3–4 | `crates/spur-tui/src/components/react_trace.rs` |
| 5 | `crates/spur-tui/src/app.rs` |
| 6 | `crates/spur-core/src/orchestrator.rs` (line 139) |
| 7 | `crates/spur-tui/**` (Lagged arms) |
| 8 | `crates/spur-acp/src/domain/events.rs` |
| 9 | `crates/spur-core/src/event_funnel.rs` (new) + `lib.rs` |
| 10–11 | `crates/spur-core/src/orchestrator.rs` (struct + all emit sites) |
| 12–13 | `crates/spur-core/src/event_sink.rs` (new) + `lib.rs` + orchestrator wire |
| 14 | `crates/spur-acp/src/domain/events.rs` (variants) |
| 15 | `crates/spur-core/src/spur_ext_interp.rs` (new) + `lib.rs` |
| 16 | `crates/spur-core/src/orchestrator.rs` (per-worker consumer) |
| ~~17~~ | ~~removed in rev 2 (see revision log)~~ |
| 17 | `crates/spur-core/src/orchestrator.rs` (file_touch synthesis + dedup) |
| 18 | `crates/spur-core/tests/event_stream_e2e.rs` (new) |
| 19 | manual smoke — no code |

---

## Phase checkpoint tags (recommended)

After each phase, tag for easy rollback:

```bash
# After Phase S1 (Task 7 complete):
git tag spurevent-stream-s1-done
# After Phase S2 (Task 11 complete):
git tag spurevent-stream-s2-done
# After Phase S3 (Task 13 complete):
git tag spurevent-stream-s3-done
# After Phase S5 (Task 17 complete):
git tag spurevent-stream-s5-done
# After integration test (Task 18):
git tag spurevent-stream-complete
```

---

## Notes and gotchas collected during planning

- **Buffer vs. grace window (S1.a):** the spec preferred the buffer pattern but it has a session-mixing issue — stragglers from turn N would replay into turn N+1's receiver. Plan uses the spec's alternative (grace window).
- **Grace window latency vs arch doc budgets (S1.a — MCTS rev 2):** the grace window adds up to 1s to the old worker's ACP-thread teardown. Arch doc §2 budgets T5b (worker spawn) at 1-3s. Since the new worker's spawn runs on a separate thread in parallel with the old worker's grace window, retry latency is unaffected. The serial cost is only on the old worker's teardown — at ~1% of T5c (worker execution), it's noise.
- **Emit funnel channel hop (Pitfall P1):** we chose Option B (mpsc → singleton task) for strict ordering. Cost is one mpsc hop per emit; at 1600 evt/s this is ~16ms/s of CPU — negligible.
- **S2 funnel preserves arch doc §1.4 invariant (MCTS rev 2):** `register_gate()` MUST be called BEFORE emitting `ExecutorReviewRequested`. Orchestrator code does this synchronously at the call site; the funnel's mpsc→task→broadcast path only adds latency to when the TUI sees the event — the gate is already registered by the time the funnel's emit completes. TUI's subsequent `SubmitReview` always finds the gate.
- **S2 funnel preserves "exactly one DelegationCompleted" (arch doc §6.1):** `finalize()` remains a single call site; the funnel changes HOW emit happens, not how many times.
- **JSONL sink backpressure — v1 accepts best-effort durability (MCTS rev 2):** the spec's design has a hot-path mpsc bridge with threshold-based emit-funnel backpressure to guarantee durability. V1 ships the simpler direct-broadcast-subscribe design. Consequence: if the sink lags (disk I/O spike), events are dropped from the log and logged via `RecvError::Lagged`. Events still reach the TUI and lineage via their own broadcast subscriptions. Phase 2 upgrade path is documented; upgrade when a concrete durability SLA exists.
- **Pattern-match breakage after seq field (Task 8):** some projection code may pattern-match `SpurEvent { occurred_at, body }` exhaustively. Use `{ seq: _, occurred_at, body }` or destructure with `..` where semantically correct.
- **ExtNotification crate API shape (Task 15):** `ExtNotificationPayload` is defined in `crates/spur-acp/src/connection/` — inspect the actual struct before writing the interpreter. The plan assumes `{ method: String, params: serde_json::Value }` based on grep findings.
- **ACP ToolCall field names (Task 17):** the `input` / `path` / `file_path` conventions differ by agent. Plan handles both; may need to extend as new agents ship.
- **Events directory path** is hardcoded to `.spur/events` (cwd-relative, same convention as `.spur/logs`). If global config has a different root, update `event_sink::events_dir()`.
- **Task 14 vocabulary scope (MCTS rev 2):** the `SpurEventBody::WorkerHeartbeat`, `WorkerProgress`, and `WorkerFileTouched` variants are all defined. Only `WorkerFileTouched` has a v1 emitter (server-side synthesis in Task 17). `WorkerHeartbeat` and `WorkerProgress` are forward-compatible wire formats — no production emitter exists until SPUR-aware agents emit `_spur/heartbeat` or `_spur/progress_milestone` via ACP directly, or until worker-side MCP access is introduced in a future spec.
- **Phase 1 refinement compatibility (MCTS rev 2):** the new Worker* variants use `executor_id: String` as the correlation key. Brain correlation is available by joining against `DelegationRequested` / `DelegationDispatched` (which carry `brain_session_id` after the refinement spec at `docs/superpowers/specs/2026-04-14-brain-worker-refinement-design.md` ships). Lineage does this join today for worker_session → brain_session mapping.
- **Darwin-first cross-platform (MCTS rev 2):** Task 1's fixture uses a bash script. SPUR is darwin-first (macOS 25.1.0); Windows CI is not in scope. If Linux CI is added later, the bash fixture ports trivially.
