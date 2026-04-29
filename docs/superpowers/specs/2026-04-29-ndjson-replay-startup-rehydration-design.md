# NDJSON replay on TUI startup — rehydrate projections from EventSink

**Beads epic:** bd-1vnk
**Status:** Design (locked after L9 multi-round MCTS evaluation)
**Date:** 2026-04-29
**Author:** Brain + collaborator workers (gemini, kimi, claude-code, codex), synthesized via L9 first-principles pass

## 1. Problem

In-memory derived projections in SPUR populate only from observing live `SpurEvent`s in the current process:

- `ExecutorLineage` — `crates/spur-core/src/lineage/projection.rs:65`
- `PlanProjectionStore` — `crates/spur-core/src/plan_projection/projection.rs:18`
- `SessionSynopsisProjection` — `crates/spur-core/src/session_synopsis/projection.rs:68`

When the TUI restarts, all three start empty. Sessions that existed before the restart appear in the picker (via `SessionsListed` from disk metadata) but have no synopsis, lineage, or plan state until the user explicitly resumes one. Concretely: the session picker preview pane is empty for any session not resumed in the current TUI process. This is the documented v1 limitation in `docs/superpowers/specs/2026-04-28-session-picker-recall-revamp-design.md` (§Risks row "Projection lost on TUI restart").

The `EventSink` already writes a JSONL of every `SpurEvent` to `.spur/events/{pid}-{ts}-{n}.ndjson` with a 64 MiB total / 8 MiB per-file rotation policy (`crates/spur-core/src/event_sink.rs:18-23,127-159`). The natural fix — canonical Tier 1 action #2 in `docs/architecture.md:772` — is: at TUI startup, replay the NDJSON through every projection's `apply(&event)` path before live broadcast subscription begins.

## 2. Goal

Implement a startup replay phase: before the TUI subscribes to the live broadcast, the App reads the EventSink's NDJSON ring (in chronological order across all rotation segments) and feeds every event through each projection's `apply(...)`. After replay, the broadcast subscription begins for live events.

The replay must be:
- **deterministic** — single-threaded, ordered by file `(unix_ms, rotation_seq)`, append-order within each file,
- **bounded** — events older than `now() - replay_horizon` are skipped at parse time (default 7 days, configurable),
- **cheap on cold start** — target <500 ms for full-disk-cap replay (~50K events) on dev hardware.

## 3. Explicit non-goals

The following are **out of scope** and tracked separately:

1. **Lagged-during-live recovery.** When the TUI's broadcast receiver returns `RecvError::Lagged(n)` mid-session (`app.rs:4135-4144` async path, `app.rs:4196-4205` drain path), it logs and permanently drops events. Closing this gap is half-B of `architecture.md:772` Tier 1 #2 and a separate epic. The replay primitive defined here is API-extensible to that follow-up, but no contract is made today.

2. **Synthetic-event divergence.** The TUI applies `ExecutorReviewRequested` and `ExecutorReviewResolved` events directly to its own lineage at `app.rs:859` and `app.rs:3348` without routing through `Orchestrator::emit`. After TUI restart, these review-state changes cannot be reconstructed from NDJSON. Tracked as **bd-1vnk-5** (separate beads issue).

3. **EventSink rotation truncation.** Events older than the disk-cap window (~64 MiB worth) are deleted by `enforce_event_cap` (`event_sink.rs:171-214`) and cannot be replayed. The session picker shows the placeholder from bd-3kx3 for affected sessions. Documented v1 limitation.

4. **Splash screen / progress UI.** The <500 ms target is below user-perceptible threshold. No UI work needed.

5. **Bot integration.** `spur-bot` does not currently hold any of the three projections (verified at `crates/spur-bot/src/runtime.rs:88-120`). The `replay_events` API is naturally bot-friendly via closure dispatch — when bot ever wants its own projections, it adopts the same primitive. No bot-side wiring required for this epic.

## 4. Architecture

### 4.1 Module layout

One new file: **`crates/spur-core/src/event_replay.rs`** (~150 LoC).

