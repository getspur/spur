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
- **H2** `AgentMessageChunk` sequences are interleaved with `ToolCall`,
  `ToolCallUpdate`, or `Plan` updates, which causes `react_trace` to push a
  **new** `AgentMessage` entry instead of appending to the previous one — the
  visual "break at end" symptom.
- **H4** `broadcast::Receiver::Lagged(_)` is silently swallowed in the TUI
  select loop, potentially dropping notifications under burst load.

## Instrumentation

Three one-line `tracing::debug!` sites. Each entry carries a
`streaming_probe = true` field so the log can be filtered to just these lines.

### (A) `crates/spur-acp/src/connection/native.rs` — `session_notification`

At the top of `SpurAcpClientDynamic::session_notification` (currently line 775),
log:
- `variant` — the `SessionUpdate` discriminant name (`AgentMessageChunk`,
  `AgentThoughtChunk`, `ToolCall`, `ToolCallUpdate`, `Plan`, …)
- `text_len` — length of extracted text when variant is a text chunk, else 0
- `session` — session id short form

### (B) `crates/spur-core/src/orchestrator.rs` — streaming emit

At the `self.emit(SpurEvent::AgentNotification { … })` call in the prompt
streaming loop (~line 520), log the same three fields plus
`since_prompt_ms` (monotonic millis since prompt was submitted).

### (C) `crates/spur-tui/src/views/session_detail.rs` — trace append

Inside the `AgentMessageChunk` branch of `handle_spur_event`, just before
`append_message`, log:
- `text_len`
- `prev_entry_kind` — discriminant of `self.react_trace.entries.last()` (or
  `"none"` if empty)
- `will_continue` — `true` if the previous entry is `AgentMessage`, else `false`

Add a small helper on `ReactTrace` to expose the last entry's kind name.

### (D) Bonus: log `broadcast::Lagged`

In `crates/spur-tui/src/app.rs` at the `RecvError::Lagged(_)` arm (~line 532),
change the `{}` body to `tracing::warn!(lagged_n = n, "TUI broadcast lagged")`
so we know if H4 ever fires.

## Runbook

1. Build: `cargo build -p spur-cli`.
2. Tail logs to a file: `SPUR_LOG=debug target/debug/spur …  2> /tmp/spur.log`
   (the existing log file sink also writes to `~/.spur/logs/`).
3. Send one message to a kiro session. Wait for the full reply.
4. Filter: `rg streaming_probe /tmp/spur.log` — paste the output back.

## What the log answers

- **Cadence (H1):** delta between consecutive `(A)` rows. If deltas are mostly
  < 50 ms → per-token. If mostly > 300 ms → coarse chunks, a kiro-cli issue.
- **Interleaving (H2):** sequence of `variant`s within a single turn. If
  `AgentMessageChunk` rows are broken up by `ToolCall`/`Plan`, confirmed.
- **Rendering (C):** `will_continue=false` rows after the first show exactly
  where the trace pushes a new entry instead of appending.
- **Lag (H4):** any warn line from the broadcast receiver.

## Non-goals for this patch

- Changing kiro-cli invocation flags.
- Modifying `react_trace` aggregation logic.
- Adjusting the broadcast channel size.

All of the above become candidates in the follow-up design after reading the
log.

## Acceptance

- `cargo build` succeeds.
- One streaming turn produces rows from sites (A), (B), (C).
- No behavior change in the TUI (same messages, same render).
