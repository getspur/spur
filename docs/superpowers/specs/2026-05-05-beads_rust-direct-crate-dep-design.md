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

Replace SPUR's current `Command::new("br")` shellouts with direct linkage of the `beads_rust` 0.2.1 crate. The new in-process adapter exposes four disciplined primitives (`read`, `write`, `batch`, plus a snapshot CAS pair `read_snapshot` / `validate_and_commit`), backed by a small reader connection pool and a single guarded writer connection. Cross-process safety comes from `beads_rust`'s built-in `.beads/.write.lock` advisory flock; intra-process Connection-handle safety comes from the writer Mutex; per-mutation DB-level atomicity comes from `beads_rust`'s internal `with_write_transaction` (multi-statement atomicity is an explicit non-goal — see "Multi-statement atomicity" below). Migration is staged across five phases gated by a compile-time Cargo feature, with shadow validation before cutover and an explicit quiescence protocol around any backend swap.

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

## How it works mechanically

### What "direct crate dependency" means

SPUR communicates with `beads_rust` via **direct in-process Rust function calls**. There is no subprocess, no IPC, no JSON marshaling, no network. `beads_rust` is linked into the SPUR binary at compile time and lives in the same address space at runtime.

**Cargo dependency** (in `crates/spur-pm/Cargo.toml`):

```toml
[dependencies]
beads_rust = { git = "https://github.com/Dicklesworthstone/beads_rust", tag = "v0.2.1" }
```

**Usage** — the public API is consumed like any other Rust library:

```rust
use beads_rust::storage::sqlite::SqliteStorage;
use beads_rust::model::{Issue, IssueUpdate};
use beads_rust::sync;
use beads_rust::error::BeadsError;

let storage = SqliteStorage::open_with_timeout(&beads_dir, Some(5_000))?;

// Read — sync function call returning a typed Rust struct
let issue: Issue = storage.get_issue("bd-1h3w")?;

// Write — sync function call returning a typed Result
let update = IssueUpdate { status: Some("closed".into()), ..Default::default() };
storage.update_issue("bd-1h3w", &update)?;

// JSONL export — atomic tmp+fsync+rename happens inside this call
sync::export_to_jsonl_with_policy(&storage, &jsonl_path, /* config */, /* policy */)?;
```

That is the entire surface. SPUR holds a `SqliteStorage` value on its own heap. `SqliteStorage` internally owns a `rusqlite::Connection`, which owns OS file handles into `.beads/beads.db` and the SQLite WAL sidecars.

### Execution path of one write

```
┌─────────────────────────────────────────────────────────────────┐
│ SPUR async code (Tokio task)                                    │
│   adapter.write(|s| s.update_issue("bd-1h3w", &update))         │
└────────────┬────────────────────────────────────────────────────┘
             │ tokio::task::spawn_blocking { … }
             ▼
┌─────────────────────────────────────────────────────────────────┐
│ SPUR sync closure (on Tokio blocking-pool thread)               │
│   1. Acquire .beads/.write.lock via flock (cross-process)       │
│   2. Lock the writer Mutex (in-process Connection guard)        │
│   3. Begin DB transaction                                       │
│   4. Call beads_rust::SqliteStorage::update_issue()             │
│   5. Commit on Ok / rollback on Err                             │
│   6. Drop guards — releases Mutex then flock (RAII)             │
└────────────┬────────────────────────────────────────────────────┘
             │ direct Rust function call (zero overhead)
             ▼
┌─────────────────────────────────────────────────────────────────┐
│ beads_rust crate (linked into SPUR binary)                      │
│   - rusqlite Connection (owns SQLite OS file handles)           │
│   - sync module (atomic tmp+fsync+rename for JSONL)             │
│   - flock primitives via blocking_write_lock_with_timeout       │
└────────────┬────────────────────────────────────────────────────┘
             │ syscalls (write, fsync, rename, flock, …)
             ▼
┌─────────────────────────────────────────────────────────────────┐
│ Operating system / filesystem                                   │
│   .beads/beads.db, .beads/beads.db-wal, .beads/beads.db-shm     │
│   .beads/issues.jsonl, .beads/.write.lock                       │
└─────────────────────────────────────────────────────────────────┘
```

### Comparison: subprocess vs direct crate

