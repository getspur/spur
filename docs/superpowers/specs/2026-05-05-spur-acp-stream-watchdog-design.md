# spur-acp Stream Watchdog & Auto-Recovery

## Problem

When spur-acp talks to an upstream agent (Claude Code, Codex, Kiro, Gemini, etc.) over the ACP protocol, a turn can die mid-stream in several ways. The user-visible symptom that triggered this work is the Claude SDK's:

> `API Error: Stream idle timeout - partial response received`

Today, the symptom is much worse than that error message implies. spur-acp surfaces the error as `AcpError::Transport(anyhow::Error)` and **does not retry, reconnect, or even consistently surface a usable affordance to the user**. Worse, the error case is the *good* one — silent stalls (process hung, network half-open, SDK deadlock) leave `prompt()` hanging forever with no error at all, and the orchestrator has no signal to act on.

We need a uniform, agent-agnostic mechanism to detect dead turns, attempt a transparent recovery for the obviously-transient cases, and surface a clear user choice for the ambiguous ones.

## Goals

1. Detect both errored stalls (F1) and silent stalls (F2) with a single mechanism.
2. Be agent-agnostic — same machinery for `claude-code`, `codex`, `kiro`, `gemini`, `opencode`, `kimi`.
3. Auto-recover transparently from a known-transient transport blip (one silent retry per turn).
4. For ambiguous wedges, surface a non-modal banner with `[Retry turn] [Reset session] [Wait longer]` and let the user decide.
5. Avoid false positives during legitimate long silence (extended thinking, multi-minute tool calls).
6. Bounded scope — ship inside spur-acp without protocol-level changes or upstream patches.

## Non-Goals

- Tuning per-tool timeout overrides (no `bash_tool_secs`, etc.). Built-in multipliers only.
- Cross-provider failover ("if Claude is down, try Codex"). Reuses the existing `[failover]` machinery, not in this scope.
- Resuming a partial response (not supported by ACP — see Verified Context).
- Auto-killing the agent subprocess. The hard-reconnect ladder rung exists but is initiated only by explicit user `[Reset session]`.
- Configurable error-classifier strings. The transient-pattern list is hard-coded for v1.

## Verified Upstream Context

Two independent cross-checks (codex + gemini) confirmed:

| Finding | Verdict | Implication for design |
|---|---|---|
| `"Stream idle timeout - partial response received"` originates in `@anthropic-ai/claude-agent-sdk` (the SDK calls `abortController.abort()` and throws). It is **not** in the ACP Rust SDK or the `@agentclientprotocol/claude-agent-acp` Node bridge. | Confirmed | Cannot fix at protocol layer. Must handle as opaque transport error. Symptoms differ per agent; mechanism must NOT depend on the specific string. |
| The Node bridge does **not** dedupe prompts. A retry without `session/cancel` enqueues a duplicate run via `pendingMessages`. | Confirmed | Tier-1 silent retry **must** issue `session/cancel` and **await its acknowledgement** before re-issuing `session/prompt`. |
| ACP defines no chunk-offset / continuation-token resumption. `session/load` replays full history; `session/resume` skips replay but does not resume mid-stream. | Confirmed | Recovery always replays the whole turn from scratch. Partial output must be discarded on retry, not reconciled. |
| The Rust SDK collapses generic errors into JSON-RPC `InternalError` (-32603) with the message string in `data`. No transient-vs-fatal classification. | Confirmed | spur-acp owns its own classifier (substring match on a small allow-list). |
| `CLAUDE_STREAM_IDLE_TIMEOUT_MS` and `CLAUDE_ENABLE_STREAM_WATCHDOG=0` env vars are respected by the underlying Claude Agent SDK and pass through the bridge unchanged. | Refined (gemini) | We may bump or disable the upstream watchdog if our own watchdog supersedes it. Out of scope for v1; noted for tuning. |

## Design

### Failure mode taxonomy

| ID | Description | Detected today | After this design |
|---|---|---|---|
| F1 | Upstream raises a stream/transport error mid-turn | yes (as opaque error) | yes; if string matches known-transient pattern, Tier 1 silent retry |
| F2 | Upstream stops streaming with no error | **no — hangs forever** | yes; watchdog fires after `(now - last_activity) > timeout × multiplier` |
| F3 | Subprocess dies | yes (EOF on stdio) | unchanged |
| F5 | Legitimate long silence (extended thinking, long tool call) | n/a | suppressed via state-aware multiplier; user can `[Wait longer]` if multiplier still under-shoots |