Dependency direction is strictly inbound:
- depends on `spur_acp::SpurEvent` (the wire type),
- depends on `crates/spur-core/src/event_sink.rs` only to share the `events_dir()` constant (promote to `pub(crate)`),
- callers (TUI, future bot) own the dispatch closure — `event_replay` does not import any frontend types.

### 4.2 Public API

```rust
use std::path::PathBuf;
use std::time::Duration;
use spur_acp::SpurEvent;

#[derive(Debug, Clone)]
pub struct ReplayConfig {
    pub events_dir: PathBuf,
    pub replay_horizon: Duration,
    pub skip_pid: Option<u32>,
    pub max_line_bytes: usize,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            events_dir: PathBuf::from(".spur/events"),
            replay_horizon: Duration::from_secs(7 * 86400),
            skip_pid: Some(std::process::id()),
            max_line_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReplayStats {
    pub files_read: usize,
    pub events_applied: u64,
    pub events_skipped_horizon: u64,
    pub events_skipped_pid: u64,
    pub malformed_lines: u64,
    pub elapsed: Duration,
}

pub fn replay_events<F>(
    config: &ReplayConfig,
    on_event: F,
) -> std::io::Result<ReplayStats>
where
    F: FnMut(&SpurEvent);
```

### 4.3 Why no `Projection` trait

Three workers (gemini, kimi, codex) proposed extracting a `Projection` trait so `replay_events` could take `&mut [&mut dyn Projection]`. Rejected, with reasoning:

- **No real reuse.** The TUI is the only caller today (`app.rs:2151,2154,2155`). The bot doesn't hold these projections.
- **Static dispatch is simpler.** A closure capturing three `&mut` borrows is trivially correct, eliminates vtable cost, and matches the existing live-flow shape at `app.rs:2149-2155`.
- **Relevance-filter savings are illusory.** Each projection's `apply()` already early-returns on irrelevant variants via `_ => {}` arms (e.g. `session_synopsis/projection.rs:135`).

If a fourth projection ever lands and the closure call site grows uncomfortable, the trait can be extracted then. Today's call site is three lines.

### 4.4 Why no `from_seq_exclusive` / delayed-subscribe today

Codex proposed a unified primitive: `from_seq_exclusive: Option<u64>` plus a host-side delayed-subscribe API, so the same code serves startup AND Lagged-recovery. The shape is correct in theory but premature for this epic:

- Lagged-recovery is structurally separate (a paused-drain → replay → merge protocol that requires its own design pass).
- Adding `from_seq_exclusive` now without a Lagged caller is dead code surface — and the field is conditionally meaningful only for the current PID (it makes no sense for prior PIDs whose `seq` resets to 0 each process). The interaction with `skip_pid` is non-obvious, which is bad API hygiene.
- The `ReplayConfig` struct is API-additive: adding `from_seq_exclusive: Option<u64>` later will not break existing callers.

The simpler shape is correct; the extension path is open.

### 4.5 Subscribe-immediate vs delayed-subscribe

Codex argued for moving `host.spawn`'s call to `orch.subscribe()` (currently at `crates/spur-interactive/src/host.rs:87`) AFTER replay completes, so no live event can be queued mid-replay. We choose subscribe-immediate-then-replay because:

- The orchestrator is idle during the TUI's startup window. No user input has been registered; no auto-resumed brain has been spawned. Realistic emit rate during the replay window is ~10 events (license check, pgid registration, orphan-reaped notifications). The 4096-slot broadcast buffer absorbs this trivially.
- Delayed-subscribe is a 50-LoC plumbing change in `host.rs` that buys correctness against a scenario that does not occur naturally. YAGNI.
- The underlying substrate is already imperfect: `EventSink` itself drops on broadcast Lagged at `event_sink.rs:67-70`. NDJSON is not a strict mirror of the broadcast. Promising "no event lost across replay" is overstating what NDJSON guarantees.

If telemetry later shows the broadcast buffer filling during replay, delayed-subscribe is added in a follow-up.

## 5. Algorithm

### 5.1 File discovery and ordering

