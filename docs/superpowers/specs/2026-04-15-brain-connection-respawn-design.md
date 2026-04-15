# Brain connection respawn — design

**Date:** 2026-04-15
**Status:** Draft — awaiting user review (v2, post-MCTS re-evaluation)
**Scope:** Option B from the `/clear` error brainstorm. Defense-in-depth
against brain subprocess death from any cause.

## Problem

When the active brain's ACP subprocess dies mid-session — whether from a kiro
vendor-extension bug, OOM, a real panic, or a network-adjacent MCP flap — spur
surfaces a `BrainError` event and the session is stuck for the rest of the TUI
run. The user must exit and relaunch to recover.

This spec is not about preventing the death; it's about surviving it.

## Grounding

Relevant existing code (verified against HEAD):
- `crates/spur-core/src/orchestrator.rs:435` — `brain: Option<BrainSession>` is
  a **local variable** inside `run_interactive`'s loop. Not Arc, not shared.
- `crates/spur-core/src/orchestrator.rs` loop is strictly sequential:
  one `InteractiveInput` processed per iteration, no concurrent brain I/O.
- `connect_brain(brain_override, permission_tx)` already exists and returns
  `(Box<dyn AgentConnection>, String)` — encapsulates spawn + initialize.
- `load_brain_session(connection, brain_name, permission_tx, session_id)` already
  exists and handles session/load, notification_pump spawn, and broadcast
  subscription. **Currently falls back silently to `new_session` when load
  fails** (orchestrator.rs:548 comment). That silent fallback is a landmine
  for reconnect — see B1.
- `subscribe_session_notifications` is called only inside
  `create_brain_session` / `load_brain_session`, so reconnect via those helpers
  automatically re-wires the notification pump. No manual rewiring needed.
- Death manifests today as `b.connection.<method>()` returning an `anyhow::Error`
  containing the phrase `"ACP thread died"`. Multiple match arms in
  `run_interactive` already emit `BrainError` on these.

Invariants to preserve (from repo convention / `spur-invariants-reviewer`):
1. `SpurEvent.seq` strictly monotonic per orchestrator lifetime.
2. Broadcast channel sizing not regressed.
3. TUI drain cap unchanged.
4. `append_message` walkback semantics across history.
5. ACP trailing-notification grace window.

## Design

Three cohesive deliverables, shipped together:

### B1 — Surface load_session fallback

**Why:** Today `load_brain_session` silently falls back to `new_session` when
the agent's session/load fails. For initial resume this is a minor UX issue;
for auto-reconnect it masks silent state loss.

**Change:** return a `LoadOutcome` alongside the `BrainSession`:

```rust
pub enum LoadOutcome {
    Restored,              // session/load succeeded
    FellBackToNew { reason: String }, // silent fallback → surface it
}
```

Touches: `orchestrator.rs::load_brain_session` signature + both existing call
sites (resume path & the new reconnect path). Existing `ResumeSession` handler
can now also raise a banner when it falls back.

### B2 — Inline reconnect helper

**New method:** `Orchestrator::try_reconnect_brain`

```rust
async fn try_reconnect_brain(
    &mut self,
    dead_brain: BrainSession,
    permission_tx: Option<mpsc::UnboundedSender<PermissionRequest>>,
    brain_override: Option<&str>,
) -> Result<(BrainSession, LoadOutcome)>
```

Implementation: drop `dead_brain` to close the old stdio, call `connect_brain`,
then `load_brain_session(new_conn, brain_name, permission_tx, acp_session_id)`.
Returns the new BrainSession and the `LoadOutcome` signal.

**Integration:** wrap the existing `BrainError`-emitting branches in the
`run_interactive` match arms (VendorExec, Prompt, SetSessionMode, Cancel,
LoadSession) so that when `b.connection.<method>()` returns an "ACP thread
died" error AND `brain.is_some()` was true, we attempt reconnect inline before
falling through to `BrainError`.

A small helper `is_connection_death(&anyhow::Error) -> bool` centralizes the
detection (match on error chain for the well-known messages).