### Two-tier recovery flow

```
                    Per-turn watchdog (one per AgentConnection prompt() call)
                         │
                         │  base_timeout_secs = 60 (default, configurable)
                         │  multiplier(state):
                         │     Idle/Streaming → 1×    Thinking → 3×    ToolRunning → 10×
                         │  reset on ANY inbound JSON-RPC frame
                         │
            ┌────────────┴────────────┐
            │                         │
       timer fires              turn completes
            │                         │
            ▼                         ▼
    Was a transient error      teardown watchdog,
    captured this turn?         emit BrainTurnComplete
    (substring match on
     allow-list)
            │
       ┌────┴────┐
      YES        NO  (or already retried this turn)
       │          │
       ▼          ▼
     Tier 1:   Tier 2:
     await     emit BrainStalled
     cancel,   { last_activity_ago, in_flight_state }
     resend    → TUI shows non-modal banner:
     same      [Retry turn] [Reset session] [Wait longer]
     prompt    → user choice → matching action
     (silent,  → if no user response in 60s, banner stays visible
      brief    → if process EOF arrives, escalate to Reset automatically
      "↻"
      indicator)
```

### Watchdog state machine

State for in-flight tracking, derived from `session/update` notifications:

```rust
enum InFlightState {
    Idle,                 // prompt sent, no events yet
    Streaming,            // last event was agent_message_chunk
    Thinking,             // last event was agent_thought_chunk
    ToolRunning(ToolId),  // tool_call_started observed, no matching tool_call_complete yet
}
```

Transitions are best-effort and forgiving — unknown event kinds reset `last_activity_at` but do not change state. If a `tool_call_started` is observed without a matching `tool_call_complete`, state stays `ToolRunning` until the next non-tool event or turn end.

Multipliers are hard-coded constants in v1:

```rust
const MULT_IDLE: f64       = 1.0;
const MULT_STREAMING: f64  = 1.0;
const MULT_THINKING: f64   = 3.0;
const MULT_TOOL_RUNNING: f64 = 10.0;
```

The watchdog computes `effective_timeout = base * multiplier(state)` on every event and reschedules. `last_activity_at` is reset on every inbound frame regardless of kind, including JSON-RPC keepalives, stderr output, and unknown notifications. This is more lenient than strict ACP-event tracking — proof-of-life from any layer counts.

### Tier-1 silent retry (auto-recovery)

Triggered when:
1. Watchdog fires.
2. AND `auto_silent_retry` is true (default).
3. AND a transient-pattern error was captured during this turn (recorded in `last_transport_error: Option<String>` on the turn context).
4. AND `silent_retries_used_this_turn < max_silent_retries_per_turn` (default 1).

Sequence:
1. Increment `silent_retries_used_this_turn`.
2. Emit `BrainSilentRetryAttempted { reason }`.
3. Issue `session/cancel`. **Await** the JSON-RPC response (with its own short timeout — 10s).
4. Re-issue `session/prompt` with the same prompt text on the same session.
5. Reset watchdog timer (`last_activity_at = now`, state = `Idle`, multiplier = 1×).
6. On success of the retried turn → emit `BrainSilentRetrySucceeded`.
7. On failure → emit `BrainSilentRetryFailed`, fall through to Tier 2.

Transient-pattern allow-list (hard-coded constant in v1):

```rust
const TRANSIENT_PATTERNS: &[&str] = &[
    "Stream idle timeout",
    "partial response received",
    "ECONNRESET",
    "EPIPE",
    "broken pipe",
    "connection reset",
];
```

Match is case-insensitive substring on the error's `Display` string. Conservative on purpose — we'd rather miss a real transient (and fall through to the user banner) than silent-retry on a fatal error and hide it.

### Tier-2 user banner

When Tier 1 is not applicable or has failed:

1. Emit `BrainStalled { last_activity_ago: Duration, in_flight_state: InFlightState, transient_error: Option<String> }`.
2. TUI consumes this event and shows a non-modal banner near the active session view:
   ```
   ⚠ No response from <agent-handle> for 2m 12s (state: Thinking).
     [R]etry turn   [S]reset session   [W]ait longer
   ```
3. spur-acp exposes a control method on `AgentConnection`:
   ```rust
   pub enum StallResolution {
       RetryTurn,
       ResetSession,
       WaitLonger,
   }

   pub async fn resolve_stall(&self, choice: StallResolution) -> Result<(), AcpError>;
   ```
