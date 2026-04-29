# NDJSON replay on TUI startup — rehydrate projections from EventSink

**Beads epic:** bd-1vnk
**Status:** Design (revised after L9 self-critique pass — original commit `63217f94`)
**Date:** 2026-04-29
**Author:** Brain + collaborator workers (gemini, kimi, claude-code, codex), synthesized via L9 first-principles pass with adversarial revision

## 1. Problem

In-memory derived projections in SPUR populate only from observing live `SpurEvent`s in the current process:

- `ExecutorLineage` — `crates/spur-core/src/lineage/projection.rs:65`
- `PlanProjectionStore` — `crates/spur-core/src/plan_projection/projection.rs:18`
- `SessionSynopsisProjection` — `crates/spur-core/src/session_synopsis/projection.rs:68`

When the TUI restarts, all three start empty. Sessions that existed before the restart appear in the picker (via `SessionsListed` from disk metadata) but have no synopsis, lineage, or plan state until the user explicitly resumes one. Concretely: the session picker preview pane is empty for any session not resumed in the current TUI process. This is the documented v1 limitation in `docs/superpowers/specs/2026-04-28-session-picker-recall-revamp-design.md` (§Risks row "Projection lost on TUI restart").

The `EventSink` already writes a JSONL of every `SpurEvent` to `.spur/events/{pid}-{ts}-{n}.ndjson` with a 64 MiB total / 8 MiB per-file rotation policy (`crates/spur-core/src/event_sink.rs:18-23,127-159`). The natural fix — canonical Tier 1 action #2 in `docs/architecture.md:772` — is: at TUI startup, replay the NDJSON through every projection's `apply(&event)` path before the live broadcast loop begins draining.

## 2. Goal

Implement a startup replay phase: before the TUI's broadcast drain loop begins, the App reads the EventSink's NDJSON ring (in chronological order across all rotation segments) and feeds every event through each projection's `apply(...)`. The broadcast subscription is created earlier (in `host::spawn` at `crates/spur-interactive/src/host.rs:87`), so live events queue into the 4096-slot broadcast buffer during replay; the TUI only starts consuming them after replay completes.

The replay must be:
- **deterministic** — single-threaded, ordered by file `(unix_ms, rotation_seq)`, append-order within each file,
- **bounded** — events older than `now() - replay_horizon` are skipped at parse time (default 7 days, configurable),
- **cheap on cold start** — target <500 ms for full-disk-cap replay (~50K events) on dev hardware.

## 3. Explicit non-goals

The following are **out of scope** and tracked separately:

1. **Lagged-during-live recovery.** When the TUI's broadcast receiver returns `RecvError::Lagged(n)` mid-session (`app.rs:4135-4144` async path, `app.rs:4196-4205` drain path), it logs and permanently drops events. Closing this gap is half-B of `architecture.md:772` Tier 1 #2 and a separate epic. The replay primitive defined here is API-extensible to that follow-up, but no contract is made today.

2. **Synthetic-event divergence.** The TUI applies `ExecutorReviewRequested` and `ExecutorReviewResolved` events directly to its own lineage at `app.rs:859` and `app.rs:3348`. Whether these are also funnel-emitted (and therefore reproducible from NDJSON) has not been verified. Tracked as **bd-1vnk-5** (separate beads issue) which begins with the verification step before deciding whether routing changes are needed.

3. **EventSink rotation truncation.** Events older than the disk-cap window (~64 MiB worth) are deleted by `enforce_event_cap` (`event_sink.rs:171-214`) and cannot be replayed. The session picker shows the placeholder from bd-3kx3 for affected sessions. Documented v1 limitation.

4. **Splash screen / progress UI.** Realistic replay window at full disk cap is 250–500 ms — at the high end this exceeds the ~200 ms threshold for perceived instantaneous response, so users may notice a brief startup pause. We accept this rather than build splash-screen UI: the alternative (sub-200 ms hard target) would force `simd-json` adoption up front. If telemetry shows real-world replay regularly exceeds 300 ms, revisit.

5. **Bot integration.** `spur-bot` does not currently hold any of the three projections (verified at `crates/spur-bot/src/runtime.rs:88-120`). The `replay_events` API is naturally bot-friendly via closure dispatch — when bot ever wants its own projections, it adopts the same primitive. No bot-side wiring required for this epic.

## 4. Architecture

### 4.1 Module layout

One new file: **`crates/spur-core/src/event_replay.rs`** (~180 LoC).

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
    pub files_skipped_pid: usize,
    pub events_applied: u64,
    pub events_skipped_horizon: u64,
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

