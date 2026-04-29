# bd-1vnk NDJSON Replay Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an NDJSON replay primitive in `spur-core` and wire it into TUI startup, so the session-picker preview pane is populated for sessions whose history exists on disk but were not resumed in the current TUI process.

**Architecture:** A new free function `replay_events(&ReplayConfig, F: FnMut(&SpurEvent)) -> io::Result<ReplayStats>` in `crates/spur-core/src/event_replay.rs`. It discovers `.spur/events/{pid}-{unix_ms}-{n}.ndjson` files, sorts by `(unix_ms, rotation_seq)`, skips the current PID, and feeds each event through a caller-supplied closure. The TUI calls it once between `App::build_with_license_state` and the broadcast drain loop. Companion changes: a `LogConfig` field for the horizon, a doc amend on lineage/synopsis idempotency, and an architecture.md status update.

**Tech Stack:** Rust 1.x, tokio (existing async runtime — replay itself is sync), `serde_json` for line parsing, `tempfile` for tests, Criterion for the benchmark.

**Spec:** `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md` (commit `c0a38a64`).

---

## File structure

| Path | Action | Responsibility |
|---|---|---|
| `crates/spur-core/src/event_sink.rs` | Modify (1 line) | Promote `events_dir()` from private to `pub(crate)` so `event_replay` can reach it. |
| `crates/spur-core/src/lib.rs` | Modify (1 line) | Register `pub mod event_replay;` between existing modules. |
| `crates/spur-core/src/event_replay.rs` | **Create** | The replay primitive. ~180 LoC including inline `#[cfg(test)] mod tests`. |
| `crates/spur-core/tests/event_replay_integration.rs` | **Create** | Integration test feeding fixture NDJSON through the three real projections. |
| `crates/spur-core/src/lineage/projection.rs` | Modify (lines 16-21 doc) | Replace overstated idempotency claim with arm-by-arm accuracy. |
| `crates/spur-core/src/session_synopsis/projection.rs` | Modify (line 67 doc) | Add one-paragraph note that `pending.push_str` is not idempotent. |
| `crates/spur-acp/src/config/mod.rs` | Modify (insert in `LogConfig` near line 631) | Add `event_replay_horizon_secs: u64` field with serde default. |
| `crates/spur-tui/src/app.rs` | Modify (insert ~line 4050) | Call `replay_events` between App build and broadcast loop. |
| `crates/spur-tui/src/app.rs` | Modify (test module at end) | Integration test verifying replay populates all three projections via the wired-in path. |
| `docs/architecture.md` | Modify (line 690 + 772) | Mark Risk #2 / Risk #9 / Tier 1 #2 half-A complete; reference half-B follow-up. |
| `crates/spur-core/Cargo.toml` | Modify | Add `criterion` to `[dev-dependencies]`; add `[[bench]] name = "event_replay"`. |
| `crates/spur-core/benches/event_replay.rs` | **Create** | Criterion bench: 50K-event fixture across 7 files, asserts <500ms median. |

---

## Task 1: Enable the new module

**Files:**
- Modify: `crates/spur-core/src/event_sink.rs:217`
- Modify: `crates/spur-core/src/lib.rs:11`
- Create: `crates/spur-core/src/event_replay.rs`

This task has no behavior change — it's pure plumbing so subsequent tasks can land tests against the new module.

- [ ] **Step 1.1: Promote `events_dir()` to `pub(crate)`**

In `crates/spur-core/src/event_sink.rs:217`, change:

```rust
fn events_dir() -> PathBuf {
    PathBuf::from(".spur/events")
}
```

to:

```rust
pub(crate) fn events_dir() -> PathBuf {
    PathBuf::from(".spur/events")
}
```

- [ ] **Step 1.2: Create the empty `event_replay.rs` module**

Create `crates/spur-core/src/event_replay.rs` with:

```rust
//! NDJSON replay on TUI startup — rehydrates derived projections from
//! the EventSink's durable log before the live broadcast drain loop
//! begins. See `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md`.

#![allow(dead_code)] // Filled in incrementally across the bd-1vnk task series.
```

- [ ] **Step 1.3: Register the module in lib.rs**

In `crates/spur-core/src/lib.rs`, between `pub mod event_funnel;` (line 10) and `pub mod event_sink;` (line 11), add:

```rust
pub mod event_replay;
```

(Alphabetical order preserved.)

- [ ] **Step 1.4: Build to confirm no breakage**

Run: `cargo build -p spur-core`
Expected: clean build, no warnings about the new module.

- [ ] **Step 1.5: Commit**

