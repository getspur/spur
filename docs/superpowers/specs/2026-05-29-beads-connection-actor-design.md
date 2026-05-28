# Design Spec: Beads connection-actor (eliminate the multi-process SQLite WAL deadlock)

- **Date**: 2026-05-29
- **Author**: Brain session (post-mortem from live `sample` of PIDs 16895 + 68822)
- **Status**: PROPOSED — not yet implemented
- **Scope**: `crates/spur-pm/src/beads_crate/*`
- **Related**:
  - `docs/rca/2026-05-28-spur-tui-hang-sqlite-wal-deadlock.md` (cost.db, same shape)
  - `docs/rca/2026-05-29-spur-tui-beads-wal-deadlock-pid16895.ipynb` (beads, prior analysis)
  - This spec supersedes that notebook's "Fix A (single-TUI guard)" recommendation.
- **Revised**: 2026-05-29 — re-evaluated against the worktree code graph. Corrected
  the open/checkpoint call-site inventory (added the `init.rs` site), the read-
  concurrency model (split the single write actor from a reader thread pool so the
  ≤4 read connections actually run in parallel — see §3/§4.1/C1), and the §6 pragma
  mechanism (per-connection pragmas can't be set via a throwaway connection).

---

## 1. Problem

Two (or more) processes that open `.beads/beads.db` concurrently can wedge the
entire Tokio runtime of each. Confirmed by sampling two live, hung `spur tui`
processes (PIDs 16895 and 68822) — **both** deadlocked identically, each with
its whole blocking pool frozen in `__psynch_mutexwait`.

### 1.1 Root cause (evidence-based)

The beads adapter (`adapter.rs`) opens a **fresh `SqliteStorage` connection per
operation** — 5 hot-path call sites (`read`:350, `read_snapshot`:392,
`validate_and_commit`:441, `write`:518, `auto_flush`:302), plus a 6th one-shot
site `init_writer_with_flush` in `init.rs`:573 — inside `spawn_blocking`, and
drops it when the closure returns. Under concurrent load this produces a **storm
of `Connection::open` / `Connection::drop` on the same inode**. In C-SQLite (the
pinned engine — see §6):

1. Every `open` takes the process-global VFS mutex (`unixEnterMutex`, via
   `findReusableFd`); every `drop`/close runs `sqlite3WalClose`, which attempts
   an **exclusive lock to checkpoint+delete the WAL** whenever the closing
   connection is the *last* live connection to the inode in this process. With no
   pooling that is the common case: any gap between overlapping ops makes the next
   close a "last close", and under bursty single-flight load it is *every* close.
2. That exclusive-lock attempt is cross-process contended (the cohabiting
   process holds the shm DMS read lock) and stalls **while holding a per-inode
   pthread lock mutex**.
3. Every other SQLite operation in the process — including unrelated DBs like
   `~/.spur/cost.db` — then piles up behind the process-global VFS mutex.
4. The blocking pool saturates; the UI render path's `spawn_blocking` calls back
   up; the TUI freezes. On Ctrl-C the runtime enters `Drop<BlockingPool>` and
   waits forever for a pool that can never drain.

### 1.2 What the samples corrected vs. the prior notebook

- **68822 is not "the healthy TUI."** It is symmetrically deadlocked. Any fix
  premised on "one good process survives" is wrong.
- **The block is not a slow checkpoint.** `checkpoint_wal_truncate_best_effort`
  already runs `busy_timeout(0)` + PASSIVE fallback and does not block. The
  block is the **connection-close storm** itself.
- **No thread holds a running lock.** Every SQLite-touching thread in both
  processes is parked on a process-local pthread mutex; the "holder" close
  thread is itself parked on `unixLock → __psynch_mutexwait`. The trigger is the
  *volume* of concurrent open/close/lock ops on one inode, amplified across
  processes.

### 1.3 Why not just forbid multiple TUIs (rejected "Fix A")

`beads.db` is SPUR's shared collaboration store (brain ↔ worker ↔ reviewer) and
is also opened by the `bd` CLI. The deadlock needs only **two beads accessors of
any kind** — not two TUIs. A single-TUI lock is simultaneously **too weak** (does
not cover `bd`/workers) and **too strict** (removes a legitimate workflow the
user actively relies on). SQLite WAL is explicitly designed for concurrent
multi-process access; the defect is in *how* spur uses it.

---

## 2. Goals / Non-goals

**Goals**
- G1. Concurrent processes on one `beads.db` never deadlock.
- G2. A single beads operation can never freeze the whole process runtime.
- G3. Preserve multi-TUI / `bd` / worker cohabitation (no new exclusivity).
- G4. Keep all public `BeadsCrateAdapter` async method signatures unchanged.
- G5. Forward-compatible with the in-flight `fsqlite` migration (where
  `SqliteStorage` becomes `!Send`).

**Non-goals**
- N1. Changing beads' on-disk format or cross-process write semantics.
- N2. Multi-statement transactional atomicity (still out of scope, as today).
- N3. Fixing C-SQLite's per-inode mutex behavior itself.

---

## 3. Design overview

Introduce **persistent, thread-pinned connections** per adapter, in two pieces:

- a **write actor** — one dedicated OS thread owning the single long-lived write
  connection; all writes are submitted to it over a channel. Cross-process writes
  are already serialized by the `.write.lock` flock, so one writer thread adds no
  contention.
- a **reader thread pool** — a bounded set of threads (start: 4), each owning one
  long-lived read connection for its lifetime, fed by a shared job channel so
  reads still run concurrently (matching today's blocking-pool parallelism).

```
Today:  read/write ──spawn_blocking──▶ open fresh SqliteStorage ▶ run ▶ DROP (close storm)
Spec:   write ──mpsc──▶ [beads-db-writer thread] ─▶ reuse long-lived write conn ─▶ reply
                              └─ 30s timer / post-write nudge ─▶ wal_checkpoint(PASSIVE)
        read  ──mpmc──▶ [beads-db-reader pool ×N] ─▶ each thread reuses its own conn ─▶ reply
```

This removes the open/close storm (G1, G2), keeps WAL + `.write.lock` as the only
cross-process coordination (G3), hides behind the existing async API (G4), and
confines every connection to exactly one thread for its lifetime so `!Send`/`!Sync`
engines are fine (G5) — **without** serializing reads behind each other (the
failure mode of a single shared actor thread; see C1 in §11).

---

## 4. Detailed design

### 4.1 New module `beads_crate/beads_db.rs`

```
// Writer side — single thread, owns the one write connection.
enum WriteMsg  { Job(Box<dyn FnOnce(&mut SqliteStorage) + Send>), Checkpoint, Shutdown }
// Reader side — N threads, each owns its own read connection for life.
type ReadJob   = Box<dyn FnOnce(&SqliteStorage) + Send>;

struct BeadsDb {
    write_tx:   SyncSender<WriteMsg>,
    read_tx:    /* MPMC sender: crossbeam-channel, flume, or Arc<Mutex<mpsc::Receiver>> */ ReadSender,
    write_join: Option<JoinHandle<()>>,
    read_joins: Vec<JoinHandle<()>>,
}
```

- `BeadsDb::spawn(beads_dir, lock_timeout_ms, reader_threads) -> Result<Self>`
  starts one named `beads-db-writer` thread (opens the long-lived write
  connection) and `reader_threads` named `beads-db-reader-{i}` threads (each opens
  and keeps one read connection). All enter their recv loops.
- `BeadsDb::submit_write<T>(f)` / `submit_read<T>(f) -> impl Future<Output =
  Result<T>>` box `f`, send it with a `tokio::oneshot` reply, and await the reply.
  The caller's runtime worker is never blocked. Writes land on the writer thread;
  reads are picked up by whichever reader thread is free (an **MPMC** channel —
  plain `std::sync::mpsc` is single-consumer and won't fan out).
- `BeadsDb::request_checkpoint()` sends a non-blocking `WriteMsg::Checkpoint` nudge
  to the writer thread (the sole checkpointer).
- The writer loop also self-checkpoints every 30 s (`recv_timeout`).

### 4.2 Connection lifecycle & pragmas

- **One long-lived write connection** lives for the adapter's lifetime → no
  per-op "last close" → `sqlite3WalClose`'s exclusive-lock path never runs in the
  hot path.
- **N long-lived read connections**, one pinned per reader thread, opened once at
  spawn and reused for the thread's lifetime → reads never reopen per call and run
  concurrently across the N threads. NOTE: the existing `ReaderPool` checkout
  abstraction in `reader_pool.rs` is **currently dead code** (only tests call it)
  and is *not needed* under thread-pinned readers — each reader thread simply holds
  its own connection. Leave `reader_pool.rs` as-is or delete it; do not build on it.
- `PRAGMA persist_wal = ON` and `PRAGMA wal_autocheckpoint = 0` **must be set on
  each long-lived connection itself**, not on a throwaway connection — both are
  per-connection settings (see §6 for why the separate-connection trick is inert
  on the pinned engine). `persist_wal` lets the eventual shutdown close skip the
  exclusive-lock-and-delete; `wal_autocheckpoint = 0` keeps the only checkpoints
  the writer's bounded PASSIVE ones.

### 4.3 Checkpoint policy

- Exactly one checkpointer: the **writer** thread, using a long-lived connection
  (its write connection, or one dedicated checkpoint connection it keeps open —
  *not* a fresh `rusqlite::Connection` per tick, which is what
  `checkpoint_wal_truncate_best_effort` does today and would reintroduce the
  open/close churn this design removes). Mode = `PASSIVE`, `busy_timeout(0)` →
  never blocks, never takes the exclusive lock, yields the WAL to the next tick if
  busy. Triggered by the 30 s timer and by `request_checkpoint()` after writes.
- Delete the **four** `checkpoint_wal_truncate_best_effort(&db_path)` calls: three
  in `adapter.rs` (309 `auto_flush`, 465 `validate_and_commit`, 533 `write`) and
  **one in `init.rs`** (`init_writer_with_flush`, ~578). The writer thread owns
  truncation now. Caveat: `init_writer_with_flush` runs *before* the actor exists
  (one-shot startup flush, gated by `can_skip_init_flush`), so either route it
  through the actor once spawned or keep its single open+checkpoint as the
  documented sole exception to the no-fresh-open rule.

### 4.4 Write-lock interaction (unchanged)

Cross-process write serialization stays exactly as today: `write` /
`validate_and_commit` / `auto_flush` first `acquire_write_lock_async`
(`.beads/.write.lock` flock), then submit the closure to the **writer thread**,
which holds the guard (moved into the closure) for the duration. Reads remain
lock-free (WAL snapshot isolation) and never touch the writer thread.

### 4.5 Shutdown

`Drop for BeadsDb` sends `WriteMsg::Shutdown` to the writer thread and closes the
reader job channel, then `join`s all threads. The joins are bounded because no
thread is ever blocked on a contended lock (the writer only runs PASSIVE
checkpoints + serial ops; readers run lock-free WAL-snapshot reads). This also
addresses the original symptom: a hung DB can no longer make `Drop<BlockingPool>`
wait forever.

### 4.6 Adapter changes (signatures unchanged)

```
pub struct BeadsCrateAdapter { /* …existing… */ db: BeadsDb }

read(f)    -> db.submit_read(move |s| f(s)).await        // reader thread, its own conn
write(f)   -> let flock = acquire_write_lock_async(...).await?;
              let out = db.submit_write(move |s| { let _flock = flock; f(s) }).await;
              db.request_checkpoint(); out
read_snapshot       -> db.submit_read   (lock-free, reader thread)
validate_and_commit -> db.submit_write  under the flock (same shape as write)
auto_flush          -> db.submit_write  under the flock
```

Reads go to `submit_read` (the reader pool), **never** the writer thread — routing
them through the writer would serialize every read behind every other read and
behind writes, which is the concurrency regression C1 (§11) calls out.

---

## 5. Public API impact

None to async method signatures. Callers in `spur-tui` / `spur-core` are
unaffected. `BeadsCrateAdapter` is constructed once per process (as today) and
held in an `Arc`; it remains `Send + Sync` because the `!Sync` connections live
only on their pinned writer/reader threads, reached via the `Send` channels.

---

## 6. Engine note (pinned vs. migrating)

- **Pinned build** (`beads_rust` rev `beff256b`, in `Cargo.lock`) uses
  **rusqlite / C-sqlite** (`libsqlite3-sys`). `SqliteStorage` is `{ conn:
  rusqlite::Connection }` — `Send`, `!Sync` — and `open_with_timeout` sets only
  `busy_timeout` (no `persist_wal`, no `wal_autocheckpoint` override) with no
  custom `Drop`. The §1 mechanism is exactly C-sqlite default close behavior.
- `beff256`'s `SqliteStorage` exposes **no raw-pragma method**. ⚠️ A *separate*
  short-lived `rusqlite::Connection` **will not work** for the §4.2 pragmas:
  `persist_wal` and `wal_autocheckpoint` are **per-connection** settings, so
  setting them on a throwaway connection that then closes does nothing to the
  long-lived `SqliteStorage` connection's close / autocheckpoint behavior. They
  must be set on the actual long-lived connection — which on `beff256` means
  **patching `open_with_timeout` upstream** (or vendoring a thin pragma setter on
  `SqliteStorage`). Mitigation: the hot-path win does **not** depend on these
  pragmas — the long-lived connection already eliminates per-op last-closes, and
  `persist_wal` only matters for the single shutdown close. If the upstream patch
  slips, ship the actor without the pragmas and accept one exclusive-lock attempt
  at process exit (Step 0's timeout already prevents that from hanging exit).
- **Migration target** (`fsqlite` revs) makes `SqliteStorage` `!Send`
  (`Cell<…>` fields, custom `checkpoint_wal_on_drop` / `mutation_count`). The
  actor model already confines connections to one thread, so it works unchanged;
  align the pragma-setting with whoever owns the `fsqlite` cutover.

---

## 7. Failure modes & invariants

- **INV-1**: each connection is pinned to exactly one thread for its entire
  lifetime (the write connection → the writer thread; each read connection → its
  own reader thread). A connection is never touched from another thread.
- **INV-2**: at most `1 + reader_threads` connections to `beads.db` exist per
  process (one writer + N readers).
- **INV-3**: no code path on any connection thread takes an exclusive WAL lock
  (PASSIVE only; `persist_wal` disables delete-on-close).
- **INV-4**: no connection thread is ever blocked on a contended lock → `submit_*`
  futures and the `Drop` joins are bounded.
- If a connection thread dies (open failure), `submit_read` / `submit_write`
  return an error promptly rather than hanging; callers surface it like any beads
  error.

---

## 8. Test plan (TDD — write first)

1. **Cohabitation regression** (`crates/spur-pm/tests/beads_cohabit.rs`, new):
   two adapters on one tempdir `beads.db`, both running a write+read loop;
   assert all ops complete under a deadline. Fails today (wedges).
2. **No open-per-call after warmup**: add `sqlite_open_total` to
   `ContentionMetrics`; assert it stays flat across many reads.
3. **WAL bounded**: after a write burst + one checkpoint tick, assert
   `beads.db-wal` ≤ a few pages.
4. **Drop is bounded**: drop the adapter mid-load; assert join < 1 s.
5. Existing `adapter.rs` tests must keep passing unchanged (API parity).

Verification gate (per `rust-idioms`): `cargo check`, `cargo clippy -- -D
warnings`, `cargo test -p spur-pm` all green before claiming done.

---

## 9. File-by-file changes

| File | Change |
|---|---|
| `beads_crate/beads_db.rs` | **new** — write actor + reader thread pool (§4.1–4.5) |
| `beads_crate/mod.rs` | register `mod beads_db;` |
| `beads_crate/adapter.rs` | add `db: BeadsDb` field; delegate the 5 methods (reads→`submit_read`, writes→`submit_write`); delete the 3 per-op checkpoint calls (309, 465, 533) |
| `beads_crate/init.rs` | `init_writer_with_flush` (573 open / ~578 checkpoint) — the 6th fresh-open + 4th checkpoint site; route through the actor or document as the one-shot exception |
| `beads_crate/reader_pool.rs` | **currently dead code** (test-only) — not needed under thread-pinned readers; leave or delete (do *not* add a sync checkout) |
| `beads_crate/metrics.rs` | add `sqlite_open_total`, `checkpoint_total` counters |
| `crates/spur-pm/tests/beads_cohabit.rs` | **new** — §8.1 |

---

## 10. Rollout

- **Step 0 (de-risk, ship immediately):** Fix D — wrap the runtime/shutdown
  blocking-DB drops in `tokio::time::timeout` so Ctrl-C always exits even if the
  pool is jammed. (Both hung PIDs would have died cleanly with this.)
- **Step 1:** land the actor (§4) behind the existing API; run §8 tests.
- **Step 2 (follow-up):** retire `checkpoint_wal_truncate_best_effort` entirely
  once the actor's checkpointer is proven.

## 11. Open questions

- Reader thread count (start at 4; tune from `sqlite_open_total` / `issue_probe`
  latency). N readers = N persistent connections + N threads — balance read
  parallelism against FD/thread budget.
- **C1 — resolved here; rejected alternative recorded.** The original draft had a
  *single* actor thread own both the write connection and a `ReaderPool`, with
  reads submitted to that one thread. That serializes every read on one thread,
  so a 4-connection pool yields **zero** read concurrency (capacity 1 ≡ capacity 4)
  and regresses today's blocking-pool parallelism for the read-heavy TUI. Chosen
  design: split the write actor from a reader thread pool (each reader owns one
  pinned connection). The simpler "single actor, serial reads, no pool" remains a
  fallback if profiling shows read contention is negligible.
- Should `auto_flush` share the write connection or get its own? (Lean: share — it
  already runs under the flock, on the writer thread.)
- Coordinate the upstream `open_with_timeout` pragma patch (§6) with the `fsqlite`
  migration owner.
