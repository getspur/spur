# Primitive-Selection Decision Cards — bd-db300.5.3.2.2

> One card per hot metadata class from HOT_METADATA_INVENTORY.md.
> Cards for M3, M4, M5, M7, M8, M9, M10, M12 are grouped as "no-change" at the end
> because they are per-connection locals that don't need new publication primitives.

---

## Card 1: M6 — PagerInner Committed State

**Current primitive:** `Mutex<PagerInner>` (pager.rs:296). Every `pager.begin()` and `pager.commit()` takes this lock.

### Chosen primitive: **Immutable Snapshot Publication with Generation Replacement**

Publish a frozen `Arc<PagerCommittedSnapshot>` via `ArcSwap` (or hand-rolled atomic pointer swap). Readers load the current snapshot pointer without locking. Writers create a new snapshot, publish it atomically, reclaim the old one via reference counting.

**Why this fits:**
- Readers need `commit_seq`, `db_size`, `freelist_count`, `journal_mode` — all read-only during a transaction.
- Writers produce a complete new state on every commit — natural snapshot boundary.
- `Arc` reference counting handles reclamation without explicit epoch tracking.
- Read side becomes wait-free. Write side allocates a small struct per commit (~64 bytes).

**EV score:** 8/10 — eliminates the #1 Mutex bottleneck on every begin/commit for readers.
**Relevance:** Critical for c4+ file-backed workloads.

### Rejected alternatives

| Alternative | Why rejected |
|------------|-------------|
| **Seqlock on PagerInner fields** | PagerInner has non-atomic fields (freelist Vec, file handle). Seqlock requires all fields to be readable without locking — Vec cannot be safely torn-read. Would require flattening freelist to an atomic count, losing the actual page list. |
| **Left-Right** | Overkill. Left-Right gives wait-free reads but requires double-buffered state including the full freelist Vec. Memory cost 2× per pager is unjustified when readers only need the scalar summary fields. |
| **RwLock** | Marginal improvement. `RwLock` still has reader-side atomic overhead (read-count increment/decrement). Under c8+, reader-side atomics on the same cache line degrade to ~Mutex performance. |
| **BRAVO** | Designed for heavily read-biased locks. Our read/write ratio is ~2:1, not 100:1. BRAVO's overhead on the write path (checking per-reader slots) is wasted at this ratio. |
| **Sharded PagerInner** | Pager state is inherently singular (one database file → one committed state). Sharding makes no semantic sense. |

### Retry and reclamation implications
- **Retry:** None. `Arc::clone` on the snapshot pointer is wait-free.
- **Reclamation:** `Arc` drop decrements refcount. When the last reader releases, the snapshot is deallocated. No explicit epoch or hazard pointer needed. Worst case: a long-running reader holds an old snapshot alive — bounded by transaction lifetime.

### Fallback trigger
If per-commit allocation of `Arc<PagerCommittedSnapshot>` measurably hurts throughput (unlikely — ~64 bytes per commit), revert to current Mutex. Detection: `FSQLITE_SNAPSHOT_ALLOCS` counter; if alloc-ns > 5% of commit-ns, fallback.

### User-visible symptom signature
- **Misbehavior:** Stale snapshot delivered to reader (should be impossible with atomic swap, but: if a reader holds an old Arc across a DDL boundary, it may see pre-DDL schema).
- **Diagnostic:** `visible_commit_seq` in snapshot < global `commit_seq`. Log field: `snapshot_staleness_commits`.

### Logging/test obligations
- **Logging:** `trace_id, metadata_class=M6, operation=publish|read, snapshot_gen, visible_commit_seq, alloc_ns, reader_count`
- **Unit tests:** Concurrent read during publish doesn't block; snapshot contents match committed state; old snapshot reclaimed after last reader drops.
- **E2E:** c4 file-backed write-heavy benchmark before/after; Mutex wait-time eliminated from flamegraph.
- **Topology:** c8 cross-NUMA: verify no regression from Arc atomic refcount bouncing.

### Adoption wedge
Shadow-run: keep Mutex path as baseline, add snapshot-read path behind `PRAGMA fsqlite_pager_snapshot_publish = ON`. Compare commit_seq consistency and latency distribution. Graduate to default after 3 clean benchmark runs.

### Rollback recipe
1. Set `PRAGMA fsqlite_pager_snapshot_publish = OFF`.
2. All new transactions use Mutex path.
3. Outstanding snapshot Arcs drain naturally (bounded by max txn lifetime).
4. No state corruption possible — snapshot is read-only once published.

---

## Card 2: M1 — PublishedPagerState (Seqlock Publication Plane)