| Aspect | Current (`Command::new("br")`) | New (direct crate) |
|---|---|---|
| Boundary | OS process | Rust function call |
| Cost per call | fork + exec + JSON parse (~ms) | function call (~ns) |
| Type safety | stringly-typed JSON over stderr | typed `Result<T, BeadsError>` |
| Shared state | none (fresh process each call) | persistent `SqliteStorage` in SPUR's heap |
| Concurrency model | OS process scheduling | Tokio + `spawn_blocking` |
| Cross-process safety | per-call (`br` re-acquires flock each time) | per-call (we acquire flock each write) — same primitive, different caller |
| Failure surface | parse stderr, child process exit codes | typed `BeadsError` enum, no parsing |

### Two layers of communication (don't confuse them)

**Layer 1 — SPUR ↔ `beads_rust`:** direct function calls. Same process. Single SPUR instance has no concurrency story to negotiate at this layer beyond the `Mutex<SqliteStorage>` guarding the Connection handle.

**Layer 2 — SPUR-instance-A ↔ SPUR-instance-B:** *not direct*. Two SPUR processes never communicate with each other in code. They communicate **only via shared files** in `.beads/`:

- Both call `flock(.beads/.write.lock, LOCK_EX)`. The OS serializes them.
- A writes to `.beads/beads.db-wal`, then atomically renames `.beads/issues.jsonl.tmp.X` → `.beads/issues.jsonl`. B sees the new state on its next read.
- SQLite's WAL file format coordinates A and B's concurrent readers via the standard reader-writer protocol.

So:
- "How does SPUR talk to `beads_rust`?" → **direct function calls** (Layer 1)
- "How do two SPURs coordinate?" → **filesystem primitives** (Layer 2)

The rest of this spec — the five primitives, the writer Mutex, the flock backoff policy, the snapshot re-validation pattern — exists to make Layer 1 use the Layer 2 primitives correctly and observably.

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

### The four primitives

```rust
impl BeadsCrateAdapter {
    /// Lock-free snapshot read. Uses one reader connection from the pool.
    /// SQLite WAL provides snapshot isolation.
    pub async fn read<T, F>(&self, f: F) -> Result<T>
    where F: FnOnce(&SqliteStorage) -> Result<T> + Send + 'static,
          T: Send + 'static;

    /// Single write under cross-process flock + in-process writer guard.
    /// Each public mutation in beads_rust (update_issue, create_issue,
    /// delete_issue) is internally atomic via the crate's pub(crate)
    /// with_write_transaction. The closure should perform exactly one such
    /// mutation, or rely on `batch` for sequential mutations.
    pub async fn write<T, F>(&self, f: F) -> Result<T>
    where F: FnOnce(&mut SqliteStorage) -> Result<T> + Send + 'static,
          T: Send + 'static;

    /// Batch of mutations under one flock acquisition. Each individual
    /// mutation is atomic at the DB level; the batch as a whole is NOT a
    /// single DB transaction (see "Multi-statement atomicity" note below).
    /// Avoids flock acquire/release thrash for bulk operations.
    pub async fn batch<T, F>(&self, f: F) -> Result<T>
    where F: FnOnce(&mut SqliteStorage) -> Result<T> + Send + 'static,
          T: Send + 'static;

    /// Read a snapshot for use as a CAS token. Returns the snapshot value
    /// and the SqliteStorage's data_version pragma at read time. Caller
    /// performs compute on the regular async runtime (not blocking pool),
    /// then calls `validate_and_commit` to apply the result.
    pub async fn read_snapshot<S, F>(&self, f: F) -> Result<Snapshot<S>>
    where F: FnOnce(&SqliteStorage) -> Result<S> + Send + 'static,
          S: Send + 'static;

    /// Apply a write conditioned on the snapshot still matching. Acquires
    /// flock, re-reads under lock, compares to snapshot, writes if matched,
    /// returns Err(Conflict) if state moved.
    pub async fn validate_and_commit<S, T, FV, FW>(
        &self,
        snapshot:     Snapshot<S>,
        validate:     FV,   // re-read function (re-runs the read closure)
        write:        FW,   // commit function, given (current, snapshot)
    ) -> Result<T>
    where FV: FnOnce(&SqliteStorage) -> Result<S> + Send + 'static,
          FW: FnOnce(&mut SqliteStorage, S, S) -> Result<T> + Send + 'static,
          S: Send + Eq + 'static, T: Send + 'static;
}
```

#### Multi-statement atomicity — explicit non-goal

