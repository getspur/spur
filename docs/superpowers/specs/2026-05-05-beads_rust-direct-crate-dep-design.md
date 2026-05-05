# beads_rust 0.2.1 as Direct Crate Dependency — Design Spec

| | |
|---|---|
| Status | Draft |
| Date | 2026-05-05 |
| Author | brain (synthesized from 3 rounds of independent review by codex + gemini) |
| Motivating bug | bd-1h3w — `.beads/issues.jsonl` torn-write corruption (3 occurrences) |
| Companion specs (deferred) | bd-1h3w reader-resilience; bd-1h3w periodic integrity check |

---

## TL;DR

Replace SPUR's current `Command::new("br")` shellouts with direct linkage of the `beads_rust` 0.2.1 crate. The new in-process adapter exposes five disciplined primitives (`read`, `write`, `batch`, `with_txn`, `rpw_optimistic`), backed by a small reader connection pool and a single guarded writer connection. Cross-process safety comes from `beads_rust`'s built-in `.beads/.write.lock` advisory flock; intra-process Connection-handle safety comes from the writer Mutex; DB-level atomicity comes from explicit transactions. Migration is staged across five phases with shadow validation before cutover.

---

## Background

### Why now

Bug `bd-1h3w` records three torn-write JSONL corruptions during high-throughput plan execution. The corrupting writer is the older `br` v0.1.14 binary that SPUR currently shells out to (`Command::new("br")` at `crates/spur-pm/src/beads.rs:280` and 20 bypass sites in `crates/spur-mcp/src/server.rs` and `crates/spur-mcp/src/plan/reconciler.rs`).