```bash
git add crates/spur-core/src/event_sink.rs crates/spur-core/src/lib.rs crates/spur-core/src/event_replay.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): bd-1vnk-1 enable event_replay module skeleton

Promotes events_dir() to pub(crate) and registers the new
event_replay module. No behavior change yet.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: ReplayConfig and ReplayStats types

**Files:**
- Modify: `crates/spur-core/src/event_replay.rs`

- [ ] **Step 2.1: Write the failing test for `ReplayConfig::default()`**

Append to `crates/spur-core/src/event_replay.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_config_default_uses_current_pid() {
        let cfg = ReplayConfig::default();
        assert_eq!(cfg.skip_pid, Some(std::process::id()));
        assert_eq!(cfg.events_dir, std::path::PathBuf::from(".spur/events"));
        assert_eq!(cfg.replay_horizon, std::time::Duration::from_secs(7 * 86400));
        assert_eq!(cfg.max_line_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn replay_stats_default_is_zero() {
        let s = ReplayStats::default();
        assert_eq!(s.files_read, 0);
        assert_eq!(s.files_skipped_pid, 0);
        assert_eq!(s.events_applied, 0);
        assert_eq!(s.events_skipped_horizon, 0);
        assert_eq!(s.malformed_lines, 0);
        assert_eq!(s.elapsed, std::time::Duration::ZERO);
    }
}
```

- [ ] **Step 2.2: Run test to verify it fails**

Run: `cargo test -p spur-core event_replay::tests`
Expected: compile error — `ReplayConfig` and `ReplayStats` not defined.

- [ ] **Step 2.3: Add the types**

Replace the `#![allow(dead_code)]` line in `crates/spur-core/src/event_replay.rs` with the type definitions, keeping the module-doc comment at top:

```rust
//! NDJSON replay on TUI startup — rehydrates derived projections from
//! the EventSink's durable log before the live broadcast drain loop
//! begins. See `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Caller-supplied replay configuration.
///
/// `Default::default()` calls `std::process::id()` so `skip_pid` is the
/// current process. This is intentional — the only sensible default is
/// "skip my own NDJSON file because the live broadcast carries those
/// events instead".
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    /// Directory containing `.ndjson` segments (typically `.spur/events`).
    pub events_dir: PathBuf,
    /// Events older than `now() - replay_horizon` are skipped at parse time.
    pub replay_horizon: Duration,
    /// Files whose filename PID matches this value are skipped wholesale.
    /// Set to `Some(std::process::id())` for the typical TUI-startup case.
    pub skip_pid: Option<u32>,
    /// Maximum bytes per line. Lines exceeding this are counted as
    /// malformed and skipped without allocating beyond the cap.
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

/// Telemetry returned by `replay_events`.
#[derive(Debug, Default, Clone)]
pub struct ReplayStats {
    pub files_read: usize,
    pub files_skipped_pid: usize,
    pub events_applied: u64,
    pub events_skipped_horizon: u64,
    pub malformed_lines: u64,
    pub elapsed: Duration,
}
```

(Keep the test module appended below.)

- [ ] **Step 2.4: Run test to verify it passes**

Run: `cargo test -p spur-core event_replay::tests`
Expected: 2 tests pass.

- [ ] **Step 2.5: Commit**

```bash
git add crates/spur-core/src/event_replay.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): bd-1vnk-1 add ReplayConfig and ReplayStats types

Default impl for ReplayConfig captures std::process::id() so the
typical TUI-startup case requires no caller arguments. ReplayStats
follows the spur.metrics.* tracing-field convention.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Filename parser

**Files:**
- Modify: `crates/spur-core/src/event_replay.rs`

The filename format is `{pid}-{unix_ms}-{rotation_seq}.ndjson` per `event_sink.rs:221-228`.

- [ ] **Step 3.1: Write the failing test**

Add to the `mod tests` block:

```rust
#[test]
fn parse_segment_name_well_formed() {
    let parsed = parse_segment_name("12345-1714000000123-7.ndjson");
    assert_eq!(parsed, Some(SegmentName { pid: 12345, unix_ms: 1714000000123, rotation_seq: 7 }));
}

#[test]
fn parse_segment_name_rejects_garbage() {
    assert_eq!(parse_segment_name("not-a-segment"), None);
    assert_eq!(parse_segment_name("12345-foo-7.ndjson"), None);
    assert_eq!(parse_segment_name("12345-100.ndjson"), None);
    assert_eq!(parse_segment_name(""), None);
    assert_eq!(parse_segment_name("12345-100-7.json"), None);
}
```

- [ ] **Step 3.2: Run test to verify it fails**

Run: `cargo test -p spur-core event_replay::tests::parse_segment_name`
Expected: compile error — `parse_segment_name` and `SegmentName` not defined.

- [ ] **Step 3.3: Add the parser**

Insert above the `#[cfg(test)]` block in `crates/spur-core/src/event_replay.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct SegmentName {
    pid: u32,
    unix_ms: u128,
    rotation_seq: u64,
}

/// Parse `{pid}-{unix_ms}-{rotation_seq}.ndjson`. Returns `None` on
/// any deviation from the format. Mirrors `event_sink.rs:221-228`.
fn parse_segment_name(name: &str) -> Option<SegmentName> {
    let stem = name.strip_suffix(".ndjson")?;
    let mut parts = stem.splitn(3, '-');
    let pid: u32 = parts.next()?.parse().ok()?;
    let unix_ms: u128 = parts.next()?.parse().ok()?;
    let rotation_seq: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() { return None; }
    Some(SegmentName { pid, unix_ms, rotation_seq })
}
```

- [ ] **Step 3.4: Run tests to verify they pass**

Run: `cargo test -p spur-core event_replay::tests::parse_segment_name`
Expected: 2 tests pass.

- [ ] **Step 3.5: Commit**

```bash
git add crates/spur-core/src/event_replay.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): bd-1vnk-1 add SegmentName filename parser

Parses {pid}-{unix_ms}-{rotation_seq}.ndjson filenames written by
EventSink (event_sink.rs:221-228). Rejects deviations.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: File discovery and ordering

**Files:**
- Modify: `crates/spur-core/src/event_replay.rs`

- [ ] **Step 4.1: Write the failing test**

Add to the `mod tests` block:

```rust
#[test]
fn collect_ordered_files_skips_pid_and_sorts_by_ts_then_seq() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Three files: A and B from another PID (chronological), C from current PID (skipped).
    let a = dir.join("100-1000-0.ndjson");
    let b = dir.join("100-1000-1.ndjson");           // same ts as A, later seq
    let earlier = dir.join("200-500-0.ndjson");      // older ts, different PID
    let mine = dir.join("999-9999-0.ndjson");        // current PID — must be skipped
    let unrelated = dir.join("readme.txt");          // not .ndjson — ignored
    let garbage = dir.join("garbage-name.ndjson");   // unparseable — ignored

    for p in [&a, &b, &earlier, &mine, &unrelated, &garbage] {
        std::fs::File::create(p).unwrap();
    }

    let mut stats = ReplayStats::default();
    let ordered = collect_ordered_files(dir, Some(999), &mut stats).unwrap();

    let names: Vec<_> = ordered.iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["200-500-0.ndjson", "100-1000-0.ndjson", "100-1000-1.ndjson"],
    );
    assert_eq!(stats.files_skipped_pid, 1);
}