`beads_rust` 0.2.1's `with_write_transaction` is `pub(crate)` (`storage/sqlite.rs:869`). The adapter cannot wrap arbitrary multi-statement closures in a single DB transaction without forking the crate. This is an accepted constraint:

- Each individual public mutation (`update_issue`, `create_issue`, `delete_issue`) is atomic — internally the crate calls its own `with_write_transaction`.
- `batch` performs multiple such mutations sequentially under one flock acquisition. If the closure errors mid-batch, prior mutations remain committed (DB-level), but no other process can interleave because we hold flock.
- For invariants that *require* multi-statement atomicity beyond the file lock, callers must either upstream a public `with_write_transaction` to `beads_rust` (preferred) or design the operation as idempotent / restartable.

If a future `beads_rust` release exposes `with_write_transaction` publicly, a `with_txn` primitive can be added without breaking this design.

#### Snapshot CAS pattern — replaces the rejected `rpw_optimistic` monolith

The earlier `rpw_optimistic` design ran the compute phase inside `spawn_blocking`, which would park a blocking-pool thread if compute called an LLM, network API, or other slow async work. The two-phase split fixes this:

```
read_snapshot              (lock-free; uses reader pool; brief spawn_blocking)
   │
   ▼
─── caller's async compute on the Tokio runtime ───
     (LLM calls, network I/O, anything that wants to .await freely)
   │
   ▼
validate_and_commit        (acquire flock; re-read; compare; write or Conflict)
```

The compute phase happens entirely on the regular async runtime. The blocking pool is only used briefly twice (once for the snapshot read, once for the validate-then-write). The `Snapshot<S>` token captures both the value `S` and the `PRAGMA data_version` at read time; `validate_and_commit` rejects the write if either has changed.

If the verify step fails, the call returns `Err(Conflict { expected, actual })` and the caller decides whether to retry, escalate, or surface to the brain. This pattern:

- Keeps slow compute on the async runtime; never starves the blocking pool
- Avoids holding `.write.lock` during compute (which would starve other SPUR instances)
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

**Lock-hold cost (important):** the export work happens *while holding* `.beads/.write.lock`. The crate has two paths:

1. **Incremental fast path** (`try_existing_jsonl_replacements_atomically`, `sync/mod.rs:2711`) — when the dirty set is small and only updates existing JSONL lines in place, work is bounded by the dirty-set size, not total issue count. This is the common case during active execution.
2. **Full re-export fallback** (`write_jsonl_lines_atomically`, `sync/mod.rs:2801`) — when the incremental check fails (added/removed issues, sort order changed, first-call-after-restart with stale metadata), the full set is rewritten. Cost is O(N issues).

The fallback path *can* hold the lock long enough to starve other SPUR instances at large dataset sizes. Mitigations:

- **Cadence:** every 60s in MCP server processes; **only on idle** (no in-flight writes for ≥5s) during active plan execution. Aggressive flushing during high-throughput execution would amplify the starvation risk.
- **Skip during contention:** before calling `auto_flush`, check if `.write.lock` was contended in the recent past (via metrics); if so, skip this tick.
- **Observability:** the `beads.lock.hold_ms` histogram bucketed by op (`write` vs `flush`) makes the starvation path visible. If the p99 of `flush` lock-hold exceeds a threshold (e.g., 500ms), this is a signal to investigate dataset growth or to upstream a streaming-export API to `beads_rust`.

Other processes (TUI) skip auto-flush entirely — they're read-mostly.

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
| `beads.lock.hold_ms{op}` | histogram | Time spent holding `.write.lock`, bucketed by op (`write`, `batch`, `flush`, `migrate`, `validate_and_commit`) |
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

### Phase 2 — Direct adapter behind compile-time feature

**Action:** implement `BeadsCrateAdapter` (the design above) alongside the existing `BeadsAdapter` (CLI shellout). Both implement `IssueTracker`. Selection via **compile-time Cargo feature** `beads_crate_backend` on the `spur-pm` crate. Default-off in Phase 2; default-on in Phase 4.

**Why compile-time, not env-var:** runtime selection via env var would let different SPUR processes on the same host (TUI, MCP server, background worker) read different env values and run mixed backends against the same `.beads/` dir. Even though both backends honor `.write.lock`, mixing in-process state (long-lived crate connections) with subprocess invocations creates an asymmetry that's hard to reason about. Compile-time feature guarantees a single binary's processes are homogeneous.

