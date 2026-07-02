# RCA: SQLite unix-VFS ABBA deadlock wedges parallel beads test targets

**Date:** 2026-07-02
**Status:** Diagnosed; upstream SQLite bug; mitigations listed
**Affected:** any test target (or production process) that opens/closes many
beads sqlite connections concurrently in one process — observed in
`spur-core` `plan_ownership` (16 tests wedged simultaneously) and, with the
same signature, `persisted_authority_flip` on a prior builder instance.

## Symptom

During `scripts/spur-cargo test --workspace`, a beads-heavy test target
stalls: a few tests complete, then every remaining test in the target
reports `has been running for over 60 seconds` simultaneously and the
process never exits. CPU ~0%. Single-target reruns pass in ~1s — the wedge
is probabilistic under parallel load (default 16 test threads).

## Root cause (evidence: live gdb stack dump, VM `i-086b2539127e73b2d`, pid 280316)

Lock-order inversion (ABBA) inside **stock SQLite 3.51.1** (bundled by
`libsqlite3-sys 0.36.0` via `rusqlite 0.38` ← `beads_rust`), between:

- the process-global unix-VFS mutex (`unixEnterMutex`, `staticMutexes`), and
- the per-inode `pInode->pLockMutex`.

Two code paths acquire them in opposite orders:

| Thread | Path | Holds | Waits for |
|---|---|---|---|
| `beads-db-reader` (LWP 280515) | `Connection::drop` → `sqlite3WalClose` → `unixLock(EXCLUSIVE)` at `sqlite3.c:41067` → `unixIsSharingShmNode` at `sqlite3.c:43881` | inode `pLockMutex` | global VFS mutex |
| `beads-db-writer` (LWP 280491) | `sqlite3Close` → `unixClose` at `sqlite3.c:41551` | global VFS mutex | the same inode `pLockMutex` (gdb: `__data.__owner == 280515`) |

`unixIsSharingShmNode` is a recent upstream robustness feature (checks
whether the `-shm` node is shared before allowing a WAL database to take an
exclusive lock during close). Its call site runs under `pLockMutex` and
takes the global mutex — inverting `unixClose`'s global→inode order. Once
the two threads cross, every other `sqlite3_open` in the process queues on
the global VFS mutex (observed: 46 waiter threads), which is why whole test
targets freeze at once: each test's `TestBeadsWorkspace::init` →
`SqliteStorage::open` blocks in `findReusableFd` → `unixEnterMutex`.

Trigger requirements: same process, ≥2 threads concurrently closing beads
connections, at least one of them a WAL database. Parallel test execution
across TempDir repos provides exactly this churn.

## Why it surfaced now

The E0275 recursion-limit breakage (fixed 2026-07-02) had made many
spur-core beads test targets compile-dead; full-workspace runs died at the
compile wall before reaching this contention profile. With those targets
compiling again, full-suite runs exercise dozens of parallel beads
open/close cycles for the first time in weeks.

## Mitigations

1. **Track upstream.** The inversion exists in bundled SQLite 3.51.1
   (`libsqlite3-sys 0.36.0`); newer `libsqlite3-sys` 0.38.x exists — check
   its bundled SQLite for a fix before bumping (`rusqlite` comes in via
   `beads_rust`, so the bump lands there).
2. **No clean compile-out:** the `#define unixIsSharingShmNode(pFile) (0)`
   fallback is gated on `SQLITE_WASI || SQLITE_OMIT_WAL` — disabling WAL is
   not acceptable.
3. **Reduce trigger surface in beads_rust:** serializing connection *close*
   across the process (a global mutex around `Connection::drop` /
   `SqliteStorage` drop) removes one side of the race.
4. **Operational:** when a workspace test run wedges with this signature
   (many tests "running over 60 seconds" in one beads-heavy target, ~0%
   CPU), kill the test process on the builder and rerun; it is not a code
   regression. Diagnose with:
   `gdb -p <pid> -batch -ex "thread apply all bt"` and look for
   `unixIsSharingShmNode` + `unixClose` frames.

## Evidence trail

- Thread wchan census: 172 threads `futex_wait_queue`, 15 `do_epoll_wait`.
- Full stack dump: `/tmp/plan_ownership_stacks.txt` on the builder (ephemeral).
- Mutex ownership confirmed via
  `print ((pthread_mutex_t*)0xffff180014c8)->__data.__owner` → LWP 280515.
- Prior green runs of the same target/code (1.2s, 1.5s) establishing
  probabilistic character.