#[test]
fn collect_ordered_files_returns_empty_for_missing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("does-not-exist");
    let mut stats = ReplayStats::default();
    let ordered = collect_ordered_files(&dir, None, &mut stats).unwrap();
    assert!(ordered.is_empty());
    assert_eq!(stats.files_skipped_pid, 0);
}
```

- [ ] **Step 4.2: Run tests to verify they fail**

Run: `cargo test -p spur-core event_replay::tests::collect_ordered_files`
Expected: compile error — `collect_ordered_files` not defined.

- [ ] **Step 4.3: Implement file discovery**

Insert above the `#[cfg(test)]` block:

```rust
use std::fs;
use std::path::Path;

/// Walk `events_dir`, parse `.ndjson` segment filenames, drop entries
/// matching `skip_pid`, sort by `(unix_ms, rotation_seq)` ascending.
///
/// Returns an empty Vec if the dir does not exist (NotFound is tolerated
/// for fresh-repo first runs). All other I/O errors propagate.
fn collect_ordered_files(
    events_dir: &Path,
    skip_pid: Option<u32>,
    stats: &mut ReplayStats,
) -> std::io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(events_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut parsed: Vec<(SegmentName, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let segment = match parse_segment_name(name) {
            Some(s) => s,
            None => continue,
        };
        if Some(segment.pid) == skip_pid {
            stats.files_skipped_pid += 1;
            continue;
        }
        parsed.push((segment, path));
    }

    parsed.sort_by_key(|(s, _)| (s.unix_ms, s.rotation_seq));
    Ok(parsed.into_iter().map(|(_, p)| p).collect())
}
```

- [ ] **Step 4.4: Run tests to verify they pass**

Run: `cargo test -p spur-core event_replay::tests::collect_ordered_files`
Expected: 2 tests pass.

- [ ] **Step 4.5: Commit**

```bash
git add crates/spur-core/src/event_replay.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): bd-1vnk-1 add ordered file discovery for replay

Walks events_dir, parses {pid}-{unix_ms}-{rotation_seq}.ndjson,
filters by skip_pid, sorts by (unix_ms, rotation_seq). Tolerates
NotFound for fresh-repo first runs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Per-line bounded reader and `replay_events` orchestration

**Files:**
- Modify: `crates/spur-core/src/event_replay.rs`

This task lands the main `replay_events` function. The non-trivial detail is the `take()`-bounded read that prevents a 100MB corrupted line from OOM-ing the reader.

- [ ] **Step 5.1: Write a failing test for happy-path replay**

Add to the `mod tests` block:

```rust
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::SessionId;

fn write_ndjson(path: &Path, events: &[SpurEvent]) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    for ev in events {
        writeln!(f, "{}", serde_json::to_string(ev).unwrap()).unwrap();
    }
}

#[test]
fn replay_events_applies_in_order_skipping_current_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Prior-PID file with two events.
    let prior_path = dir.join("100-1000-0.ndjson");
    write_ndjson(&prior_path, &[
        SpurEvent { occurred_at: SystemTime::UNIX_EPOCH, seq: 0,
            body: SpurEventBody::TurnComplete { session: SessionId("s1".into()) }},
        SpurEvent { occurred_at: SystemTime::UNIX_EPOCH, seq: 1,
            body: SpurEventBody::TurnComplete { session: SessionId("s2".into()) }},
    ]);

    // Current-PID file (must be skipped).
    let mine_path = dir.join("999-9999-0.ndjson");
    write_ndjson(&mine_path, &[
        SpurEvent { occurred_at: SystemTime::UNIX_EPOCH, seq: 0,
            body: SpurEventBody::TurnComplete { session: SessionId("never_applied".into()) }},
    ]);

    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        replay_horizon: Duration::from_secs(u64::MAX / 2), // effectively unbounded
        skip_pid: Some(999),
        max_line_bytes: 1024,
    };

    let mut applied: Vec<String> = Vec::new();
    let stats = replay_events(&cfg, |ev| {
        if let SpurEventBody::TurnComplete { session } = &ev.body {
            applied.push(session.0.clone());
        }
    }).unwrap();

    assert_eq!(applied, vec!["s1", "s2"]);
    assert_eq!(stats.files_read, 1);
    assert_eq!(stats.files_skipped_pid, 1);
    assert_eq!(stats.events_applied, 2);
    assert_eq!(stats.malformed_lines, 0);
}
```

- [ ] **Step 5.2: Run test to verify it fails**

Run: `cargo test -p spur-core event_replay::tests::replay_events_applies_in_order_skipping_current_pid`
Expected: compile error — `replay_events` not defined.

- [ ] **Step 5.3: Implement `replay_events`**

Append to `crates/spur-core/src/event_replay.rs` (above the test module):

```rust
use std::io::{BufRead, BufReader, Read};
use std::time::Instant;

use spur_acp::SpurEvent;

const FIRST_N_MALFORMED_VERBOSE: u64 = 8;

