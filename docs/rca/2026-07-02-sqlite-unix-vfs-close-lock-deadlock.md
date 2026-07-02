# RCA: SQLite unix-VFS ABBA deadlock wedges parallel beads test targets

**Date:** 2026-07-02
**Status:** Fixed upstream — beads_rust hotfix rev `cd65582` bumps rusqlite to
0.40 (bundled SQLite 3.53.2 ≥ 3.51.2 fix); spur workspace rusqlite bumped in
lockstep. An interim in-repo vendored 3.51.2 patch was applied and then
superseded the same day (see Resolution).
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

**Final fix (upstream, same day):** `beads_rust` is getspur-maintained, so
the rusqlite pin was bumped upstream instead of carrying a local patch:
branch `hotfix/0.1.15-rusqlite-0.40` (rev `cd65582`, PR
getspur/beads_rust#1), cut from spur's pinned v0.1.14 rev. rusqlite 0.40 →
libsqlite3-sys 0.38.1 → bundled SQLite **3.53.2** (contains the 3.51.2
fix; verified the mutex-free `unixIsSharingShmNode`). Because
`libsqlite3-sys` is a `links = "sqlite3"` crate (one version per graph),
the spur workspace `rusqlite` moved 0.38 → 0.40 in the same change.
`beads_rust` main (0.2.x) has since migrated to fsqlite (pure-Rust SQLite)
where this bug class does not exist — migration tracked as bd-35m8o.

**Interim fix (superseded):** for a few hours the repo carried
`[patch.crates-io] libsqlite3-sys = { path = "vendor/libsqlite3-sys" }` —
the pristine 0.36.0 crate with the amalgamation replaced by official
3.51.2 (`sqlite3.c` SHA3-256
`733b3fcc6cccb1e334424b9b91a9d68b618385b76ebfcbb106690bd3a9e61367`,
matching the sqlite.org changelog). Removed once the upstream hotfix
landed.

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