Note: `ReplayConfig::default()` calls `std::process::id()`, which makes `Default` non-pure. This is intentional — the only sensible default for `skip_pid` is the current process. Documented in the rustdoc.

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

- During the TUI's startup window the orchestrator is mostly idle. Realistic emit count, even with auto-resume kicking off a `SessionHistory` and a flurry of `BrainSpawned`/`AgentSessionReady`, is on the order of dozens to a hundred events. The 4096-slot broadcast buffer absorbs that comfortably.
- Delayed-subscribe is ~50 LoC of plumbing in `host.rs` that buys correctness against a scenario that does not occur naturally. YAGNI.
- The underlying substrate is already imperfect: `EventSink` itself drops on broadcast Lagged at `event_sink.rs:67-70`. NDJSON is not a strict mirror of the broadcast. Promising "no event lost across replay" is overstating what NDJSON guarantees.

If telemetry later shows the broadcast buffer filling during replay, delayed-subscribe is added in a follow-up.

## 5. Algorithm

### 5.1 File discovery and ordering

1. `fs::read_dir(events_dir)`. Tolerate `NotFound` → return empty `ReplayStats`.
2. Filter to `.ndjson` extension.
3. Parse filename `{pid}-{unix_ms}-{rotation_seq}.ndjson` (`event_sink.rs:221-228`). On parse failure, log + skip the file.
4. Drop entries where `Some(pid) == config.skip_pid`. Increment `stats.files_skipped_pid`.
5. Sort by `(unix_ms, rotation_seq)` ascending. Cross-PID interleaving is acceptable: each session's events come from one orchestrator process at a time, so per-session causal order is preserved within each file. Cross-session interleaving across PIDs is not used by any of the three projections.

### 5.2 Per-file streaming

```rust
let start = Instant::now();
let cutoff = SystemTime::now().checked_sub(config.replay_horizon);

for path in ordered_paths {
    let file = match File::open(&path) {
        Ok(f) => f,
        // Tolerate concurrent enforce_event_cap deletion.
        Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
        Err(e) => return Err(e),
    };
    stats.files_read += 1;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut buf: Vec<u8> = Vec::with_capacity(4096);

    loop {
        buf.clear();
        // Bound the per-line allocation BEFORE read_until allocates.
        let mut limited = (&mut reader).take(config.max_line_bytes as u64);
        let n = limited.read_until(b'\n', &mut buf)?;
        if n == 0 { break; }

        let terminated = buf.last() == Some(&b'\n');
        if !terminated {
            // Either EOF mid-line (acceptable; treat as one final line) or
            // the line exceeded max_line_bytes. Distinguish by checking
            // remaining bytes: if more bytes follow without the cap being
            // hit, we genuinely hit max_line_bytes → drain + skip.
            if n as u64 == config.max_line_bytes as u64 {
                stats.malformed_lines += 1;
                drain_until_newline(&mut reader)?;
                continue;
            }
            // Otherwise: legitimate untermined trailing line. Fall through.
        }
        let line = if terminated { &buf[..buf.len()-1] } else { &buf[..] };

        let event: SpurEvent = match serde_json::from_slice(line) {
            Ok(ev) => ev,
            Err(e) => {
                if stats.malformed_lines < FIRST_N_MALFORMED_VERBOSE {
                    tracing::warn!(error = %e, ?path, "malformed NDJSON line");
                }
                stats.malformed_lines += 1;
                continue;
            }
        };

        if cutoff.is_some_and(|c| event.occurred_at < c) {
            stats.events_skipped_horizon += 1;
            continue;
        }

        on_event(&event);
        stats.events_applied += 1;
    }
}

stats.elapsed = start.elapsed();
Ok(stats)
```

`drain_until_newline` is a small helper that reads in 64 KB chunks until the next `\n` or EOF, discarding bytes. Total state during read of one over-cap line is bounded by `max_line_bytes + 64 KB`.

`FIRST_N_MALFORMED_VERBOSE` is a small const (e.g. `8`). After the threshold, malformed lines silently increment the counter; the aggregate count surfaces in the final `tracing::info!`.

### 5.3 Horizon enforcement

`cutoff = SystemTime::now().checked_sub(config.replay_horizon)`. If subtraction underflows (e.g. wall-clock near `UNIX_EPOCH`), `cutoff` is `None` and horizon filtering is skipped — apply everything. The horizon is a soft cost-bound; correctness does not depend on it.

### 5.4 Failure modes