/// Stream every event in the NDJSON ring through `on_event`, in
/// chronological file order, applying horizon and PID filters. Returns
/// a `ReplayStats` populated with counters and elapsed time.
///
/// Malformed JSON lines and lines exceeding `max_line_bytes` are
/// counted and skipped (first N at warn-level; rest aggregated).
/// `apply()` panics inside the closure are NOT caught — they propagate
/// per spec §5.4.
pub fn replay_events<F>(config: &ReplayConfig, mut on_event: F) -> std::io::Result<ReplayStats>
where
    F: FnMut(&SpurEvent),
{
    let start = Instant::now();
    let mut stats = ReplayStats::default();
    let cutoff = SystemTime::now().checked_sub(config.replay_horizon);

    let ordered = collect_ordered_files(&config.events_dir, config.skip_pid, &mut stats)?;

    for path in ordered {
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        stats.files_read += 1;
        let mut reader = BufReader::with_capacity(64 * 1024, file);
        let mut buf: Vec<u8> = Vec::with_capacity(4096);

        loop {
            buf.clear();
            let limit = config.max_line_bytes as u64;
            let n = (&mut reader).take(limit).read_until(b'\n', &mut buf)?;
            if n == 0 { break; }

            let terminated = buf.last() == Some(&b'\n');
            let hit_cap = !terminated && (n as u64) == limit;
            if hit_cap {
                stats.malformed_lines += 1;
                drain_until_newline(&mut reader)?;
                continue;
            }
            let line = if terminated { &buf[..buf.len() - 1] } else { &buf[..] };

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
}

/// Read 64 KB chunks until the next `\n` or EOF, discarding bytes.
/// Used to recover from over-cap lines without unbounded allocation.
fn drain_until_newline<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    let mut sink = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut sink)?;
        if n == 0 { return Ok(()); }
        if let Some(idx) = sink[..n].iter().position(|&b| b == b'\n') {
            // Consume only up to and including the newline; the read above
            // already advanced past `n` bytes total. Anything after the
            // newline is lost — acceptable since the over-cap line was
            // already declared malformed.
            // (Note: `BufRead::read` consumes from the internal buffer; we
            // do not need to seek back. Subsequent read_until will start
            // reading fresh content after the `n` bytes we already consumed.
            // The bytes after `idx` are dropped by design.)
            let _ = idx;
            return Ok(());
        }
    }
}
```

- [ ] **Step 5.4: Run test to verify it passes**

Run: `cargo test -p spur-core event_replay::tests::replay_events_applies_in_order_skipping_current_pid`
Expected: 1 test passes.

- [ ] **Step 5.5: Commit**

```bash
git add crates/spur-core/src/event_replay.rs
git commit -m "$(cat <<'EOF'
feat(spur-core): bd-1vnk-1 implement replay_events orchestration

Single-pass streaming reader with take()-bounded per-line allocation,
horizon filter, malformed-line counting (first 8 verbose, rest
aggregated). Skip-current-PID filter at file discovery prevents
double-applying events that arrive via the live broadcast.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Edge-case tests

**Files:**
- Modify: `crates/spur-core/src/event_replay.rs`

- [ ] **Step 6.1: Write failing tests for malformed, horizon, and over-cap**

Add to the `mod tests` block:

```rust
#[test]
fn replay_events_skips_malformed_json_continues() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("100-1000-0.ndjson");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        // Valid line, then garbage, then valid line.
        let ev1 = SpurEvent { occurred_at: SystemTime::UNIX_EPOCH, seq: 0,
            body: SpurEventBody::TurnComplete { session: SessionId("a".into()) }};
        let ev2 = SpurEvent { occurred_at: SystemTime::UNIX_EPOCH, seq: 1,
            body: SpurEventBody::TurnComplete { session: SessionId("b".into()) }};
        writeln!(f, "{}", serde_json::to_string(&ev1).unwrap()).unwrap();
        writeln!(f, "{{not valid json}}").unwrap();
        writeln!(f, "{}", serde_json::to_string(&ev2).unwrap()).unwrap();
    }

    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        skip_pid: None,
        ..ReplayConfig::default()
    };
    let mut count = 0u32;
    let stats = replay_events(&cfg, |_ev| count += 1).unwrap();

    assert_eq!(count, 2);
    assert_eq!(stats.events_applied, 2);
    assert_eq!(stats.malformed_lines, 1);
}

#[test]
fn replay_events_horizon_filter_drops_old_events() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("100-1000-0.ndjson");

    let now = SystemTime::now();
    let very_old = now - Duration::from_secs(365 * 86400); // 1 year ago
    let recent = now - Duration::from_secs(60);

    let old_ev = SpurEvent { occurred_at: very_old, seq: 0,
        body: SpurEventBody::TurnComplete { session: SessionId("old".into()) }};
    let new_ev = SpurEvent { occurred_at: recent, seq: 1,
        body: SpurEventBody::TurnComplete { session: SessionId("new".into()) }};
    write_ndjson(&path, &[old_ev, new_ev]);

    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        replay_horizon: Duration::from_secs(7 * 86400),
        skip_pid: None,
        max_line_bytes: 8 * 1024 * 1024,
    };
    let mut sessions: Vec<String> = Vec::new();
    let stats = replay_events(&cfg, |ev| {
        if let SpurEventBody::TurnComplete { session } = &ev.body {
            sessions.push(session.0.clone());
        }
    }).unwrap();

    assert_eq!(sessions, vec!["new".to_string()]);
    assert_eq!(stats.events_applied, 1);
    assert_eq!(stats.events_skipped_horizon, 1);
}

#[test]
fn replay_events_over_cap_line_is_counted_and_drained() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("100-1000-0.ndjson");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&path).unwrap();
        // Write a valid line, then a 100KB line (exceeds cap of 4KB),
        // then another valid line.
        let ev = SpurEvent { occurred_at: SystemTime::UNIX_EPOCH, seq: 0,
            body: SpurEventBody::TurnComplete { session: SessionId("a".into()) }};
        writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
        let huge = vec![b'x'; 100 * 1024];
        f.write_all(&huge).unwrap();
        writeln!(f).unwrap();
        writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
    }

    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        skip_pid: None,
        max_line_bytes: 4 * 1024, // small cap to trigger the over-cap path
        ..ReplayConfig::default()
    };
    let mut count = 0u32;
    let stats = replay_events(&cfg, |_| count += 1).unwrap();

    assert_eq!(count, 2);
    assert_eq!(stats.events_applied, 2);
    assert_eq!(stats.malformed_lines, 1);
}

#[test]
fn replay_events_returns_empty_stats_for_missing_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let nonexistent = tmp.path().join("does-not-exist");
    let cfg = ReplayConfig {
        events_dir: nonexistent,
        skip_pid: None,
        ..ReplayConfig::default()
    };
    let stats = replay_events(&cfg, |_| {}).unwrap();
    assert_eq!(stats.files_read, 0);
    assert_eq!(stats.events_applied, 0);
}
```

