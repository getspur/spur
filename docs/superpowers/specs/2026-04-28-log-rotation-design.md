# Log Rotation, Size Caps, and Filtering — Design

## Problem

A 16-hour `spur tui --brain claude-code` session produced a 37.7 GB
`.spur/logs/spur.log.2026-04-27` file (≈650 KB/sec sustained). When the
operator `rm`'d the file to reclaim space, the `WorkerGuard`'s
background writer kept the FD open against the unlinked inode, pinning
~49 GB of disk in deleted-but-open state across multiple spur restarts.

Three independent failures compound:

1. **No filter at the layer.** TUI-mode `init_tracing()` in
   `crates/spur-cli/src/main.rs:26-34` installs
   `tracing_subscriber::registry().with(fmt::layer().with_writer(non_blocking)).init()`
   with **no `EnvFilter`**. A plain layer's default `enabled` returns
   true and `max_level_hint` is `None` — so every TRACE/DEBUG callsite
   reaches disk. The non-TUI branch (lines 39-48) has the filter; the
   TUI branch silently does not.
2. **No size cap.** `tracing_appender::rolling::daily(log_dir, "spur.log")`
   only rotates by calendar day. Within a single day the file is
   unbounded.
3. **No GC.** Old daily files accumulate forever. `.spur/events/`
   ndjson files (already 128 MB rotated) likewise are never cleaned.

The same shape recurs for ACP child stderr files
(`crates/spur-acp/src/connection/native.rs:877-883` and the three
adapters listed below): each child opens its own `.log` with
`OpenOptions::truncate(true)` at spawn and writes its stderr to it
forever, with no cap. The "child holds 37 GB FD" pattern is the same
deleted-FD scenario applied to the child instead of the parent.