Guard: do **not** auto-reconnect on first-connect failures. The guard
`brain.is_some()` at call time handles this since we only reach the inline
recovery path from match arms that dereference `b`.

### B3 — Circuit breaker + events

**State added to `run_interactive`** (local vars):

```rust
let mut reconnect_failures: VecDeque<Instant> = VecDeque::new();
const RECONNECT_CIRCUIT_LIMIT: usize = 2;
const RECONNECT_CIRCUIT_WINDOW: Duration = Duration::from_secs(60);
```

On each failed reconnect, push `Instant::now()`, trim entries older than the
window. If length ≥ limit, skip reconnect and go straight to
`BrainReconnectFailed`. Reset (clear the deque) on any successful normal RPC
post-reconnect.

**New SpurEvent variants:**

```rust
SpurEventBody::BrainReconnecting   { session, brain_name, reason }
SpurEventBody::BrainReconnected    { session, brain_name, outcome: LoadOutcome }
SpurEventBody::BrainReconnectFailed{ session, brain_name, reason }
```

Emitted only from the reconnect path. Ordering: `Reconnecting` emitted BEFORE
`connect_brain` so the TUI shows a banner immediately (subprocess spawn can take
>1s).

**TUI** (`crates/spur-tui/src/app.rs` or the toast/banner module — confirm
during plan phase): map the three events to a top-of-screen banner.
`BrainReconnected` with `LoadOutcome::FellBackToNew` gets a warning color
("history may have been lost"). Auto-dismiss on `Reconnected` after 3s;
persistent on `Failed`.

### Explicit invariant sub-task

Audit the **ACP trailing-notification grace timer** behavior when the
connection dies mid-grace. If the timer task assumes the broadcast sender is
live, add a `select!` arm on connection-closed or swap to `try_send` so the
old grace task exits cleanly on reconnect. Flag for
`spur-invariants-reviewer` in the implementation plan.

## Non-goals

- No ConnectionHealth watch channel / supervisor task — inline recovery in the
  sequential loop is sufficient and simpler.
- No worker-agent respawn.
- No multi-brain fan-out.
- No automatic replay of the in-flight turn's prompt.
- No user-configurable circuit breaker thresholds. Hardcode 2 failures / 60s.

## Acceptance criteria

1. Test fixture: an agent script that exits after first prompt. On second
   prompt the orchestrator emits
   `BrainReconnecting` → `BrainReconnected { outcome }` (and the second prompt
   subsequently succeeds if outcome is Restored).
2. `SpurEvent.seq` monotonic across the reconnect window — covered by an
   integration test.
3. Circuit breaker: two scripted failures within 60s produce one
   `BrainReconnectFailed`; a third input does not trigger a third reconnect
   attempt.
4. `LoadOutcome::FellBackToNew` path emits the warning variant and the TUI
   renders it.
5. `cargo test -p spur-core -p spur-acp -p spur-tui` passes; the
   `spur-invariants-reviewer` agent passes on the diff.
6. Manual test with kiro: `kill -9` the `kiro-cli` PID mid-turn; TUI shows
   Reconnecting → Reconnected banner; next `/status`-like prompt round-trips.

## Risk & rollback

- Silent load-session fallback is surfaced (B1); any agent whose session/load
  is broken will now be visibly flagged. Acceptable — that's the point.
- Grace-timer audit is the highest-risk unknown; if it turns out to require
  broader redesign, that's pulled out into its own spec and this one ships
  without it (the code path will occasionally log a benign error on reconnect
  until fixed).
- Rollback is clean — remove `try_reconnect_brain`, revert the error-branch
  wrappers, drop the three SpurEvent variants; `LoadOutcome` can stay (it's a
  strict UX improvement).

## Sequencing vs. Spec A

Independent of `2026-04-15-kiro-vendor-exec-fallback-design.md`. Ship A first
(config change, same day), B second (~1–2 days focused work + invariants pass).
With A in place, the common kiro-command trigger for subprocess death
disappears, so B's urgency is driven only by other crash modes.
