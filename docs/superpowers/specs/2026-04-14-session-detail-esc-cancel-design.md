# Session Detail — Esc to Cancel In-Flight Stream

**Date:** 2026-04-14
**Scope:** `crates/spur-tui/src/views/session_detail.rs`, `crates/spur-tui/src/action.rs`, `crates/spur-tui/src/app.rs`, `crates/spur-core/src/orchestrator.rs`, `crates/spur-acp/src/domain/events.rs`
**Status:** Design approved; awaiting plan.

## Problem

Users can interrupt a streaming turn today only by typing a `!…` message (`interrupt: true` → `SendMessage` path → orchestrator calls `Connection::cancel` and queues the new message). There is no way to *purely halt* an in-flight response without also sending something. This design adds `Esc` as a pure-cancel keybinding on `SessionDetailView`.

## Goals

1. `Esc` halts an in-flight stream through the existing ACP cancel infrastructure, without tearing down the session when the transport is ACP-native.
2. Feedback is acknowledged within the 100ms perception threshold and persists until `TurnComplete` arrives (the cancel has observable latency: 0–5000ms).
3. The feature surfaces honestly across all four transports (ACP-soft vs. process-kill).
4. No regression of existing `Esc` behavior (NavigateBack on empty input, popup dismiss, auth banner clear, etc.).

## Non-goals

- Adding a `BrainStatus::Cancelling` enum variant. Cancellation is a view-local transient; a local `bool` suffices.
- A `/cancel` slash command. Redundant with `Esc`.
- `Ctrl-C` as an additional alias. Esc alone is sufficient and matches Claude Code's mental model.
- Retroactively hiding or trimming agent chunks that arrive post-cancel. Destructive and surprising.
- Distinguishing "agent-honored cancel" from "5s force-timeout" in the event stream. Current `TurnComplete` is enough for v1; the orchestrator already logs the timeout path.

## Transport-aware cancel semantics (critical context)

`AgentConnection::cancel(session_id)` is polymorphic across transports. Only one transport performs a true ACP `session/cancel` notification; the others SIGTERM/SIGKILL the subprocess. This is existing behavior inherited from the `!…` interrupt path.

| Transport | `cancel()` implementation | Preserves session? |
|---|---|---|
| `TransportKind::Acp` (`NativeAcpConnection`) | `CancelNotification::new(session_id)` → `ClientSideConnection::cancel(..)`; sends ACP `session/cancel` over the live JSON-RPC connection | ✅ Yes — session context intact |
| `TransportKind::Stdio` (`StdioAdapter`) | `kill -TERM <pid>` to the subprocess | ❌ Process dies |
| `TransportKind::CliWrap` (`CliWrapAdapter`) | `child.kill()`; `self.child = None` (one-shot anyway) | ❌ Process dies |
| `TransportKind::StreamJson` (`StreamJsonAdapter`) | `child.kill()`; `self.child = None` | ❌ Process dies |

This design does not change that polymorphism. It surfaces the difference in the UI so the user understands what happened (see "Feedback on Esc press" below).

## Keybinding decision: overload `Esc`

`Esc` is taken today — on empty input it emits `NavigateBack` (session_detail.rs:739); with a popup open it dismisses (:610–614); with an auth banner it clears (:555–557). We overload with a strict priority chain that preserves every existing behavior:

```
if stream_in_flight && !cancelling_in_flight:
    dispatch Action::CancelStream { session }
else:
    fall through to existing Esc handling
```

The cancel path short-circuits **before** existing handlers. Once cancellation is in flight, a second `Esc` resumes the pre-existing semantics — first press cancels, second press navigates back. This avoids the common failure mode "did it register? *press Esc again* — now I'm on Dashboard."

## Design

### State additions on `SessionDetailView`

```rust
/// True from the first agent chunk of the current turn until TurnComplete.
stream_in_flight: bool,

/// True from the moment we dispatch `Action::CancelStream` until TurnComplete.
/// Overrides the normal streaming status label with `cancelling…`.
cancelling_in_flight: bool,

/// How the active brain's transport handles cancel. Populated from
/// `SpurEventBody::AgentSessionReady.cancel_mode`. `None` until we know.
cancel_mode: Option<spur_acp::CancelMode>,
```

Transitions (all in `handle_spur_event`):

| Event | `stream_in_flight` | `cancelling_in_flight` |
|---|---|---|
| `AgentMessageChunk` / `AgentThoughtChunk` (first of turn) | set to `true` | no change |
| `TurnComplete` | set to `false` | set to `false` |
| `AgentSessionReady` | no change (but populates `cancel_mode`) | no change |