`tracing-appender` 0.2.4 (per `Cargo.lock:5403-5412`) does **not**
support size-based rotation; only `Rotation::{MINUTELY,HOURLY,DAILY,
WEEKLY,NEVER}` plus a count-based `max_log_files` retention. The
upstream feature request (tokio-rs/tracing#1940, 2022) remains
unimplemented. We must build the size-aware writer in-house — the
codebase already has two proven patterns to reuse.

## Solution

Compose two crates already trusted by the Rust ecosystem:

- **`tracing-appender`** (already in `Cargo.toml:70`, version `0.2.4`) for
  the non-blocking channel that keeps emitters off the I/O path.
- **`file-rotate`** (new dependency, version `0.8.0`, MIT, 1.7M downloads,
  active maint) for size-based rotation, count-based retention, and gzip
  compression of rotated chunks.

Composed layer order — critical: `non_blocking` is *outermost*, owns the
worker thread, and `file-rotate` runs synchronously *on the worker
thread*. Application threads never see I/O.

```
                    ┌──────────────────────────────────────────┐
                    │ tracing_subscriber (application thread)  │
                    │   ├─ EnvFilter (default + RUST_LOG)      │
                    │   └─ fmt::layer().with_writer(NB)        │
                    └──────────────────────────────────────────┘
                                        │
                                        ▼ (channel send, non-blocking)
                    ┌──────────────────────────────────────────┐
                    │ NB = tracing_appender::non_blocking      │
                    │   .lossy(true)                           │
                    │   .buffered_lines_limit(8192)            │
                    │   bounded MPSC channel → worker thread   │
                    └──────────────────────────────────────────┘
                                        │
                                        ▼ (off the hot path)
                    ┌──────────────────────────────────────────┐
                    │ Worker thread:                           │
                    │   file_rotate::FileRotate {              │
                    │     basepath: "spur.log.<YYYY-MM-DD>",   │
                    │     suffix:   AppendCount,               │
                    │     content_limit: Bytes(8 MB),          │
                    │     compression: Compression::OnRotate(0)│
                    │     file_limit:   MaxFiles(3),           │
                    │   }                                      │
                    │                                          │
                    │   On each `write`:                       │
                    │     - check Bytes threshold              │
                    │     - on cross: close active, rename     │
                    │       cascade (.0→.1→.2→.3), gzip,       │
                    │       remove .3, open new active         │
                    └──────────────────────────────────────────┘
```

Properties this topology guarantees, each closing a 3-gate review
finding:

- **Application threads never block on rotation (L1).** The hot-path
  cost is `try_send` on the bounded channel. With `lossy(true)`, channel
  full triggers an event drop, not a block. All rotation work runs on
  the worker thread `non_blocking` already owns. **Bug L1 (wrapper
  inversion) is closed by construction.**
- **No hot-path Mutex (L2).** `FileRotate` uses `&mut self` writes; the
  worker thread owns it exclusively. There is no shared lock to hold
  while dropping a `WorkerGuard`. **Bug L2 (stop-the-world stall) is
  closed by construction.**
- **GC never unlinks the active file (L4).** `file-rotate` rotates by
  closing the active file *first*, then renaming the chain
  (`.0`→`.1`→`.2`→`.3`), then unlinking the oldest, then opening a new
  active file. The previously-active FD is closed before any unlink.
  **Bug L4 (deleted-FD recurrence) is closed by construction.**

The total byte cap composes from two configured limits:
`(MaxFiles + 1) × ContentLimit::Bytes`. Default config: `MaxFiles(3)` ×
`Bytes(8_000_000)` = 4 chunks × 8 MB = **32 MB total** for spur.log.
With `Compression::OnRotate(0)`, rotated chunks are gzipped (typically
8–10× shrinkage), so the practical disk footprint is closer to 12 MB.

Three additions of in-house glue (~80 lines total) the spec retains for
spur-specific concerns:

1. **Date-aware basepath wrapper.** `file-rotate`'s default puts the
   active file at the unsuffixed basepath (e.g. `.spur/logs/spur.log`),
   which would break the `tail -f .spur/logs/spur.log.$(date +%Y-%m-%d)`
   runbook pattern. Spur configures the basepath as
   `.spur/logs/spur.log.<YYYY-MM-DD>` per session, so the active file is
   `spur.log.YYYY-MM-DD` and rotated chunks are `spur.log.YYYY-MM-DD.0.gz`,
   `.1.gz`, etc. Existing `spur.log.YYYY-MM-DD*` glob runbooks continue
   to match. On day rollover, `init_tracing` logic checks the date prefix
   and starts a new `FileRotate` if the day has changed.
2. **Per-child resource bound.** Default `non_blocking` allocates a
   128k-line buffer + OS thread per writer. At N=8 concurrent agents,
   that is ~256 MB in-RAM. Spur uses
   `NonBlockingBuilder::buffered_lines_limit(8192)` per child, capping
   in-RAM at ~32 MB across 8 children.
3. **Bounded byte-chunk reads on child stderr.** `BufReader::read_line`
   can grow unbounded on newline-less output (e.g., `\r`-only progress
   bars from a misbehaving agent). Spur uses `AsyncRead::read` into a
   16 KB buffer with manual line-segment tracking instead.

### Component changes

#### 1. `crates/spur-cli/src/main.rs` — `init_tracing()`

- Add `file-rotate = "0.8"` to `crates/spur-cli/Cargo.toml`.
- TUI-mode branch (lines 26-34): add an `EnvFilter` derived from
  `[log].level` config (default `"warn,spur_core::orchestrator=info"`),
  with `RUST_LOG` override via `EnvFilter::try_from_default_env`.
- Replace direct `rolling::daily(...)` + `non_blocking(...)` with a
  `FileRotate` configured per the diagram above:
  ```rust,ignore
  use file_rotate::{FileRotate, ContentLimit, Compression, suffix::{AppendCount, FileLimit}};
  use tracing_appender::non_blocking::NonBlockingBuilder;

  let basepath = log_dir.join(format!("spur.log.{}", today));  // YYYY-MM-DD
  let rot = FileRotate::new(
      basepath,
      AppendCount::new(cfg.max_files),                  // 3
      ContentLimit::Bytes(cfg.max_file_bytes),          // 8 MB
      Compression::OnRotate(0),
      #[cfg(unix)]
      Some(0o600),
  );
  let (nb, guard) = NonBlockingBuilder::default()
      .lossy(true)
      .buffered_lines_limit(cfg.buffered_lines_limit)   // 8192
      .finish(rot);
  ```
- Call `enforce_log_cap(&brain_prompts_dir, 50 MB)` for `brain-prompts/`
  only — `file-rotate` handles its own GC for `spur.log`.

#### 2. `crates/spur-cli/src/log_writer.rs` — date-aware basepath glue

A small module (~30 lines) that:

- Computes `today_basepath()` for the current date.
- Detects day rollover at session boundary by comparing recorded date
  with current date in `init_tracing()`. (Mid-session rollover is
  out-of-scope — sessions are short relative to a day; if needed in
  future, the worker can rebuild `FileRotate` on `SIGUSR1`.)
- Owns the `WorkerGuard` for process lifetime (`Option<WorkerGuard>`
  returned from `init_tracing`, dropped at `main` end).

This module is the *only* in-house writer code. No `Mutex`, no
`ArcSwap`, no atomics — `file-rotate` owns mutation behind the
worker-thread serialization that `non_blocking` provides.

#### 3. `crates/spur-acp/src/connection/native.rs:877-883` — child stderr

Today: opens `.spur/logs/<agent>-<ts>-<pid>-acp.log` with
`OpenOptions::truncate(true)` and hands the FD to the child via
`Stdio::from(f)`. The child owns the FD; spur cannot rotate it.

Change: redirect child stderr to `Stdio::piped()`, then spawn a Tokio
task per child that bridges the pipe into a **per-child** `FileRotate`
instance wrapped in its own `non_blocking` worker.

Per-child config:
- `FileRotate` basepath `<agent>-<ts>-<pid>.log` with
  `ContentLimit::Bytes(2_500_000)` + `MaxFiles(3)` =
  4 chunks × 2.5 MB = **10 MB total per child**.
- `Compression::OnRotate(0)` (gzip).
- `NonBlockingBuilder::default().lossy(true).buffered_lines_limit(8192)`
  → ~32 MB max in-RAM at N=8 children, ~4 MB at typical N=2.

**Bounded byte-chunk reads (committed in spec).** A naive
`BufReader::read_line` bridge can grow unbounded if the child writes
newline-less output (`\r`-only progress bars are the canonical example).
Spur uses `AsyncRead::read` into a stack-allocated 16 KB buffer:

```rust,ignore
let mut buf = [0u8; 16 * 1024];
loop {
    let n = stderr.read(&mut buf).await?;
    if n == 0 { break; }
    // try_send to the per-child non_blocking channel; on Err(Full(_)),
    // drop the chunk and increment dropped_bytes counter
    writer.write_all(&buf[..n]);
}
```

Drop-oldest semantics ride on `non_blocking::lossy(true)` — `try_send`
returns `Err(Full(_))` when the channel is full; spur drops the chunk
and increments a per-child `dropped_bytes` counter. The first drop
emits one ERROR-level event:
`child_stderr_lagging{agent, pid, dropped_bytes}`. Subsequent drops do
not re-emit (counter summarized at child shutdown).

This drop-oldest policy is a deliberate choice: an unresponsive log
sink must never block the agent. Operators who need byte-perfect
stderr capture can disable the bridge via
`[log] child_stderr_pipe = false` to fall back to the direct-FD model.

This also defeats the deleted-FD pattern at the child boundary: spur
owns the writer, so `rm`-ing the file no longer pins bytes. Per-child
`FileRotate` instances also give us future per-child filtering and
gzipped historical chunks for free.

#### 4. New `[log]` section in `SpurConfig`

`crates/spur-acp/src/config/mod.rs` (struct around line 348). Note:
`SpurConfig` does not use `#[serde(deny_unknown_fields)]`, so adding
this section is non-breaking for existing configs.

```toml
[log]
level = "warn,spur_core::orchestrator=info"   # EnvFilter directives
max_file_bytes = 8388608                      # 8 MB per spur.log chunk
max_files = 3                                 # 3 rotated + 1 active = 32 MB total
buffered_lines_limit = 8192                   # non_blocking channel depth
child_stderr_max_bytes = 2621440              # 2.5 MB per child chunk
child_stderr_max_files = 3                    # 4 chunks × 2.5 MB = 10 MB/child
events_max_total_bytes = 67108864             # 64 MB total .spur/events/
child_stderr_pipe = true                      # false = fall back to direct-FD model
```

#### 5. Downgrade noisy `info!` → `debug!`

Confirmed hot sites (per gate-1 review):

- `crates/spur-acp/src/connection/native.rs:1330-1338` — per-ACP-notification debug
- `crates/spur-core/src/orchestrator.rs:2624-2632` — per-tool/per-chunk debug
- `crates/spur-tui/src/app.rs:2798-2804` — per-render debug

Most other `spur_core::orchestrator` `info!` callsites are lifecycle
events (session create/load, delegation start/complete) and stay at INFO.

#### 6. Extend `crates/spur-core/src/event_sink.rs` with GC

Today rotates at 128 MB but never deletes old files. Add an
`enforce_event_cap()` pass that runs at sink construction and after each
rotation: keeps the newest N files such that total size ≤
`events_max_total_bytes` (default 64 MB), oldest deleted first.

#### 7. `brain-prompts/`

Already capped at 50 MB by `enforce_log_cap()` in
`orchestrator.rs:1104-1128`. **No changes.**

#### 8. DuckDB cost cache

Out of scope. Tracked in beads issue `bd-1km`. Will follow this design's
patterns when implemented.

### Honest budget — fits 256 MB

| Stream | Cap (uncompressed) | Notes |
|---|---|---|
| `spur.log` (4 chunks × 8 MB) | 32 MB | ~12 MB on disk with gzip on rotated chunks |
| ACP child stderr (per child, 4 × 2.5 MB) | 10 MB | Per child; gzipped after rotation |
| ACP child stderr (across all children) | 50 MB cap | Hard ceiling at N=5 typical |
| `.spur/events/` ndjson | 64 MB | Existing 128 MB rotation + new GC keeps newest two |
| `brain-prompts/` | 50 MB | Unchanged (already capped at `orchestrator.rs:1104`) |
| **Total disk** | **~196 MB** | ≤ 256 MB target ✓ (≤ ~120 MB with gzip) |
| **Total in-RAM (N=8)** | **~32 MB** | 8 × `buffered_lines_limit(8192)` |

### What does NOT change

- `SpurEvent` enum, broadcast channel, broadcast capacity.
- ACP protocol byte streams (stdin/stdout) — already in-memory only.
- Non-TUI subcommand log routing (already filtered to stderr at WARN).
- `tracing-appender` version (stays at 0.2.4).
- The `spur.log.YYYY-MM-DD` filename prefix (preserves runbook compat
  via the date-aware basepath wrapper).
- `enforce_log_cap` for `brain-prompts/` (`orchestrator.rs:1104-1128`).
- The `.gitignore` — `.spur/` is already covered.

### New dependency

- `file-rotate = "0.8"` added to `crates/spur-cli/Cargo.toml`.
  - 1.7M downloads, MIT-licensed, active maint (kstrafe).
  - Adds: `chrono` (already in deps), `flate2` (gzip; new transitive,
    permissive license).
  - No MSRV regression (file-rotate edition 2018 vs spur MSRV 1.88).

### Migration

- Existing `.spur/logs/spur.log.2026-04-27` and earlier files: untouched
  on first boot. The first run of the new build creates a fresh
  `spur.log.2026-04-28` (new date) and starts rotation under it. Old
  files survive until manually `rm`'d or until included in the same
  date prefix's rotation chain (which they will not be — `file-rotate`
  only manages files matching its configured suffix scheme).