**Verification:** all existing tests pass with both feature settings. CI runs the suite under both `--no-default-features` and `--features beads_crate_backend`. New unit tests for each primitive. New multi-process integration tests for the matrix above.

**Rollback:** rebuild with the feature off; ship.

**Risk:** medium. New code path, but isolated behind the feature.

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

**Action:** flip the default Cargo feature so `beads_crate_backend` is on by default. Ship. The CLI path remains compiled in (behind `--no-default-features`) for one release cycle as escape hatch.

**Quiescence protocol (mandatory before any backend swap, including initial rollout and rollback):**

1. **Drain in-flight writes** — stop accepting new `write` / `batch` / `validate_and_commit` calls; wait for all in-flight `spawn_blocking` write closures to complete (with a hard timeout, e.g. 30s).
2. **Force WAL checkpoint** — call `PRAGMA wal_checkpoint(TRUNCATE)` to flush all WAL pages to the main db file. Verifies the SQLite db is in a clean state.
3. **Force JSONL flush** — call `auto_flush()` unconditionally so the JSONL is in sync with the SQLite db.
4. **Close all connections** — drop reader pool, drop writer Mutex, finalize all prepared statements.
5. **Verify clean shutdown** — confirm no `.beads/.tmp_*` files remain, `.beads/.write.lock` is unheld.
6. **Restart with new backend.**

This protocol applies to both the cutover (CLI → crate) and the rollback (crate → CLI). Without it, the legacy CLI adapter on rollback would trip over WAL frames or unfinalized statements left behind by the crate adapter.

**Verification:** observability dashboards green for one week. No regressions in plan execution metrics. Quiescence protocol exercised in CI before each release.

**Rollback:** rebuild without the feature, run quiescence protocol, redeploy.

**Risk:** medium. Production traffic on new path.

### Phase 5 — Decommission

**Action:** delete `BeadsAdapter` shellout path. Delete `Command::new("br")` invocations from runtime code and most tests (test fixtures may still use `br` CLI for setup convenience, then progressively migrate). Drop CLI dependency from runtime `Cargo.toml`.

**Verification:** code search returns zero matches for `Command::new("br")` and `run_br` in non-test code.

**Rollback:** revert Phase 5 in git.

**Risk:** low. Only runs after Phase 4 has stabilized.

---

## Runbooks & incident response

Concrete operator playbook for the failure modes the metrics surface. Each entry covers: what triggers it, how to confirm, what to do, and what NOT to do.

### R1 — WAL grows unbounded (`beads.wal.size_bytes` keeps climbing)

**Trigger:** `beads.wal.size_bytes` > 50MB and rising for >10 minutes; `beads.checkpoint.outcome{kind=busy}` fires repeatedly.

**Likely cause:** a long-lived reader connection holds an open transaction or unfinalized prepared statement, pinning a WAL snapshot and preventing checkpoint truncation.

**Confirm:** `lsof | grep beads.db-wal` shows multiple SPUR processes holding the WAL file. Check `beads.read.duration_ms` p99 — if there's a multi-minute outlier, suspect a stuck reader.

**Action:**
1. Identify which SPUR process owns the longest-lived reader connection (PID via lsof).
2. SIGTERM that process gracefully (it should drop connections cleanly).
3. Trigger a manual checkpoint from any remaining SPUR: call the adapter's `force_checkpoint()` admin function (to be added in Phase 2).
4. If the checkpoint still fails, last resort: stop ALL SPUR processes, run `sqlite3 .beads/beads.db "PRAGMA wal_checkpoint(TRUNCATE);"` directly, restart.

**Do NOT:** delete `.beads/beads.db-wal` or `.beads/beads.db-shm` while any process has the db open. This will corrupt the database. Always stop processes first.

### R2 — Conflict counter spikes (`beads.conflict.total` rate >> baseline)

**Trigger:** `beads.conflict.total` rate exceeds 5/sec for >2 minutes during normal plan execution.

**Likely cause:** two or more SPUR instances writing to the same issues without plan-scoped lease (the `spur:plan-owner:*` label discipline) being respected, OR a snapshot read predicate is too narrow (missing labels/deps/comments in the snapshot).

**Confirm:** check `beads.conflict.exhausted_total` — if it's also rising, callers are giving up; investigate which `validate_and_commit` call sites correspond to the conflicted issue IDs.