**Current primitive:** Seqlock on atomic fields + `publish_lock: Mutex` + `sequence_cv: Condvar` (pager.rs:1234-1251). Write side takes Mutex, bumps sequence odd, writes atomics, bumps sequence even, wakes Condvar.

### Chosen primitive: **Retain Seqlock, Eliminate Condvar, Add Striped Counters**

The seqlock read path is already wait-free and bounded-retry. The problems are:
1. `publish_lock` Mutex serializes writers (unnecessary if writers already hold PagerInner Mutex during commit).
2. `sequence_cv` Condvar creates cross-node wake storms under c8+.

Fix: (a) Remove `publish_lock` — publication only happens inside PagerInner Mutex critical section, so it's already serialized. (b) Replace `sequence_cv` Condvar with polling on `sequence` atomic — readers that need to wait for a new commit spin-check `sequence` with exponential backoff (1µs, 2µs, 4µs, max 1ms). This eliminates the Condvar futex syscall and cross-node wake.

**EV score:** 6/10 — moderate improvement; seqlock reads are already fast. Main win is Condvar elimination.
**Relevance:** c8+ file-backed with frequent cross-connection visibility checks.

### Rejected alternatives

| Alternative | Why rejected |
|------------|-------------|
| **Replace seqlock with Arc snapshot** | Seqlock is already essentially free for readers (2 atomic loads + field reads + 1 verify load). Arc swap adds allocation + refcount. Not worth it when seqlock works and fields are all atomics. |
| **Epoch-based publication** | Over-engineered. The seqlock already provides bounded-retry reads. EBR adds complexity without measurable benefit when retry rate is <1%. |
| **Remove seqlock entirely (just atomics)** | Fields are individually atomic but readers need a *consistent* snapshot across all fields. Without seqlock, a reader could see `commit_seq` from commit N and `db_size` from commit N+1. Seqlock prevents this torn read. |

### Retry and reclamation implications
- **Retry:** Unchanged from current seqlock — bounded to ~0-2 retries. `read_retry_count` striped counter already tracks this.
- **Reclamation:** None — all fields are in-place atomics.

### Fallback trigger
If polling-based wait causes excessive CPU burn (detect: `FSQLITE_PUBLICATION_POLL_SPINS` counter exceeds 10000/sec per connection), revert to Condvar.

### User-visible symptom signature
- **Misbehavior:** Reader sees torn snapshot (seqlock retry detects and retries — should never surface). If retry budget exhausted → stale read.
- **Diagnostic:** `read_retry_count` spike above baseline.

### Logging/test obligations
- **Logging:** `publication_write_count, read_retry_count, sequence_value, publish_lock_wait_ns` (last field → 0 after Mutex removal)
- **Unit tests:** Concurrent publish during read → retry count ≤ 3; Condvar removal doesn't cause missed wakeups.
- **E2E:** c8 file-backed mixed workload; measure publication latency p99 before/after Condvar removal.

### Adoption wedge
Condvar removal is a pure deletion + polling replacement. Shadow-compare: log retry counts and wait durations side by side. If polling wait p99 < 5µs, graduate.

### Rollback recipe
1. Re-add Condvar + `publish_lock`.
2. Replace polling loop with `sequence_cv.wait()`.
3. No state changes — atomic fields are unchanged.

---

## Card 3: M11 — ConcurrentRegistry + PageLockTable

**Current primitive:**
- `ConcurrentRegistry`: `Arc<Mutex<ConcurrentRegistry>>` — global Mutex for session begin/commit/abort.
- `InProcessPageLockTable`: CAS flat array for pages 1-65536 + sharded Mutex HashMap for pages >65536.

### Chosen primitive: **Sharded Registry + Existing CAS Page Locks**

Split `ConcurrentRegistry` into N shards (N = number of hardware threads, capped at 64). Each session is assigned to a shard by hash of `session_id`. `begin_concurrent()` only takes the shard Mutex, not the global Mutex. `get_mut()` and `remove_and_recycle()` index into the right shard directly.

Page locks stay as-is — CAS on flat array is already optimal for the fast path.

**EV score:** 7/10 — reduces registry contention from O(N) to O(N/shards) per concurrent begin/commit.
**Relevance:** c8+ file-backed concurrent-writer workloads.

### Rejected alternatives

| Alternative | Why rejected |
|------------|-------------|
| **Lock-free registry (crossbeam-epoch)** | ConcurrentRegistry tracks session state (handles, write sets) with non-trivial ownership. Lock-free reclamation of session state is complex and error-prone. The Mutex-per-shard approach is simpler and nearly as fast under realistic shard counts. |
| **RwLock on registry** | `begin_concurrent()` is a write operation (inserts session). `commit()` is a write operation (removes session). Read-only queries of the registry are rare. RwLock's read-biased optimization doesn't help. |
| **Per-connection session pre-allocation** | Would avoid registry lookup on begin, but sessions carry write-set state that can't be pre-allocated without knowing transaction size. |

