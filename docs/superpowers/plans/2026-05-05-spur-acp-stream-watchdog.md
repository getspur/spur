# spur-acp Stream Watchdog & Auto-Recovery — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Inside `crates/spur-acp`, add an active heartbeat watchdog and a two-tier auto-recovery coordinator that detects stalled turns (errored or silent), transparently retries known-transient blips, and surfaces a non-modal `[Retry / Reset / Wait]` choice for ambiguous wedges.

**Architecture:** Three new internal modules (`watchdog.rs`, `recovery.rs`, `turn_context.rs`) plus a small Native-specific change to allow cancel-during-prompt (out-of-band cancel channel through `cx.spawn`). The `AgentConnection` trait gains three methods (`subscribe_recovery_events`, `resolve_stall`, `current_stall_state`) with default impls so existing adapters compile unchanged; each adapter is then wired in turn.

**Tech Stack:** Rust 2021, `tokio` (workspace, `full` features → includes `test-util`), `tokio::sync::broadcast` for event fan-out, `tokio::sync::oneshot` and `mpsc::unbounded_channel` for control-plane signals, `agent-client-protocol = "0.11"`, `uuid` for stall_id, `thiserror` for error variants. Tests use `tokio::time::pause()`/`advance()` for deterministic timeout assertions; integration fixtures live under `crates/spur-acp/tests/fixtures/` (`.mjs` / `.sh` patterns already established).

**Spec:** `docs/superpowers/specs/2026-05-05-spur-acp-stream-watchdog-design.md`

**Plan-time deviations from spec** (engineering rationale documented inline):

