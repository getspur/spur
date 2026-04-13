# Realtime streaming diagnosis — kiro-cli via NativeAcpConnection

**Date:** 2026-04-13
**Status:** Draft — verification step only

## Problem

Chat messages streamed from kiro-cli into `SessionDetailView` do not render in
real time, and the message appears "broken at the end" — the tail looks
truncated or visually split from the body of the reply.

## Scope

This is a **measurement-only** patch. No user-visible behavior changes. We add
structured trace logs at three pipeline boundaries so a single streaming turn
produces enough data to localize the bottleneck.

Fix design is deferred to a follow-up spec once the log is read.

## Pipeline under test

```
kiro-cli acp
  → agent_client_protocol::ClientSideConnection
    → SpurAcpClientDynamic::session_notification   (A)
      → unbounded mpsc
        → prompt() stream → orchestrator loop      (B)
          → broadcast<SpurEvent>
            → TUI app select
              → react_trace.append_message         (C)
                → 33 ms tick re-render
```

## Hypotheses the log must disambiguate

- **H1** kiro-cli emits coarse chunks (per line / paragraph / whole message).
- **H1′** Drain-then-render coalescing: `app.rs:521-573` drains **all** pending
  crossterm + spur events before each render. A burst of N chunks becomes a
  single frame update instead of progressive typewriter output — technically
  each chunk arrives within 33 ms, but perceived as chunky.
- **H2** `AgentMessageChunk` sequences are interleaved with `ToolCall`,
  `ToolCallUpdate`, or `Plan` updates, which causes `react_trace` to push a
  **new** `AgentMessage` entry instead of appending to the previous one — the
  visual "break at end" symptom.
- **H4** `broadcast::Receiver::Lagged(_)` is silently swallowed in the TUI
  select loop, potentially dropping notifications under burst load.
- **H5** Dead-tx race. At `native.rs:628-633`, when `prompt()` returns, the
  per-prompt `notification_tx` is replaced by a throwaway `dead_tx` and the
  original sender is dropped. If the ACP library's runtime schedules the
  `prompt_response` future to resolve before its final queued
  `session_notification` callbacks run, those trailing notifications land on
  `dead_tx`. The send error at `native.rs:779` is **swallowed**
  (`let _ = self.notification_tx.borrow().send(args);`), so the trailing text
  disappears silently. Mechanistically matches "message is break in the end."

## Instrumentation

Six `tracing::debug!` / `tracing::warn!` sites. Each entry carries a
`streaming_probe = true` field so the log can be filtered cleanly.

### (A) `crates/spur-acp/src/connection/native.rs` — `session_notification`

Replace the swallowed-error forward at line 779 with a logged version:
- `variant` — the `SessionUpdate` discriminant name (`AgentMessageChunk`,
  `AgentThoughtChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, …)
- `text_len` — length of extracted text when variant is a text chunk, else 0
- `session` — session id
- `send_result` — `"ok"` or `"err"` (the latter is H5 confirmed)

**Rationale:** H5 detection requires knowing whether the send succeeded. The
current code silently drops send errors.

### (B) `crates/spur-acp/src/connection/native.rs` — dead-tx swap

At the `*notification_tx.borrow_mut() = dead_tx` line (currently 633 and 691),
log a single event with `session`, `site = "prompt_end"` or `"load_session_end"`,
and the current monotonic timestamp. This establishes a **swap time** that
later probe (A) entries can be compared against to catch trailing notifications.

### (C) `crates/spur-core/src/orchestrator.rs` — streaming emit

At the `self.emit(SpurEvent::AgentNotification { … })` call in the prompt
streaming loop (~line 520), log `variant`, `text_len`, `since_prompt_ms`
(monotonic millis since prompt was submitted).

### (D) `crates/spur-tui/src/views/session_detail.rs` — trace append

Inside the `AgentMessageChunk` branch of `handle_spur_event`, just before
`append_message`, log:
- `text_len`
- `prev_entry_kind` — discriminant of `self.react_trace.entries.last()` (or
  `"none"` if empty)
- `will_continue` — `true` if the previous entry is `AgentMessage`, else `false`

Add a small helper on `ReactTrace` to expose the last entry's kind name.

### (E) `crates/spur-tui/src/app.rs` — broadcast lag warning

At the `RecvError::Lagged(_)` arm (~line 532), change the `{}` body to
`tracing::warn!(streaming_probe = true, lagged_n = n, "TUI broadcast lagged")`.

### (F) `crates/spur-tui/src/app.rs` — per-frame drain count

In the main loop (~line 521-573), count how many events the Phase-2 + Phase-3
drains consume between renders. If `dirty` is true, log `events_drained`
before `terminal.draw(...)`. This detects H1′.

## Runbook

1. Build: `cargo build -p spur-cli`.
2. Tail logs to a file: `SPUR_LOG=debug target/debug/spur …  2> /tmp/spur.log`
   (the existing log file sink also writes to `~/.spur/logs/`).
3. Send one message to a kiro session. Wait for the full reply.
4. Filter: `rg streaming_probe /tmp/spur.log` — paste the output back.

## Decision tree — what the log answers

| Observation | Hypothesis confirmed | Fix sketch |
|---|---|---|
| Any `(A)` row with `send_result = "err"`, or any `(A)` row with timestamp **after** a matching `(B)` swap row for the same session | **H5** dead-tx race | Keep the per-prompt sender alive until the orchestrator signals drain-complete (oneshot back to the ACP thread). ~50 LOC in `native.rs`. |
| `(D)` rows with `will_continue = false` **after** the first chunk of a turn | **H2** interleave split | `append_message` walks backwards past non-message entries to find the last `AgentMessage` from the same agent within the turn, appends there. ~30 LOC in `react_trace.rs`. |
| `(F)` `events_drained` regularly > 5 during streaming | **H1′** drain-coalescing | Cap drain to N events per iteration or render per-event below a rate threshold. ~10 LOC in `app.rs`. |
| Deltas between consecutive `(A)` rows mostly > 300 ms intra-turn | **H1** kiro coarse cadence | Out of our control; document as kiro-cli limitation. |
| Any `(E)` warn line | **H4** broadcast lag | Bump channel from 256 to 4096 in orchestrator. ~1 LOC. |

Priority-order for applying fixes if multiple hypotheses confirm:
**H5 > H2 > H1′ > H4 > H1.** H5 is silent data loss; H2 is visual correctness;
H1′ is perceived smoothness; H4 is defensive; H1 is not ours to fix.

## Non-goals for this patch

- Changing kiro-cli invocation flags.
- Modifying `react_trace` aggregation logic.
- Adjusting the broadcast channel size.

All of the above become candidates in the follow-up design after reading the
log.

## Acceptance

- `cargo build` succeeds.
- One streaming turn produces rows from sites (A), (B), (C), (D), (F); (E) is
  runbook-dependent (only fires under burst load).
- No behavior change in the TUI (same messages, same render).
- Send-error handling at (A) is no longer silent — errors are logged but still
  swallowed at the Result level (to keep the `Client` trait contract).