1. `fs::read_dir(events_dir)`. Tolerate `NotFound` → return empty `ReplayStats`.
2. Filter to `.ndjson` extension.
3. Parse filename `{pid}-{unix_ms}-{rotation_seq}.ndjson` (`event_sink.rs:221-228`). On parse failure, log + skip.
4. Drop entries where `Some(pid) == config.skip_pid`.
5. Sort by `(unix_ms, rotation_seq)` ascending. Cross-PID interleaving is acceptable: each session's events come from one orchestrator process at a time, so per-session causal order is preserved within each file. Cross-session interleaving across PIDs is not used by any of the three projections.

### 5.2 Per-file streaming

```rust
let file = File::open(&path)?;
let mut reader = BufReader::with_capacity(64 * 1024, file);
let mut buf: Vec<u8> = Vec::with_capacity(4096);

loop {
    buf.clear();
    let n = reader.read_until(b'\n', &mut buf)?;
    if n == 0 { break; }
    if buf.len() > config.max_line_bytes {
        stats.malformed_lines += 1;
        continue;
    }
    let line = if buf.last() == Some(&b'\n') { &buf[..buf.len()-1] } else { &buf[..] };
    let event: SpurEvent = match serde_json::from_slice(line) {
        Ok(ev) => ev,
        Err(e) => { tracing::warn!(error = %e, ?path, "malformed NDJSON line"); stats.malformed_lines += 1; continue; }
    };
    if cutoff.is_some_and(|c| event.occurred_at < c) {
        stats.events_skipped_horizon += 1;
        continue;
    }
    on_event(&event);
    stats.events_applied += 1;
}
```

### 5.3 Horizon enforcement

`cutoff = SystemTime::now().checked_sub(config.replay_horizon)`. If subtraction underflows (e.g. wall-clock near `UNIX_EPOCH`), skip horizon filtering — apply everything. This is a soft cost-bound; correctness does not depend on it.

### 5.4 Failure modes

| Failure | Handling |
|---|---|
| `events_dir` does not exist | Return empty `ReplayStats` |
| Filename parse fails | Log + skip the file |
| File `NotFound` mid-iteration (concurrent `enforce_event_cap`) | Skip + continue |
| Line exceeds `max_line_bytes` | Increment `malformed_lines` + continue |
| `serde_json::from_slice` fails | Log + increment `malformed_lines` + continue |
| `apply()` panics inside the closure | **No `catch_unwind`.** Propagates. Matches the strict contract of the live event loop and surfaces real bugs rather than hiding them. |

## 6. Schema-evolution patch

Add a forward-compatible fallback variant to `SpurEventBody`:

```rust
// crates/spur-acp/src/domain/events.rs:346
#[non_exhaustive]
pub enum SpurEventBody {
    // ... existing variants ...
    #[serde(other)]
    Unknown,
}
```

Rationale: `#[non_exhaustive]` at line 345 today only affects Rust match-exhaustiveness — serde still fails to deserialize lines with renamed/removed variants. Adding `#[serde(other)] Unknown` makes deserialization forward-compatible: lines whose variant tag isn't recognized deserialize to `Unknown`, replay's `apply()` ignores them via existing `_ => {}` fallbacks (verified at `lineage/projection.rs:392`, `session_synopsis/projection.rs:135`, and `plan_projection/projection.rs:24`). This costs one no-op variant; reverting a wire format change requires no NDJSON-side fix.

Test: round-trip a JSONL line with `{"body": {"FutureUnseenVariant": {...}}}`, expect deserialization to `SpurEventBody::Unknown`.

## 7. Doc amendment

Amend `crates/spur-core/src/lineage/projection.rs:16-21` to acknowledge what the kimi adversarial pass discovered:

```rust
//! ## Idempotency
//!
//! Most event arms are idempotent — applying the same event twice produces
//! the same state as applying it once. **Counter-incrementing arms are
//! NOT idempotent under double-apply**: `WorkerNotification(ToolCall)` at
//! :289 (tool_call_count += 1), `WorkerFileTouched(Write)` at :322
//! (files_touched_count += 1), and `CostUpdate` at adapter.rs:287
//! (cost_usd += ...). The replay model in `crates/spur-core/src/event_replay.rs`
//! is structurally guarded against double-apply via PID-filtered file selection
//! (current process's events come live; prior processes' events are applied
//! once to a fresh empty projection).
```

