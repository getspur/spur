# `/clear` Command - Transport-Aware Session Reset

**Date:** 2026-04-19
**Scope:** `crates/spur-tui/src/commands/`, `crates/spur-tui/src/action.rs`, `crates/spur-tui/src/app.rs`, `crates/spur-tui/src/session_metadata.rs`, `crates/spur-cli/src/main.rs`, `crates/spur-core/src/orchestrator.rs`, `crates/spur-acp/src/connection/`
**Status:** Draft - grounded in `spur-acp` transport audit

## Problem

We want a slash command, `/clear`, from `SessionDetailView` that closes the
current conversation as the active session and starts a fresh empty one.

The naive implementation path is to alias `/clear` to the existing
`Action::NewSessionRequested` flow in `spur-tui`, which currently sends
`UserInput::NewSessionWithMessage { blocks: [] }`.

That approach is wrong in two distinct ways:

1. **UI/state-machine problem in `spur-tui`.**
   The current `NewSessionRequested` path is lazy. It drops back to Dashboard
   and defers actual session creation until a later message. During that gap,
   local state still has stale notions of the active session.
2. **Freshness problem in `spur-acp`.**
   The meaning of "new session" is transport-dependent. Reusing the current
   connection and blindly calling `new_session()` does not produce a truly
   fresh conversation on every transport.

The second point is the load-bearing one. After auditing `spur-acp`, the
earlier proposal "reuse the preserved connection and eagerly call
`create_brain_session`" is correct for native ACP, but incorrect as a universal
rule.

## Goals

1. `/clear` must end the current conversation as the active one and eagerly
   create a replacement empty session.
2. The feature must be honest across all transports supported by `spur-acp`.
3. The TUI must not locally echo into the retired session or allow a
   double-spawn race while reset is in flight.
4. Restarting SPUR during reset must not auto-resume the retired session.

## Non-goals

- A true remote "delete session" or "close session" RPC on the agent side.
- Deleting the retired session from picker metadata or agent storage.
- Reworking all session-switching and resume flows in the same patch.
- Multi-session tabs or concurrent active brains.

## Grounding from `spur-acp`

### 1. The connection trait has no session-close primitive

`AgentConnection` defines `initialize()`, `new_session()`, `prompt()`,
`cancel()`, `shutdown()`, `load_session()`, `list_sessions()`, and
`set_session_mode()`. There is no `close_session()` or equivalent.

That is deliberate, not an omission. The abstraction models connection
lifecycle and session creation/resume, not session disposal.

### 2. Native ACP supports real session creation on a live connection

`NativeAcpConnection` maps `new_session()` to ACP `NewSessionRequest`.
Its shutdown path explicitly notes that ACP has no explicit shutdown RPC and
relies on closing stdin / tearing down the subprocess.

For native ACP, reusing an initialized connection and issuing a fresh
`new_session()` is a valid way to start a new conversation.

### 3. `StdioAdapter` does not have real session boundaries

`StdioAdapter` documents that "the process IS the session". Its
`new_session()` method returns only a synthetic UUID while reusing the same
subprocess.

Implication: calling `new_session()` on a reused stdio connection does not
guarantee a fresh conversation.

### 4. `StreamJsonAdapter` preserves Claude conversation state across "new"

`StreamJsonAdapter` also returns a synthetic SPUR session id from
`new_session()`. But `prompt()` adds `--resume <claude_session_id>` whenever
that internal Claude id is populated.

Implication: if `/clear` reuses the connection without clearing
`claude_session_id`, the next "fresh" session silently resumes the prior Claude
conversation under a new SPUR wrapper. That is semantically wrong.

### 5. `CliWrapAdapter` has synthetic sessions and one-shot prompt processes

`CliWrapAdapter` has no durable session persistence. Freshness is mostly local:
its "session" is metadata around a one-shot CLI process per prompt.

Implication: `/clear` for `CliWrapAdapter` mostly needs to reset SPUR-side
session identity and transcript state.

## Transport table

| Transport | What `new_session()` means today | Can `/clear` reuse connection? |
|---|---|---|
| `TransportKind::Acp` | Real agent-side new session | Yes |
| `TransportKind::Stdio` | Synthetic id on same persistent process | No - reconnect required |
| `TransportKind::CliWrap` | Synthetic id; future prompt spawns one-shot process | Yes |
| `TransportKind::StreamJson` | Synthetic SPUR id; underlying Claude session persists via `--resume` | Yes, but only after clearing adapter session state |

## Decision

`/clear` will be a first-class command with a first-class backend intent:

- `Action::ClearSession`
- `UserInput::ClearSession`
- `InteractiveInput::ClearSession`

It will **not** alias to `Action::NewSessionRequested`.

To make the behavior correct across transports, "fresh conversation" becomes an
explicit `spur-acp` contract. Add a transport hook on `AgentConnection`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionResetOutcome {
    Ready,
    ReconnectRequired,
}

async fn reset_for_new_session(&mut self) -> anyhow::Result<SessionResetOutcome> {
    Ok(SessionResetOutcome::Ready)
}
```

This hook means:

- "I have cleared any transport-local state needed so the next `new_session()`
  is semantically fresh" -> `Ready`
- "A fresh conversation is not possible on this live connection" ->
  `ReconnectRequired`

This keeps freshness semantics inside the transport abstraction where they
belong, rather than leaking them into ad hoc orchestrator policy.

## User model

`/clear` means:

1. Stop treating the current conversation as active immediately.
2. Start a replacement empty session immediately.
3. Do not delete the retired session from history or picker metadata.

This is an **active-context reset**, not session deletion.

If the transport persists old sessions, the user may still resume the retired
session later from the picker. That is acceptable and honest because ACP has no
universal remote session-close primitive.

## Design

### 1. TUI command surface

Add a local slash command in `crates/spur-tui/src/commands/spur_local.rs`:

```rust
CommandEntry {
    name: "clear".into(),
    description: "Start a fresh session".into(),
    hint: None,
    source: CommandSource::Spur,
    dispatch: Dispatch::SpurLocal(Action::ClearSession),
}
```

`submit_router::route("/clear", ...)` should return:

```rust
SubmitDecision::Local { action: Action::ClearSession }
```

### 2. New action and input variants

Add:

- `Action::ClearSession`
- `UserInput::ClearSession`
- `InteractiveInput::ClearSession`

This keeps the command semantics explicit through every layer.

### 3. App-side reset state machine

Add a local app flag:

```rust
session_reset_in_flight: bool,
```

Rationale: after `/clear`, there is a short window before the replacement
`BrainSpawned` arrives. In that window the app must not behave like the old
session is still attached, and must not allow a second spawn intent to slip in.

#### `Action::ClearSession` handling

In `App::process_action`:

1. `force_flush_active_draft()`
2. Drop any pending permission request and clear its trace markers
3. Clear the top-level auto-resume target in metadata
4. Remove local active-session UI state:
   - `session_detail = None`
   - `brain_name = None`
   - `brain_status = BrainStatus::Idle`
   - `current_view = ViewId::Dashboard`
5. Set `session_reset_in_flight = true`
6. `sync_brain_status()`
7. Send `UserInput::ClearSession`

#### Reset gate

While `session_reset_in_flight` is true:

- Dashboard submit actions must not emit another spawn request
- New `SendMessage` / `NewSessionWithMessage` intents are ignored

The gate clears on:

- `SpurEventBody::BrainSpawned` for the replacement session
- `SpurEventBody::BrainError` if reset fails

No new top-level view is introduced in this spec. Dashboard is the transient
landing state while reset is pending.

### 4. Metadata semantics

The retired session entry remains in `.spur/session_metadata.json`.
Drafts are preserved.

What must be cleared immediately is the **resume target**, not the whole entry.

Add a method such as:

```rust
pub fn clear_resume_target(&mut self) {
    self.metadata.last_active_session_id = None;
    self.metadata.last_active_at = None;
    self.metadata.last_active_acp_session_id = None;
    self.metadata.last_active_brain = None;
}
```

Rationale:

- Startup auto-resume uses ACP identity, not just the SPUR id
- If `/clear` retires session A and the process exits before replacement
  session B reaches `AgentSessionReady`, the next launch must not resurrect A

`AgentSessionReady` for the replacement session will repopulate the top-level
resume target normally.

### 5. CLI translator

In `crates/spur-cli/src/main.rs`, translate:

```rust
spur_tui::UserInput::ClearSession
    -> spur_core::InteractiveInput::ClearSession
```

No other CLI behavior changes.

### 6. Orchestrator reset flow

Add a new `InteractiveInput::ClearSession` arm in `run_interactive`.

High-level algorithm:

```rust
Self::retire_active_brain(&mut brain, &mut agent_connection);

let (mut connection, brain_name) = match agent_connection.take() {
    Some(existing) => existing,
    None => self.connect_brain(brain_override.as_deref(), permission_tx.clone()).await?,
};