4. Action mapping:
   - `RetryTurn` → cancel (await ack) → re-send same prompt to same session. Watchdog restarts.
   - `ResetSession` → cancel (await ack) → `session/new` → re-send prompt to new session. Emit `BrainResetByUser`.
   - `WaitLonger` → reset watchdog with the same threshold for one more grace period; emit `BrainStallExtended`. (Not a permanent threshold bump — the bias should be against staying stuck.)
5. While the banner is up, if subprocess EOF arrives → automatic escalation: emit `BrainProcessExited`, the connection moves to a terminal failed state, banner replaced with reset prompt.

### Public API surface

Additions to `crates/spur-acp/src/connection/mod.rs`:

```rust
pub trait AgentConnection {
    // existing methods unchanged

    fn subscribe_recovery_events(&self) -> broadcast::Receiver<RecoveryEvent>;
    async fn resolve_stall(&self, choice: StallResolution) -> Result<(), AcpError>;
}

pub enum RecoveryEvent {
    Stalled       { session_id: SessionId, last_activity_ago: Duration, state: InFlightState, transient_error: Option<String> },
    SilentRetryAttempted { session_id: SessionId, reason: String },
    SilentRetrySucceeded { session_id: SessionId },
    SilentRetryFailed    { session_id: SessionId, error: String },
    StallExtended { session_id: SessionId },
    ResetByUser   { session_id: SessionId, new_session_id: SessionId },
}
```

`RecoveryEvent` is emitted in addition to (not instead of) the existing `SessionNotification` broadcast. Consumers (TUI, telemetry) subscribe via the new dedicated channel.

### Configuration

New optional table per agent in `.spur/config.toml`:

```toml
[agents.entries.recovery]
heartbeat_base_secs = 60        # optional override; default 60
auto_silent_retry   = true      # optional; default true
```

Schema additions in `crates/spur-acp/src/config/mod.rs`:

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentRecoveryPolicy {
    pub heartbeat_base_secs: u64,    // default 60
    pub auto_silent_retry: bool,     // default true
}

impl Default for AgentRecoveryPolicy {
    fn default() -> Self {
        Self { heartbeat_base_secs: 60, auto_silent_retry: true }
    }
}
```

`max_silent_retries_per_turn` is **not** configurable in v1 — fixed at 1. Multipliers are not configurable. This is YAGNI: ship the simplest knobs and add more if real usage demands them.

### Domain events

Additions to `crates/spur-acp/src/domain/events.rs` `SpurEventBody`:

```rust
BrainStalled { session_id, last_activity_ago, in_flight_state, transient_error },
BrainSilentRetryAttempted { session_id, reason },
BrainSilentRetrySucceeded { session_id },
BrainSilentRetryFailed    { session_id, error },
BrainStallExtended { session_id },
BrainResetByUser   { session_id, new_session_id },
```

Distinct from the existing `BrainReconnecting`/`BrainReconnected`/`BrainReconnectFailed` family, which operate at orchestrator-brain level (full provider switch). The new events are finer-grained, scoped to a single connection / single turn.

## Implementation outline

Five small modules / changes. Estimated total ~470 LOC of production code + ~250 LOC of tests.

| File | Purpose | ~LOC |
|---|---|---|
| `crates/spur-acp/src/connection/watchdog.rs` (new) | Timer task, `InFlightState`, multiplier table, broadcast subscriber loop. Owns one `tokio::task::JoinHandle` per turn. | 150 |
| `crates/spur-acp/src/connection/recovery.rs` (new) | Tier-1 silent retry policy, transient-pattern allow-list, cancel-await-then-retry sequence, Tier-2 stall resolver. | 100 |
| `crates/spur-acp/src/connection/mod.rs` | Wire `Watchdog::spawn(...)` into `prompt()` lifecycle (start on send, drop on completion). Expose `subscribe_recovery_events()` and `resolve_stall()`. | 40 |
| `crates/spur-acp/src/domain/events.rs` | Add the six new `SpurEventBody` variants and `RecoveryEvent` enum. | 30 |
| `crates/spur-acp/src/config/mod.rs` | Add `AgentRecoveryPolicy` and parse it under `[agents.entries.recovery]`. | 30 |
| `crates/spur-acp/src/connection/native.rs`, `stream_json_adapter.rs`, `stdio_adapter.rs`, `cli_wrap_adapter.rs` | Minimal wiring to feed the watchdog from each transport's notification stream and to record `last_transport_error` on observed errors. | 120 (split across 4 files) |

The watchdog's lifetime is strictly the scope of one `prompt()` call. On `Drop`, it cancels its inner timer task. No timers leak across turns.

## Testing strategy

New integration tests in `crates/spur-acp/tests/`:

1. **`stream_watchdog_silent_stall.rs`** — Node fixture script that emits two `session/update` notifications then sleeps forever. Assert `BrainStalled` event fires within `base_timeout + small grace`; assert `prompt()` returns a `Stalled` error or stays open until resolver is called.

2. **`stream_watchdog_tier1_silent_retry.rs`** — Fixture script that, on first `session/prompt`, errors out mid-stream with the literal string `"Stream idle timeout - partial response received"`. On second `session/prompt`, completes normally. Assert: `BrainSilentRetryAttempted` fires; `BrainSilentRetrySucceeded` fires; final `prompt()` returns the second turn's content; only ONE `session/cancel` was sent in between.

3. **`stream_watchdog_tier2_banner.rs`** — Fixture script that errors with a non-allow-listed string. Assert: no silent retry attempted; `BrainStalled` fires; `resolve_stall(StallResolution::RetryTurn)` triggers cancel + re-prompt; success on second attempt.

4. **`stream_watchdog_tool_call_grace.rs`** — Fixture script that emits `tool_call_started`, then sleeps 90s, then emits `tool_call_complete`. With `base_timeout_secs = 60` and `MULT_TOOL_RUNNING = 10`, watchdog should NOT fire (effective threshold 600s). Assert no `BrainStalled`. Test runs in real-time; gated by `#[ignore]` or behind a `slow-tests` feature so it doesn't block CI by default.