Same caveat lands in a one-line note on `session_synopsis/projection.rs:67` (`UserMessageChunk` appends to pending) for completeness.

## 8. TUI integration

Single insertion at **`crates/spur-tui/src/app.rs:4046`**, between `App::build_with_license_state` returning and the broadcast loop at `:4118`:

```rust
let mut app = App::build_with_license_state(/* ... */).await?;

let stats = spur_core::event_replay::replay_events(
    &spur_core::event_replay::ReplayConfig {
        replay_horizon: cfg.event_replay_horizon(),
        ..Default::default()
    },
    |ev| {
        app.lineage.apply(ev);
        app.plan_projection.apply(ev);
        app.synopsis.apply(ev);
    },
).unwrap_or_else(|e| {
    tracing::warn!(error = %e, "event replay failed; starting with empty projections");
    spur_core::event_replay::ReplayStats::default()
});
tracing::info!(
    target: "spur.metrics.event_replay",
    files = stats.files_read,
    applied = stats.events_applied,
    horizon_skipped = stats.events_skipped_horizon,
    pid_skipped = stats.events_skipped_pid,
    malformed = stats.malformed_lines,
    elapsed_ms = stats.elapsed.as_millis() as u64,
);
```

Add `event_replay_horizon: Duration` to `LogConfig` (`crates/spur-acp/src/config/mod.rs`) with default `Duration::from_secs(7 * 86400)` and env var `SPUR_EVENT_REPLAY_HORIZON_SECS` for override.

## 9. Performance

Per-event cost (single thread, dev hardware, BufReader 64 KB, `serde_json::from_slice` on borrowed bytes, reused line `Vec<u8>`):

- `read_until`: ~2 µs typical line.
- `serde_json::from_slice`: ~5–15 µs typical body.
- 3× projection apply: ~3 µs.

Total: ~10–20 µs per event. At 50K events (full disk cap): 500 ms–1.0 s.

**Optimization headroom if needed**: swap `serde_json` for `simd-json` (~2–3× parse speedup, drop-in dep). Not required up front — measure first.

### 9.1 Benchmark commitment

Add `crates/spur-core/benches/event_replay.rs` using Criterion. Generates a 50K-event fixture split across 7 NDJSON files (matching the realistic rotation pattern at the disk cap) with 1% intentionally-malformed lines. Asserts replay completes in <500 ms median on dev hardware. Logged to a CI baseline; failure is a warning (not a hard block) until we have stable runner perf data.

## 10. Observability

Single `tracing::info!` at `target: "spur.metrics.event_replay"` after replay completes. Fields: `files`, `applied`, `horizon_skipped`, `pid_skipped`, `malformed`, `elapsed_ms`. Format matches the `spur.metrics.outcome_swept` and worktree-authority metric tracing conventions already in `orchestrator.rs:1539-1547` and `worktree_authority.rs:102-104`.

Malformed lines log per-incident at `tracing::warn!` with rate-limiting via the standard `tracing` filter (no custom rate limiter needed for v1; if a corrupt log floods we add one then).

## 11. Decomposition

Five sub-issues. Sub-issues 1–4 belong to bd-1vnk; sub-issue 5 is a separate beads issue tracked in parallel.

| ID | Title | Precondition | Notes |
|---|---|---|---|
| **bd-1vnk-1** | `#[serde(other)] Unknown` variant on `SpurEventBody`; lineage doc amend | none | Single-line schema patch + doc edit. TDD: failing test deserializing a future-variant line. |
| **bd-1vnk-2** | `event_replay.rs` module | bd-1vnk-1 | File discovery, `(unix_ms, rot_seq)` ordering, horizon filter, malformed handling. Unit tests with `tempfile`. |
| **bd-1vnk-3** | TUI wiring at `app.rs:4046` + `event_replay_horizon` config | bd-1vnk-2 | Integration test: write fixture NDJSON → build App → replay → assert all three projections converge. |
| **bd-1vnk-4** | Criterion bench `bench_replay_full_cap` | bd-1vnk-2 | 50K-event fixture, <500ms target on dev hardware. |
| **bd-1vnk-5** *(separate)* | Route synthetic events at `app.rs:859,3348` through `Orchestrator::emit` | none | Closes TUI-restart divergence for review state. Independent of bd-1vnk; tracked as its own beads issue. |

