# ACP Notification Bus — Eliminate per-turn channel race

**Date:** 2026-04-14
**Status:** Approved for planning
**Area:** `spur-acp` transport, `spur-core` orchestrator
**Related prior work:** `2026-04-13-realtime-streaming-diagnosis-design.md`, `2026-04-14-spurevent-stream-backbone-design.md`

## Problem

`claude-code-acp` advertises ~144 slash commands via standard ACP `session/update { sessionUpdate: "available_commands_update" }`, but none of them appear in the spur TUI command popup. Codex and kiro are unaffected — codex because it carries a static `/compact` in `seed_agents.toml`, kiro because it routes its commands through a separate vendor-extension channel.

Log probes at `crates/spur-acp/src/connection/native.rs:1184` (the `A_session_notification` site) recorded **304 dropped notifications** today across 2 claude-code-acp sessions:

| variant | dropped |
|---|---|
| tool_call_update | 115 |
| tool_call | 114 |
| agent_message_chunk | 41 |
| agent_thought_chunk | 23 |
| user_message_chunk | 7 |
| available_commands_update | 4 |

So missing commands are the most visible symptom of a wider bug: **any `SessionNotification` whose callback runs after the per-turn `notification_tx` has been swapped to `dead_tx` is silently discarded.** In live sessions this corrupts tool-call traces and truncates streamed messages.

## Root cause

`acp_thread_main` (native.rs:788–1074) gives `SpurAcpClientDynamic` a `Rc<RefCell<UnboundedSender<SessionNotification>>>` that is swapped per `Prompt` / `LoadSession`:

1. `LoadSession` arm (native.rs:1006–1074): installs a fresh `(tx, rx)`, calls `connection.load_session(req).await`, runs a grace loop, then swaps to `dead_tx` before handing `rx` back to the caller.
2. `Prompt` arm (native.rs:871–960): same shape.

The grace loop (native.rs:1026–1038, mirrored at the prompt end) gates the swap on `last_notification_at`, which is stamped **inside** `session_notification` (native.rs:1201). Under `LocalSet` scheduling, the SDK parses the wire and *schedules* the callback; the callback runs later. When the grace loop queries `last_notification_at` between SDK-parse and callback-run, it reads a stale timestamp, declares "idle long enough", and swaps to `dead_tx`. The pending callback then lands on `dead_tx` and errs.

Observed example (first claude-code-acp `available_commands_update`):

- `00:10:51.054` — wire recv (SDK)
- `00:10:51.095168` — `NativeAcpConnection: load_session completed` (after dead_tx swap)
- `00:10:51.095473` — `A_session_notification … send_result="err"` (pending callback runs on dead_tx)

The race is not limited to `available_commands_update`; it fires on trailing chunks at `prompt_end` too. The grace window's invariant — "if no callback has run for 250ms, the SDK has nothing queued" — is false. Nothing the current code path inspects can prove the SDK's internal queue is drained.

## Design

Replace per-turn mpsc channels + grace window + `dead_tx` swap with a single connection-scoped broadcast. `SessionNotification` callbacks always have a live sink; ownership of the receiver moves out of the prompt-return contract and into the orchestrator's event bus.

### Components

**spur-acp / connection/native.rs**