| Failure | Handling |
|---|---|
| `events_dir` does not exist | `read_dir` returns `NotFound` → return empty `ReplayStats` |
| Filename parse fails | Log + skip the file |
| File `NotFound` mid-iteration (concurrent `enforce_event_cap`) | Skip + continue |
| Line exceeds `max_line_bytes` | Drain to next newline, increment `malformed_lines` |
| `serde_json::from_slice` fails (corrupted JSON, **or unknown enum variant from a renamed `SpurEventBody` arm**) | Log first N at `warn`, increment `malformed_lines` + continue |
| `apply()` panics inside the closure | **No `catch_unwind`.** Propagates. Matches the strict contract of the live event loop and surfaces real bugs rather than hiding them. |

**Schema evolution discharged by this row.** `SpurEventBody` is externally-tagged (no `#[serde(tag=...)]` attribute at `events.rs:344-346`), so `#[serde(other)]` cannot be used. The hazard ("future binary renames a variant; old NDJSON has the old name") falls under "serde_json::from_slice fails" and is silently counted as malformed. If telemetry shows post-rename floods, an optional sub-counter `unknown_variant_lines` (extracted from `serde::de::Error` message inspection) is a one-paragraph follow-up; not warranted for v1.

## 6. Doc amendment (lineage projection)

Amend `crates/spur-core/src/lineage/projection.rs:16-21`. The current doc:

> "Every event arm is idempotent — applying the same event twice produces the same state as applying it once. Exception: `SpurEventBody::CostUpdate` is deliberately additive (two updates accumulate). Tests enforce both invariants."

is partially false. The test `applying_same_event_twice_is_idempotent_except_cost` at `crates/spur-core/tests/lineage_integration.rs:317-354` covers `ExecutorSpawned` and `ExecutorPhaseChanged` only. Counter-incrementing arms (`tool_call_count += 1` at `:289`, `files_touched_count += 1` at `:322`) and append arms (`SessionSynopsisProjection` `pending.push_str` at `session_synopsis/projection.rs:79-82`) are NOT idempotent under double-apply.

Replacement doc text:

```rust
//! ## Idempotency
//!
//! Most state-mutation arms are idempotent — applying the same event twice
//! produces the same state as applying it once. The exceptions are:
//!
//! - `SpurEventBody::CostUpdate` (additive: `cost_usd += ...` at
//!   `adapter.rs:287`),
//! - `WorkerNotification(ToolCall)` (counter: `tool_call_count += 1` at :289),
//! - `WorkerFileTouched(Write)` (counter: `files_touched_count += 1` at :322).
//!
//! `crates/spur-core/tests/lineage_integration.rs:317` covers the spawn/phase
//! arms; counter arms are intentionally not idempotency-tested.
//!
//! The replay model in `crates/spur-core/src/event_replay.rs` is structurally
//! guarded against double-apply via PID-filtered file selection: the current
//! process's events arrive via the live broadcast subscription; prior
//! processes' events are applied exactly once to fresh empty projections.
```

The same caveat (one short paragraph) lands on `session_synopsis/projection.rs:67` for the `pending.push_str` append.

## 7. TUI integration

Single insertion in `run_tui_with_license` at `crates/spur-tui/src/app.rs`, between `App::build_with_license_state` returning (line 4040) and the `loop {` that starts the broadcast drain (around line 4115). Concretely, the new lines go between `let mut event_rx = event_rx;` and the `#[cfg(unix)]` signal-handler block:

```rust
let mut app = App::build_with_license_state(
    user_input_tx,
    start_in_picker_with_preselect,
    config.clone(),
    license_state,
    landing,
);
let mut tick_interval = tokio::time::interval(Duration::from_millis(33));
let mut event_stream = crossterm::event::EventStream::new();
let mut event_rx = event_rx;

// === bd-1vnk: rehydrate projections from prior NDJSON before drain begins ===
let replay_cfg = spur_core::event_replay::ReplayConfig {
    replay_horizon: config.log.event_replay_horizon,
    ..Default::default()
};
match spur_core::event_replay::replay_events(&replay_cfg, |ev| {
    app.lineage.apply(ev);
    app.plan_projection.apply(ev);
    app.synopsis.apply(ev);
}) {
    Ok(stats) => tracing::info!(
        target: "spur.metrics.event_replay",
        files = stats.files_read,
        skipped_pid = stats.files_skipped_pid,
        applied = stats.events_applied,
        horizon_skipped = stats.events_skipped_horizon,
        malformed = stats.malformed_lines,
        elapsed_ms = stats.elapsed.as_millis() as u64,
    ),
    Err(e) => tracing::error!(
        error = %e,
        "event replay failed; starting with empty projections"
    ),
}
// ============================================================================
```