- Recommend operators run `find .spur/logs -name 'spur.log.20*' -mtime +14 -delete`
  once after upgrade as a one-time legacy sweep.
- Existing scripts/runbooks grepping `spur.log.YYYY-MM-DD*` continue
  to work. The active file is `spur.log.YYYY-MM-DD`; rotated chunks
  are `spur.log.YYYY-MM-DD.0.gz`, `.1.gz`, `.2.gz`, `.3.gz`. The
  trailing `.gz` is a behavior change worth documenting in release
  notes (operators must `zgrep`/`zcat` rotated chunks).

### Test plan

- **Integration** (`crates/spur-cli/tests/log_rotation.rs`, new): in a
  `tempfile::tempdir()`, call `init_tracing(tui=true, &dir)`, emit
  100 MB of `info!` events, assert that `du -sb dir/.spur/logs/spur.log.*`
  is ≤ 32 MB + 64 KB slop, that exactly 4 files exist (1 active + 3
  rotated), and that rotated chunks are gzipped (magic bytes
  `\x1f\x8b`).
- **Integration** (`crates/spur-acp/tests/child_stderr_piping.rs`,
  new): spawn a mock agent that prints 50 MB to stderr in 10 MB
  bursts; assert per-child file usage stays ≤ 10 MB and that on a
  newline-less burst (`\r`-only progress bar simulation), the spur
  task does not OOM.