`cancelling_in_flight` is set only in `handle_key_inner` at the moment we return `Action::CancelStream`.

### New `CancelMode` enum

In `crates/spur-acp/src/domain/events.rs` (or the adjacent types module, wherever best fits existing imports):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelMode {
    /// ACP `session/cancel` notification. Agent honors soft-cancel; session stays live.
    AcpSoft,
    /// Subprocess is SIGTERM/SIGKILLed. Session ends; next message respawns.
    ProcessKill,
}
```

Mapping in `orchestrator.rs` (where `build_connection_from_transport` lives, or alongside it):

```rust
fn cancel_mode_for(transport: TransportKind) -> CancelMode {
    match transport {
        TransportKind::Acp => CancelMode::AcpSoft,
        TransportKind::Stdio
        | TransportKind::CliWrap
        | TransportKind::StreamJson => CancelMode::ProcessKill,
    }
}
```

### Extend `SpurEventBody::AgentSessionReady`

Add a `cancel_mode: CancelMode` field. The orchestrator computes it from `AgentConfig::transport` at the point it emits `AgentSessionReady` (fresh or resumed). The TUI view stores it on receipt.

### New `Action` variant

In `crates/spur-tui/src/action.rs`:

```rust
/// Halt an in-flight stream via the ACP cancel path. Emitted by
/// `SessionDetailView` on `Esc` when `stream_in_flight && !cancelling_in_flight`.
CancelStream { session: SessionId },
```

### New `UserInput` variant

In `crates/spur-tui/src/app.rs`:

```rust
/// Halt an in-flight stream. Orchestrator matches this inside its streaming
/// `select!` loop and calls `connection.cancel(..)`.
CancelStream { session: SessionId },
```

`Action::CancelStream` maps 1:1 to `UserInput::CancelStream` in app.rs's action dispatcher.

### Orchestrator: new `select!` arm

In `crates/spur-core/src/orchestrator.rs` inside the streaming loop at ~:663–742, add a fourth branch:

```rust
Some(InteractiveInput::CancelStream { session }) = user_input_rx.recv(), if matches_current(&session) => {
    let _ = b.connection.cancel(&b.acp_session_id).await;
    cancel_deadline = Some(
        tokio::time::Instant::now() + std::time::Duration::from_secs(5),
    );
}
```

(The exact `select!` shape may differ — this is conceptually the same as the `interrupt: true` arm at :710–715 **minus** the `pending_messages.push_back(..)` call. The point is: cancel without enqueuing a follow-on message.)

**Outer loop handling** (when no stream is in flight): the top-level `user_input_rx.recv()` also matches `InteractiveInput::CancelStream` and drops it with a debug log. This protects against the race where the user presses `Esc` just as `TurnComplete` arrives at the TUI.

### Feedback on Esc press

All three feedback channels fire at the moment `Action::CancelStream` is dispatched:

1. **Trace entry** (transport-aware text):
   - `CancelMode::AcpSoft`: `"⏹ Cancellation requested — waiting for agent…"`
   - `CancelMode::ProcessKill`: `"⏹ Stopping agent (process will restart on next message)"`
   - If `cancel_mode` is `None` (haven't seen `AgentSessionReady` yet — rare race): fall back to the generic `"⏹ Cancellation requested"`.
2. **InputBar status label:** `[{agent}: cancelling…]`, persistent until `TurnComplete` (the label is always visible regardless of scroll position — this is the ack that satisfies the perception-threshold criterion).
3. **StatusBar hint:** when `stream_in_flight && !cancelling_in_flight`, render `Esc to stop` in the status bar. Disappears once cancelling or once the stream ends. Discoverability for a new keybinding.

### Key routing in `session_detail.rs`

The cancel check sits at the **top** of `handle_key_inner` so it wins over every existing Esc handler. Skeleton:

```rust
fn handle_key_inner(&mut self, key: KeyEvent) -> Option<Action> {
    // Existing: dismiss auth banner, toggle plan mode, etc.

    // NEW: Esc-to-cancel takes priority when a stream is in flight.
    if matches!(key.code, KeyCode::Esc)
        && self.stream_in_flight
        && !self.cancelling_in_flight
    {
        self.cancelling_in_flight = true;
        self.push_cancel_note();      // helper that consults self.cancel_mode
        return Some(Action::CancelStream { session: self.session_id.clone() });
    }

    // …existing body unchanged…
}
```

This sits **after** auth-banner clearing (which runs on any keystroke) but **before** every other `Esc` branch (popup dismiss, NavigateBack, etc.).

## Edge cases

1. **Stray CancelStream after `TurnComplete`.** The outer orchestrator loop matches it and logs-and-drops. No-op. ✅
2. **Second Esc while cancel pending.** `cancelling_in_flight == true` makes the cancel branch false; falls through to existing Esc. Typical outcome: popup dismissed if any, else NavigateBack. The in-flight cancel continues to completion on the orchestrator side independently of the view. ✅
3. **Cancel with no brain yet (lazy-spawn state).** `stream_in_flight == false`, so Esc never triggers CancelStream. ✅
4. **Force-timeout at 5s.** Stream ends, `TurnComplete` fires, both flags clear. Indistinguishable from agent-honored cancel in v1. Acceptable — the user sees the cancel finish; no user-facing lie.
5. **Typing during cancel.** Existing `pending_messages` queue in orchestrator handles it (orchestrator.rs:727–731). New message delivered after `TurnComplete`. No change needed. ✅
6. **`AgentSessionReady` arrives after a cancel is pressed** (very narrow race). `cancel_mode` is `None` at the moment; we use the generic fallback text. Next cancel uses the correct text. Acceptable — this race is sub-millisecond and the fallback is honest.
7. **`InputBar` status label collision.** The existing `set_brain_status` path writes the streaming label on `BrainStatus::Streaming`. When `cancelling_in_flight` is true, the view must suppress/override that write with `[{agent}: cancelling…]` until `TurnComplete`. Implemented as a check inside the view's status-label rendering (before forwarding to `input_bar.set_status`).

## Testing plan

**Unit — `session_detail.rs`:**
- `esc_with_stream_in_flight_emits_cancel_stream` — seeds `stream_in_flight = true`, presses Esc, asserts `Action::CancelStream` returned and `cancelling_in_flight == true`.
- `esc_when_cancelling_in_flight_falls_through` — seeds both flags true, presses Esc, asserts the action is NOT `CancelStream` (either NavigateBack or None depending on input state).
- `esc_without_stream_preserves_navigate_back` — baseline: no flags set, empty input, asserts `Action::NavigateBack`.
- `turn_complete_clears_both_flags` — seeds both true, dispatches `TurnComplete` via `handle_spur_event`, asserts both cleared.
- `agent_session_ready_populates_cancel_mode` — dispatches `AgentSessionReady` with `CancelMode::AcpSoft`, asserts `self.cancel_mode == Some(AcpSoft)`.
- `cancel_note_text_is_transport_aware` — three cases: AcpSoft, ProcessKill, None; asserts the trace entry text matches the spec.

**Unit — orchestrator:**
- `cancel_stream_calls_connection_cancel_and_sets_deadline` — uses a mock `AgentConnection` that records `cancel()` invocations; dispatches a prompt to start streaming; sends `InteractiveInput::CancelStream`; asserts mock's `cancel()` was called and stream breaks within 5s.
- `cancel_stream_does_not_enqueue_pending_message` — same setup; asserts `pending_messages` is empty after the cancel path (proves this is not the `!…` path).
- `stray_cancel_stream_outside_turn_is_dropped` — sends `CancelStream` with no active stream; asserts no side effects.

**Integration (deferred, optional):** drive `NativeAcpConnection` against a fake ACP server that yields 100 chunks/sec; assert `session/cancel` JSON-RPC notification observed on the wire after Esc, followed by stream termination.

## Files touched (summary)

- `crates/spur-acp/src/domain/events.rs` — add `CancelMode` enum; add field to `SpurEventBody::AgentSessionReady`.
- `crates/spur-core/src/orchestrator.rs` — `cancel_mode_for(transport)` helper; populate `cancel_mode` on `AgentSessionReady`; new `select!` arm for `InteractiveInput::CancelStream`; outer-loop drop arm.
- `crates/spur-tui/src/action.rs` — `Action::CancelStream` variant.
- `crates/spur-tui/src/app.rs` — `UserInput::CancelStream` variant; action dispatcher maps one to the other.
- `crates/spur-tui/src/views/session_detail.rs` — new fields; Esc handler branch; status-label override; `handle_spur_event` updates for `AgentMessageChunk`/`AgentThoughtChunk`/`TurnComplete`/`AgentSessionReady`; helper `push_cancel_note()`.
- `crates/spur-tui/src/components/status_bar.rs` — render `Esc to stop` hint when stream in-flight (view passes a new `StatusBarProps` field).
- Test files for the two unit suites above.

## Open questions

None. All UX decisions made during brainstorming (see chat log): overload Esc (Option A), persistent label + system note + discoverability hint (Option B+hint), transport-aware feedback (Option 3).