Note: `App::build_with_license_state` is **synchronous** (no `.await`, no `Result`) — verified at `app.rs:531`. `config` here is the `Arc<SpurConfig>` parameter of `run_tui_with_license`; `event_replay_horizon` is a plain field on `LogConfig`.

**Config addition** in `crates/spur-acp/src/config/mod.rs` `LogConfig` (line 607), next to `events_max_total_bytes` at line 631:

```rust
/// How far back to replay NDJSON events on TUI startup. Default 7 days.
/// Override with `SPUR_EVENT_REPLAY_HORIZON_SECS`.
#[serde(default = "default_event_replay_horizon", with = "duration_secs_serde")]
pub event_replay_horizon: std::time::Duration,
```

with `default_event_replay_horizon() -> Duration { Duration::from_secs(7 * 86400) }` and a `duration_secs_serde` module mirroring the existing `option_arc_*_serde` patterns elsewhere in the file.

## 8. Performance

Per-event cost (single thread, dev hardware, BufReader 64 KB, `serde_json::from_slice` on borrowed bytes, reused line `Vec<u8>`):

- `read_until`: ~0.1 µs amortized per buffer-hit; ~50 µs per 64 KB syscall.
- `serde_json::from_slice`: ~5–10 µs typical body.
- 3× projection apply: ~0.5–1 µs (HashMap insert/get_mut + simple field writes; lineage also dispatches through `apply_legacy` adapter).

Total: ~6–12 µs per event. At 50K events (full disk cap): **300–600 ms estimated**. The bench commitment in §8.1 is <500 ms median; if the bench misses, `simd-json` is a drop-in dep that delivers ~2–3× parse speedup, dropping us cleanly under target.

### 8.1 Benchmark commitment

Add `crates/spur-core/benches/event_replay.rs` using Criterion. Generates a 50K-event fixture split across 7 NDJSON files (matching the realistic rotation pattern at the disk cap) with 1% intentionally-malformed lines. Asserts replay completes in <500 ms median on dev hardware. Logged to a CI baseline; failure is a warning (not a hard block) until we have stable runner perf data.

## 9. Observability

Single `tracing::info!` at `target: "spur.metrics.event_replay"` after replay completes. Fields: `files`, `skipped_pid`, `applied`, `horizon_skipped`, `malformed`, `elapsed_ms`. Format matches the `spur.metrics.outcome_swept` and worktree-authority metric tracing conventions already in `orchestrator.rs:1539-1547` and `worktree_authority.rs:102-104`.

Malformed lines: first `FIRST_N_MALFORMED_VERBOSE = 8` log per-incident at `tracing::warn!` (with file path + serde error). After the threshold, malformed lines silently increment the counter — the aggregate count surfaces in the final `info!`. This avoids a worst-case 500-line warn-flood on a 1%-corrupt 50K log without needing a custom rate-limiter.

## 10. Decomposition

Three sub-issues within bd-1vnk plus one separate sibling. Each sub-issue is reviewable in isolation and produces a working state.

| ID | Title | Precondition | Notes |
|---|---|---|---|
| **bd-1vnk-1** | `event_replay.rs` module | none | File discovery, `(unix_ms, rot_seq)` ordering, horizon filter, `take()`-bounded per-line read, malformed handling. Unit tests with `tempfile`. |
| **bd-1vnk-2** | TUI wiring + `event_replay_horizon` config + lineage doc amend + architecture.md update | bd-1vnk-1 | Single insertion in `run_tui_with_license`; new `LogConfig` field with env override; doc amend per §6; mark `architecture.md:772` Tier 1 #2 half-A complete and reference half-B follow-up. Integration test: write fixture NDJSON → build App → replay → assert all three projections converge. |
| **bd-1vnk-3** | Criterion bench `bench_replay_full_cap` | bd-1vnk-1 | 50K-event fixture, <500ms target on dev hardware. |
| **bd-1vnk-5** *(separate beads issue)* | Verify and (if needed) route synthetic events at `app.rs:859,3348` through `Orchestrator::emit` | none | Step 1: verify whether the orchestrator already emits these via the funnel, or only the TUI applies them locally. Step 2 (conditional): route through funnel if step 1 finds true divergence. Independent of bd-1vnk. |

## 11. Acceptance criteria