`beads_rust` 0.2.1 (https://github.com/Dicklesworthstone/beads_rust) already fixes the writer:

- Atomic temp-file + `fsync` + rename for JSONL exports (`sync::write_jsonl_lines_atomically`)
- Cross-process `.beads/.write.lock` advisory file lock (`sync::blocking_write_lock_with_timeout`)
- Bounded lock acquisition via `open_with_timeout`
- Post-write count verification before atomic rename
- SQLite WAL mode

Linking `beads_rust` directly as a Rust crate eliminates the subprocess layer entirely. This:

1. Removes the version drift surface (no risk of stale `br` binary on PATH)
2. Eliminates fork+exec overhead per call
3. Replaces stderr-JSON parsing with typed `BeadsError` enum
4. Eliminates the dual-process race surface that still exists when SPUR shells out
5. Enables in-process discipline (transaction boundaries, snapshot validation)

### Why not just upgrade the `br` binary?

We'd still own the shellout layer and the 20 bypass call sites. Direct crate linkage centralizes the contract once and lets us enforce discipline (transaction boundaries, R-P-W validation) inside the type system rather than across a process boundary.

---

## Goals

1. **Eliminate `Command::new("br")` from SPUR runtime code** — production paths call `beads_rust` directly.
2. **Multi-instance correctness** — multiple SPUR processes (worker, brain, MCP server, TUI) sharing one `.beads/` directory must not corrupt data, lose updates silently, or stall indefinitely.
3. **Operational observability** — lock contention, write duration, conflict rate, WAL size, auto-flush activity, and panic counters are all measurable.
4. **Safe migration** — staged rollout with shadow validation; no big-bang cutover.
5. **Discipline encoded in the API** — primitives make the right thing easy and the wrong thing hard.

## Non-goals

- bd-1h3w reader-side resilience (parser skip+log+continue) — separate companion spec
- bd-1h3w periodic integrity check + auto-archive — separate companion spec
- bd-1h3w manual repair documentation — separate companion spec
- Optimistic concurrency via row-level CAS (`WHERE content_hash = $expected`) — `beads_rust` 0.2.1's public mutation API does not natively support this; we use snapshot re-validation under the file lock instead
- Any change to the on-disk JSONL format or SQLite schema
- Any change to `beads_rust` itself (no fork required)

---

## Design

### Adapter shape

```
struct BeadsCrateAdapter {
    reader_pool: ReaderPool,          // small fixed pool of SqliteStorage (e.g. 4)
    writer:      Mutex<SqliteStorage>, // in-process Connection handle guard
    flock_path:  PathBuf,              // .beads/.write.lock
    metrics:     ContentionMetrics,
    config:      AdapterConfig,
}
```

**Why this shape, point by point:**

- **Reader pool, not a single reader.** `rusqlite::Connection` is `!Sync`. WAL mode permits concurrent readers via *multiple* connections, not via one connection called concurrently. A small fixed pool gives the WAL concurrency that `beads_rust` was designed for, while bounding the file-handle footprint.
- **`Mutex<SqliteStorage>` for the writer.** This is *not* redundant with `.write.lock`. The flock protects files across processes; the Mutex protects the in-memory Connection handle inside one process. Two concurrent `spawn_blocking` closures in the same SPUR process would otherwise race on the same writer connection.
- **`flock_path` separate from the writer.** The cross-process flock is acquired *outside* the writer Mutex closure scope to make the ordering explicit and to allow lock-free operations (init detection, sweeps) that don't need the writer connection.
- **`ContentionMetrics` first-class.** Cross-process contention manifests as silent hangs without instrumentation; metrics are part of the adapter contract, not an afterthought.

### The five primitives

```rust
impl BeadsCrateAdapter {
    /// Lock-free snapshot read. Uses one reader connection from the pool.
    /// SQLite WAL provides snapshot isolation.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where F: FnOnce(&SqliteStorage) -> Result<T> + Send + 'static,
          T: Send + 'static;

    /// Single write under cross-process flock + in-process writer guard +
    /// implicit DB transaction.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where F: FnOnce(&mut SqliteStorage) -> Result<T> + Send + 'static,
          T: Send + 'static;

    /// Batch of mutations under one flock acquisition + one DB transaction.
    /// Avoids 50-acquire thrash for bulk operations.
    pub async fn batch<T, F>(&self, f: F) -> Result<T>
    where F: FnOnce(&mut SqliteStorage) -> Result<T> + Send + 'static,
          T: Send + 'static;

    /// Explicit transaction scope — alias for `write` that documents intent
    /// when callers need DB-level atomicity for a multi-statement invariant.
    pub async fn with_txn<T, F>(&self, f: F) -> Result<T>
    where F: FnOnce(&mut Transaction) -> Result<T> + Send + 'static,
          T: Send + 'static;

    /// Read-process-write with optimistic snapshot re-validation at the
    /// commit boundary. Avoids holding the file lock during the (potentially
    /// slow) compute phase. Returns Conflict if state moved between read and
    /// write — caller decides retry policy.
    pub async fn rpw_optimistic<S, T, FR, FC, FV, FW>(
        &self,
        read_phase:     FR,   // unlocked initial snapshot
        compute_phase:  FC,   // pure compute, no DB calls
        validate_phase: FV,   // re-read inside lock for matching
        write_phase:    FW,   // commit
    ) -> Result<T>
    where /* … bounds … */;
}
```

#### `rpw_optimistic` — the heart of multi-instance correctness

```
Read  (unlocked, fast)  →  Compute  (unlocked, may be slow)  →
   Lock  →  Re-read  →  Verify snapshot still matches  →  Write  →  Unlock
```

If the verify step fails, the closure returns `Err(Conflict)` and the caller decides whether to retry, escalate, or surface to the brain. This pattern:

- Avoids holding `.write.lock` during slow processing (which would starve other SPUR instances)
- Avoids the lost-update problem (write is gated on snapshot still matching)
- Avoids forking `beads_rust` to add native CAS to the mutation API
- Surfaces conflicts as typed errors rather than silent overwrites

### Discipline rules (encoded in the API + enforced in code review)

1. **No DB call across `.await`.** Every `read` / `write` / `batch` / `with_txn` / `rpw_optimistic` invocation runs to completion inside one `spawn_blocking` closure. Holding a Connection across an `.await` would either be unsound (Connection is `!Sync`) or block the runtime.

2. **Multi-statement writes MUST use `with_txn` or be wrapped by the implicit transaction inside `write` / `batch`.** The file lock is *serialization*, not *atomicity*. A panic mid-closure must roll back, not leave partial state.

3. **No business logic inside lock-holding closures.** Compute outside the lock; mutate inside. `rpw_optimistic` enforces this structurally; `write` and `batch` rely on the closure author to keep work brief.

4. **Reader connections finalize prepared statements between operations.** A pinned reader snapshot prevents WAL truncation and grows the WAL unboundedly under steady writes. The reader pool wrapper enforces statement-finalization on connection return.

5. **No assumption of single-writer in SPUR-side caches.** Reconciler tick state, lineage snapshots, "last seen" issue views, ready-queue caches — every such cache must either invalidate via SQLite's `data_version` pragma or live entirely within one `spawn_blocking` closure. An audit pass during Phase 1 enumerates and flags violators.

---

## Multi-instance correctness

### Concerns this design addresses (with mechanism)

| Concern | Mechanism |
|---|---|
| Torn writes | `beads_rust` 0.2.1 atomic tmp+fsync+rename (existing) |
| Cross-process write serialization | `.write.lock` advisory flock (existing) |
| In-process Connection handle race | `Mutex<SqliteStorage>` on writer |
| Reader concurrency | Small reader pool over WAL |
| Open/migration race | First-open guarded under `.write.lock` (see below) |
| Lost updates | `rpw_optimistic` snapshot re-validation at commit |
| Orphan tmp on crash | Init-time sweep: no flock holder + exact name match + conservative TTL |
| Lock starvation | Bounded backoff with ceiling + jitter; typed `Busy` error |
| Auto-flush duplicate work | Idempotent under-lock no-op (no leader election) |
| Stale in-process cache | `data_version` pragma poll OR no cache across `.await` |
| Long-lived read txns | Statement finalization on reader return; checkpoint policy |
| Network filesystem unsafety | Init-time non-local-fs detection; refuse or warn loudly |
| Legacy `br` binary on PATH | Init-time refusal: refuse to boot if `br < 0.2.1` detected |
| Watcher spam on `-wal`/`-shm` | Documented contract: downstream watchers ignore, react to JSONL rename only |
| Stale JSONL after crash | Init-time discrepancy detection + force sync |
| Panic mid-write | RAII flock release + DB transaction rollback (no extra mutex needed) |

### Open / migration safety

This is the highest-risk gap surfaced in review. Schema initialization, PRAGMA setup (including WAL mode transition), and version migrations are *writes* — but they happen before any in-process serialization is established.

**Mechanism:** `BeadsCrateAdapter::open()` acquires `.beads/.write.lock` *before* opening the writer connection and running any migration logic. Other processes attempting to open concurrently will block on the same flock. Once migration is complete and verified, the writer Mutex is initialized and the lock is released.

```rust
async fn open(beads_dir: &Path) -> Result<Self> {
    detect_non_local_fs(beads_dir)?;        // refuse NFS/SMB or warn
    detect_legacy_br_binary_on_path()?;     // refuse if br < 0.2.1
    let _migration_guard = blocking_write_lock_with_timeout(beads_dir, ...)?;
    sweep_stale_tmps(beads_dir)?;
    detect_stale_jsonl_vs_db_and_force_sync(beads_dir)?;
    let writer = SqliteStorage::open(beads_dir)?;  // schema + migrations run here
    let reader_pool = ReaderPool::new(beads_dir, /*size*/ 4)?;
    drop(_migration_guard);
    Ok(BeadsCrateAdapter { writer: Mutex::new(writer), reader_pool, ... })
}
```

### Snapshot re-validation pattern (alternative to row-level CAS)

The proposer initially considered using `beads_rust::model::Issue::content_hash` as an optimistic-concurrency token (`UPDATE … WHERE content_hash = $expected`). Review concluded this is unsound to rely on: `beads_rust` 0.2.1's public mutation API does not expose a CAS form, and forcing it via raw SQL would couple SPUR to internal schema details. Snapshot re-validation gives the same guarantee through the public API.

Snapshot semantics for `rpw_optimistic`:

- The "snapshot" is whatever the read phase returns. Typically: the issue's `content_hash` field, or the issue's `(updated_at, version_counter)` tuple, or a hash of the relevant subset.
- The "match" predicate is whatever the validate phase asserts. Common: `current.content_hash == snapshot.content_hash`.
- On mismatch, return `Err(Conflict { id, expected, actual })`. Callers (the brain, the reconciler) decide retry policy. Most callers will: log, refresh state, retry once, then escalate to a signal if still conflicting.

**Limitations of this approach:**

- The hash must cover everything the write logically depends on (labels, deps, comments, status). If `content_hash` only covers core issue fields, a label change between read and write won't be detected. Discipline: `rpw_optimistic` callers must explicitly choose a snapshot that covers their dependencies; the API does not pick for them.
- Under heavy contention, livelock is possible (every retry sees a fresh conflict). Mitigation: caller-side retry budget + exponential backoff; on exhaustion, escalate as a typed `ConflictExceeded` error.

### Backoff policy

Replaces the rejected fixed `3 × 50/200/800ms` policy:

```rust
struct BackoffPolicy {
    initial: Duration,    // e.g. 50ms
    max:     Duration,    // e.g. 2s
    factor:  f64,         // e.g. 1.5
    jitter:  f64,         // e.g. 0.25 (±25%)
    ceiling: Duration,    // e.g. 10s wall-clock total
}
```

On retryable lock errors (`Busy`, `Timeout`), the adapter retries until either success or the wall-clock ceiling. On ceiling, it returns `Err(Busy { holder_pid, op, elapsed })` with metadata for caller diagnosis.

### Auto-flush

`beads_rust::sync::auto_flush` exports SQLite → JSONL. Multiple SPUR processes calling it concurrently is *correct* (the file lock serializes), but wasteful (each does redundant work).

**Mechanism:** idempotent under-lock no-op. Every process may call `auto_flush()` periodically. Inside the file lock, it computes the dirty-set hash; if nothing changed since the last flush metadata, it returns immediately. No leader election, no designated process, no split-brain risk.

**Cadence:** every 30s in MCP server processes; every 5s during active plan execution (configurable). Other processes (TUI) skip auto-flush entirely — they're read-mostly.

### Stale tmp sweep

Naive `mtime > N seconds → delete` is unsafe (clock skew, NFS, slow legitimate writers). The correct check:

1. Acquire `.beads/.write.lock` (with short timeout — if held, skip; we'll try again next init)
2. List files matching the *exact* `beads_rust` temp naming scheme (specifically NOT `-wal`, `-shm`, or any other SQLite sidecar)
3. For each, check mtime > conservative threshold (e.g. 1 hour). Optionally check for open file handles via platform mechanisms (`lsof` on Unix when available); skip the check if the tool isn't present rather than failing init.
4. Delete

Runs at adapter `open()` time only. Periodic sweep deferred unless metrics show accumulation.

### Legacy `br` binary guard

Mandatory startup check:

```rust
fn detect_legacy_br_binary_on_path() -> Result<()> {
    let Ok(output) = Command::new("br").arg("--version").output() else {
        return Ok(()); // no br on PATH; fine
    };
    let version = parse_version(&output.stdout)?;
    if version < semver::Version::new(0, 2, 1) {
        return Err(BeadsError::LegacyBinaryDetected {
            found: version,
            min_required: "0.2.1".into(),
            remediation: "Run: cargo install --git https://github.com/Dicklesworthstone/beads_rust",
        });
    }
    Ok(())
}
```

If the user runs an old `br` binary in another terminal while SPUR is running, the old binary ignores `.write.lock` and recreates the corruption pattern from bd-1h3w. SPUR refuses to start in this environment. The check is one-shot at startup; we accept the rare case where a new `br` is installed mid-session.

### Filesystem detection

Advisory `flock` is not portable across NFS/SMB. On startup, `BeadsCrateAdapter::open()` calls `statvfs` (or platform equivalent) on the `.beads/` directory. If the filesystem is non-local, log a loud warning and proceed — or refuse, controlled by config. Default: warn loudly.

### Watcher contract

External file watchers (e.g., editor file-change watches, the existing `daemon.log`-emitting watcher) must:

1. Ignore `.beads/issues.jsonl-wal`, `.beads/issues.jsonl-shm`, and any `.beads/*.tmp.*` files
2. React only to atomic-rename events on `.beads/issues.jsonl` itself
3. Use stable-sampling (read once size+mtime+hash, sleep brief, read again, only act if all three match) to avoid acting on a file mid-replace

This is documented in the spec as a contract; implementing it in any specific watcher is out of scope.

### Stale-JSONL boot detection

If a SPUR process crashes between the SQLite write commit and the JSONL flush, the next process boots seeing a SQLite db newer than the JSONL.

**Mechanism:** `BeadsCrateAdapter::open()` (under the migration lock) compares the SQLite metadata (last-flushed sequence) to the JSONL metadata. If db > jsonl, force a flush before opening for normal use. This is the same `auto_flush` codepath, just unconditional.

---

## Observability

All metrics emitted via SPUR's existing tracing/metrics layer.

| Metric | Type | Purpose |
|---|---|---|
| `beads.lock.wait_ms` | histogram | Time spent waiting to acquire `.write.lock` |
| `beads.lock.hold_ms` | histogram | Time spent holding `.write.lock` (from acquire to release) |
| `beads.lock.busy_total` | counter | Times we hit retryable `Busy` |
| `beads.lock.ceiling_total` | counter | Times we exhausted backoff ceiling |
| `beads.write.duration_ms` | histogram | Time inside writer Mutex |
| `beads.write.error_total{kind}` | counter | Errors by kind (Busy, IO, Schema, Conflict, Panic) |
| `beads.read.duration_ms` | histogram | Time per `read` call |
| `beads.blocking_pool.saturation` | gauge | % of Tokio blocking pool in use by beads work |
| `beads.conflict.total` | counter | `rpw_optimistic` conflicts (lost-update prevented) |
| `beads.conflict.exhausted_total` | counter | Conflict retries that hit ceiling |
| `beads.checkpoint.outcome{kind}` | counter | WAL checkpoint result (success, busy, fail) |
| `beads.checkpoint.lag_pages` | gauge | WAL pages pending checkpoint |
| `beads.wal.size_bytes` | gauge | Current WAL file size |
| `beads.auto_flush.outcome{kind}` | counter | Auto-flush result (skipped, dirty, success, fail) |
| `beads.tmp_sweep.removed_total` | counter | Stale tmp files cleaned at init |
| `beads.legacy_br_detected` | gauge (0/1) | 1 if old `br` was found on PATH at startup (we'd have refused to start, but useful for debugging) |

### Tracing spans

Every primitive entry/exit emits a tracing span with the operation name, duration, and outcome. `rpw_optimistic` spans include the snapshot/current fingerprint pair on conflict for forensic analysis.

---

## Testing strategy

### Unit tests (per primitive)

- `read` returns `IssueNotFound` for missing IDs (not `Ok(None)`, matching existing trait)
- `write` rolls back on closure panic
- `batch` rolls back atomically on closure error mid-batch
- `with_txn` enforces transaction boundary (visible to readers only after commit)
- `rpw_optimistic` returns `Conflict` when state changes between read and validate
- Backoff respects ceiling and emits typed `Busy` on exhaustion
- `Mutex<SqliteStorage>` properly serializes concurrent in-process writers
- Reader pool gives different connections to concurrent readers

### Multi-process integration tests

These are the tests that matter most for correctness and didn't exist before:

- **Concurrent-write stress** — N processes (e.g. 8), each writing K times in a tight loop for T seconds. Assert: zero JSONL corruption, write count matches, no orphan tmps after.
- **Crash-during-write** — start a process mid-write, SIGKILL it, verify next process can recover (no flock leak, no torn JSONL, stale tmp swept on init).
- **Migration race** — two processes attempt first-open simultaneously against an empty `.beads/`. Assert: only one runs migrations, the other waits and joins.
- **Snapshot conflict** — two processes both `rpw_optimistic` the same issue. Assert: one wins, the other gets `Conflict`.
- **Lock contention escalation** — N processes contend on writes; assert backoff metrics fire, eventual `Busy` returns within ceiling.
- **Non-local filesystem detection** — mount tmpfs/NFS, assert the detection fires (or is bypassed by config).
- **Legacy `br` detection** — install `br` 0.1.x in PATH (or mock), assert SPUR refuses to start.

### Shadow validation in CI

During Phase 3 (see Migration plan), every test that exercises `IssueTracker` runs against both backends (CLI shellout and direct crate) and asserts byte-equivalent JSONL output. This is the conformance harness.

### Test ergonomics

- `BeadsCrateAdapter::open_memory(...)` for fast unit tests (no flock, no FS)
- `MultiProcessTestHarness` helper for the multi-process tests above
- Existing `br init` / `br create` test fixture flow is preserved during phases 1–4; replaced in phase 5 with direct-crate setup

---

## Migration plan

Five phases, each independently verifiable and rollback-able.

### Phase 1 — Trait centralization (no engine change)

**Action:** refactor the 20 bypass `Command::new("br")` sites in `crates/spur-mcp/src/server.rs` (lines 7483, 9039, 9205, 9270) and `crates/spur-mcp/src/plan/reconciler.rs` (lines 2206–2411 across 16 sites) to go through the existing `IssueTracker` trait in `spur-pm`. Add any missing trait methods.

**Verification:** all existing tests pass; no behavior change in plan execution.

**Rollback:** trivial git revert.

**Risk:** low.

### Phase 2 — Direct adapter behind feature flag

**Action:** implement `BeadsCrateAdapter` (the design above) alongside the existing `BeadsAdapter` (CLI shellout). Both implement `IssueTracker`. Selection via env var `SPUR_BEADS_BACKEND={cli,crate}` (default `cli`).

**Verification:** all existing tests pass with both backends. New unit tests for each primitive. New multi-process integration tests for the matrix above.

**Rollback:** flip env var back; ship continues on CLI path.

**Risk:** medium. New code path, but isolated behind the flag.

### Phase 3 — Shadow validation in CI

**Action:** dedicated CI job wraps every `IssueTracker` call to run **both** backends and assert byte-equivalent JSONL output. Cover the operation set:

- Issue create / update / delete
- Status transitions
- Labels (add/remove)
- Dependencies (add/remove)
- Comments (append, including sentinel comments)
- List / ready filtering
- Metadata
- JSONL line ordering and hash stability
- Error mapping (typed errors equivalent)
- Lock timeout behavior
- Idempotency (replay same op → same JSONL state)

**Verification:** CI green for 50 consecutive runs across the operation matrix (concrete threshold; can be tuned based on observed flake rate).

**Rollback:** disable the shadow CI job; production unaffected.

**Risk:** low. Parallel run, no commit to main path.

### Phase 4 — Cutover

**Action:** flip default to `SPUR_BEADS_BACKEND=crate`. Ship. Keep CLI path as escape hatch for one release cycle.

**Verification:** observability dashboards green for one week. No regressions in plan execution metrics.

**Rollback:** flip env var back to `cli`.

**Risk:** medium. Production traffic on new path.

### Phase 5 — Decommission

**Action:** delete `BeadsAdapter` shellout path. Delete `Command::new("br")` invocations from runtime code and most tests (test fixtures may still use `br` CLI for setup convenience, then progressively migrate). Drop CLI dependency from runtime `Cargo.toml`.

**Verification:** code search returns zero matches for `Command::new("br")` and `run_br` in non-test code.

**Rollback:** revert Phase 5 in git.

**Risk:** low. Only runs after Phase 4 has stabilized.

---

## Open questions / deferred items

- **bd-1h3w reader-resilience** (parser skip+log+continue, auto-archive of corrupt files) — explicitly deferred; separate companion spec
- **bd-1h3w periodic integrity check** — separate companion spec
- **bd-1h3w manual repair documentation** — separate companion spec
- **CAS support upstream** — if `beads_rust` ever adds a native `WHERE content_hash = $expected` mutation API, `rpw_optimistic` could narrow its lock scope further. Not blocking for this design.
- **WAL checkpoint cadence tuning** — start with per-write-batch and per-30s; tune based on metrics in production.
- **Reader pool size** — start at 4; tune based on `beads.read.duration_ms` and blocking-pool saturation.

---

## References

- bd-1h3w (motivating bug)
- `beads_rust` 0.2.1 — https://github.com/Dicklesworthstone/beads_rust
- Three rounds of independent architecture review by codex + gemini (delegation IDs: c9a8533f-8c29-456a-80e5-fd5a946e1d72, 68349679-9823-4260-b560-a0ca5d58d547, 76e0a3f9-ab14-4554-898e-69032add085d, 152997bc-5897-49fe-8ee9-f62c2dbb2d0d, b234e618-66ed-450a-9e17-9f71c7bc2ea4)
- Existing `IssueTracker` trait — `crates/spur-pm/src/adapter.rs`
- Existing CLI shellout adapter — `crates/spur-pm/src/beads.rs:280`
- Bypass call sites — `crates/spur-mcp/src/server.rs:{7483,9039,9205,9270}`, `crates/spur-mcp/src/plan/reconciler.rs:2206-2411`
- Plan-scoped lease (already implemented) — `crates/spur-mcp/src/plan/ownership.rs`