### Retry and reclamation implications
- **Retry:** Shard Mutex → no retry, just reduced contention. CAS page locks → unchanged bounded retry.
- **Reclamation:** Session removal is synchronous under shard Mutex — same as today, just less contention.

### Fallback trigger
If sharded registry introduces session-lookup misrouting bugs (detect: `session_not_found` error rate > 0 in production), revert to global Mutex.

### User-visible symptom signature
- **Misbehavior:** `SQLITE_BUSY` errors from page lock CAS failure (expected under hot-page contention; not a primitive failure).
- **Diagnostic:** `concurrent_registry_lock_wait_ns` per-shard; if any shard is 10× hotter than mean, rebalance hash.

### Logging/test obligations
- **Logging:** `shard_id, session_id, operation=begin|commit|abort, lock_wait_ns, active_sessions_in_shard`
- **Unit tests:** Sessions assigned to correct shard; concurrent begin/commit across shards doesn't lose sessions.
- **E2E:** c8 hot-page contention benchmark; registry Mutex wait-time reduced vs. baseline.

### Adoption wedge
Feature-gated: `PRAGMA fsqlite_sharded_registry = ON`. Compare against global Mutex baseline. Graduate after c4/c8 file-backed wins confirmed.

### Rollback recipe
1. Set `PRAGMA fsqlite_sharded_registry = OFF`.
2. All new sessions route to global Mutex.
3. In-flight sessions in shards drain naturally (bounded by txn lifetime).

---

## Cards 4-12: Per-Connection Locals (No Primitive Change)

These metadata classes (M2, M3, M4, M5, M7, M8, M9, M10, M12) are per-connection or per-handle locals accessed via `RefCell`, `Cell`, or owned fields. They have **no cross-thread contention** and do not need publication primitives.

| Class | Current | Decision | Rationale |
|-------|---------|----------|-----------|
| M2 (BoundPagerPublication) | Stack Copy | **Keep** | Write-once local struct. Zero overhead. |
| M3 (Schema cookie/generation) | RefCell/Cell | **Keep** | Per-connection scalar. No sharing. |
| M4 (Cached read snapshot) | RefCell | **Keep** | Per-connection txn handle. Park/reuse pattern already optimal. |
| M5 (Cached write txn) | RefCell | **Keep** | Per-connection :memory: only. Already eliminates Mutex. |
| M7 (WAL frame count) | Owned field | **Keep** | Per-handle. Refresh reads disk — no lock. |
| M8 (WAL generation) | Owned Copy | **Keep** | 12-byte Copy struct per-handle. |
| M9 (Staleness gate) | RefCell | **Keep** | Per-connection u64. Comparison target (M1) is addressed in Card 2. |
| M10 (Cached VDBE engine) | RefCell | **Keep** | Per-connection. Already reuses allocations. |
| M12 (Parse/compiled cache) | RefCell | **Keep** | Per-connection LRU. No cross-thread access. |

**Rejected alternatives for all:** Any shared-primitive approach (RCU, seqlock, etc.) would add overhead to data that is never shared. The per-connection `RefCell` pattern is strictly optimal for single-threaded access within a connection.

---

## Composition Interference Notes

Per bd-db300.7.8.4 (Controller Composition Proof):

| Card | Composes with | Interference risk | Guard |
|------|--------------|-------------------|-------|
| Card 1 (M6 snapshot publish) | D1 WAL policy | LOW — snapshot publish is inside commit; WAL policy affects commit timing but not snapshot structure | None needed |
| Card 2 (M1 Condvar removal) | E4 admission | MEDIUM — admission controller reads M1 for tail evidence; polling changes read latency distribution | Tainted-sample detector from §4 of composition proof |
| Card 3 (M11 sharded registry) | E4 admission | LOW — admission sees session count, not registry internals | None needed |

---

## Summary: Implementation Priority

| Priority | Card | Primitive | Expected impact | Effort |
|----------|------|-----------|----------------|--------|
| 1 | Card 1 (M6) | Arc snapshot publication | Eliminate #1 Mutex bottleneck | Medium (new struct + ArcSwap + split begin/commit) |
| 2 | Card 3 (M11) | Sharded registry | Reduce c8+ session contention | Low (partition existing Mutex) |
| 3 | Card 2 (M1) | Condvar elimination | Reduce cross-node wake overhead | Low (delete + polling loop) |