5. **`stream_watchdog_thinking_grace.rs`** — Same shape but with `agent_thought_chunk` events. Confirms the `Thinking` multiplier path.

6. **Unit tests** in `watchdog.rs` and `recovery.rs` for the state-transition table and the substring matcher (no I/O, fast).

Existing tests that may need updating:
- `tests/process_kill_on_drop.rs` — verify watchdog teardown happens on drop, no leaked tokio tasks.
- `tests/load_session_error_propagation.rs` — confirm load-session errors are NOT classified as transient (they should NOT trigger silent retry).

## Rejected alternatives

- **Pure passive (status quo).** Dominated. F2 hangs forever.
- **Pure error classifier (no watchdog).** Cannot detect F2 silent stalls. The watchdog is a strict generalization.
- **Strict per-state separate timers.** Overkill. The single-timer-with-multiplier collapses the same expressiveness into one fewer concept.
- **Configurable transient-pattern list.** YAGNI. Six hand-curated strings cover the observed cases. Revisit if real usage produces false negatives.
- **Auto-escalating recovery (silent retry → silent reset → silent reconnect).** Removes user agency for destructive actions. The `[Reset session]` button must be explicit.

## Open questions

1. **Should `WaitLonger` increment a per-session counter?** If a user has hit `WaitLonger` three times in a row, that's a strong signal something is wrong; we might surface a "this session has been stalling repeatedly" hint. Out of scope for v1; revisit after we have data.

2. **Telegram bot integration.** The Telegram brain interface (`bot.telegram` in config) presumably can't show a non-modal banner. For v1, the bot consumer can either auto-pick `RetryTurn` on stall (Tier-1 behavior extended to Tier-2 for non-interactive surfaces) or simply emit the stalled error to the user as a chat message. Decision deferred — the `RecoveryEvent` channel exposes everything needed for either policy.

3. **Should `BrainStalled` count as an `AcpError`?** Currently the design is: `prompt()` does NOT return Err on stall — it stays open while the watchdog and resolver dance. Only if `Reset` happens does the original `prompt()` future get cancelled. This may complicate caller code. Alternative: `prompt()` returns `Err(AcpError::Stalled { resolver })` immediately, the resolver is held by the caller. Pick during writing-plans phase based on actual call-site shape.

## Future work (out of scope)

- Surface `CLAUDE_STREAM_IDLE_TIMEOUT_MS` as a managed env passthrough so users can tune the upstream watchdog independently of ours.
- Per-tool-name multiplier overrides for tools known to be slow (e.g. `Bash` running long builds).
- Automatic learning of typical activity gaps per agent over the session, with the timeout adapting toward observed P99.
- Surfacing watchdog state in `session/update`-derived telemetry for offline analysis of stall frequency.