- A `replay_events(...)` API in `spur-core` that streams NDJSON through a caller-supplied closure, returning a populated `ReplayStats`.
- `run_tui_with_license` calls replay between `App::build_with_license_state` and the broadcast drain loop.
- bd-evz7's session-picker preview is populated for any session whose history exists in the NDJSON ring within the replay horizon AND was not rotated out by `enforce_event_cap`.
- bd-3kx3's placeholder appears for sessions whose events were rotated out by `enforce_event_cap` OR were older than `replay_horizon`.
- Replay performance: <500 ms median for full-disk-cap (~50K events) on dev hardware. Criterion bench gates this as a soft warning (not a hard CI block, per §8.1).
- JSONL lines whose `SpurEventBody` variant tag is unknown to the running binary (post-rename) deserialize-fail, increment `malformed_lines`, and do not abort replay.
- Lineage projection doc accurately describes which arms are idempotent and which are not (per §6).
- `docs/architecture.md` Tier 1 #2 (line 772) updated to mark half-A complete with reference to half-B follow-up.

## 12. Risks and counter-arguments

1. **`serde_json` parse is the long pole at 50K events.** Counter: dev-hardware bench measures the actual number; `simd-json` is a drop-in escape hatch.
2. **Cross-PID file ordering quirks.** Counter: per-session causal order is preserved within each PID's file sequence. The three projections key on `SessionId` / `ExecutorId` / plan ID; cross-session ordering doesn't affect their final state.
3. **PID recycling on long-uptime hosts (rare).** Modern Linux defaults `pid_max` to 4 194 304; PID reuse rate is bounded by total process churn between `enforce_event_cap` rotations. Worst case: a new spur process inherits a PID whose old NDJSON is still on disk — `skip_pid` filters that file, and the prior session's preview falls back to the bd-3kx3 placeholder. Probability per startup is low; documented v1 acceptable. If it ever bites, swap `skip_pid` for a process-start-timestamp filter that keys on `unix_ms` rather than `pid`.
4. **EventSink fsync absence.** The sink uses `BufWriter::flush()` on rotation/interval/shutdown but no `sync_all`. If a previous spur process crashed between flush and OS sync, the on-disk content is the kernel-buffered version — no corruption (kernel guarantees write-order), just possibly missing the last few events. Documented v1 acceptable.
5. **Synthetic events at `app.rs:859,3348` may diverge across restart.** Tracked as bd-1vnk-5; not blocking this epic because the verification step has not yet established the divergence is real, and review state does not affect the synopsis preview.

## 13. Alternatives considered

1. **Snapshot/checkpoint projection state on shutdown, reload at startup** — rejected. Projection structs change frequently as features land (10+ shape changes to `ExecutorNode` in the last 3 months). Maintaining serde migrations for projection types is a permanent tax. The event format is forward-stable by team convention; projection types are not. Replay decouples persistence from projection shape.
2. **Trait-based replay over `&mut [&mut dyn Projection]`** — rejected. Closure dispatch is simpler, no vtable cost, and the bot doesn't hold these projections.
3. **Unified primitive with `from_seq_exclusive` for both startup and Lagged-recovery** — deferred. The Lagged path needs a paused-drain merge protocol that is out of scope here. The `ReplayConfig` struct is additively extensible to that shape.
4. **Delayed subscribe in `host.rs`** — deferred. The realistic startup-window emit count is on the order of dozens to a hundred events; the broadcast buffer (4096) absorbs it. Plumbing delayed-subscribe buys correctness against a non-occurring scenario.
5. **`#[serde(other)] Unknown` variant on `SpurEventBody`** — rejected after compile-time analysis. `SpurEventBody` is externally-tagged (no `#[serde(tag=...)]`); serde forbids `#[serde(other)]` on variants of externally-tagged enums. The hazard is instead discharged by the existing malformed-line counter (§5.4).

## 14. References

- bd-1vnk (this epic).
- `docs/architecture.md:697` (Risk #9), `:772` (Tier 1 #2 canonical action).
- `docs/superpowers/specs/2026-04-28-session-picker-recall-revamp-design.md` — closed bd-evz7; this epic closes the §Risks rows "Projection lost on TUI restart" (full close) and "Broadcast `Lagged` during history replay" (half-close: startup only).
- bd-evz7 (closed) — session picker recall.
- bd-3kx3 (closed) — empty-state placeholder; remains the bridge for sessions whose events rotated out or fell outside the replay horizon.
- bd-1vnk-5 (separate, to be filed) — synthetic-event divergence verification at `app.rs:859,3348`.
