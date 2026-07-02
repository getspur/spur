# RCA: SQLite unix-VFS ABBA deadlock wedges parallel beads test targets

**Date:** 2026-07-02
**Status:** Fixed via vendored SQLite 3.51.2 (see Resolution); upstream bug fixed in SQLite 3.51.2
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

## Resolution

Upstream fixed this in **SQLite 3.51.2** (2026-01-09): *"Fix an obscure
deadlock in the new broken-posix-lock detection logic"*. The 3.51.2
`unixIsSharingShmNode` no longer takes the global VFS mutex — it reads
`pShmNode->nRef` with an atomic load — which removes the inode→global
acquisition and the ABBA cycle.

Applied here as `[patch.crates-io] libsqlite3-sys = { path =
"vendor/libsqlite3-sys" }`: the pristine 0.36.0 crate with the bundled
amalgamation replaced by the official 3.51.2 amalgamation
(`sqlite3.c` SHA3-256
`733b3fcc6cccb1e334424b9b91a9d68b618385b76ebfcbb106690bd3a9e61367`,
matching the sqlite.org changelog). 3.51.2 is a patch release; the crate's
pregenerated bindings remain valid.

**Exit criteria for the patch:** drop `vendor/libsqlite3-sys` and the
`[patch.crates-io]` entry once `beads_rust` (and any other rusqlite user in
the graph) moves to a rusqlite/libsqlite3-sys pairing whose bundled SQLite
is ≥ 3.51.2 (`libsqlite3-sys` 0.37+ ships post-3.51.2 amalgamations).

## Rejected alternatives

1. **Compile-out:** the `#define unixIsSharingShmNode(pFile) (0)` fallback
   is gated on `SQLITE_WASI || SQLITE_OMIT_WAL` — disabling WAL is not
   acceptable.
2. **System sqlite via `LIBSQLITE3_SYS_USE_PKG_CONFIG`:** Debian 12 on the
   builder ships SQLite 3.40 — too old, and version skew per machine.
3. **Serializing connection close in `beads_rust`:** works but papers over
   the bug and costs a cross-repo change; superseded by the upstream fix.

## Operational note

If a workspace test run ever wedges with this signature again (many tests
"running over 60 seconds" in one beads-heavy target, ~0% CPU): kill the
test process on the builder and rerun; diagnose with
`gdb -p <pid> -batch -ex "thread apply all bt"` and look for
`unixIsSharingShmNode` + `unixClose` frames.

## Evidence trail

- Thread wchan census: 172 threads `futex_wait_queue`, 15 `do_epoll_wait`.
- Full stack dump: `/tmp/plan_ownership_stacks.txt` on the builder (ephemeral).
- Mutex ownership confirmed via
  `print ((pthread_mutex_t*)0xffff180014c8)->__data.__owner` → LWP 280515.
- Prior green runs of the same target/code (1.2s, 1.5s) establishing
  probabilistic character.