- The spec's Tier-1 step "Issue `session/cancel`. **Await** the JSON-RPC response (with its own short timeout — 10s)" is technically incorrect: ACP `session/cancel` is a JSON-RPC **notification** with no agent ack. We implement: send the cancel notification, wait fixed 500ms grace, re-issue the prompt. Phase 3 also adds Fix B-1 (out-of-band cancel channel for Native) so the cancel actually reaches the wire while a Prompt is wedged on the cmd loop — without this, Tier-1 silent retry deadlocks on Native. Both adjustments preserve the spec's intent.
- `BrainStallResolved` event added (the spec mentions it implicitly in the flow diagram but doesn't list it under Domain events). It carries `stall_id` and `ResolvedBy` so the TUI can deterministically clear the banner regardless of what resolved the stall.
- `ResolvedBy` enum is the union of resolver causes the spec lists (SilentRetry, UserRetry, UserReset, UserWait, TurnCompleted, ProcessExited).

---

## File Structure

| File | New / Modified | Responsibility |
|---|---|---|
| `crates/spur-acp/src/connection/turn_context.rs` | NEW | `TurnContext`, `StallState`, `StallStateSnapshot` types. One Arc-Mutex shared between watchdog, prompt-handler, error-capture sites. |
| `crates/spur-acp/src/connection/watchdog.rs` | NEW | `InFlightState`, multiplier table, transition logic, `Watchdog` struct (timer task per turn, broadcast subscriber feed, drop-cancellation). |
| `crates/spur-acp/src/connection/recovery.rs` | NEW | `RecoveryCoordinator`, transient-pattern allow-list, Tier-1 silent retry sequence, Tier-2 stall emission, `resolve_stall` dispatcher, `current_stall_state` snapshot. |
| `crates/spur-acp/src/connection/mod.rs` | MOD | Add new trait methods (default-impl-bearing); export new types; `RecoveryEvent`, `ResolvedBy`, `StallResolution`, `StallStateSnapshot`. Update `TestStubConnection`. |
| `crates/spur-acp/src/connection/native.rs` | MOD | Phase 3: add `cancel_signal_tx`, spawn cancel sibling via `cx.spawn` in `acp_thread_main`, route public `cancel()` via the new channel, delete `AcpCommand::Cancel` arm. Phase 6: wire watchdog + recovery into prompt() lifecycle. |
| `crates/spur-acp/src/connection/stdio_adapter.rs` | MOD | Phase 6: feed watchdog from notification stream; capture errors into TurnContext. Cancel = subprocess kill (existing). |
| `crates/spur-acp/src/connection/cli_wrap_adapter.rs` | MOD | Phase 6: same pattern as stdio. |
| `crates/spur-acp/src/connection/stream_json_adapter.rs` | MOD | Phase 6: same pattern; also handle the per-turn subprocess lifecycle in the cancel = kill path. |
| `crates/spur-acp/src/domain/events.rs` | MOD | Add the six new `SpurEventBody` variants. |
| `crates/spur-acp/src/error.rs` | MOD | Add `AcpError::Stalled { stall_id }` (when prompt() returns due to stall) and `AcpError::StaleStallId { stall_id }`. |
| `crates/spur-acp/src/config/mod.rs` | MOD | Add `AgentRecoveryPolicy`, per-kind defaults via `resolved(&AgentKind)`. |
| `crates/spur-acp/Cargo.toml` | MOD | Add `tokio = { workspace = true, features = ["test-util"] }` to `[dev-dependencies]` (workspace tokio is `full` so this is for clarity; verify no-op if already inherited). |
| `crates/spur-acp/tests/fixtures/watchdog_silent_stall.mjs` | NEW | Streams two `session/update`s, then sleeps. |
| `crates/spur-acp/tests/fixtures/watchdog_tier1_recovery.mjs` | NEW | First prompt errors mid-stream with the literal Stream-idle-timeout string; second prompt completes. |
| `crates/spur-acp/tests/fixtures/watchdog_tier2_nonmatch.mjs` | NEW | Errors with non-allow-listed string. |
| `crates/spur-acp/tests/fixtures/watchdog_tool_call_grace.mjs` | NEW | Emits `ToolCall`, holds, then `ToolCallUpdate { status: Completed }`. |
| `crates/spur-acp/tests/fixtures/watchdog_thinking_grace.mjs` | NEW | Emits `AgentThoughtChunk` then holds. |
| `crates/spur-acp/tests/fixtures/cancel_during_prompt.mjs` | NEW | Streams forever; verifies B-1 cancel wakes it. |
| `crates/spur-acp/tests/stream_watchdog_*.rs` | NEW (6 files) | Integration tests, one per scenario. |
| `crates/spur-acp/tests/cancel_during_prompt.rs` | NEW | B-1 verification. |
| `crates/spur-acp/tests/stall_id_staleness.rs` | NEW | Stale resolve_stall returns Err. |

---

## Phase 0 — Setup

### Task 0.1: Worktree + branch

**Files:** none (git ops)

- [ ] **Step 1: Confirm worktree.** Per the brainstorming skill's worktree pattern, create or switch into a feature worktree.

```bash
git worktree list
# If a feature worktree for this work doesn't exist, create one:
git worktree add ../spur-stream-watchdog -b feat/spur-acp-stream-watchdog
cd ../spur-stream-watchdog
```

- [ ] **Step 2: Confirm clean tree.**

```bash
git status -s
```
Expected: empty output (or only the spec file already committed on this branch).

- [ ] **Step 3: Verify dev-dep features.** The workspace `tokio` is already `features = ["full"]`, which includes `test-util`. Confirm:

```bash
grep -n '^tokio\s*=' Cargo.toml
```
Expected: `tokio = { version = "1", features = ["full"] }`

No Cargo.toml change is needed for tokio. If `tokio::time::pause()` fails to compile in tests in later phases, revisit by adding an explicit dev-dep entry.

---

## Phase 1 — Type foundation

### Task 1.1: Add `AcpError` variants

**Files:**
- Modify: `crates/spur-acp/src/error.rs`

- [ ] **Step 1: Write the failing test.** Append to `crates/spur-acp/src/error.rs` inside the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn stalled_display_includes_stall_id() {
    let id = uuid::Uuid::nil();
    let e = AcpError::Stalled { stall_id: id };
    let s = e.to_string();
    assert!(s.contains("stalled"), "Display must mention stalled; got {s:?}");
    assert!(s.contains(&id.to_string()), "Display must include stall_id; got {s:?}");
}

#[test]
fn stale_stall_id_display_distinguishable() {
    let id = uuid::Uuid::nil();
    let e = AcpError::StaleStallId { stall_id: id };
    let s = e.to_string();
    assert!(s.to_lowercase().contains("stale"), "Display must mention stale; got {s:?}");
}
```

- [ ] **Step 2: Run tests, expect failure.**

```bash
cargo test -p spur-acp --lib error::
```
Expected: compilation error — variants don't exist.

- [ ] **Step 3: Add the variants.** Edit `crates/spur-acp/src/error.rs`:

```rust
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("agent capability missing: {0}")]
    CapabilityMissing(&'static str),

    #[error("turn stalled (stall_id={stall_id})")]
    Stalled { stall_id: Uuid },

    #[error("stale stall_id (no active stall matches {stall_id})")]
    StaleStallId { stall_id: Uuid },

    #[error(transparent)]
    Transport(#[from] anyhow::Error),
}
```

- [ ] **Step 4: Run tests, expect pass.**

```bash
cargo test -p spur-acp --lib error::
```
Expected: all 4 tests pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-acp/src/error.rs
git commit -m "spur-acp: add AcpError::{Stalled,StaleStallId} variants for watchdog recovery"
```

---

### Task 1.2: Add new `SpurEventBody` variants

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`

- [ ] **Step 1: Locate the `SpurEventBody` enum.**

```bash
grep -n "^pub enum SpurEventBody\|BrainReconnecting\|BrainReconnected" crates/spur-acp/src/domain/events.rs | head -10
```
Note the line number where the enum is declared and where the existing `BrainReconnecting` variant lives — group new variants near it.

- [ ] **Step 2: Write the failing test.** Add a new test file (or append to the existing tests module if there is one) for serialization round-tripping. Since the test must reference enum variants that don't yet exist, the test compilation itself is the assertion. Add to `crates/spur-acp/src/domain/events.rs`:

```rust
#[cfg(test)]
mod watchdog_event_tests {
    use super::*;
    use uuid::Uuid;

    fn dummy_session() -> agent_client_protocol::schema::SessionId {
        agent_client_protocol::schema::SessionId("test-session".into())
    }

    #[test]
    fn brain_stalled_carries_stall_id_and_session() {
        let stall = Uuid::nil();
        let body = SpurEventBody::BrainStalled {
            stall_id: stall,
            session_id: dummy_session(),
            last_activity_ago_ms: 60_000,
            in_flight_state: "Streaming".into(),
            transient_error: Some("Stream idle timeout".into()),
        };
        match body {
            SpurEventBody::BrainStalled { stall_id, .. } => assert_eq!(stall_id, stall),
            other => panic!("expected BrainStalled, got {other:?}"),
        }
    }

    #[test]
    fn brain_stall_resolved_carries_resolved_by() {
        let stall = Uuid::nil();
        let body = SpurEventBody::BrainStallResolved {
            stall_id: stall,
            session_id: dummy_session(),
            resolved_by: ResolvedBy::SilentRetry,
        };
        match body {
            SpurEventBody::BrainStallResolved { resolved_by, .. } => {
                assert!(matches!(resolved_by, ResolvedBy::SilentRetry));
            }
            other => panic!("expected BrainStallResolved, got {other:?}"),
        }
    }
}
```

- [ ] **Step 3: Run tests, expect compilation failure.**

```bash
cargo test -p spur-acp --lib domain::events::
```
Expected: error — variants and `ResolvedBy` don't exist.

- [ ] **Step 4: Add the variants and `ResolvedBy`.** Insert near the existing `BrainReconnecting` variant in the `SpurEventBody` enum:

```rust
BrainStalled {
    stall_id: uuid::Uuid,
    session_id: agent_client_protocol::schema::SessionId,
    last_activity_ago_ms: u64,
    in_flight_state: String,            // serialized debug name; richer struct deferred
    transient_error: Option<String>,
},
BrainSilentRetryAttempted {
    stall_id: uuid::Uuid,
    session_id: agent_client_protocol::schema::SessionId,
    reason: String,
},
BrainSilentRetrySucceeded {
    stall_id: uuid::Uuid,
    session_id: agent_client_protocol::schema::SessionId,
},
BrainSilentRetryFailed {
    stall_id: uuid::Uuid,
    session_id: agent_client_protocol::schema::SessionId,
    error: String,
},
BrainStallResolved {
    stall_id: uuid::Uuid,
    session_id: agent_client_protocol::schema::SessionId,
    resolved_by: ResolvedBy,
},
BrainProcessExited {
    session_id: agent_client_protocol::schema::SessionId,
    code: Option<i32>,
},
```

Add the `ResolvedBy` enum at module-level in the same file (above or below `SpurEventBody`):

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ResolvedBy {
    SilentRetry,
    UserRetry,
    UserReset { new_session_id: agent_client_protocol::schema::SessionId },
    UserWait,
    TurnCompleted,
    ProcessExited,
}
```

If `SpurEventBody` derives `Serialize`/`Deserialize`/`Clone`/`Debug`, the new variants must too — match the existing variants' field types. If a derive on the enum requires a missing trait on `SessionId`, fall back to wrapping it in a string or use the SessionId's already-derived impls (verify by trying to compile).

- [ ] **Step 5: Run tests, expect pass.**

```bash
cargo test -p spur-acp --lib domain::events::
```
Expected: pass.

- [ ] **Step 6: Run full crate compile to surface enum-exhaustiveness regressions.**

```bash
cargo check -p spur-acp
```
Expected: clean (enum is `#[non_exhaustive]` if convention; if not, fix any exhaustive matches surfaced as warnings/errors).

- [ ] **Step 7: Commit.**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "spur-acp: add 6 new SpurEventBody variants for watchdog recovery"
```

---

### Task 1.3: Add `AgentRecoveryPolicy` to config

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs`

- [ ] **Step 1: Write the failing test.** Append to the existing `#[cfg(test)] mod tests` (or create one if missing) in `crates/spur-acp/src/config/mod.rs`:

```rust
#[test]
fn recovery_policy_default_for_claude_code() {
    let policy = AgentRecoveryPolicy::default();
    let resolved = policy.resolved(&crate::types::AgentKind::ClaudeCodeAcp);
    assert_eq!(resolved.heartbeat_base_secs, 60);
    assert!(resolved.auto_silent_retry);
}

#[test]
fn recovery_policy_default_for_codex_is_120() {
    let policy = AgentRecoveryPolicy::default();
    let resolved = policy.resolved(&crate::types::AgentKind::CodexAcp);
    assert_eq!(resolved.heartbeat_base_secs, 120);
}

#[test]
fn recovery_policy_default_for_generic_is_120() {
    let policy = AgentRecoveryPolicy::default();
    let resolved = policy.resolved(&crate::types::AgentKind::Generic);
    assert_eq!(resolved.heartbeat_base_secs, 120);
}

#[test]
fn recovery_policy_user_override_wins() {
    let policy = AgentRecoveryPolicy {
        heartbeat_base_secs: Some(30),
        auto_silent_retry: Some(false),
    };
    let resolved = policy.resolved(&crate::types::AgentKind::ClaudeCodeAcp);
    assert_eq!(resolved.heartbeat_base_secs, 30);
    assert!(!resolved.auto_silent_retry);
}

#[test]
fn recovery_policy_parses_from_toml() {
    let toml_str = r#"
        heartbeat_base_secs = 90
        auto_silent_retry = false
    "#;
    let policy: AgentRecoveryPolicy = toml::from_str(toml_str).expect("parse");
    assert_eq!(policy.heartbeat_base_secs, Some(90));
    assert_eq!(policy.auto_silent_retry, Some(false));
}
```

- [ ] **Step 2: Run tests, expect failure.**

```bash
cargo test -p spur-acp --lib config::
```
Expected: error — `AgentRecoveryPolicy` doesn't exist.

- [ ] **Step 3: Add the types.** Insert into `crates/spur-acp/src/config/mod.rs`:

```rust
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct AgentRecoveryPolicy {
    #[serde(default)]
    pub heartbeat_base_secs: Option<u64>,
    #[serde(default)]
    pub auto_silent_retry: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
pub struct ResolvedRecoveryPolicy {
    pub heartbeat_base_secs: u64,
    pub auto_silent_retry: bool,
}

impl AgentRecoveryPolicy {
    pub fn resolved(&self, kind: &crate::types::AgentKind) -> ResolvedRecoveryPolicy {
        let (default_base, default_retry) = match kind {
            crate::types::AgentKind::ClaudeCodeAcp
            | crate::types::AgentKind::ClaudeStreamJson => (60, true),
            _ => (120, true),
        };
        ResolvedRecoveryPolicy {
            heartbeat_base_secs: self.heartbeat_base_secs.unwrap_or(default_base),
            auto_silent_retry: self.auto_silent_retry.unwrap_or(default_retry),
        }
    }
}
```

- [ ] **Step 4: Wire into the per-agent config struct.** Locate the existing per-agent entry struct (e.g. `AgentEntry` or whatever the `[[agents.entries]]` table parses into). Add an optional field:

```rust
#[serde(default)]
pub recovery: AgentRecoveryPolicy,
```

The `default` ensures that agents without a `[recovery]` sub-table parse cleanly with the empty policy (which then resolves to per-kind defaults).

- [ ] **Step 5: Run tests, expect pass.**

```bash
cargo test -p spur-acp --lib config::
```
Expected: 5 new tests pass; existing config tests still pass.

- [ ] **Step 6: Smoke-parse the live config.**

```bash
cargo run -p spur-acp --example compat_spike 2>&1 | head -20 || true
```
If the example doesn't exist or fails for unrelated reasons, instead just compile-check:

```bash
cargo check -p spur-acp
```
Expected: clean.

- [ ] **Step 7: Commit.**

```bash
git add crates/spur-acp/src/config/mod.rs
git commit -m "spur-acp: add AgentRecoveryPolicy with per-AgentKind defaults"
```

---

## Phase 2 — Watchdog primitive

### Task 2.1: Create `turn_context.rs` with `TurnContext`, `StallState`, `StallStateSnapshot`

**Files:**
- Create: `crates/spur-acp/src/connection/turn_context.rs`
- Modify: `crates/spur-acp/src/connection/mod.rs` (add `pub mod turn_context;`)

- [ ] **Step 1: Write the failing test.** Create `crates/spur-acp/src/connection/turn_context.rs`:

```rust
//! Per-turn shared context. Lives for the duration of one `prompt()` call.
//!
//! Constructed at prompt entry, cleared between turns. Watchdog, recovery
//! coordinator, and adapter error sites all hold an `Arc<Mutex<TurnContext>>`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use agent_client_protocol::schema::SessionId;
use uuid::Uuid;

use super::watchdog::InFlightState;

#[derive(Debug, Clone)]
pub struct StallState {
    pub stall_id: Uuid,
    pub fired_at: Instant,
    pub in_flight_state: InFlightState,
    pub transient_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StallStateSnapshot {
    pub stall_id: Uuid,
    pub session_id: SessionId,
    pub fired_at: Instant,
    pub in_flight_state: InFlightState,
    pub transient_error: Option<String>,
}

#[derive(Debug)]
pub struct TurnContext {
    pub session_id: SessionId,
    pub started_at: Instant,
    pub last_transport_error: Option<String>,
    pub silent_retries_used: u32,
    pub current_stall: Option<StallState>,
}

impl TurnContext {
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            started_at: Instant::now(),
            last_transport_error: None,
            silent_retries_used: 0,
            current_stall: None,
        }
    }

    pub fn arc_mutex(session_id: SessionId) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::new(session_id)))
    }

    pub fn snapshot_stall(&self) -> Option<StallStateSnapshot> {
        self.current_stall.as_ref().map(|s| StallStateSnapshot {
            stall_id: s.stall_id,
            session_id: self.session_id.clone(),
            fired_at: s.fired_at,
            in_flight_state: s.in_flight_state.clone(),
            transient_error: s.transient_error.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> SessionId {
        SessionId("t".into())
    }

    #[test]
    fn new_starts_with_no_error_no_stall_no_retries() {
        let ctx = TurnContext::new(sid());
        assert!(ctx.last_transport_error.is_none());
        assert!(ctx.current_stall.is_none());
        assert_eq!(ctx.silent_retries_used, 0);
    }

    #[test]
    fn snapshot_stall_returns_none_when_idle() {
        let ctx = TurnContext::new(sid());
        assert!(ctx.snapshot_stall().is_none());
    }

    #[test]
    fn snapshot_stall_returns_some_when_active() {
        let mut ctx = TurnContext::new(sid());
        ctx.current_stall = Some(StallState {
            stall_id: Uuid::nil(),
            fired_at: Instant::now(),
            in_flight_state: InFlightState::Streaming,
            transient_error: None,
        });
        let snap = ctx.snapshot_stall().expect("some");
        assert_eq!(snap.stall_id, Uuid::nil());
        assert_eq!(snap.session_id, sid());
    }
}
```

- [ ] **Step 2: Wire the module.** Add to `crates/spur-acp/src/connection/mod.rs` near the other `pub mod` lines:

```rust
pub mod turn_context;
pub use turn_context::{StallState, StallStateSnapshot, TurnContext};

pub mod watchdog;
pub use watchdog::{InFlightState, Watchdog};
```

(`watchdog.rs` is created in the next task; this single line addition is fine — we'll add the file before next compile.)

- [ ] **Step 3: Run tests, expect failure.**

```bash
cargo test -p spur-acp --lib connection::turn_context
```
Expected: error — `super::watchdog::InFlightState` doesn't exist yet.

- [ ] **Step 4: Defer compile.** This task ends with a known compile failure that resolves at the end of Task 2.2. Do NOT commit yet — Task 2.2 finishes the dependency.

---

### Task 2.2: Create `watchdog.rs` with `InFlightState` + transition table

**Files:**
- Create: `crates/spur-acp/src/connection/watchdog.rs`

- [ ] **Step 1: Write the failing tests.** Create `crates/spur-acp/src/connection/watchdog.rs`:

```rust
//! Per-turn watchdog: detects stalls by tracking time-since-last-activity
//! against an in-flight-state-aware threshold.

use std::time::Duration;

use agent_client_protocol::schema::SessionUpdate;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InFlightState {
    Idle,
    Streaming,
    Thinking,
    ToolRunning { active_count: usize },
}

impl InFlightState {
    pub fn multiplier(&self) -> f64 {
        match self {
            InFlightState::Idle | InFlightState::Streaming => 1.0,
            InFlightState::Thinking => 3.0,
            InFlightState::ToolRunning { .. } => 10.0,
        }
    }

    /// Apply a `SessionUpdate` and return the new state.
    /// Caller is responsible for resetting `last_activity_at` separately.
    pub fn after_update(self, update: &SessionUpdate) -> InFlightState {
        match update {
            SessionUpdate::AgentMessageChunk(_) | SessionUpdate::UserMessageChunk(_) => {
                InFlightState::Streaming
            }
            SessionUpdate::AgentThoughtChunk(_) => InFlightState::Thinking,
            SessionUpdate::ToolCall(_) => match self {
                InFlightState::ToolRunning { active_count } => InFlightState::ToolRunning {
                    active_count: active_count.saturating_add(1),
                },
                _ => InFlightState::ToolRunning { active_count: 1 },
            },
            SessionUpdate::ToolCallUpdate(u) => match (self, tool_call_update_terminal(u)) {
                (InFlightState::ToolRunning { active_count }, true) => {
                    let next = active_count.saturating_sub(1);
                    if next == 0 {
                        InFlightState::Streaming
                    } else {
                        InFlightState::ToolRunning { active_count: next }
                    }
                }
                (InFlightState::ToolRunning { active_count }, false) => {
                    InFlightState::ToolRunning { active_count }
                }
                // Orphan: no prior ToolCall — treat update as a fresh ToolCall.
                (_, _) => InFlightState::ToolRunning { active_count: 1 },
            },
            // Other variants (Plan, AgentMessage whole, etc.) don't change state.
            _ => self,
        }
    }
}

/// Returns true if the ToolCallUpdate carries a terminal status
/// (Completed / Failed / Cancelled).
fn tool_call_update_terminal(u: &agent_client_protocol::schema::ToolCallUpdate) -> bool {
    use agent_client_protocol::schema::ToolCallStatus;
    matches!(
        u.fields.status,
        Some(ToolCallStatus::Completed) | Some(ToolCallStatus::Failed)
    )
}

#[derive(Debug)]
pub struct Watchdog {
    // Implementation in Task 2.3
    _todo: (),
}

#[cfg(test)]
mod state_transition_tests {
    use super::*;
    use agent_client_protocol::schema::{
        ContentBlock, SessionUpdate, ToolCallId, ToolCallUpdate,
    };

    fn agent_message_chunk_update() -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentBlock::Text(
            agent_client_protocol::schema::TextContent::new("hi"),
        ))
    }
    fn agent_thought_chunk_update() -> SessionUpdate {
        SessionUpdate::AgentThoughtChunk(ContentBlock::Text(
            agent_client_protocol::schema::TextContent::new("hmm"),
        ))
    }

    #[test]
    fn idle_stays_idle_for_unknown_variant() {
        // Construct a Plan update if the variant exists; otherwise skip.
        // For now: AgentMessageChunk → Streaming.
        let s = InFlightState::Idle.after_update(&agent_message_chunk_update());
        assert_eq!(s, InFlightState::Streaming);
    }

    #[test]
    fn agent_thought_chunk_transitions_to_thinking() {
        let s = InFlightState::Idle.after_update(&agent_thought_chunk_update());
        assert_eq!(s, InFlightState::Thinking);
    }

    #[test]
    fn multipliers_match_spec() {
        assert!((InFlightState::Idle.multiplier() - 1.0).abs() < 1e-9);
        assert!((InFlightState::Streaming.multiplier() - 1.0).abs() < 1e-9);
        assert!((InFlightState::Thinking.multiplier() - 3.0).abs() < 1e-9);
        assert!(
            (InFlightState::ToolRunning { active_count: 1 }
                .multiplier()
                - 10.0)
                .abs()
                < 1e-9
        );
    }
}
```

- [ ] **Step 2: Run tests.**

```bash
cargo test -p spur-acp --lib connection::watchdog::state_transition_tests
```
Expected: pass.

- [ ] **Step 3: Confirm `turn_context` now compiles.**

```bash
cargo test -p spur-acp --lib connection::turn_context
```
Expected: pass (the compile error from Task 2.1 is resolved).

- [ ] **Step 4: Commit Tasks 2.1 + 2.2 together.**

```bash
git add crates/spur-acp/src/connection/turn_context.rs crates/spur-acp/src/connection/watchdog.rs crates/spur-acp/src/connection/mod.rs
git commit -m "spur-acp: add TurnContext + InFlightState transition table for watchdog"
```

> **Note on `tool_call_update_terminal`:** the exact path to `ToolCallStatus` and the field name on `ToolCallUpdate` (`fields.status`?) depend on the SDK shape. If `cargo check` complains, inspect with:
> ```bash
> grep -n "pub struct ToolCallUpdate\|enum ToolCallStatus" ~/.cargo/registry/src/index.crates.io-*/agent-client-protocol-0.11.1/src/session.rs | head -10
> ```
> Adjust the field access accordingly.

---

### Task 2.3: Implement `Watchdog` struct (timer task + drop semantics)

**Files:**
- Modify: `crates/spur-acp/src/connection/watchdog.rs`

- [ ] **Step 1: Write the failing test.** Add to `watchdog.rs`:

```rust
#[cfg(test)]
mod watchdog_timer_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    /// Test fires when (now - last_activity) > base * multiplier.
    /// Uses paused tokio time for determinism.
    #[tokio::test(start_paused = true)]
    async fn fires_after_base_timeout_when_idle() {
        let (fire_tx, mut fire_rx) = mpsc::unbounded_channel();
        let _wd = Watchdog::spawn(
            Duration::from_secs(60),
            Arc::new(Mutex::new(InFlightState::Idle)),
            Arc::new(Mutex::new(std::time::Instant::now())),
            fire_tx,
        );

        // Advance just under threshold — no fire.
        tokio::time::advance(Duration::from_secs(59)).await;
        tokio::task::yield_now().await;
        assert!(fire_rx.try_recv().is_err());

        // Advance past threshold — fires.
        tokio::time::advance(Duration::from_secs(2)).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), fire_rx.recv())
            .await
            .expect("watchdog should have fired");
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_fire_when_thinking_until_3x() {
        let (fire_tx, mut fire_rx) = mpsc::unbounded_channel();
        let _wd = Watchdog::spawn(
            Duration::from_secs(60),
            Arc::new(Mutex::new(InFlightState::Thinking)),
            Arc::new(Mutex::new(std::time::Instant::now())),
            fire_tx,
        );

        // 60s elapsed; with 3x multiplier, threshold is 180s — no fire yet.
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert!(fire_rx.try_recv().is_err());

        // 181s — fires.
        tokio::time::advance(Duration::from_secs(121)).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), fire_rx.recv())
            .await
            .expect("should fire at 3x");
    }

    #[tokio::test(start_paused = true)]
    async fn drop_cancels_timer() {
        let (fire_tx, mut fire_rx) = mpsc::unbounded_channel();
        let wd = Watchdog::spawn(
            Duration::from_secs(60),
            Arc::new(Mutex::new(InFlightState::Idle)),
            Arc::new(Mutex::new(std::time::Instant::now())),
            fire_tx,
        );
        drop(wd);
        tokio::time::advance(Duration::from_secs(120)).await;
        tokio::task::yield_now().await;
        assert!(fire_rx.try_recv().is_err(), "dropped watchdog must not fire");
    }
}
```

- [ ] **Step 2: Run tests, expect failure.**

```bash
cargo test -p spur-acp --lib connection::watchdog::watchdog_timer_tests
```
Expected: error — `Watchdog::spawn` doesn't have the right signature, or the type stub returns `_todo: ()`.

- [ ] **Step 3: Implement `Watchdog`.** Replace the stub `Watchdog` in `watchdog.rs`:

```rust
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct Watchdog {
    handle: JoinHandle<()>,
}

impl Watchdog {
    pub fn spawn(
        base: Duration,
        state: Arc<Mutex<InFlightState>>,
        last_activity_at: Arc<Mutex<Instant>>,
        fire_tx: mpsc::UnboundedSender<WatchdogFired>,
    ) -> Self {
        let handle = tokio::spawn(async move {
            loop {
                let (eff_timeout, last) = {
                    let s = state.lock().expect("state mutex poisoned").clone();
                    let last = *last_activity_at.lock().expect("last_activity mutex poisoned");
                    let mult = s.multiplier();
                    let secs = (base.as_secs_f64() * mult).max(1.0);
                    (Duration::from_secs_f64(secs), last)
                };
                let elapsed = last.elapsed();
                if elapsed >= eff_timeout {
                    let _ = fire_tx.send(WatchdogFired {
                        elapsed,
                        state: state.lock().unwrap().clone(),
                    });
                    return;
                }
                let remaining = eff_timeout - elapsed;
                // Tick at fine granularity so state changes (which shorten or extend
                // the effective timeout) are picked up promptly. 250ms is fine for
                // human-scale stalls.
                let tick = remaining.min(Duration::from_millis(250));
                tokio::time::sleep(tick).await;
            }
        });
        Self { handle }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[derive(Debug, Clone)]
pub struct WatchdogFired {
    pub elapsed: Duration,
    pub state: InFlightState,
}
```

Update the test signatures above to match: `Watchdog::spawn` takes `(Duration, Arc<Mutex<InFlightState>>, Arc<Mutex<Instant>>, mpsc::UnboundedSender<WatchdogFired>)`. The test imports also need `WatchdogFired`. If `mpsc::UnboundedSender<WatchdogFired>` differs from the test stub `mpsc::UnboundedSender<()>` written above, edit the test to match.

- [ ] **Step 4: Run tests.**

```bash
cargo test -p spur-acp --lib connection::watchdog
```
Expected: pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-acp/src/connection/watchdog.rs
git commit -m "spur-acp: implement Watchdog timer task with state-aware multiplier"
```

---

### Task 2.4: Add `transient pattern` classifier

**Files:**
- Modify: `crates/spur-acp/src/connection/recovery.rs` (NEW)

- [ ] **Step 1: Write the failing test.** Create `crates/spur-acp/src/connection/recovery.rs`:

```rust
//! Two-tier auto-recovery coordinator. Tier-1 silent retry on classified
//! transient errors; Tier-2 user-resolvable banner via RecoveryEvent broadcast.

const TRANSIENT_PATTERNS: &[&str] = &[
    "stream idle timeout",
    "partial response received",
    "econnreset",
    "epipe",
    "broken pipe",
    "connection reset",
];

/// Returns true if `err` looks like a transient transport blip
/// that's safe to silently retry.
pub fn is_transient(err: &str) -> bool {
    let lower = err.to_lowercase();
    TRANSIENT_PATTERNS.iter().any(|p| lower.contains(p))
}

#[cfg(test)]
mod classifier_tests {
    use super::*;

    #[test]
    fn matches_stream_idle_timeout_exact_case() {
        assert!(is_transient("API Error: Stream idle timeout - partial response received"));
    }

    #[test]
    fn matches_mixed_case_econnreset() {
        assert!(is_transient("Got EConnReset from upstream"));
    }

    #[test]
    fn matches_broken_pipe() {
        assert!(is_transient("write error: Broken pipe"));
    }

    #[test]
    fn rejects_authentication_error() {
        assert!(!is_transient("authentication required"));
    }

    #[test]
    fn rejects_resource_not_found() {
        assert!(!is_transient("Resource not found: session abc"));
    }

    #[test]
    fn rejects_empty() {
        assert!(!is_transient(""));
    }
}
```

- [ ] **Step 2: Wire the module.** In `crates/spur-acp/src/connection/mod.rs` add:

```rust
pub mod recovery;
```

- [ ] **Step 3: Run tests.**

```bash
cargo test -p spur-acp --lib connection::recovery::classifier_tests
```
Expected: pass.

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-acp/src/connection/recovery.rs crates/spur-acp/src/connection/mod.rs
git commit -m "spur-acp: add transient-error classifier (case-insensitive substring allow-list)"
```

---

## Phase 3 — Native ACP cancel concurrency (Fix B-1)

> **Why this phase:** spec line 126 says Tier-1 should "Issue session/cancel. Await the JSON-RPC response (with its own short timeout — 10s)." This is incorrect on two counts: (1) ACP `session/cancel` is a notification with no agent ack to await, and (2) on `NativeAcpConnection`, the public `cancel()` enqueues an `AcpCommand::Cancel` onto the same serial cmd loop that processes `AcpCommand::Prompt`, which is currently blocked on `cx.send_request(prompt).block_task().await` until the prompt's PromptResponse arrives (`native.rs:1572-1597`). So a cancel sent during a wedged prompt sits behind the wedged prompt and the cancel never reaches the wire until the wedge naturally clears.
>
> Fix: spawn a sibling task via `cx.spawn(...)` (the SDK's documented mechanism, already used at `native.rs:1363`) listening on a dedicated `cancel_signal_tx` channel. The sibling task holds a clone of `cx` (`ConnectionTo: Clone` per `agent-client-protocol-0.11.1/src/jsonrpc.rs:1432`) and calls `cx.send_notification(CancelNotification::new(session_id))` directly — concurrent with the cmd loop's `block_task().await`. Public `cancel()` routes onto `cancel_signal_tx`. `AcpCommand::Cancel` is removed from the cmd loop entirely.

### Task 3.1: Add cancel sibling task to `NativeAcpConnection`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Write the failing integration test.** Create `crates/spur-acp/tests/cancel_during_prompt.rs`:

```rust
//! Verifies Fix B-1: cancel issued during a wedged prompt actually reaches
//! the wire (instead of queuing behind the wedge).

use std::time::Duration;
use tokio::time::timeout;

mod common;
use common::spawn_native_with_fixture;

#[tokio::test]
async fn cancel_during_wedged_prompt_returns_promptly() {
    // Fixture script writes one session/update then sleeps forever.
    // Without B-1, public cancel() would sit in cmd queue until the
    // (never-arriving) PromptResponse.
    let (mut conn, _child) = spawn_native_with_fixture("cancel_during_prompt.mjs").await;

    // Initialize, new_session, send a prompt.
    common::init_and_new_session(&mut conn).await;
    let prompt_fut = common::send_test_prompt(&mut conn);

    // Wait for first chunk so we know the bridge is in mid-turn.
    common::wait_for_first_chunk(&mut conn, Duration::from_secs(5)).await;

    // Issue cancel; expect it to return quickly (< 1s) regardless of the
    // still-running prompt.
    let session_id = common::session_id(&mut conn);
    let cancel_fut = conn.cancel(&session_id);
    let cancel_outcome = timeout(Duration::from_secs(1), cancel_fut)
        .await
        .expect("cancel should not block on prompt");
    assert!(cancel_outcome.is_ok(), "cancel returned err: {cancel_outcome:?}");

    // The prompt itself may still hang — that's the bridge's problem; we
    // only assert that cancel didn't block.
    drop(prompt_fut);
}
```

Create the test fixture `crates/spur-acp/tests/fixtures/cancel_during_prompt.mjs` (a Node script following the `load_error_stub.mjs` pattern):

```javascript
#!/usr/bin/env node
// Mock ACP agent that streams forever after first prompt.
// Reads JSON-RPC framed messages from stdin, replies to initialize/newSession,
// then on prompt streams session/update notifications without ever sending
// PromptResponse.

import { stdin, stdout } from 'node:process';

let buf = '';
function emit(obj) {
  const s = JSON.stringify(obj) + '\n';
  stdout.write(s);
}

stdin.setEncoding('utf8');
stdin.on('data', (chunk) => {
  buf += chunk;
  const lines = buf.split('\n');
  buf = lines.pop();
  for (const line of lines) {
    if (!line.trim()) continue;
    let msg;
    try { msg = JSON.parse(line); } catch { continue; }
    if (msg.method === 'initialize') {
      emit({ jsonrpc: '2.0', id: msg.id, result: { protocolVersion: 1, agentCapabilities: {} } });
    } else if (msg.method === 'session/new') {
      emit({ jsonrpc: '2.0', id: msg.id, result: { sessionId: 'sess-cdp-1' } });
    } else if (msg.method === 'session/prompt') {
      // Stream one chunk, then nothing forever.
      emit({
        jsonrpc: '2.0',
        method: 'session/update',
        params: {
          sessionId: 'sess-cdp-1',
          update: { kind: 'agent_message_chunk', content: { type: 'text', text: 'thinking...' } },
        },
      });
      // Never reply to the prompt request.
    }
    // Cancel notifications are silently accepted (no reply needed).
  }
});
```

Also create `crates/spur-acp/tests/common.rs` (test-helper module) if it doesn't already exist; many existing tests have similar helpers — adapt from `tests/load_session_error_propagation.rs`.

- [ ] **Step 2: Run the test, expect failure (or hang past timeout).**

```bash
cargo test -p spur-acp --test cancel_during_prompt -- --nocapture
```
Expected: the cancel call hangs and the timeout assertion fails. (If `block_task().await` IS already cooperative enough that cancel works without B-1, this test pass — meaning the issue described in the spec is already moot. In that case, document the finding in the commit message and skip B-1 implementation.)

- [ ] **Step 3: Add `cancel_signal_tx` field to `NativeAcpConnection`.** In `crates/spur-acp/src/connection/native.rs`:

Locate the struct definition:

```bash
grep -n "^pub struct NativeAcpConnection\|cmd_tx: Option" crates/spur-acp/src/connection/native.rs | head -5
```

Add the new field next to `cmd_tx`:

```rust
cancel_signal_tx: Option<tokio::sync::mpsc::UnboundedSender<CancelSignal>>,
```

Add the `CancelSignal` type at module level near `AcpCommand`:

```rust
struct CancelSignal {
    session_id: agent_client_protocol::schema::SessionId,
    ack: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
}
```

In the struct's `new()` / `Default` impl, initialize `cancel_signal_tx: None`.

- [ ] **Step 4: Pass `cancel_rx` into `acp_thread_main` and spawn the sibling task.** In `initialize()` (around `native.rs:396`), create the channel before the thread spawn:

```rust
let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<AcpCommand>();
let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<CancelSignal>();
self.cmd_tx = Some(cmd_tx.clone());
self.cancel_signal_tx = Some(cancel_tx);
```

Update the `acp_thread_main` signature to accept `cancel_rx`. Inside `acp_thread_main`, after `connect_with(...)` builds the handlers and inside the `connect_with` async closure (just BEFORE the `while let Some(cmd) = cmd_rx.recv().await` loop at `native.rs:1547`):

```rust
let cx_for_cancel = cx.clone();
cx.spawn(async move {
    let mut cancel_rx = cancel_rx;
    while let Some(signal) = cancel_rx.recv().await {
        let cancel = agent_client_protocol::schema::CancelNotification::new(signal.session_id);
        let res = cx_for_cancel
            .send_notification(cancel)
            .map_err(|e| anyhow::anyhow!("cancel notification send failed: {e}"));
        let _ = signal.ack.send(res);
    }
    Ok(())
})?;
```

> **Important:** the closure must NEVER return `Err`, or `cx.spawn`'s contract is violated and the whole connection shuts down. We map send errors into the oneshot ack instead.

`cancel_rx` must be moved into the closure — make sure you've taken ownership (the variable is captured `move` already).

- [ ] **Step 5: Remove the `AcpCommand::Cancel` arm.** Delete the match arm at `native.rs:1599-1608`. Also delete the `AcpCommand::Cancel { session_id, reply }` variant from the `AcpCommand` enum at `native.rs:112` (you'll get a non-exhaustive-match warning until done).

- [ ] **Step 6: Update public `cancel()` to use `cancel_signal_tx`.** Replace the body of `cancel()` at `native.rs:555-582`:

```rust
async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
    let tx = self.cancel_signal_tx.as_ref().ok_or_else(|| {
        anyhow::anyhow!("NativeAcpConnection '{}': not initialized", self.agent_name)
    })?;

    tracing::debug!(
        agent = %self.agent_name,
        session = %session_id,
        "NativeAcpConnection: routing cancel through out-of-band channel (B-1)"
    );

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    tx.send(CancelSignal {
        session_id: agent_client_protocol::schema::SessionId(session_id.to_string().into()),
        ack: ack_tx,
    })
    .map_err(|_| {
        anyhow::anyhow!(
            "NativeAcpConnection '{}': cancel channel closed (thread dead?)",
            self.agent_name
        )
    })?;

    ack_rx
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "NativeAcpConnection '{}': cancel sibling task dropped before ack",
                self.agent_name
            )
        })?
}
```

- [ ] **Step 7: Run the cancel-during-prompt test.**

```bash
cargo test -p spur-acp --test cancel_during_prompt -- --nocapture
```
Expected: pass within 1s.

- [ ] **Step 8: Run the full test suite to catch regressions.**

```bash
cargo test -p spur-acp
```
Expected: all existing tests still pass. Particularly verify `tests/process_kill_on_drop.rs` and any cancel-touching test.

- [ ] **Step 9: Commit.**

```bash
git add crates/spur-acp/src/connection/native.rs \
        crates/spur-acp/tests/cancel_during_prompt.rs \
        crates/spur-acp/tests/fixtures/cancel_during_prompt.mjs \
        crates/spur-acp/tests/common.rs
git commit -m "spur-acp: native cancel via out-of-band sibling task (Fix B-1)

Cancel notifications now bypass the cmd loop via a dedicated channel +
sibling task spawned through cx.spawn. This unblocks Tier-1 silent
retry from queueing behind a wedged prompt. Verified by new test
cancel_during_prompt.rs which would deadlock without this change."
```

---

### Task 3.2: Sibling-task lifecycle hardening

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Write the failing test.** Add to an existing `tests/process_kill_on_drop.rs` or create `tests/cancel_sibling_lifecycle.rs`:

```rust
#[tokio::test]
async fn cancel_signal_after_shutdown_returns_err() {
    let (mut conn, _child) = common::spawn_native_with_fixture("cancel_during_prompt.mjs").await;
    common::init_and_new_session(&mut conn).await;
    conn.shutdown().await.expect("shutdown");

    // After shutdown, cancel must return a sane error rather than panic
    // or hang.
    let res = conn.cancel("any-session").await;
    assert!(res.is_err(), "cancel after shutdown must Err");
}
```

- [ ] **Step 2: Run, expect failure.**

```bash
cargo test -p spur-acp --test cancel_sibling_lifecycle
```
Expected: panic or timeout on shutdown / cancel.

- [ ] **Step 3: Ensure shutdown drops `cancel_signal_tx`.** Locate `shutdown()` at `native.rs:586`. After taking `cmd_tx`, also clear `cancel_signal_tx`:

```rust
async fn shutdown(&mut self) -> anyhow::Result<()> {
    tracing::info!(agent = %self.agent_name, "NativeAcpConnection: shutting down");

    if let Some(cmd_tx) = self.cmd_tx.take() {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let _ = cmd_tx.send(AcpCommand::Shutdown { reply: reply_tx });
        let _ = reply_rx.await;
    }
    // Drop cancel channel — sibling task observes recv()=None and exits Ok.
    self.cancel_signal_tx = None;

    if let Some(handle) = self.thread_handle.take() {
        let _ = handle.join();
    }
    self.health_status = AgentHealth::Unknown;
    Ok(())
}
```

- [ ] **Step 4: Run, expect pass.**

```bash
cargo test -p spur-acp --test cancel_sibling_lifecycle
```
Expected: pass — the post-shutdown `cancel()` returns Err immediately because `cancel_signal_tx.as_ref()` is None.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-acp/src/connection/native.rs crates/spur-acp/tests/cancel_sibling_lifecycle.rs
git commit -m "spur-acp: drop cancel_signal_tx on shutdown so sibling task exits"
```

---

## Phase 4 — Recovery coordinator

### Task 4.1: Define `RecoveryEvent`, `StallResolution`, `StallStateSnapshot` API types

**Files:**
- Modify: `crates/spur-acp/src/connection/recovery.rs`

- [ ] **Step 1: Append types to `recovery.rs`.**

```rust
use std::time::Duration;

use agent_client_protocol::schema::SessionId;
use uuid::Uuid;

use crate::domain::events::ResolvedBy;
use super::watchdog::InFlightState;

/// Events emitted by the recovery coordinator. Mirrors `SpurEventBody`
/// variants but is the connection-level fan-out (vs orchestrator-level
/// SpurEvent fan-out). Bridge code in spur-core converts between.
#[derive(Debug, Clone)]
pub enum RecoveryEvent {
    Stalled {
        stall_id: Uuid,
        session_id: SessionId,
        last_activity_ago: Duration,
        state: InFlightState,
        transient_error: Option<String>,
    },
    SilentRetryAttempted {
        stall_id: Uuid,
        session_id: SessionId,
        reason: String,
    },
    SilentRetrySucceeded {
        stall_id: Uuid,
        session_id: SessionId,
    },
    SilentRetryFailed {
        stall_id: Uuid,
        session_id: SessionId,
        error: String,
    },
    StallResolved {
        stall_id: Uuid,
        session_id: SessionId,
        by: ResolvedBy,
    },
    ProcessExited {
        session_id: SessionId,
        code: Option<i32>,
    },
}

/// User-selectable resolution for an active stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallResolution {
    RetryTurn,
    ResetSession,
    WaitLonger,
}

/// Broadcast capacity for the recovery channel. Late subscribers may miss
/// events; they MUST call `current_stall_state()` to reconcile.
pub const RECOVERY_BROADCAST_CAPACITY: usize = 32;
```

- [ ] **Step 2: Compile-check.**

```bash
cargo check -p spur-acp
```
Expected: clean.

- [ ] **Step 3: Commit.**

```bash
git add crates/spur-acp/src/connection/recovery.rs
git commit -m "spur-acp: define RecoveryEvent + StallResolution + broadcast capacity"
```

---

### Task 4.2: Implement `RecoveryCoordinator` (Tier-1 + Tier-2 control flow)

**Files:**
- Modify: `crates/spur-acp/src/connection/recovery.rs`

- [ ] **Step 1: Write failing tests.** Append to `recovery.rs`:

```rust
#[cfg(test)]
mod coordinator_tests {
    use super::*;
    use crate::connection::turn_context::TurnContext;
    use std::sync::{Arc, Mutex};

    fn sid() -> SessionId {
        SessionId("test-coord".into())
    }

    #[test]
    fn coordinator_emits_stalled_when_no_transient_error() {
        let ctx = TurnContext::arc_mutex(sid());
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let coord = RecoveryCoordinator::new(ctx.clone(), tx);

        let stall_id = coord.fire_stall(InFlightState::Streaming, Duration::from_secs(60));
        let evt = rx.try_recv().expect("event sent");
        match evt {
            RecoveryEvent::Stalled { stall_id: id, .. } => assert_eq!(id, stall_id),
            other => panic!("expected Stalled, got {other:?}"),
        }
    }

    #[test]
    fn current_stall_state_returns_some_after_fire() {
        let ctx = TurnContext::arc_mutex(sid());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let coord = RecoveryCoordinator::new(ctx.clone(), tx);

        coord.fire_stall(InFlightState::Streaming, Duration::from_secs(60));
        let snap = ctx.lock().unwrap().snapshot_stall();
        assert!(snap.is_some());
    }

    #[test]
    fn resolve_stall_with_stale_id_errors() {
        let ctx = TurnContext::arc_mutex(sid());
        let (tx, _rx) = tokio::sync::broadcast::channel(8);
        let coord = RecoveryCoordinator::new(ctx.clone(), tx);

        coord.fire_stall(InFlightState::Streaming, Duration::from_secs(60));
        let bogus = Uuid::nil();
        let res = coord.mark_resolved(bogus, ResolvedBy::UserRetry);
        assert!(res.is_err());
    }

    #[test]
    fn resolve_stall_with_correct_id_clears_state_and_emits() {
        let ctx = TurnContext::arc_mutex(sid());
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let coord = RecoveryCoordinator::new(ctx.clone(), tx);

        let stall_id = coord.fire_stall(InFlightState::Streaming, Duration::from_secs(60));
        // Drain the Stalled event.
        let _ = rx.try_recv();
        coord
            .mark_resolved(stall_id, ResolvedBy::UserRetry)
            .expect("resolve should succeed");
        let evt = rx.try_recv().expect("StallResolved emitted");
        match evt {
            RecoveryEvent::StallResolved { stall_id: id, .. } => assert_eq!(id, stall_id),
            other => panic!("expected StallResolved, got {other:?}"),
        }
        assert!(ctx.lock().unwrap().current_stall.is_none());
    }
}
```

- [ ] **Step 2: Run tests, expect failure.**

```bash
cargo test -p spur-acp --lib connection::recovery::coordinator_tests
```
Expected: error — `RecoveryCoordinator` doesn't exist.

- [ ] **Step 3: Implement `RecoveryCoordinator`.** Append to `recovery.rs`:

```rust
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::connection::turn_context::{StallState, StallStateSnapshot, TurnContext};
use crate::error::AcpError;

#[derive(Debug)]
pub struct RecoveryCoordinator {
    turn_ctx: Arc<Mutex<TurnContext>>,
    events: tokio::sync::broadcast::Sender<RecoveryEvent>,
}

impl RecoveryCoordinator {
    pub fn new(
        turn_ctx: Arc<Mutex<TurnContext>>,
        events: tokio::sync::broadcast::Sender<RecoveryEvent>,
    ) -> Self {
        Self { turn_ctx, events }
    }

    pub fn snapshot(&self) -> Option<StallStateSnapshot> {
        self.turn_ctx.lock().ok().and_then(|c| c.snapshot_stall())
    }

    /// Set TurnContext.current_stall and broadcast `Stalled`. Returns the new stall_id.
    pub fn fire_stall(
        &self,
        state: InFlightState,
        last_activity_ago: Duration,
    ) -> Uuid {
        let stall_id = Uuid::new_v4();
        let session_id = {
            let mut ctx = self.turn_ctx.lock().unwrap();
            let transient = ctx.last_transport_error.clone();
            ctx.current_stall = Some(StallState {
                stall_id,
                fired_at: Instant::now(),
                in_flight_state: state.clone(),
                transient_error: transient.clone(),
            });
            ctx.session_id.clone()
        };
        let _ = self.events.send(RecoveryEvent::Stalled {
            stall_id,
            session_id: session_id.clone(),
            last_activity_ago,
            state,
            transient_error: self
                .turn_ctx
                .lock()
                .unwrap()
                .last_transport_error
                .clone(),
        });
        stall_id
    }

    /// Clear the stall and broadcast `StallResolved`.
    /// Returns Err(AcpError::StaleStallId) if `stall_id` doesn't match the active stall.
    pub fn mark_resolved(&self, stall_id: Uuid, by: ResolvedBy) -> Result<(), AcpError> {
        let session_id = {
            let mut ctx = self.turn_ctx.lock().unwrap();
            match &ctx.current_stall {
                Some(s) if s.stall_id == stall_id => {
                    let sid = ctx.session_id.clone();
                    ctx.current_stall = None;
                    sid
                }
                _ => return Err(AcpError::StaleStallId { stall_id }),
            }
        };
        let _ = self.events.send(RecoveryEvent::StallResolved {
            stall_id,
            session_id,
            by,
        });
        Ok(())
    }

    /// Decide whether Tier-1 silent retry applies for the current stall.
    pub fn should_attempt_tier1(&self, auto_silent_retry: bool, max_retries: u32) -> bool {
        if !auto_silent_retry {
            return false;
        }
        let ctx = self.turn_ctx.lock().unwrap();
        let Some(err) = ctx.last_transport_error.as_deref() else {
            return false;
        };
        if !is_transient(err) {
            return false;
        }
        ctx.silent_retries_used < max_retries
    }

    /// Mark a Tier-1 retry as attempted and emit the event.
    pub fn mark_silent_retry_attempted(&self, stall_id: Uuid, reason: String) {
        let session_id = {
            let mut ctx = self.turn_ctx.lock().unwrap();
            ctx.silent_retries_used = ctx.silent_retries_used.saturating_add(1);
            ctx.session_id.clone()
        };
        let _ = self.events.send(RecoveryEvent::SilentRetryAttempted {
            stall_id,
            session_id,
            reason,
        });
    }

    pub fn mark_silent_retry_succeeded(&self, stall_id: Uuid) {
        let sid = self.turn_ctx.lock().unwrap().session_id.clone();
        let _ = self.events.send(RecoveryEvent::SilentRetrySucceeded {
            stall_id,
            session_id: sid,
        });
    }

    pub fn mark_silent_retry_failed(&self, stall_id: Uuid, error: String) {
        let sid = self.turn_ctx.lock().unwrap().session_id.clone();
        let _ = self.events.send(RecoveryEvent::SilentRetryFailed {
            stall_id,
            session_id: sid,
            error,
        });
    }
}
```

- [ ] **Step 4: Run tests.**

```bash
cargo test -p spur-acp --lib connection::recovery
```
Expected: all 4 coordinator tests + 6 classifier tests pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-acp/src/connection/recovery.rs
git commit -m "spur-acp: implement RecoveryCoordinator with Tier-1/Tier-2 control flow"
```

---

### Task 4.3: Tier-1 silent retry sequence (cancel + grace + re-prompt)

**Files:**
- Modify: `crates/spur-acp/src/connection/recovery.rs`

- [ ] **Step 1: Write the failing test.** Append to `recovery.rs`:

```rust
#[cfg(test)]
mod tier1_sequence_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock that records cancel calls and re-prompt calls.
    struct MockCancellable {
        cancel_count: Arc<AtomicUsize>,
        reprompt_count: Arc<AtomicUsize>,
        // True → first reprompt errors, second succeeds.
        fail_first: bool,
        attempts: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tier1Driver for MockCancellable {
        async fn cancel(&self, _session_id: &str) -> anyhow::Result<()> {
            self.cancel_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn reprompt(&self) -> anyhow::Result<()> {
            self.reprompt_count.fetch_add(1, Ordering::SeqCst);
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.fail_first && n == 0 {
                Err(anyhow::anyhow!("still wedged"))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn tier1_sends_one_cancel_and_one_reprompt_on_success() {
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let reprompt_count = Arc::new(AtomicUsize::new(0));
        let attempts = Arc::new(AtomicUsize::new(0));
        let driver = MockCancellable {
            cancel_count: cancel_count.clone(),
            reprompt_count: reprompt_count.clone(),
            fail_first: false,
            attempts,
        };

        run_tier1(&driver, "test-session", Duration::from_millis(500)).await
            .expect("should succeed");
        assert_eq!(cancel_count.load(Ordering::SeqCst), 1);
        assert_eq!(reprompt_count.load(Ordering::SeqCst), 1);
    }
}
```

- [ ] **Step 2: Run, expect failure.**

```bash
cargo test -p spur-acp --lib connection::recovery::tier1_sequence_tests
```
Expected: error — `Tier1Driver` and `run_tier1` don't exist.

- [ ] **Step 3: Add `Tier1Driver` trait + `run_tier1` function.** Append to `recovery.rs`:

```rust
#[async_trait::async_trait]
pub trait Tier1Driver: Send + Sync {
    async fn cancel(&self, session_id: &str) -> anyhow::Result<()>;
    async fn reprompt(&self) -> anyhow::Result<()>;
}

/// Execute the Tier-1 silent-retry sequence:
///   1. cancel (returns when notification is on the wire — no agent ack).
///   2. wait `cancel_grace`.
///   3. re-issue the prompt.
pub async fn run_tier1(
    driver: &dyn Tier1Driver,
    session_id: &str,
    cancel_grace: Duration,
) -> anyhow::Result<()> {
    driver.cancel(session_id).await?;
    tokio::time::sleep(cancel_grace).await;
    driver.reprompt().await
}
```

- [ ] **Step 4: Run, expect pass.**

```bash
cargo test -p spur-acp --lib connection::recovery::tier1_sequence_tests
```
Expected: pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/spur-acp/src/connection/recovery.rs
git commit -m "spur-acp: add Tier1Driver trait + run_tier1 cancel/grace/reprompt sequence"
```

---

## Phase 5 — Trait surface + TestStub

### Task 5.1: Extend `AgentConnection` trait with recovery methods

**Files:**
- Modify: `crates/spur-acp/src/connection/mod.rs`

- [ ] **Step 1: Add trait methods with default impls.** Append to the `AgentConnection` trait body in `mod.rs`:

```rust
/// Subscribe to `RecoveryEvent`s emitted by the connection's recovery coordinator.
///
/// Returns `None` for transports without a coordinator (only `NativeAcpConnection`
/// and the other native-stream adapters override this with `Some`).
fn subscribe_recovery_events(
    &self,
) -> Option<broadcast::Receiver<crate::connection::recovery::RecoveryEvent>> {
    None
}

/// Resolve an active stall. The `stall_id` must match the current stall;
/// stale clicks return `Err(AcpError::StaleStallId)`.
async fn resolve_stall(
    &self,
    stall_id: uuid::Uuid,
    choice: crate::connection::recovery::StallResolution,
) -> Result<(), AcpError> {
    let _ = (stall_id, choice);
    Err(AcpError::CapabilityMissing("resolve_stall"))
}

/// Snapshot of the current stall state, if any. Used by late subscribers
/// to reconcile after a Lagged broadcast.
fn current_stall_state(&self) -> Option<crate::connection::turn_context::StallStateSnapshot> {
    None
}
```

- [ ] **Step 2: Update `TestStubConnection`.** At `mod.rs:293`, add the new methods (the trait's default impls already cover most cases; only verify it compiles):

```bash
cargo check -p spur-acp
```
Expected: clean.

- [ ] **Step 3: Update any other `impl AgentConnection` blocks** that exist in tests / examples / inside other crates. Search:

```bash
grep -rn "impl AgentConnection" --include="*.rs" .
```
Each impl that doesn't pick up the default impl (e.g., legacy NullConn variants in test code) needs a one-line `// uses default` comment, or no change since default impls cover them. Compile-check after every modification:

```bash
cargo check --workspace
```

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-acp/src/connection/mod.rs
git commit -m "spur-acp: add subscribe_recovery_events/resolve_stall/current_stall_state to AgentConnection"
```

---

## Phase 6 — Adapter integration

### Task 6.1: Wire watchdog + recovery into `NativeAcpConnection`

**Files:**
- Modify: `crates/spur-acp/src/connection/native.rs`

- [ ] **Step 1: Add fields to `NativeAcpConnection`.** Add (next to existing fields):

```rust
recovery_events_tx: Option<tokio::sync::broadcast::Sender<crate::connection::recovery::RecoveryEvent>>,
turn_ctx: Option<std::sync::Arc<std::sync::Mutex<crate::connection::turn_context::TurnContext>>>,
```

In `new()`/init, initialize both as `None`. In `initialize()`, after the cmd channel setup, also create the broadcast:

```rust
let (rec_tx, _) = tokio::sync::broadcast::channel(
    crate::connection::recovery::RECOVERY_BROADCAST_CAPACITY,
);
self.recovery_events_tx = Some(rec_tx);
```

- [ ] **Step 2: Override the trait methods.** In the `impl AgentConnection for NativeAcpConnection` block, add:

```rust
fn subscribe_recovery_events(
    &self,
) -> Option<broadcast::Receiver<crate::connection::recovery::RecoveryEvent>> {
    self.recovery_events_tx.as_ref().map(|t| t.subscribe())
}

fn current_stall_state(&self) -> Option<crate::connection::turn_context::StallStateSnapshot> {
    self.turn_ctx
        .as_ref()
        .and_then(|c| c.lock().ok().and_then(|t| t.snapshot_stall()))
}

async fn resolve_stall(
    &self,
    stall_id: uuid::Uuid,
    choice: crate::connection::recovery::StallResolution,
) -> Result<(), AcpError> {
    use crate::connection::recovery::{RecoveryCoordinator, StallResolution};
    use crate::domain::events::ResolvedBy;

    let coord = self.recovery_coordinator()?;
    match choice {
        StallResolution::WaitLonger => coord.mark_resolved(stall_id, ResolvedBy::UserWait),
        StallResolution::RetryTurn => {
            // 1) cancel + grace; 2) re-prompt is initiated by the orchestrator
            //    holding the original prompt — we just clear the stall here.
            //    Actual re-prompt wiring is the orchestrator's responsibility.
            coord.mark_resolved(stall_id, ResolvedBy::UserRetry)
        }
        StallResolution::ResetSession => {
            // Same: clear the stall; orchestrator decides to call new_session.
            //    The new_session_id is filled in by orchestrator when it knows.
            coord.mark_resolved(
                stall_id,
                ResolvedBy::UserReset {
                    new_session_id: agent_client_protocol::schema::SessionId(
                        "pending".into(),
                    ),
                },
            )
        }
    }
}
```

> **Note:** the orchestrator is the agent of *action* for retry/reset; spur-acp's role is to clear the stall state and emit the resolution event. The orchestrator listens on RecoveryEvent::StallResolved and dispatches the corresponding cancel/new_session/prompt cycle. This design keeps spur-acp's surface narrow and lets the orchestrator decide policy (e.g. how to reuse the original prompt request).

- [ ] **Step 3: Add `recovery_coordinator()` helper.** In the `impl NativeAcpConnection` (private) block:

```rust
fn recovery_coordinator(&self) -> Result<crate::connection::recovery::RecoveryCoordinator, AcpError> {
    let tx = self
        .recovery_events_tx
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("recovery channel not initialized"))?;
    let ctx = self
        .turn_ctx
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("turn context not initialized"))?;
    Ok(crate::connection::recovery::RecoveryCoordinator::new(
        ctx.clone(),
        tx.clone(),
    ))
}
```

- [ ] **Step 4: Spawn watchdog inside `prompt()`.** Locate the `AcpCommand::Prompt` arm at `native.rs:1572`. BEFORE the `cx.send_request(request).block_task().await` call:

```rust
// Construct per-turn context.
let turn_ctx = crate::connection::turn_context::TurnContext::arc_mutex(
    request.session_id.clone(),
);
*self.turn_ctx_slot.lock().unwrap() = Some(turn_ctx.clone());

let in_flight_state = std::sync::Arc::new(std::sync::Mutex::new(
    crate::connection::watchdog::InFlightState::Idle,
));
let last_activity_at = std::sync::Arc::new(std::sync::Mutex::new(
    std::time::Instant::now(),
));

let policy = self.resolved_recovery_policy.clone();
let (fire_tx, mut fire_rx) = tokio::sync::mpsc::unbounded_channel::<
    crate::connection::watchdog::WatchdogFired,
>();
let _wd = crate::connection::watchdog::Watchdog::spawn(
    std::time::Duration::from_secs(policy.heartbeat_base_secs),
    in_flight_state.clone(),
    last_activity_at.clone(),
    fire_tx,
);

// Subscribe to broadcast notifications and update state on each event.
let session_notif_rx_for_state = self.session_notif_tx.subscribe();
let in_flight_for_pump = in_flight_state.clone();
let last_for_pump = last_activity_at.clone();
let pump = tokio::spawn(async move {
    let mut rx = session_notif_rx_for_state;
    while let Ok(notif) = rx.recv().await {
        let mut s = in_flight_for_pump.lock().unwrap();
        *s = s.clone().after_update(&notif.update);
        *last_for_pump.lock().unwrap() = std::time::Instant::now();
    }
});

// ... existing send_request(prompt).block_task().await call ...

// Stop the pump on turn end.
pump.abort();
```

> **Note:** `turn_ctx_slot` is a new `Arc<Mutex<Option<Arc<Mutex<TurnContext>>>>>` field on the connection so the recovery coordinator helper can find the live ctx. Add it.

- [ ] **Step 5: Handle watchdog fires in the prompt arm.** Add `tokio::select!` around the `block_task().await`:

```rust
let prompt_result = tokio::select! {
    res = cx.send_request(request).block_task() => res.map(|_| ()),
    Some(fired) = fire_rx.recv() => {
        let coord = crate::connection::recovery::RecoveryCoordinator::new(
            turn_ctx.clone(),
            self.recovery_events_tx.as_ref().unwrap().clone(),
        );
        let stall_id = coord.fire_stall(fired.state, fired.elapsed);
        // For now: do NOT auto-tier-1 inside the connection; emit Stalled and
        // let the orchestrator decide. Tier-1 driving lives in the
        // orchestrator (separate plan / phase).
        let _ = stall_id;
        Err(anyhow::anyhow!("turn stalled"))
    }
};
```

> **Plan-time note:** Tier-1 *automation* (the silent retry loop) is wired by the orchestrator that holds the prompt request, NOT inside spur-acp. spur-acp emits the events; the orchestrator reacts by calling `cancel()` + a fresh `prompt()`. Keeping policy outside the transport keeps the AgentConnection trait clean. This is consistent with how `BrainReconnecting/Reconnected/ReconnectFailed` already live at the orchestrator level.

- [ ] **Step 6: Capture errors into TurnContext.** In the `Err(e) =>` log branch at `native.rs:1591`:

```rust
Err(e) => {
    if let Some(ctx) = self.turn_ctx.as_ref() {
        ctx.lock().unwrap().last_transport_error = Some(e.to_string());
    }
    tracing::warn!(
        agent = %agent_name_loop,
        session = %session_id_for_probe,
        "NativeAcpConnection: prompt failed: {e}"
    );
}
```

- [ ] **Step 7: Compile + run existing tests.**

```bash
cargo test -p spur-acp
```
Expected: all existing tests still pass.

- [ ] **Step 8: Commit.**

```bash
git add crates/spur-acp/src/connection/native.rs
git commit -m "spur-acp: wire watchdog + recovery coordinator into NativeAcpConnection.prompt()"
```

---

### Task 6.2: Wire into `StdioAdapter`

**Files:**
- Modify: `crates/spur-acp/src/connection/stdio_adapter.rs`

- [ ] **Step 1: Skim StdioAdapter's prompt() and reader-task structure.**

```bash
grep -n "fn prompt\|fn cancel\|tokio::spawn" crates/spur-acp/src/connection/stdio_adapter.rs | head -20
```

- [ ] **Step 2: Mirror the Native pattern.** Add `recovery_events_tx`, `turn_ctx`, and override the same three trait methods. The adapter's reader task already produces a stream of `SessionNotification`s — pump them through the same `after_update` + `last_activity_at` machinery. On reader Err, write to TurnContext.last_transport_error.

(Detailed code mirrors Task 6.1 verbatim with the adapter's struct names substituted. Apply the same five points: fields, trait overrides, helper, watchdog spawn, error capture.)

- [ ] **Step 3: Compile + run existing tests.**

```bash
cargo test -p spur-acp
```

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-acp/src/connection/stdio_adapter.rs
git commit -m "spur-acp: wire watchdog + recovery into StdioAdapter"
```

---

### Task 6.3: Wire into `CliWrapAdapter`

**Files:**
- Modify: `crates/spur-acp/src/connection/cli_wrap_adapter.rs`

- [ ] Same shape as Task 6.2. Run tests; commit.

---

### Task 6.4: Wire into `StreamJsonAdapter`

**Files:**
- Modify: `crates/spur-acp/src/connection/stream_json_adapter.rs`

- [ ] Same shape as Task 6.2 with one wrinkle: StreamJsonAdapter spawns a **fresh subprocess per turn**. The TurnContext is naturally per-turn already; just ensure the watchdog is constructed AFTER the subprocess spawn and BEFORE the first read. On subprocess EOF, emit `RecoveryEvent::ProcessExited` before returning.

Run tests; commit.

---

## Phase 7 — Integration tests

> Each test gets a Node fixture under `tests/fixtures/` mirroring the existing `load_error_stub.mjs` / `agent_trailing_notification.sh` patterns. The fixtures are short (≈40 lines). The test files exercise the public AgentConnection surface end-to-end against the fixture subprocess.
>
> All tests use `#[tokio::test(start_paused = true)]` where time-dependence matters.

### Task 7.1: `stream_watchdog_silent_stall.rs`

**Files:**
- Create: `crates/spur-acp/tests/stream_watchdog_silent_stall.rs`
- Create: `crates/spur-acp/tests/fixtures/watchdog_silent_stall.mjs`

- [ ] **Step 1: Fixture.** `watchdog_silent_stall.mjs`:

```javascript
#!/usr/bin/env node
// Streams two AgentMessageChunks, then sleeps forever.
import { stdin, stdout } from 'node:process';
let buf = '';
function emit(o) { stdout.write(JSON.stringify(o) + '\n'); }
stdin.setEncoding('utf8');
stdin.on('data', (c) => {
  buf += c;
  const lines = buf.split('\n');
  buf = lines.pop();
  for (const line of lines) {
    if (!line.trim()) continue;
    let m; try { m = JSON.parse(line); } catch { continue; }
    if (m.method === 'initialize')
      emit({ jsonrpc:'2.0', id:m.id, result:{ protocolVersion:1, agentCapabilities:{} } });
    else if (m.method === 'session/new')
      emit({ jsonrpc:'2.0', id:m.id, result:{ sessionId:'sess-ss-1' } });
    else if (m.method === 'session/prompt') {
      emit({ jsonrpc:'2.0', method:'session/update', params:{
        sessionId:'sess-ss-1',
        update:{ kind:'agent_message_chunk', content:{ type:'text', text:'one' } } } });
      emit({ jsonrpc:'2.0', method:'session/update', params:{
        sessionId:'sess-ss-1',
        update:{ kind:'agent_message_chunk', content:{ type:'text', text:'two' } } } });
      // Then nothing forever.
    }
  }
});
```

- [ ] **Step 2: Test.**

```rust
//! With paused tokio time, after 60s of silence (base) following the second
//! chunk, BrainStalled / RecoveryEvent::Stalled fires.

mod common;
use std::time::Duration;
use tokio::time::{advance, pause};

#[tokio::test(start_paused = true)]
async fn watchdog_fires_after_base_timeout_on_silent_stall() {
    let (mut conn, _child) =
        common::spawn_native_with_fixture("watchdog_silent_stall.mjs").await;
    common::init_and_new_session(&mut conn).await;
    let mut rx = conn.subscribe_recovery_events().expect("recovery channel");

    let _stream = common::send_test_prompt(&mut conn).await;

    // Drive past 2 chunks (delivered ~immediately) then silent.
    advance(Duration::from_secs(61)).await;
    tokio::task::yield_now().await;

    let evt = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("watchdog should fire")
        .expect("event");
    use spur_acp::connection::recovery::RecoveryEvent;
    assert!(matches!(evt, RecoveryEvent::Stalled { .. }));
}
```

- [ ] **Step 3: Run.**

```bash
cargo test -p spur-acp --test stream_watchdog_silent_stall
```

- [ ] **Step 4: Commit.**

```bash
git add crates/spur-acp/tests/stream_watchdog_silent_stall.rs \
        crates/spur-acp/tests/fixtures/watchdog_silent_stall.mjs
git commit -m "spur-acp: integration test — watchdog fires on silent stall"
```

---

### Task 7.2: `stream_watchdog_tool_call_grace.rs`

**Files:**
- Create test + fixture mirroring 7.1.

- [ ] **Fixture:** emits `ToolCall { id:'tc-1' }`, then NOTHING for "200s" of paused time, then `ToolCallUpdate { id:'tc-1', status:'completed' }`, then a final `agent_message_chunk` "done".
- [ ] **Test:** with `base = 60`, `MULT_TOOL_RUNNING = 10`, threshold = 600s. Advance 200s — assert NO Stalled event. Advance further — assert turn completes normally.
- [ ] **Run + commit.**

---

### Task 7.3: `stream_watchdog_thinking_grace.rs`

- [ ] Same shape. Fixture emits `AgentThoughtChunk` then holds 120s. With `base=60` and `MULT_THINKING=3`, threshold=180s — no fire.

---

### Task 7.4: `stream_watchdog_tier1_signal.rs`

> **Note:** Phase 6 wires the watchdog and emits Stalled / SilentRetry events but does NOT drive the actual cancel-and-reprompt sequence. The orchestrator does that. So the integration test here verifies that the SIGNAL (RecoveryEvent::Stalled with `transient_error: Some("Stream idle timeout...")`) is emitted, not that auto-recovery actually completes the turn. The end-to-end recovery test is an orchestrator-layer test (out of scope for this plan).

- [ ] **Fixture:** Streams one chunk, emits a JSON-RPC error response with the literal "Stream idle timeout - partial response received" message.
- [ ] **Test:** assert `RecoveryEvent::Stalled` fires with `transient_error.unwrap().contains("Stream idle timeout")`.

---

### Task 7.5: `stall_id_staleness.rs`

- [ ] **Test:** fire a stall (programmatically inside spur-acp's TestStub or via a fixture), call `resolve_stall(stall_id, RetryTurn)` — expect Ok. Call again with the same `stall_id` — expect `Err(AcpError::StaleStallId)`.

---

### Task 7.6: `current_stall_state_resync.rs`

- [ ] **Test:** subscribe AFTER the watchdog has already fired. Subscriber's first `recv()` would miss the original Stalled event. Call `current_stall_state()` — assert `Some(snapshot)` matching the active stall.

---

### Task 7.7: Update existing tests

- [ ] `tests/process_kill_on_drop.rs`: verify watchdog and pump tasks are aborted when the connection is dropped (no leaked tokio tasks). Use `tokio::runtime::Handle::current().metrics()` if available, else manual JoinHandle assertions.
- [ ] `tests/load_session_error_propagation.rs`: confirm `is_transient(load_session_err.to_string())` returns `false` (Resource not found is NOT in the allow-list).

---

## Phase 8 — Documentation + final verification

### Task 8.1: Update `crates/spur-acp/README.md` (if present) or module-level docs

- [ ] Add a short section describing the watchdog & recovery model. One paragraph.

### Task 8.2: Run the full workspace test suite

- [ ] ```bash
cargo test --workspace
```
Expected: clean.

### Task 8.3: Run clippy on spur-acp

- [ ] ```bash
cargo clippy -p spur-acp --all-targets -- -D warnings
```
Expected: no warnings.

### Task 8.4: Final commit + push

- [ ] ```bash
git push -u origin feat/spur-acp-stream-watchdog
```

---

## Self-review checklist (run by the executor before marking the plan complete)

- [ ] **Spec coverage:** Every requirement in `docs/superpowers/specs/2026-05-05-spur-acp-stream-watchdog-design.md` corresponds to a task. Sections covered: Goals 1–6 ✓, Failure modes F1/F2/F3/F5 ✓, Two-tier flow ✓, Watchdog state machine ✓ (Task 2.2/2.3), Tier-1 ✓ (Task 4.3 + orchestrator scope note), Tier-2 ✓ (Task 4.2 + 5.1), Public API ✓ (Task 5.1), Configuration ✓ (Task 1.3), Domain events ✓ (Task 1.2), Implementation outline file list ✓, Testing strategy ✓ (Phase 7).
- [ ] **No placeholders:** every task has concrete file paths, test code, and run commands. The orchestrator-side Tier-1 driving is explicitly noted as out of scope and not a placeholder TODO.
- [ ] **Type consistency:** `InFlightState`, `TurnContext`, `StallStateSnapshot`, `ResolvedBy`, `RecoveryEvent`, `StallResolution` use the same names everywhere they appear. Channel name: `cancel_signal_tx` (not `cancel_tx`). Method name: `current_stall_state()` (not `snapshot_stall_state()`).
- [ ] **B-1 caveat:** if Task 3.1 Step 2 reveals that cancel ALREADY works without B-1 (because `block_task().await` cooperatively yields enough that the cmd loop drains queued commands during the prompt's wait), document the finding and skip Phase 3 implementation. The other phases stand alone.
- [ ] **Plan-time deviations from spec** are flagged in the header so reviewers see them up front.