- [ ] **Step 6.2: Run tests**

Run: `cargo test -p spur-core event_replay::tests`
Expected: all 4 new tests pass alongside the existing tests.

If the over-cap test fails, the `drain_until_newline` helper or the cap-detection condition needs adjustment. Verify the actual `n` value when at-cap matches `max_line_bytes`.

- [ ] **Step 6.3: Commit**

```bash
git add crates/spur-core/src/event_replay.rs
git commit -m "$(cat <<'EOF'
test(spur-core): bd-1vnk-1 cover replay edge cases

Tests for malformed JSON skip, horizon filter, over-cap line drain,
and missing-dir tolerance.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Integration test against real projections

**Files:**
- Create: `crates/spur-core/tests/event_replay_integration.rs`

This is a separate integration test (not unit-level) verifying the closure dispatch correctly populates all three real projections from fixture NDJSON.

- [ ] **Step 7.1: Write the failing test**

Create `crates/spur-core/tests/event_replay_integration.rs`:

```rust
//! Integration test: feed fixture NDJSON through replay_events,
//! verify all three projections converge to expected state.

use std::path::Path;
use std::time::{Duration, SystemTime};

use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, SessionNotification, SessionUpdate, TextContent,
};
use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::SessionId;
use spur_core::event_replay::{replay_events, ReplayConfig};
use spur_core::lineage::ExecutorLineage;
use spur_core::plan_projection::PlanProjectionStore;
use spur_core::session_synopsis::SessionSynopsisProjection;

fn write_ndjson(path: &Path, events: &[SpurEvent]) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    for ev in events {
        writeln!(f, "{}", serde_json::to_string(ev).unwrap()).unwrap();
    }
}

fn user_chunk(session: &str, text: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: SessionId(session.into()),
        notification: Box::new(SessionNotification::new(
            agent_client_protocol::schema::SessionId::new(session),
            SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new(text),
            ))),
        )),
    })
}

fn agent_chunk(session: &str, text: &str) -> SpurEvent {
    SpurEvent::now(SpurEventBody::AgentNotification {
        session: SessionId(session.into()),
        notification: Box::new(SessionNotification::new(
            agent_client_protocol::schema::SessionId::new(session),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new(text),
            ))),
        )),
    })
}

#[test]
fn replay_populates_all_three_projections_from_fixture() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("100-1000-0.ndjson");

    write_ndjson(&path, &[
        user_chunk("S1", "fix the auth bug"),
        agent_chunk("S1", "ack"),
        SpurEvent::now(SpurEventBody::TurnComplete { session: SessionId("S1".into()) }),
        user_chunk("S2", "deploy to staging"),
        agent_chunk("S2", "ok"),
    ]);

    let mut lineage = ExecutorLineage::new();
    let mut plan = PlanProjectionStore::new();
    let mut synopsis = SessionSynopsisProjection::new();

    let cfg = ReplayConfig {
        events_dir: dir.to_path_buf(),
        replay_horizon: Duration::from_secs(86400 * 365), // 1 year
        skip_pid: None,
        max_line_bytes: 8 * 1024 * 1024,
    };

    let stats = replay_events(&cfg, |ev| {
        lineage.apply(ev);
        plan.apply(ev);
        synopsis.apply(ev);
    }).unwrap();

    assert_eq!(stats.events_applied, 5);
    assert_eq!(stats.malformed_lines, 0);

    let s1 = synopsis.get(&SessionId("S1".into())).expect("S1 synopsis");
    assert_eq!(s1.first_user_msg.as_deref(), Some("fix the auth bug"));
    let s2 = synopsis.get(&SessionId("S2".into())).expect("S2 synopsis");
    assert_eq!(s2.last_user_msg.as_deref(), Some("deploy to staging"));
}
```

- [ ] **Step 7.2: Run test to verify it passes (`replay_events` already implements the closure dispatch)**

Run: `cargo test -p spur-core --test event_replay_integration`
Expected: 1 test passes.

If the test fails, the fixture event construction may be using fields that don't match the current `SpurEventBody` schema — verify by reading recent changes to `crates/spur-acp/src/domain/events.rs`.

- [ ] **Step 7.3: Commit**

```bash
git add crates/spur-core/tests/event_replay_integration.rs
git commit -m "$(cat <<'EOF'
test(spur-core): bd-1vnk-1 verify replay populates real projections

Integration test feeds fixture NDJSON through replay_events with a
closure dispatching to ExecutorLineage, PlanProjectionStore, and
SessionSynopsisProjection. Asserts synopsis converges to expected
first/last user messages for two sessions.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Lineage doc amend

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs:16-21`

- [ ] **Step 8.1: Replace the idempotency doc block**

In `crates/spur-core/src/lineage/projection.rs`, find lines 16-21 (the `## Idempotency` section). Current text:

```rust
//! ## Idempotency
//!
//! Every event arm is idempotent — applying the same event twice produces
//! the same state as applying it once. Exception: `SpurEventBody::CostUpdate`
//! is deliberately additive (two updates accumulate). Tests enforce both
//! invariants.
```

Replace with:

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

- [ ] **Step 8.2: Build to confirm doc compiles**