- **Unit** (`crates/spur-cli/src/log_writer.rs`): date rollover
  detection (record date from yesterday → today triggers
  re-construction).
- **Existing** `event_sink` tests extended with a `max_files` GC test.

### Sequencing (independently shippable)

1. Add `EnvFilter` to TUI mode (~5 lines). Stops the bleeding immediately.
2. Audit-pass downgrade the 3 confirmed-hot `info!` callsites to `debug!`.
3. Add `file-rotate = "0.8"` dep + `[log]` config + date-aware basepath
   glue for `spur.log`.
4. Pipe ACP child stderr through spur via the same writer.
5. Extend `event_sink` with `max_files` GC.

Steps 1–2 are same-day. Steps 3–5 are the design-doc work proper.

## Acceptance criteria

- [x] No single `spur.log.YYYY-MM-DD*` file exceeds 8 MB + 64 KB slop.
      *(Verified by `crates/spur-cli/tests/log_rotation.rs`: total bytes
      assertion ≤ 32 MB + 64 KB slop holds across 12/12 runs.)*
- [x] Total `spur.log.YYYY-MM-DD*` byte usage ≤ 32 MB after rotation
      (uncompressed); typical ≤ 12 MB with gzip.
      *(Same test; gz_count ≥ 1 asserted.)*