**Action:**
1. Use plan ownership inspection (`spur-mcp` tools or `br show <id>`) to identify which plans are touching the conflicted issues.
2. If two plans legitimately race for the same issue, that's a brain-level dispatch bug — escalate to plan ownership review.
3. If one plan is generating the conflicts, the snapshot definition is too narrow; widen to include the changing field.

**Do NOT:** raise the conflict retry budget as a workaround. Conflicts are signal, not noise.

### R3 — Blocking pool saturated (`beads.blocking_pool.saturation` > 80%)

**Trigger:** `beads.blocking_pool.saturation` exceeds 80% sustained, OR Tokio task latency degrades broadly.

**Likely cause:** beads work is monopolizing the Tokio blocking pool (default 512 threads). Either too many concurrent writes, or compute work leaked into a `spawn_blocking` closure (violating the discipline rule).

**Confirm:** check `beads.write.duration_ms` p99 — if >100ms, individual writes are too slow. Check `beads.lock.hold_ms` — if writer Mutex hold time correlates, the writer connection is the bottleneck.

**Action:**
1. Short term: configure Tokio to allocate a larger blocking pool (`tokio::runtime::Builder::worker_threads`).
2. Audit recent `write` / `batch` call sites for non-trivial compute inside the closure.
3. If a specific call site is at fault, refactor to use the snapshot CAS pattern (`read_snapshot` + `validate_and_commit`).

**Do NOT:** raise the writer Mutex to allow concurrent writers. The flock would still serialize them, just with worse contention.

### R4 — Stuck `.write.lock`

**Trigger:** `beads.lock.busy_total` and `beads.lock.ceiling_total` both rising; new writes timeout with `Busy { holder_pid }`.

**Likely cause:** a SPUR process crashed mid-write and somehow leaked the flock (rare — OS should release on process exit), OR an external `br` invocation is holding it.

**Confirm:** `ls -la .beads/.write.lock` — check ownership. `lsof .beads/.write.lock` — see what process holds it. If no process appears to hold it, the flock is genuinely orphaned.

**Action:**
1. If a real process holds it: investigate why it's not releasing; SIGTERM if appropriate.
2. If the lock file is genuinely orphaned (no `lsof` match), it's safe to delete the lock file — `flock` advisory locks live in the kernel, not the file content. The file is just a presence sentinel.
   ```bash
   rm .beads/.write.lock
   ```
3. After deletion, the next process to acquire will create a fresh lock file.

**Do NOT:** `kill -9` a process holding the lock — this can leave a half-rename in flight. Prefer SIGTERM and wait.

### R5 — Legacy `br` detected at startup, SPUR refuses to boot

**Trigger:** SPUR exits at startup with `LegacyBinaryDetected { found: 0.1.x, min_required: 0.2.1 }`.

**Likely cause:** an old `br` binary is on PATH, often from a prior install or a CI image.

**Action:**
1. Identify the old binary: `which -a br` shows all `br` on PATH.
2. Upgrade: `cargo install --git https://github.com/Dicklesworthstone/beads_rust --tag v0.2.1` (or higher).
3. Verify: `br --version` reports ≥ 0.2.1.

**Emergency bypass (USE WITH EXTREME CAUTION):** set `SPUR_BYPASS_LEGACY_BR_CHECK=1` to skip the assertion. **This re-opens the bd-1h3w corruption surface** if the old `br` is actually run against the same `.beads/`. Use only when you can guarantee the old binary will not be invoked (e.g., headless CI where you've audited every script).

### R6 — Stale JSONL detected at boot

**Trigger:** SPUR logs `StaleJsonlDetected { db_seq: X, jsonl_seq: Y }` at startup; auto-recovers via forced flush.

**Likely cause:** previous SPUR process crashed between SQLite commit and JSONL flush. Recovery is automatic.

**Action:** none required — startup forces an `auto_flush` to bring JSONL up to date. Check audit logs to confirm what was lost (nothing should be — the SQLite db is the source of truth, JSONL is a derived view).

### R7 — Non-local filesystem detected

**Trigger:** SPUR logs `NonLocalFilesystem { path, fs_type }` at startup with a loud warning (or refuses to start, depending on config).

**Likely cause:** `.beads/` lives on NFS, SMB, or another network mount where `flock` semantics aren't reliable.

**Action:**
1. Move `.beads/` to local storage if at all possible.
2. If you must run on a network mount, set the config to `allow_non_local_fs: true` AND ensure only ONE SPUR process accesses the directory at a time. Multi-instance correctness is NOT guaranteed.

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