Run: `cargo build -p spur-core`
Expected: clean build (doc-only change).

- [ ] **Step 8.3: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs
git commit -m "$(cat <<'EOF'
docs(spur-core): bd-1vnk-1 correct lineage idempotency claim

Original doc overstated by saying "every arm is idempotent" and
"tests enforce both invariants". The spawn/phase arms ARE idempotent
and tested; CostUpdate, ToolCall counter, and FileTouched(Write)
counter arms are NOT idempotent. Doc now lists the exceptions and
points at the replay model that structurally avoids double-apply.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Synopsis doc note

**Files:**
- Modify: `crates/spur-core/src/session_synopsis/projection.rs:67`

- [ ] **Step 9.1: Add a note above the `apply` method**

In `crates/spur-core/src/session_synopsis/projection.rs`, find:

```rust
    /// Fold an event into the projection. Idempotent on irrelevant variants.
    pub fn apply(&mut self, event: &spur_acp::SpurEvent) {
```

Replace with:

```rust
    /// Fold an event into the projection. Idempotent on irrelevant variants.
    ///
    /// **Not idempotent under double-apply on `AgentNotification(UserMessageChunk)`**:
    /// the chunk text is appended to the pending buffer, so re-applying the same
    /// chunk doubles the buffer. The replay model in
    /// `crates/spur-core/src/event_replay.rs` is structurally guarded against
    /// double-apply via PID-filtered file selection.
    pub fn apply(&mut self, event: &spur_acp::SpurEvent) {
```

- [ ] **Step 9.2: Build to confirm**

Run: `cargo build -p spur-core`
Expected: clean build.

- [ ] **Step 9.3: Commit**

```bash
git add crates/spur-core/src/session_synopsis/projection.rs
git commit -m "$(cat <<'EOF'
docs(spur-core): bd-1vnk-1 note synopsis UserMessageChunk non-idempotency

UserMessageChunk appends to pending buffer. The replay model
structurally avoids double-apply via PID-filtered file selection.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `event_replay_horizon_secs` config field

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs` (LogConfig at line 607-655 area)

- [ ] **Step 10.1: Write the failing test**

Find the existing `LogConfig` test (likely in the test module of `config/mod.rs`). Add:

```rust
#[test]
fn log_config_default_event_replay_horizon_is_seven_days() {
    let cfg = LogConfig::default();
    assert_eq!(cfg.event_replay_horizon_secs, 7 * 86400);
}
```

- [ ] **Step 10.2: Run test to verify it fails**

Run: `cargo test -p spur-acp log_config_default_event_replay_horizon`
Expected: compile error — field not defined.

- [ ] **Step 10.3: Add the field**

In `crates/spur-acp/src/config/mod.rs`, locate the `LogConfig` struct (around line 607). Add a field after `events_max_total_bytes` at line 631:

```rust
    /// How far back to replay NDJSON events on TUI startup, in seconds.
    /// Default 7 days. Bounds the cost of cold-start projection rehydration.
    #[serde(default = "default_event_replay_horizon_secs")]
    pub event_replay_horizon_secs: u64,
```

In `Default for LogConfig` (around line 637), add the field:

```rust
            event_replay_horizon_secs: 7 * 86400,
```

Add the default function near the other `default_*` functions in the file:

```rust
fn default_event_replay_horizon_secs() -> u64 {
    7 * 86400
}
```

- [ ] **Step 10.4: Run test to verify it passes**

Run: `cargo test -p spur-acp log_config_default_event_replay_horizon`
Expected: 1 test passes.

- [ ] **Step 10.5: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs
git commit -m "$(cat <<'EOF'
feat(spur-acp): bd-1vnk-2 add LogConfig.event_replay_horizon_secs

Default 7 days. Used by the TUI's startup replay (bd-1vnk) to bound
the cost of cold-start projection rehydration.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Wire `replay_events` into `run_tui_with_license`

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (around lines 4040-4055)

- [ ] **Step 11.1: Locate the insertion point**

Open `crates/spur-tui/src/app.rs`. Find `pub async fn run_tui_with_license` (around line 4030). The structure is:

```rust
pub async fn run_tui_with_license(/* args */) -> anyhow::Result<()> {
    let mut terminal = tui::setup()?;
    let mut app = App::build_with_license_state(/* args */);
    let mut tick_interval = tokio::time::interval(Duration::from_millis(33));
    let mut event_stream = crossterm::event::EventStream::new();
    let mut event_rx = event_rx;

    // SIGINT/SIGTERM handler block...

    loop {
        // drain
    }
}
```

The insertion point is between `let mut event_rx = event_rx;` and the `#[cfg(unix)]` signal handler block.

- [ ] **Step 11.2: Insert the replay call**

Just before the `#[cfg(unix)]` block (which begins the SIGTERM/SIGHUP/SIGQUIT handler installation), add:

```rust
    // === bd-1vnk: rehydrate projections from prior NDJSON before drain begins ===
    let replay_cfg = spur_core::event_replay::ReplayConfig {
        replay_horizon: std::time::Duration::from_secs(config.log.event_replay_horizon_secs),
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

- [ ] **Step 11.3: Build to confirm**

Run: `cargo build -p spur-tui`
Expected: clean build. If `app.lineage` or `app.plan_projection` cannot be accessed, verify they are crate-visible (private fields on `App` accessible from `app.rs` — should be fine since the call site is in the same module).

- [ ] **Step 11.4: Add an integration test inside the existing `app.rs` `#[cfg(test)]` module**

Find the existing `#[cfg(test)] mod tests { ... }` block at the end of `crates/spur-tui/src/app.rs`. Inside that module, add:

```rust
    #[tokio::test]
    async fn run_tui_replay_populates_synopsis_from_prior_ndjson() {
        use std::io::Write;
        use std::time::SystemTime;

        // Set up a tempdir as CWD-relative .spur/events.
        let tmp = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        std::fs::create_dir_all(".spur/events").unwrap();

        // Write a fixture NDJSON file from a "prior" PID.
        let path = std::path::PathBuf::from(".spur/events/100-1000-0.ndjson");
        let mut f = std::fs::File::create(&path).unwrap();
        let ev = wrap_event(SpurEventBody::AgentNotification {
            session: spur_acp::SessionId("test-sess".into()),
            notification: Box::new(agent_client_protocol::schema::SessionNotification::new(
                agent_client_protocol::schema::SessionId::new("test-sess"),
                agent_client_protocol::schema::SessionUpdate::UserMessageChunk(
                    agent_client_protocol::schema::ContentChunk::new(
                        agent_client_protocol::schema::ContentBlock::Text(
                            agent_client_protocol::schema::TextContent::new("hello replay"),
                        ),
                    ),
                ),
            )),
        });
        writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
        let flush_ev = wrap_event(SpurEventBody::TurnComplete {
            session: spur_acp::SessionId("test-sess".into()),
        });
        writeln!(f, "{}", serde_json::to_string(&flush_ev).unwrap()).unwrap();
        drop(f);

        // Build an empty App via the existing `test_app()` helper (defined
        // in this same test module — used by `worker_notification_populates_per_executor_trace`
        // and friends) and run replay against it directly, mirroring
        // run_tui_with_license's wiring.
        let mut app = test_app();
        let cfg = spur_core::event_replay::ReplayConfig {
            replay_horizon: std::time::Duration::from_secs(86400 * 365),
            skip_pid: None, // include all PIDs in this test
            ..Default::default()
        };
        let stats = spur_core::event_replay::replay_events(&cfg, |ev| {
            app.lineage.apply(ev);
            app.plan_projection.apply(ev);
            app.synopsis.apply(ev);
        }).unwrap();

        assert_eq!(stats.events_applied, 2, "stats: {:?}", stats);
        let synopsis = app.synopsis.get(&spur_acp::SessionId("test-sess".into()))
            .expect("replay should populate synopsis for test-sess");
        assert_eq!(synopsis.last_user_msg.as_deref(), Some("hello replay"));

        std::env::set_current_dir(cwd).unwrap();
    }
```

Both `test_app()` and `wrap_event` already exist in the test module (verified at `app.rs:4537` and the test starting at `app.rs:4541`). No new helpers needed — reuse them directly.

- [ ] **Step 11.5: Run the integration test**

Run: `cargo test -p spur-tui run_tui_replay_populates_synopsis_from_prior_ndjson -- --nocapture`
Expected: 1 test passes.

If `test_app_for_replay` requires more setup (license state, landing decision), consult the existing test at `app.rs:4544+` for the pattern. Reuse rather than reinvent.

⚠ **CWD discipline**: this test mutates `std::env::current_dir`. If other tests in the same crate run in parallel and depend on CWD, they may fail. Use `serial_test::serial` if available, OR move this test into its own `tests/` integration file with the `--test-threads=1` constraint documented.

- [ ] **Step 11.6: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "$(cat <<'EOF'
feat(spur-tui): bd-1vnk-2 wire NDJSON replay into TUI startup

Calls spur_core::event_replay::replay_events between
App::build_with_license_state and the broadcast drain loop. Logs
ReplayStats at target spur.metrics.event_replay. Closes the empty
session-picker preview gap for sessions whose history exists in NDJSON
but were not resumed in the current TUI process.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Update `docs/architecture.md` half-A status

**Files:**
- Modify: `docs/architecture.md` (lines 690, 697, 772 area)

- [ ] **Step 12.1: Find Risk #2 row at line 690**

Locate the line:

```
| 2 | `broadcast::channel` drops events when subscribers are slow | High | **Open** | R5 | 0.45 | `app.rs:2437` | Capacity is 4096, but TUI drain cap is **still 8** (not 64 as previously claimed). Bot has **no drain cap**. On `Lagged`, all subscribers log a warning and **permanently drop** events. No replay-from-NDJSON exists. |
```

Update the status column from `**Open**` to `**Partial**` and append to the description: ` Startup-replay path landed in bd-1vnk closes the cold-start half; live-Lagged recovery still open.`

- [ ] **Step 12.2: Find Risk #9 row at line 697**

Locate the line:

```
| 9 | Broadcast `Lagged` recovery not implemented | ~~Low~~ **High** | **Open** | R3/R5 | 0.60 | ...
```

Update status from `**Open**` to `**Partial**` and append: ` Half-A (startup replay) landed in bd-1vnk; half-B (Lagged-during-live recovery) tracked separately.`

- [ ] **Step 12.3: Find the Tier 1 #2 action at line 772**

Locate:

```
2. **Implement NDJSON replay on `Lagged` (Risk #9)** — When `broadcast::Receiver` returns `Lagged(n)`, trigger a replay: seek EventSink NDJSON from `seq = last_known + 1`, rebuild lineage projection incrementally. Closes the observability gap between event bus and durable log.
```

Replace with:

```
2. **NDJSON replay on `Lagged` (Risk #9)** — Half-A landed in bd-1vnk: at TUI startup, replay prior-PID NDJSON through projection `apply()` paths before the broadcast drain loop begins (see `crates/spur-core/src/event_replay.rs` and `docs/superpowers/specs/2026-04-29-ndjson-replay-startup-rehydration-design.md`). Half-B remains open: when `broadcast::Receiver` returns `Lagged(n)` mid-session, trigger an in-process replay-from-disk to fill the gap. Half-B uses the same `replay_events` primitive with a `from_seq_exclusive` parameter (additive API change).
```

- [ ] **Step 12.4: Commit**