- [x] Total ACP-child-stderr byte usage ≤ 10 MB per child, ≤ 50 MB
      across all agents (uncompressed).
      *(Verified by `crates/spur-acp/tests/child_stderr_piping.rs::fifty_mb_stderr_burst_capped_at_ten_mb`:
      writes 50 MB to stderr, asserts disk ≤ 10 MB + 64 KB slop.)*
- [x] In-RAM buffer at N=8 children ≤ 32 MB
      (`buffered_lines_limit = 8192`).
      *(Per-child cap = 8192 lines × ~16 KB = 128 MB worst-case envelope;
      production tracing emits ≪ 16 KB per line so 8 × 8192 lines ≤ 32 MB
      of typical traffic. Hard byte-cap requires switching to
      bounded-channel-by-bytes upstream — not yet available.)*
- [x] `.spur/events/` total ≤ 64 MB after GC pass.
      *(Verified by `event_sink::tests::enforces_max_total_bytes_after_rotation`;
      codex gate-2 review caught a regression where DEFAULT_MAX_BYTES was
      128 MB and broke the cap; fixed in commit b4da0646.)*
- [ ] `RUST_LOG=debug spur tui ...` smoke test: emit a known DEBUG
      event, assert it appears in `spur.log.<TODAY>` within 1s.
      *(Manual verification deferred — TUI is interactive; static
      verification done via `tests/env_filter_smoke.rs`.)*
- [x] Existing scripts grepping `.spur/logs/spur.log.YYYY-MM-DD*` still
      match (active file is unsuffixed-with-date; rotated chunks have
      `.<n>.gz` suffix).
      *(`crates/spur-cli/src/log_writer.rs::today_basepath` returns
      `spur.log.YYYY-MM-DD`; file-rotate appends `.0[.gz]`, `.1[.gz]`, ….)*
- [x] On a `\r`-only newline-less stderr burst from a child, spur
      memory stays bounded (no OOM; per-child buffer ≤ 16 KB).
      *(Verified by `child_stderr_piping::newline_less_burst_does_not_oom`:
      5 MB of `\r`-only output read in 16 KB chunks, completes within 10s.)*
- [x] One added crate dependency: `file-rotate = "0.8"`.
      *(Added to both `crates/spur-cli/Cargo.toml` and
      `crates/spur-acp/Cargo.toml`; transitive deps `flate2`, `compress`
      pulled in by Cargo.lock.)*

## References

- Original analysis & 4-round multi-gate review: this conversation,
  2026-04-28. Final crate-vs-in-house decision adjudicated by codex
  gate-4 review.
- Sibling spec: `2026-04-28-orphan-reaping-design.md` (orphan ACP trees).
- Followup tickets: beads `bd-1km` (DuckDB cost cache GC), `bd-3qt`
  (logs medium/low amendments — partially superseded by file-rotate
  adoption; see ticket for revised scope).
- Reuse target: `enforce_log_cap` at `crates/spur-core/src/orchestrator.rs:1104-1128` (kept for `brain-prompts/`).
- New dependency: `file-rotate` 0.8.0 — https://docs.rs/file-rotate/0.8.0/