## 12. Acceptance criteria

- A `replay_events(...)` API in `spur-core` that streams NDJSON through a caller-supplied closure.
- TUI's `run_tui_with_license` calls replay before entering the broadcast loop.
- bd-evz7's session-picker preview is populated for any session whose history exists in the NDJSON ring within the replay horizon.
- bd-3kx3's placeholder still appears for sessions whose events were rotated out by `enforce_event_cap`.
- Replay performance: <500 ms for full-disk-cap (~50K events) on dev hardware. Criterion bench gates this.
- Schema-evolution: a JSONL line containing a `SpurEventBody` variant unknown to the running binary deserializes to `Unknown` and is silently ignored by all three projections.

## 13. Risks and counter-arguments

1. **`serde_json` parse is the long pole at 50K events.** Counter: dev-hardware bench measures the actual number; `simd-json` is a drop-in escape hatch.
2. **Cross-PID file ordering quirks.** Counter: per-session causal order is preserved within each PID's file sequence. The three projections key on `SessionId` / `ExecutorId` / plan ID; cross-session ordering doesn't affect their final state.
3. **PID recycling on long-uptime hosts (rare).** A new TUI process spawning with the same PID as a long-dead orphan NDJSON file means `skip_pid` skips the orphan's history. Worst case: that prior session's preview is empty (bd-3kx3 placeholder shows). Probability per startup ≈ 1/32K on Linux. Documented v1 acceptable; revisit if it ever bites.
4. **Synthetic events at `app.rs:859,3348` lost across restart.** Tracked as bd-1vnk-5; not blocking this epic because review state does not affect the synopsis preview.

## 14. Alternatives considered

1. **Snapshot/checkpoint projection state on shutdown, reload at startup** — rejected. Projection structs change frequently as features land (10+ shape changes to `ExecutorNode` in the last 3 months). Maintaining serde migrations for projection types is a permanent tax. The event format is forward-stable by team convention; projection types are not. Replay decouples persistence from projection shape.
2. **Trait-based replay over `&mut [&mut dyn Projection]`** — rejected. Closure dispatch is simpler, no vtable cost, and the bot doesn't hold these projections.
3. **Unified primitive with `from_seq_exclusive` for both startup and Lagged-recovery** — deferred. The Lagged path needs a paused-drain merge protocol that is out of scope here. The `ReplayConfig` struct is additively extensible to that shape.
4. **Delayed subscribe in `host.rs`** — deferred. The realistic startup-window emit rate is ~10 events; the broadcast buffer (4096) absorbs it. Plumbing delayed-subscribe buys correctness against a non-occurring scenario.
5. **Two-phase parse with `serde_json::value::RawValue`** — rejected for now. `#[serde(other)] Unknown` discharges the same hazard at one line of code. RawValue is the right tool when we need to extract envelope metadata before parsing the body — not the case today.

## 15. References

- bd-1vnk (this epic).
- `docs/architecture.md:697` (Risk #9), `:772` (Tier 1 #2 canonical action).
- `docs/superpowers/specs/2026-04-28-session-picker-recall-revamp-design.md` — closed bd-evz7; this epic closes the §Risks rows "Projection lost on TUI restart" (full close) and "Broadcast `Lagged` during history replay" (half-close: startup only).
- bd-evz7 (closed) — session picker recall.
- bd-3kx3 (closed) — empty-state placeholder; remains the bridge for sessions whose events rotated out.
- bd-1vnk-5 (separate, to be filed) — synthetic-event divergence at `app.rs:859,3348`.