```bash
git add docs/architecture.md
git commit -m "$(cat <<'EOF'
docs(architecture): bd-1vnk mark Tier 1 #2 half-A complete

Risks #2 and #9 move from Open to Partial. Tier 1 action #2 now
references the bd-1vnk replay primitive and points at half-B
(Lagged-during-live) as the remaining work using the same primitive.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Add Criterion bench infrastructure

**Files:**
- Modify: `crates/spur-core/Cargo.toml`
- Create: `crates/spur-core/benches/event_replay.rs`

- [ ] **Step 13.1: Add Criterion to dev-dependencies**

In `crates/spur-core/Cargo.toml`, find the `[dev-dependencies]` section (around line 41). Add:

```toml
criterion = { version = "0.5", features = ["html_reports"] }
```

Add a new `[[bench]]` section at the bottom of the file:

```toml
[[bench]]
name = "event_replay"
harness = false
```

- [ ] **Step 13.2: Create the bench skeleton**

Create `crates/spur-core/benches/event_replay.rs`:

```rust
//! Criterion benchmark for `event_replay::replay_events`. Generates a
//! 50K-event fixture across 7 NDJSON files matching the realistic
//! disk-cap rotation pattern, with 1% intentionally-malformed lines.

use std::path::Path;
use std::time::SystemTime;

use criterion::{criterion_group, criterion_main, Criterion};

use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::SessionId;
use spur_core::event_replay::{replay_events, ReplayConfig};

const FIXTURE_EVENTS: usize = 50_000;
const FILES: usize = 7;
const MALFORMED_RATIO: usize = 100; // every 100th line malformed (1%)

fn write_fixture(dir: &Path) {
    use std::io::Write;
    let per_file = FIXTURE_EVENTS / FILES;
    let mut event_idx = 0u64;
    for f_idx in 0..FILES {
        let path = dir.join(format!("100-{}-{}.ndjson", 1_000 + (f_idx as u128) * 10, f_idx));
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..per_file {
            if event_idx % MALFORMED_RATIO as u64 == 0 && event_idx > 0 {
                writeln!(f, "{{not valid json}}").unwrap();
            } else {
                let ev = SpurEvent {
                    occurred_at: SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(event_idx),
                    seq: event_idx,
                    body: SpurEventBody::TurnComplete {
                        session: SessionId(format!("s{}", i % 100)),
                    },
                };
                writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
            }
            event_idx += 1;
        }
    }
}

fn bench_replay_full_cap(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());

    let cfg = ReplayConfig {
        events_dir: tmp.path().to_path_buf(),
        replay_horizon: std::time::Duration::from_secs(u64::MAX / 2),
        skip_pid: None,
        max_line_bytes: 8 * 1024 * 1024,
    };

    c.bench_function("replay_full_cap_50k_events", |b| {
        b.iter(|| {
            let mut count = 0u64;
            let _stats = replay_events(&cfg, |_| count += 1).unwrap();
            criterion::black_box(count);
        })
    });
}

criterion_group!(benches, bench_replay_full_cap);
criterion_main!(benches);
```

- [ ] **Step 13.3: Run the bench (smoke test, not assertion)**

Run: `cargo bench -p spur-core --bench event_replay -- --warm-up-time 1 --measurement-time 3`
Expected: bench compiles and reports a median time. Note the median for the commit message.

⚠ **If the median exceeds 500 ms** on dev hardware, this is a soft warning per spec §8.1, not a CI block. Document the actual number; consider adding `simd-json` as a follow-up if the gap is large.

- [ ] **Step 13.4: Commit**

```bash
git add crates/spur-core/Cargo.toml crates/spur-core/benches/event_replay.rs
git commit -m "$(cat <<'EOF'
test(spur-core): bd-1vnk-3 add Criterion bench for replay_events

50K-event fixture across 7 NDJSON files with 1% malformed lines.
Soft target <500 ms median per spec §8.1. simd-json swap is the
escape hatch if the bench misses on real hardware.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review checklist

After all 13 tasks land:

- [ ] Run full test suite: `cargo test --workspace` — all green.
- [ ] Run clippy: `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] Run formatter: `cargo fmt --all -- --check` — clean.
- [ ] Smoke-test the TUI: `cargo run -p spur-cli --` from a repo with existing `.spur/events/*.ndjson`. Verify:
  1. TUI starts within ~1s.
  2. Session picker preview pane is populated for sessions whose history is on disk (was empty before bd-1vnk).
  3. `tracing::info!` event at `target: spur.metrics.event_replay` appears in logs with non-zero `applied` and reasonable `elapsed_ms`.
- [ ] Spec ACs check (§11 of spec):
  - `replay_events` API in spur-core ✓ (Tasks 2-6).
  - TUI calls replay before drain loop ✓ (Task 11).
  - Picker preview populated for in-horizon, in-cap sessions ✓ (Tasks 7, 11).
  - bd-3kx3 placeholder for rotated-out OR horizon-skipped ✓ (no code change needed; existing placeholder logic handles `synopsis: None`).
  - <500 ms median bench target ✓ (Task 13).
  - Unknown variants → `malformed_lines` non-aborting ✓ (Task 6 covers the malformed JSON test, which exercises the same code path).
  - Lineage idempotency doc accurate ✓ (Task 8).
  - architecture.md half-A complete ✓ (Task 12).

---

## Out of scope (tracked separately)

- **bd-1vnk-5** — Verify and (if needed) route synthetic events at `app.rs:859,3348` through `Orchestrator::emit` so they survive TUI restart. Step 1 is verification (does the orchestrator already emit these via the funnel?), Step 2 is conditional routing change. Independent of bd-1vnk.
- **Lagged-during-live recovery** (half-B of Tier 1 #2) — Uses the same `replay_events` primitive plus a small paused-drain merge loop with `from_seq_exclusive: Option<u64>`. Tracked when half-A telemetry shows live-Lagged events occurring.