- Inside `acp_thread_main`, construct `let (session_notif_tx, _initial_sub) = tokio::sync::broadcast::channel::<SessionNotification>(1024);` once, alongside the existing `ext_notification_tx` (native.rs:135).
- `SpurAcpClientDynamic` holds `session_notif_tx: broadcast::Sender<SessionNotification>`. Its `session_notification` impl calls `self.session_notif_tx.send(args)` and logs at the existing `A_session_notification` probe site. Never swapped.
- Expose `NativeAcpConnection::subscribe_session_notifications(&self) -> broadcast::Receiver<SessionNotification>`. Orchestrator calls this once per connection on startup.
- `NativeAcpConnection::prompt(req)` signature changes from `Result<Pin<Box<dyn Stream<Item = SessionNotification>>>>` to `Result<PromptResponse>` (returns only the agent's terminal response; notifications flow through the broadcast).
- `NativeAcpConnection::load_session(req)` likewise returns `Result<()>` and relies on the broadcast for any replayed history.
- Delete: `last_notification_at` RefCell, the grace loops at native.rs:901–960 and 1026–1051, the `dead_tx` swaps, the `_initial_rx` pattern at native.rs:788, and the "S1.a" / "H5" comment machinery.

**spur-core / orchestrator.rs**

- On connection setup (brain or worker), spawn a task that consumes `connection.subscribe_session_notifications()` and emits `SpurEventBody::AgentNotification { session: spur_session_id, notification }` for each message. This replaces the per-prompt stream drain at orchestrator.rs:345–363 and the per-load history drain at orchestrator.rs:515–526.
- `TurnComplete` remains emitted synchronously when `connection.prompt().await` returns (orchestrator.rs ~line 800), matching current semantics.
- Session-history replay from disk (orchestrator.rs:530–538) is unchanged — it fires only when no history came from the wire, which is still observable as "no AgentNotification arrived between load and the `TurnComplete` barrier".

**spur-tui**

- No changes required. The TUI already listens on `SpurEventBody::AgentNotification` and filters by `session_id` (session_detail.rs:872). Commands, tool calls, and message chunks all flow through unchanged.

### Broadcast capacity and lag

`broadcast::channel(4096)` — sized above the invariant floor (anchor `3ff4e86`) to absorb bursty history replay from `load_session`. If the subscriber falls behind, `broadcast::Receiver::recv()` returns `RecvError::Lagged(skipped)`; the orchestrator task logs a warning and continues. Lag is not expected under normal load (TUI drain rate >> agent emission rate); the log is a diagnostic for future regressions, not part of the hot path.

### Error and shutdown handling

- Dropping the connection drops `session_notif_tx`; any outstanding `Receiver` yields `RecvError::Closed` on next recv. The orchestrator task interprets this as "connection dead" and exits.
- Agent subprocess crash: unchanged — the ACP thread exits, the `cmd_tx` half drops, and `NativeAcpConnection` health goes to `Failed`.

## Tests

1. **Regression test (new, `crates/spur-acp/tests/post_load_notification_propagates.rs`).** Spin up a mock ACP agent that delays `available_commands_update` until *after* responding to `session/load`. Today's code drops it (probe log reports `send_result="err"`); the fixed code must propagate it. Assert via subscribed broadcast receiver.
2. **Prompt-end trailing chunk test.** Agent emits one final `agent_message_chunk` after the `session/prompt` response. Assert the chunk is delivered through the orchestrator event bus.
3. **Orchestrator fan-out test.** Given two concurrent sessions with the same connection, assert `AgentNotification` events carry the correct `spur_session_id` tag for each.
4. **Existing tests** (`crates/spur-tui/tests/session_update_handling.rs`, `crates/spur-tui/tests/command_registry.rs`) continue to pass unchanged — they operate on `SessionNotification` values, not on the transport plumbing.

## Out of scope

- Reworking `SpurEventBody` variants (keep `AgentNotification` as-is).
- Telegram / dashboard views (they subscribe to the same event bus).
- `cli_wrap_adapter` / `stream_json_adapter` — these already use simpler mpsc pipes and don't exhibit the bug. Keep them unchanged.
- Adding a `claude-code-acp` static-command fallback in `seed_agents.toml` — rejected; hides the popup symptom while leaving the trace-corruption bug in place.

## Verification plan

1. Write the regression test; confirm it fails against current `main`.
2. Implement the broadcast migration in `spur-acp`, then `spur-core`.
3. `cargo test -p spur-acp -p spur-core -p spur-tui` — all green.
4. Live run: `spur run` with `claude-code-acp`, open `/` popup, confirm all advertised commands appear.
5. Grep a fresh `.spur/logs/spur.log.*` for `send_result="err"` — expect zero occurrences.