match connection.reset_for_new_session().await? {
    SessionResetOutcome::Ready => {}
    SessionResetOutcome::ReconnectRequired => {
        let _ = connection.shutdown().await;
        (connection, brain_name) =
            self.connect_brain(brain_override.as_deref(), permission_tx.clone()).await?;
    }
}

brain = Some(
    self.create_brain_session(connection, brain_name, permission_tx.clone()).await?
);
```

Important properties:

- No prompt is queued
- No lazy spawn
- No `SessionCompleted` event is emitted for the retired session
- The replacement session is established immediately via the normal
  `BrainSpawned` -> `AgentSessionReady` path

### 7. `AgentConnection` transport hook

Add `reset_for_new_session()` defaulting to `SessionResetOutcome::Ready`.

Implementations:

#### `NativeAcpConnection`

Return `Ready`.

Reason: ACP-native `new_session()` already has real session semantics on a live
connection.

#### `StreamJsonAdapter`

Clear transport-local conversation identity:

- `claude_session_id = None`
- any stale per-session wrapper ids if needed

Return `Ready`.

Reason: the connection object can be reused, but only if its internal Claude
resume pointer is cleared first.

#### `StdioAdapter`

Return `ReconnectRequired`.

Reason: the process is the session. A truly fresh conversation requires a new
process.

#### `CliWrapAdapter`

Return `Ready`.

Reason: prompts are one-shot subprocesses already; no durable remote session
state must be cleared.

### 8. Relationship to existing session-switching design

This spec **supersedes only one narrow assumption** from
`2026-04-14-session-switching-design.md`:

- "reuse the preserved connection for every fresh-session path"

That assumption remains valid for native ACP, but not as a transport-agnostic
rule.

`ResumeSession` remains a separate path with different semantics:

- resume tries to restore prior state
- clear must start a semantically fresh conversation

Future cleanup should make picker `+ New session` reuse this same
transport-aware clear-session backend path instead of piggybacking on empty
`NewSessionWithMessage`.

## Alternatives rejected

### A. Alias `/clear` to `Action::NewSessionRequested`

Rejected because:

- it is lazy, not eager
- it leaves stale local active-session state during the gap
- it inherits transport-unsound freshness semantics

### B. Always reconnect for `/clear`

Rejected as the default because:

- correct but unnecessarily expensive for native ACP
- throws away the existing initialized connection on transports that can safely
  reuse it

Reconnect remains the fallback where the transport says it is required.

## Testing

### TUI

1. `/clear` routes to `Action::ClearSession`
2. `Action::ClearSession` clears local active-session state immediately
3. `clear_resume_target()` wipes top-level SPUR and ACP resume pointers
4. While `session_reset_in_flight`, dashboard submit is ignored
5. `BrainSpawned` clears `session_reset_in_flight`

### Orchestrator

1. `InteractiveInput::ClearSession` eagerly creates a replacement session
2. On `SessionResetOutcome::Ready`, the orchestrator reuses the connection
3. On `SessionResetOutcome::ReconnectRequired`, the orchestrator shuts down the
   preserved connection and reconnects before `create_brain_session`

### ACP adapters

1. `StreamJsonAdapter::reset_for_new_session()` clears `claude_session_id`
2. `StdioAdapter::reset_for_new_session()` returns
   `SessionResetOutcome::ReconnectRequired`
3. `CliWrapAdapter` and `NativeAcpConnection` return `Ready`

### Regression

Add an app-level regression test that proves:

- after clear, the first subsequent user message is not locally echoed into the
  retired session

This is the same class of bug as the old cross-session replay failure and is
worth pinning explicitly.

## Files touched

- `crates/spur-tui/src/commands/spur_local.rs`
- `crates/spur-tui/src/commands/submit_router.rs`
- `crates/spur-tui/src/action.rs`
- `crates/spur-tui/src/app.rs`
- `crates/spur-tui/src/session_metadata.rs`
- `crates/spur-cli/src/main.rs`
- `crates/spur-core/src/orchestrator.rs`
- `crates/spur-acp/src/connection/mod.rs`
- `crates/spur-acp/src/connection/native.rs`
- `crates/spur-acp/src/connection/stream_json_adapter.rs`
- `crates/spur-acp/src/connection/stdio_adapter.rs`
- `crates/spur-acp/src/connection/cli_wrap_adapter.rs`

## Open questions

1. Should picker `+ New session` be migrated in the same patch to call the new
   `ClearSession` backend path, or follow as a cleanup?
2. Should Dashboard show a dedicated "starting new session..." overlay during
   `session_reset_in_flight`, or is the transient gated Dashboard sufficient
   for v1?
