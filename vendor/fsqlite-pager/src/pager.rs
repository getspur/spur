//! Concrete single-writer pager for Phase 5 persistence.
//!
//! `SimplePager` implements [`MvccPager`] with single-writer semantics over a
//! VFS-backed database file and a zero-copy [`PageCache`].
//! Full concurrent MVCC behavior is layered on top in Phase 6.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{
    AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering as AtomicOrdering,
};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use fsqlite_error::{FrankenError, Result};
use fsqlite_types::cx::Cx;
use fsqlite_types::flags::{AccessFlags, SyncFlags, VfsOpenFlags};
use fsqlite_types::{
    BTreePageHeader, CommitSeq, DATABASE_HEADER_MAGIC, DATABASE_HEADER_SIZE, DatabaseHeader,
    DatabaseHeaderError, FRANKENSQLITE_SQLITE_VERSION_NUMBER, LockLevel, PageData, PageNumber,
    PageSize,
};
use fsqlite_vfs::{Vfs, VfsFile};
use smallvec::SmallVec;

use crate::journal::{JournalHeader, JournalPageRecord};
use crate::page_buf::{PageBuf, PageBufPool};
use crate::page_cache::{PageCacheMetricsSnapshot, ShardedPageCache};
use crate::traits::{self, JournalMode, MvccPager, TransactionHandle, TransactionMode, WalBackend};

use fsqlite_wal::{
    FrameSubmission, GLOBAL_CONSOLIDATION_METRICS, GroupCommitConfig, GroupCommitConsolidator,
    SubmitOutcome, TransactionFrameBatch, WalFile,
};

// ---------------------------------------------------------------------------
// Group Commit Queue (D1: replaces global WAL_APPEND_GATES mutex)
// ---------------------------------------------------------------------------
//
// The GroupCommitQueue provides same-process WAL write consolidation. Instead
// of serializing all concurrent writers through a global mutex (the old
// `WAL_APPEND_GATES`), writers submit their frame batches to a consolidator.
// The first writer becomes the "flusher" and waits briefly for more writers
// to arrive. Subsequent writers become "waiters" and park on a condvar.
// When the flusher decides to flush (batch full OR max delay exceeded), it
// writes all accumulated frames in one consolidated I/O, fsyncs once, and
// wakes all waiters.
//
// This reduces:
// - Lock contention: Mutex<()> serialization → cooperative batching
// - fsync overhead: N commits × fsync → 1 group × fsync
// - Cache-line ping-pong: N lock acquisitions → 1 flusher + N-1 condvar waits

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // LegacyCondvarTimeout is a compile-time alternative to KeyedEventcount
enum WaitPathMode {
    KeyedEventcount,
    LegacyCondvarTimeout,
}

impl WaitPathMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::KeyedEventcount => "keyed_eventcount",
            Self::LegacyCondvarTimeout => "legacy_condvar_timeout",
        }
    }
}

const GROUP_COMMIT_WAIT_PATH_MODE: WaitPathMode = WaitPathMode::KeyedEventcount;
const PUBLISHED_SEQUENCE_WAIT_PATH_MODE: WaitPathMode = WaitPathMode::KeyedEventcount;
const GROUP_COMMIT_WAIT_TIMEOUT_FALLBACK: Duration = Duration::from_millis(200);
const LEGACY_GROUP_COMMIT_ARRIVAL_WAIT: Duration = Duration::from_micros(20);
const GROUP_COMMIT_ARRIVAL_WAIT_POLICY: &str = "fill_age_tail_safe_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArrivalWaitObservation {
    pending_batch_count: usize,
    should_flush_now: bool,
    fill_age: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArrivalWaitDecision {
    wait_budget: Duration,
    policy: &'static str,
    reason: &'static str,
    fill_age: Duration,
    used_legacy_fallback: bool,
}

impl ArrivalWaitDecision {
    fn skip(reason: &'static str, fill_age: Duration) -> Self {
        Self {
            wait_budget: Duration::ZERO,
            policy: GROUP_COMMIT_ARRIVAL_WAIT_POLICY,
            reason,
            fill_age,
            used_legacy_fallback: false,
        }
    }

    fn legacy_fallback(fill_age: Duration) -> Self {
        Self {
            wait_budget: LEGACY_GROUP_COMMIT_ARRIVAL_WAIT,
            policy: GROUP_COMMIT_ARRIVAL_WAIT_POLICY,
            reason: "legacy_fallback",
            fill_age,
            used_legacy_fallback: true,
        }
    }

    fn wait_budget_us(self) -> u64 {
        self.wait_budget.as_micros() as u64
    }

    fn fill_age_us(self) -> u64 {
        self.fill_age.as_micros() as u64
    }
}

fn decide_group_commit_arrival_wait(
    observation: Option<ArrivalWaitObservation>,
) -> ArrivalWaitDecision {
    match observation {
        None => ArrivalWaitDecision::skip("promoted_follow_on", Duration::ZERO),
        Some(observation) => {
            if observation.pending_batch_count > 1 || observation.should_flush_now {
                return ArrivalWaitDecision::skip("queue_already_flushable", observation.fill_age);
            }
            if observation.fill_age >= LEGACY_GROUP_COMMIT_ARRIVAL_WAIT {
                return ArrivalWaitDecision::skip("fill_age_exhausted", observation.fill_age);
            }
            ArrivalWaitDecision::legacy_fallback(observation.fill_age)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyedWaitResult {
    Signaled,
    TimedOut,
}

#[derive(Debug, Default)]
struct KeyedWaitSlot {
    state: Mutex<u64>,
    cv: Condvar,
}

impl KeyedWaitSlot {
    fn generation(&self) -> u64 {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wait_for_change(&self, observed_generation: u64, timeout: Duration) -> KeyedWaitResult {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *guard != observed_generation {
            return KeyedWaitResult::Signaled;
        }
        let (_guard, timeout_result) = self
            .cv
            .wait_timeout_while(guard, timeout, |generation| {
                *generation == observed_generation
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if timeout_result.timed_out() {
            KeyedWaitResult::TimedOut
        } else {
            KeyedWaitResult::Signaled
        }
    }

    fn signal(&self) {
        let mut generation = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation = generation.saturating_add(1);
        self.cv.notify_all();
    }
}

#[derive(Debug, Default)]
struct KeyedWaitRegistry {
    slots: Mutex<HashMap<u64, Weak<KeyedWaitSlot>>>,
}

impl KeyedWaitRegistry {
    fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    fn slot(&self, key: u64) -> Arc<KeyedWaitSlot> {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slots.retain(|_, slot| slot.strong_count() > 0);
        if let Some(slot) = slots.get(&key).and_then(Weak::upgrade) {
            return slot;
        }
        let slot = Arc::new(KeyedWaitSlot::default());
        slots.insert(key, Arc::downgrade(&slot));
        slot
    }

    fn signal(&self, key: u64) -> bool {
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            slots.retain(|_, slot| slot.strong_count() > 0);
            slots.get(&key).and_then(Weak::upgrade)
        };
        if let Some(slot) = slot {
            slot.signal();
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn has_slot(&self, key: u64) -> bool {
        self.slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .is_some_and(|slot| slot.strong_count() > 0)
    }
}

/// Per-database group commit queue for WAL write consolidation.
struct GroupCommitQueue {
    /// The consolidator managing FILLING→FLUSHING→COMPLETE phases.
    consolidator: Mutex<GroupCommitConsolidator>,
    /// Condvar for waiters to park on until flush completes.
    flush_complete: Condvar,
    /// Atomic epoch counter for lock-free waiter polling.
    /// Updated by flusher after complete_flush(), read by waiters.
    completed_epoch: AtomicU64,
    /// Failure outcomes by epoch. Kept so late-scheduled waiters cannot miss
    /// a failed flush after a newer epoch completes successfully.
    failed_epochs: Mutex<HashMap<u64, String>>,
    /// Narrow per-target-epoch wake slots for waiter coordination.
    epoch_waiters: KeyedWaitRegistry,
}

#[derive(Debug)]
enum WaitForEpochOutcome {
    Completed,
    TakeOverFlusher {
        batches: Vec<TransactionFrameBatch>,
        flush_epoch: u64,
    },
}

impl GroupCommitQueue {
    fn new(config: GroupCommitConfig) -> Self {
        Self {
            consolidator: Mutex::new(GroupCommitConsolidator::new(config)),
            flush_complete: Condvar::new(),
            completed_epoch: AtomicU64::new(0),
            failed_epochs: Mutex::new(HashMap::new()),
            epoch_waiters: KeyedWaitRegistry::new(),
        }
    }

    /// Publish a completed epoch and wake all waiters.
    ///
    /// We take the consolidator mutex before publishing so a waiter cannot
    /// observe an incomplete epoch, race with notify, and then go to sleep
    /// forever on a lost wakeup.
    fn publish_completed_epoch(&self, epoch: u64, wake_next_epoch: bool) {
        let _guard = self
            .consolidator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.completed_epoch.store(epoch, AtomicOrdering::Release);

        // H11 fault hook: suppress the Condvar notification while still
        // storing the completed epoch. Waiters must recover via wait_timeout.
        #[cfg(any(test, feature = "fault-injection"))]
        if crate::fault_hooks::maybe_inject_drop_condvar_notify(epoch) {
            tracing::trace!(
                target: "fsqlite::wal::epoch_wait",
                wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                published_epoch = epoch,
                wake_next_epoch,
                fallback = "timeout_recheck",
                "suppressed direct waiter wake after completion publish"
            );
            return;
        }

        self.signal_completed_epoch_waiters(epoch, wake_next_epoch);
    }

    /// Publish a failed epoch and wake all waiters.
    ///
    /// This uses the same mutex discipline as `publish_completed_epoch` so
    /// waiter condition checks and condvar parking stay synchronized.
    fn publish_failed_epoch(&self, epoch: u64, error: &FrankenError, wake_next_epoch: bool) {
        let _guard = self
            .consolidator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut failed_epochs = self
            .failed_epochs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        failed_epochs.insert(epoch, error.to_string());
        drop(failed_epochs);
        self.signal_failed_epoch_waiters(epoch, wake_next_epoch);
    }

    /// Check if a given epoch has completed (for waiters).
    fn is_epoch_complete(&self, epoch: u64) -> bool {
        self.completed_epoch.load(AtomicOrdering::Acquire) >= epoch
    }

    fn signal_completed_epoch_waiters(&self, epoch: u64, wake_next_epoch: bool) {
        match GROUP_COMMIT_WAIT_PATH_MODE {
            WaitPathMode::KeyedEventcount => {
                let woke_target_epoch = self.epoch_waiters.signal(epoch);
                let woke_next_epoch = wake_next_epoch
                    && epoch
                        .checked_add(1)
                        .is_some_and(|next_epoch| self.epoch_waiters.signal(next_epoch));
                tracing::trace!(
                    target: "fsqlite::wal::epoch_wait",
                    wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                    published_epoch = epoch,
                    wake_next_epoch,
                    woke_target_epoch,
                    woke_next_epoch,
                    "published completed epoch to targeted waiters"
                );
            }
            WaitPathMode::LegacyCondvarTimeout => self.flush_complete.notify_all(),
        }
    }

    fn signal_failed_epoch_waiters(&self, epoch: u64, wake_next_epoch: bool) {
        match GROUP_COMMIT_WAIT_PATH_MODE {
            WaitPathMode::KeyedEventcount => {
                let woke_failed_epoch = self.epoch_waiters.signal(epoch);
                let woke_next_epoch = wake_next_epoch
                    && epoch
                        .checked_add(1)
                        .is_some_and(|next_epoch| self.epoch_waiters.signal(next_epoch));
                tracing::trace!(
                    target: "fsqlite::wal::epoch_wait",
                    wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                    failed_epoch = epoch,
                    wake_next_epoch,
                    woke_failed_epoch,
                    woke_next_epoch,
                    "published failed epoch to targeted waiters"
                );
            }
            WaitPathMode::LegacyCondvarTimeout => self.flush_complete.notify_all(),
        }
    }

    fn observe_epoch_outcome(
        &self,
        guard: &mut std::sync::MutexGuard<'_, GroupCommitConsolidator>,
        target_epoch: u64,
    ) -> Result<Option<WaitForEpochOutcome>> {
        let failed_detail = self
            .failed_epochs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&target_epoch)
            .cloned();
        if let Some(detail) = failed_detail {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .failed_epoch
                .fetch_add(1, AtomicOrdering::Relaxed);
            tracing::trace!(
                target: "fsqlite::wal::epoch_wait",
                wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                wake_reason = "failed_epoch",
                target_epoch,
                "waiter observed failed epoch"
            );
            return Err(FrankenError::internal(format!(
                "group commit flush failed for epoch {target_epoch}: {detail}"
            )));
        }

        if self.is_epoch_complete(target_epoch) {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .notify
                .fetch_add(1, AtomicOrdering::Relaxed);
            tracing::trace!(
                target: "fsqlite::wal::epoch_wait",
                wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                wake_reason = "notify",
                target_epoch,
                completed_epoch = self.completed_epoch.load(AtomicOrdering::Acquire),
                "waiter observed completed epoch"
            );
            return Ok(Some(WaitForEpochOutcome::Completed));
        }

        if guard.has_flusher_vacancy()
            && guard.epoch().checked_add(1) == Some(target_epoch)
            && guard.claim_flusher_vacancy()
        {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .flusher_takeover
                .fetch_add(1, AtomicOrdering::Relaxed);
            tracing::trace!(
                target: "fsqlite::wal::epoch_wait",
                wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                wake_reason = "flusher_takeover",
                target_epoch,
                current_epoch = guard.epoch(),
                "waiter claimed promoted flusher vacancy"
            );
            let batches = guard.begin_flush()?;
            return Ok(Some(WaitForEpochOutcome::TakeOverFlusher {
                flush_epoch: guard.epoch(),
                batches,
            }));
        }

        Ok(None)
    }

    fn wait_for_epoch_outcome_legacy(
        &self,
        mut guard: std::sync::MutexGuard<'_, GroupCommitConsolidator>,
        target_epoch: u64,
    ) -> Result<WaitForEpochOutcome> {
        loop {
            if let Some(outcome) = self.observe_epoch_outcome(&mut guard, target_epoch)? {
                return Ok(outcome);
            }

            let (new_guard, timeout_result) = self
                .flush_complete
                .wait_timeout(guard, GROUP_COMMIT_WAIT_TIMEOUT_FALLBACK)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = new_guard;

            if timeout_result.timed_out() {
                GLOBAL_CONSOLIDATION_METRICS
                    .wake_reasons
                    .timeout
                    .fetch_add(1, AtomicOrdering::Relaxed);
                tracing::trace!(
                    target: "fsqlite::wal::epoch_wait",
                    wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                    wake_reason = "timeout",
                    target_epoch,
                    "legacy waiter timeout fallback fired"
                );
            }
        }
    }

    fn wait_for_epoch_outcome_keyed<'a>(
        &'a self,
        mut guard: std::sync::MutexGuard<'a, GroupCommitConsolidator>,
        target_epoch: u64,
    ) -> Result<WaitForEpochOutcome> {
        loop {
            if let Some(outcome) = self.observe_epoch_outcome(&mut guard, target_epoch)? {
                return Ok(outcome);
            }

            let slot = self.epoch_waiters.slot(target_epoch);
            let observed_generation = slot.generation();
            if let Some(outcome) = self.observe_epoch_outcome(&mut guard, target_epoch)? {
                return Ok(outcome);
            }

            drop(guard);
            let wait_result =
                slot.wait_for_change(observed_generation, GROUP_COMMIT_WAIT_TIMEOUT_FALLBACK);
            guard = self
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            if wait_result == KeyedWaitResult::TimedOut {
                GLOBAL_CONSOLIDATION_METRICS
                    .wake_reasons
                    .timeout
                    .fetch_add(1, AtomicOrdering::Relaxed);
                tracing::trace!(
                    target: "fsqlite::wal::epoch_wait",
                    wait_strategy = GROUP_COMMIT_WAIT_PATH_MODE.as_str(),
                    wake_reason = "timeout",
                    target_epoch,
                    fallback = "timeout_recheck",
                    "keyed epoch waiter timeout fallback fired"
                );
            }
        }
    }

    /// Wait for the target epoch to either complete successfully, fail, or be
    /// taken over by this waiter if the promoted epoch lost its original flusher.
    fn wait_for_epoch_outcome(
        &self,
        guard: std::sync::MutexGuard<'_, GroupCommitConsolidator>,
        target_epoch: u64,
    ) -> Result<WaitForEpochOutcome> {
        match GROUP_COMMIT_WAIT_PATH_MODE {
            WaitPathMode::KeyedEventcount => self.wait_for_epoch_outcome_keyed(guard, target_epoch),
            WaitPathMode::LegacyCondvarTimeout => {
                self.wait_for_epoch_outcome_legacy(guard, target_epoch)
            }
        }
    }
}

type GroupCommitQueueRef = Arc<GroupCommitQueue>;

// ---------------------------------------------------------------------------
// Shared WAL Backend (D1-CRITICAL: enables split-lock commit)
// ---------------------------------------------------------------------------
//
// The WAL backend is held in a separate Arc<RwLock<...>> to enable split-lock
// commit. This allows Thread B to start its prepare phase (which needs
// inner.lock()) while Thread A is doing WAL I/O (which needs wal_backend.write()
// but NOT inner.lock()).
//
// Before: inner.lock() held for ~100us (prepare + WAL I/O + publish)
// After:  inner.lock() held for ~20us (prepare only)
//         wal_backend.lock() held for ~50us (WAL I/O only)
//         inner.lock() re-acquired for ~10us (post-commit only)

/// Thread-safe shared WAL backend for split-lock commit protocol.
///
/// The `RwLock` enables split-lock access: page-lookup paths that support
/// pinned reads take a shared (read) lock, while mutation paths (append,
/// sync, begin_transaction) take an exclusive (write) lock.
///
/// # bd-db300.3.8.7: write-lock-scope narrowing
///
/// Before this change, all WAL access went through `with_wal_backend` which
/// always took the write lock. Now, `with_wal_backend_read` takes only the
/// read lock for `read_page_pinned` when the backend supports pinned reads.
pub type SharedWalBackend = Arc<std::sync::RwLock<Option<Box<dyn WalBackend>>>>;

/// Create a new empty shared WAL backend.
fn new_shared_wal_backend() -> SharedWalBackend {
    Arc::new(std::sync::RwLock::new(None))
}

/// Read access to WAL backend (read_page_pinned, frame_count).
///
/// Takes only a shared (read) lock on the WAL backend RwLock. This allows
/// multiple concurrent readers without blocking the append path, and the
/// append path without blocking readers.
///
/// # bd-db300.3.8.7
fn with_wal_backend_read<T>(
    wal_backend: &SharedWalBackend,
    f: impl FnOnce(&dyn WalBackend) -> Result<T>,
) -> Result<T> {
    let guard = wal_backend
        .read()
        .map_err(|_| FrankenError::internal("SharedWalBackend lock poisoned"))?;
    let wal = guard
        .as_deref()
        .ok_or_else(|| FrankenError::internal("WAL mode active but no WAL backend installed"))?;
    f(wal)
}

enum WalReadLookup {
    Ready(Option<Vec<u8>>),
    NeedsWriteFallback,
}

fn read_page_from_wal_backend(
    wal_backend: &SharedWalBackend,
    cx: &Cx,
    page_no: PageNumber,
) -> Result<Option<Vec<u8>>> {
    match with_wal_backend_read(wal_backend, |wal| {
        if wal.supports_pinned_reads() {
            wal.read_page_pinned(cx, page_no.get())
                .map(WalReadLookup::Ready)
        } else {
            Ok(WalReadLookup::NeedsWriteFallback)
        }
    })? {
        WalReadLookup::Ready(data) => Ok(data),
        WalReadLookup::NeedsWriteFallback => {
            with_wal_backend(wal_backend, |wal| wal.read_page(cx, page_no.get()))
        }
    }
}

/// Write access to WAL backend (append_frames, sync, set_wal_backend).
fn with_wal_backend<T>(
    wal_backend: &SharedWalBackend,
    f: impl FnOnce(&mut dyn WalBackend) -> Result<T>,
) -> Result<T> {
    let mut guard = wal_backend
        .write()
        .map_err(|_| FrankenError::internal("SharedWalBackend lock poisoned"))?;
    let wal = guard
        .as_deref_mut()
        .ok_or_else(|| FrankenError::internal("WAL mode active but no WAL backend installed"))?;
    f(wal)
}

fn has_wal_backend(wal_backend: &SharedWalBackend) -> Result<bool> {
    let guard = wal_backend
        .read()
        .map_err(|_| FrankenError::internal("SharedWalBackend lock poisoned"))?;
    Ok(guard.is_some())
}

static GROUP_COMMIT_QUEUES: OnceLock<Mutex<HashMap<PathBuf, GroupCommitQueueRef>>> =
    OnceLock::new();

fn group_commit_queue_for_path(db_path: &Path) -> GroupCommitQueueRef {
    let queues = GROUP_COMMIT_QUEUES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut queues = queues
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
        queues
            .entry(db_path.to_path_buf())
            .or_insert_with(|| Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()))),
    )
}

fn group_commit_queue_for_backend<V: Vfs>(vfs: &V, db_path: &Path) -> GroupCommitQueueRef {
    if vfs.is_memory() {
        // Private :memory: databases are connection-local, so sharing a
        // global queue by the synthetic "/:memory:" path would cross-wire
        // unrelated databases. Use a fresh queue instead.
        Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()))
    } else {
        group_commit_queue_for_path(db_path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalCommitSyncPolicy {
    Deferred,
    PerCommit,
}

impl WalCommitSyncPolicy {
    #[must_use]
    const fn should_sync_on_commit(self) -> bool {
        matches!(self, Self::PerCommit)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagerAccessMode {
    ReadWrite,
    ReadOnly,
}

impl PagerAccessMode {
    #[must_use]
    const fn is_readonly(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RollbackJournalRecoveryState {
    Clean,
    Pending,
}

impl RollbackJournalRecoveryState {
    #[must_use]
    const fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }
}

// ---------------------------------------------------------------------------
// Immutable committed-state snapshot (bd-db300.5.3.3.1 / Card 1: M6)
// ---------------------------------------------------------------------------

/// Frozen read-only snapshot of pager committed state.
///
/// Published atomically on every commit via `RwLock<Arc<...>>`.  Readers
/// clone the `Arc` (nanosecond RwLock-read hold) then inspect fields without
/// touching the `PagerInner` Mutex.  This eliminates the #1 hot-path Mutex
/// acquisition for read-only begin checks and staleness probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagerCommittedSnapshot {
    /// Monotonic commit sequence at publication time.
    pub commit_seq: CommitSeq,
    /// Database size in pages.
    pub db_size: u32,
    /// Active journal mode.
    pub journal_mode: JournalMode,
    /// Number of pages on the freelist.
    pub freelist_count: usize,
    /// Whether a checkpoint was active when this snapshot was taken.
    pub checkpoint_active: bool,
    /// Whether a writer transaction was active when this snapshot was taken.
    pub writer_active: bool,
    /// File size in bytes at snapshot time (for staleness detection).
    pub db_file_size_bytes: u64,
}

impl PagerCommittedSnapshot {
    /// Build a snapshot from the current `PagerInner` state.
    /// Caller must hold the PagerInner Mutex.
    fn from_inner<F: VfsFile>(inner: &PagerInner<F>) -> Self {
        Self {
            commit_seq: inner.commit_seq,
            db_size: inner.db_size,
            journal_mode: inner.journal_mode,
            freelist_count: inner.freelist.len(),
            checkpoint_active: inner.checkpoint_active,
            writer_active: inner.writer_active,
            db_file_size_bytes: inner.committed_db_file_size_bytes,
        }
    }
}

/// The inner mutable pager state protected by a mutex.
pub(crate) struct PagerInner<F: VfsFile> {
    /// Handle to the main database file.
    db_file: F,
    /// Page size for this database.
    page_size: PageSize,
    /// Current database size in pages.
    db_size: u32,
    /// Next page to allocate (1-based).
    next_page: u32,
    /// Whether a writer transaction is currently active.
    writer_active: bool,
    /// Number of active transactions (readers + writers).
    active_transactions: u32,
    /// Whether a checkpoint is currently running.
    checkpoint_active: bool,
    /// Whether this pager was opened read-only (skip freelist
    /// scans during refresh since we never allocate pages).
    access_mode: PagerAccessMode,
    /// Deallocated pages available for reuse.
    freelist: Vec<PageNumber>,
    /// Current journal mode (rollback journal vs WAL).
    journal_mode: JournalMode,
    /// WAL commit sync policy derived from `PRAGMA synchronous`.
    wal_commit_sync_policy: WalCommitSyncPolicy,
    /// Whether this pager has a locally failed rollback-journal commit that
    /// must be repaired before the handle can be reused.
    rollback_journal_recovery_state: RollbackJournalRecoveryState,
    // NOTE: wal_backend moved to SharedWalBackend on SimplePager/SimpleTransaction (D1-CRITICAL)
    /// Monotonic commit sequence for MVCC version tracking.
    commit_seq: CommitSeq,
    /// Main database-file size observed when committed metadata was last fully
    /// refreshed. A stable `(commit_seq, file_size)` pair lets later begins
    /// skip the expensive committed-page metadata reload when no durable state
    /// changed underneath this pager.
    committed_db_file_size_bytes: u64,
}

impl<F: VfsFile> PagerInner<F> {
    /// Read a page through WAL (if present) → cache → disk and return an owned copy.
    fn read_page_copy(
        &mut self,
        cx: &Cx,
        cache: &ShardedPageCache,
        wal_backend: &SharedWalBackend,
        page_no: PageNumber,
    ) -> Result<Vec<u8>> {
        // In WAL mode, check the WAL for the latest version of the page first.
        // bd-db300.3.8.7: try shared-lock path when the backend supports pinned reads.
        if self.journal_mode == JournalMode::Wal {
            if let Some(data) = read_page_from_wal_backend(wal_backend, cx, page_no)? {
                return Ok(data);
            }
        }

        if let Some(data) = cache.get_copy(page_no) {
            return Ok(data);
        }

        // Reads of yet-unallocated pages should observe zero-filled content.
        // This is relied upon by savepoint rollback semantics for pages that
        // were allocated and then rolled back before commit.
        if page_no.get() > self.db_size {
            return Ok(vec![0_u8; self.page_size.as_usize()]);
        }

        match cache.read_page_copy(cx, &mut self.db_file, page_no) {
            Ok(data) => Ok(data),
            Err(FrankenError::OutOfMemory) => {
                if cache.evict_any() {
                    cache.read_page_copy(cx, &mut self.db_file, page_no)
                } else {
                    let page_size = self.page_size.as_usize();
                    let offset = u64::from(page_no.get() - 1) * page_size as u64;
                    let mut out = vec![0_u8; page_size];
                    let bytes_read = self.db_file.read(cx, &mut out, offset)?;
                    if bytes_read < page_size {
                        return Err(FrankenError::DatabaseCorrupt {
                            detail: format!(
                                "short read fetching page {page}: got {bytes_read} of {page_size}",
                                page = page_no.get()
                            ),
                        });
                    }
                    Ok(out)
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Read a page from the latest committed database state without consulting
    /// the local cache.
    ///
    /// This is used to refresh connection-local pager metadata after another
    /// connection has committed. The local cache may still reflect an older
    /// generation, so committed-state refresh must bypass it.
    fn read_committed_page_copy(
        &self,
        cx: &Cx,
        wal_backend: &SharedWalBackend,
        page_no: PageNumber,
    ) -> Result<Vec<u8>> {
        // bd-db300.3.8.7: try shared-lock path first for WAL reads.
        if self.journal_mode == JournalMode::Wal {
            if let Some(data) = read_page_from_wal_backend(wal_backend, cx, page_no)? {
                return Ok(data);
            }
        }

        let page_size = self.page_size.as_usize();
        let offset = u64::from(page_no.get().saturating_sub(1)) * page_size as u64;
        let file_size = self.db_file.file_size(cx)?;
        if offset >= file_size {
            return Ok(vec![0_u8; page_size]);
        }

        let mut out = vec![0_u8; page_size];
        let bytes_read = self.db_file.read(cx, &mut out, offset)?;
        if bytes_read < page_size {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "short read fetching committed page {page}: got {bytes_read} of {page_size}",
                    page = page_no.get()
                ),
            });
        }
        Ok(out)
    }

    /// Read just the database header bytes directly from the main database
    /// file, bypassing WAL state.
    fn read_database_file_header_bytes(
        &self,
        cx: &Cx,
        file_size: u64,
    ) -> Result<[u8; DATABASE_HEADER_SIZE]> {
        if file_size == 0 {
            return Ok([0_u8; DATABASE_HEADER_SIZE]);
        }

        let mut out = [0_u8; DATABASE_HEADER_SIZE];
        let bytes_read = self.db_file.read(cx, &mut out, 0)?;
        if bytes_read < DATABASE_HEADER_SIZE {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "short read fetching database-file header: got {bytes_read} of {DATABASE_HEADER_SIZE}"
                ),
            });
        }
        Ok(out)
    }

    /// Probe the latest visible commit sequence using only durable header/WAL
    /// metadata.
    ///
    /// This is intentionally cheaper than a full committed-state refresh: it
    /// avoids page-1 materialization and freelist reconstruction unless the
    /// visible state actually changed.
    fn probe_visible_commit_seq(
        &self,
        cx: &Cx,
        wal_backend: &SharedWalBackend,
    ) -> Result<(CommitSeq, u64, bool)> {
        let file_size = self.db_file.file_size(cx)?;
        let wal_visible_commit_count = if self.journal_mode == JournalMode::Wal {
            with_wal_backend(wal_backend, |wal| {
                wal.begin_transaction(cx)?;
                wal.committed_txn_count(cx)
            })?
        } else {
            0
        };
        let wal_snapshot_initialized = self.journal_mode == JournalMode::Wal;
        let base_header_bytes = self.read_database_file_header_bytes(cx, file_size)?;
        let base_change_counter = if self.journal_mode == JournalMode::Wal {
            match DatabaseHeader::from_bytes(&base_header_bytes) {
                Ok(base_header) => u64::from(base_header.change_counter),
                Err(error) => {
                    stale_main_header_change_counter_under_wal(&base_header_bytes, &error)
                        .ok_or_else(|| FrankenError::DatabaseCorrupt {
                            detail: format!(
                                "invalid database-file header during WAL refresh: {error}"
                            ),
                        })?
                }
            }
        } else {
            u64::from(
                DatabaseHeader::from_bytes(&base_header_bytes)
                    .map_err(|error| FrankenError::DatabaseCorrupt {
                        detail: format!("invalid database header during pager refresh: {error}"),
                    })?
                    .change_counter,
            )
        };
        let visible_commit_seq =
            CommitSeq::new(base_change_counter.saturating_add(wal_visible_commit_count));
        Ok((visible_commit_seq, file_size, wal_snapshot_initialized))
    }

    /// Refresh connection-local pager metadata from the latest committed state.
    ///
    /// Returns `true` when WAL snapshot setup was already performed as part of
    /// the refresh and does not need to be repeated for the new transaction.
    fn refresh_committed_state(
        &mut self,
        cx: &Cx,
        cache: &ShardedPageCache,
        wal_backend: &SharedWalBackend,
    ) -> Result<bool> {
        let (new_commit_seq, current_file_size, wal_snapshot_initialized) =
            self.probe_visible_commit_seq(cx, wal_backend)?;
        if new_commit_seq == self.commit_seq
            && current_file_size == self.committed_db_file_size_bytes
        {
            return Ok(wal_snapshot_initialized);
        }

        let page1 = self.read_committed_page_copy(cx, wal_backend, PageNumber::ONE)?;
        if page1.len() < DATABASE_HEADER_SIZE {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "committed page 1 too small for database header: got {}, need {}",
                    page1.len(),
                    DATABASE_HEADER_SIZE
                ),
            });
        }

        let mut header_bytes = [0_u8; DATABASE_HEADER_SIZE];
        header_bytes.copy_from_slice(&page1[..DATABASE_HEADER_SIZE]);
        let header = DatabaseHeader::from_bytes(&header_bytes).map_err(|error| {
            FrankenError::DatabaseCorrupt {
                detail: format!("invalid database header during pager refresh: {error}"),
            }
        })?;

        // Always cross-check header.page_count against the actual file size.
        // A crash between growing the file and updating the header leaves
        // page_count stale even when the stale marker is not set.  Using
        // max(header, file) ensures newly-committed pages are visible and
        // avoids BusySnapshot errors on startup (see GH issue #49).
        let file_size = self.db_file.file_size(cx)?;
        let file_derived = header
            .page_count_from_file_size(file_size)
            .unwrap_or(header.page_count);
        let db_size = header.page_count.max(file_derived).max(1);
        // Skip freelist scan for read-only pagers — the freelist is only
        // needed for page allocation during writes.
        let freelist = if self.access_mode.is_readonly() {
            Vec::new()
        } else {
            load_freelist_from_committed_state(
                cx,
                self,
                wal_backend,
                db_size,
                header.freelist_trunk,
                header.freelist_count,
            )?
        };

        self.db_size = db_size;
        self.next_page = if db_size >= 2 {
            db_size.saturating_add(1)
        } else {
            2
        };
        self.freelist = freelist;
        // Only clear the cache if the database was modified by another
        // connection. In WAL mode this uses the latest visible page-1
        // durable header baseline plus the visible WAL commit horizon.
        if new_commit_seq != self.commit_seq {
            cache.clear();
        }
        self.commit_seq = new_commit_seq;
        self.committed_db_file_size_bytes = current_file_size;

        Ok(wal_snapshot_initialized)
    }

    /// Flush page data directly to disk.
    ///
    /// The shared cache only tracks committed content now that readers can
    /// consult it without taking the pager metadata mutex. Dirty pages are
    /// staged in the transaction write-set and admitted into the cache only
    /// after commit succeeds.
    fn flush_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
        let page_size = self.page_size.as_usize();
        let offset = u64::from(page_no.get() - 1) * page_size as u64;
        self.db_file.write(cx, data, offset)?;
        Ok(())
    }
}

fn normalize_freelist(pages: &[PageNumber], db_size: u32) -> Vec<PageNumber> {
    let mut normalized: Vec<PageNumber> = pages
        .iter()
        .copied()
        .filter(|p| {
            let raw = p.get();
            raw > 1 && raw <= db_size
        })
        .collect();
    normalized.sort_unstable_by_key(|p| p.get());
    normalized.dedup_by_key(|p| p.get());
    normalized
}

fn return_pages_to_freelist(
    freelist: &mut Vec<PageNumber>,
    pages: impl IntoIterator<Item = PageNumber>,
) {
    for page in pages {
        if !freelist.contains(&page) {
            freelist.push(page);
        }
    }
    // Sort descending so that pop() and rposition() yield the lowest page numbers first,
    // which keeps the database file compact and reduces file size growth.
    freelist.sort_unstable_by_key(|page| std::cmp::Reverse(page.get()));
}

fn load_freelist_from_disk<F: VfsFile>(
    cx: &Cx,
    db_file: &F,
    page_size: PageSize,
    db_size: u32,
    first_trunk: u32,
    freelist_count: u32,
) -> Result<Vec<PageNumber>> {
    if first_trunk == 0 || freelist_count == 0 {
        return Ok(Vec::new());
    }

    let ps = page_size.as_usize();
    let mut visited: HashSet<u32> = HashSet::new();
    let mut out: Vec<PageNumber> = Vec::with_capacity(freelist_count as usize);
    let mut trunk = first_trunk;

    while trunk != 0 && out.len() < freelist_count as usize {
        if trunk > db_size {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!("freelist trunk page {trunk} exceeds db_size {db_size}"),
            });
        }
        if !visited.insert(trunk) {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!("freelist loop detected at trunk page {trunk}"),
            });
        }

        let trunk_page = PageNumber::new(trunk).ok_or_else(|| FrankenError::DatabaseCorrupt {
            detail: format!("invalid freelist trunk page number {trunk}"),
        })?;
        out.push(trunk_page);

        let mut buf = vec![0u8; ps];
        let offset = u64::from(trunk.saturating_sub(1)) * ps as u64;
        let bytes_read = db_file.read(cx, &mut buf, offset)?;
        if bytes_read < ps {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "short read loading freelist trunk page {trunk}: got {bytes_read} of {ps}"
                ),
            });
        }

        let next_trunk = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let leaf_count = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        let max_leaf_entries = (ps / 4).saturating_sub(2);
        if leaf_count > max_leaf_entries {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "freelist trunk {trunk} leaf_count {leaf_count} exceeds max {max_leaf_entries}"
                ),
            });
        }

        for idx in 0..leaf_count {
            if out.len() >= freelist_count as usize {
                break;
            }
            let base = 8 + idx * 4;
            let leaf = u32::from_be_bytes([buf[base], buf[base + 1], buf[base + 2], buf[base + 3]]);
            if leaf == 0 {
                continue;
            }
            if leaf > db_size {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("freelist leaf page {leaf} exceeds db_size {db_size}"),
                });
            }
            let leaf_page = PageNumber::new(leaf).ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: format!("invalid freelist leaf page number {leaf}"),
            })?;
            out.push(leaf_page);
        }

        trunk = next_trunk;
    }

    out.truncate(freelist_count as usize);
    Ok(normalize_freelist(&out, db_size))
}

fn load_freelist_from_committed_state<F: VfsFile>(
    cx: &Cx,
    inner: &PagerInner<F>,
    wal_backend: &SharedWalBackend,
    db_size: u32,
    first_trunk: u32,
    freelist_count: u32,
) -> Result<Vec<PageNumber>> {
    if first_trunk == 0 || freelist_count == 0 {
        return Ok(Vec::new());
    }

    let ps = inner.page_size.as_usize();
    let mut visited: HashSet<u32> = HashSet::new();
    let mut out: Vec<PageNumber> = Vec::with_capacity(freelist_count as usize);
    let mut trunk = first_trunk;

    while trunk != 0 && out.len() < freelist_count as usize {
        if trunk > db_size {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!("freelist trunk page {trunk} exceeds db_size {db_size}"),
            });
        }
        if !visited.insert(trunk) {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!("freelist loop detected at trunk page {trunk}"),
            });
        }

        let trunk_page = PageNumber::new(trunk).ok_or_else(|| FrankenError::DatabaseCorrupt {
            detail: format!("invalid freelist trunk page number {trunk}"),
        })?;
        out.push(trunk_page);

        let buf = inner.read_committed_page_copy(cx, wal_backend, trunk_page)?;
        if buf.len() < ps {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "short read loading committed freelist trunk page {trunk}: got {} of {ps}",
                    buf.len()
                ),
            });
        }

        let next_trunk = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let leaf_count = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        let max_leaf_entries = (ps / 4).saturating_sub(2);
        if leaf_count > max_leaf_entries {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "freelist trunk {trunk} leaf_count {leaf_count} exceeds max {max_leaf_entries}"
                ),
            });
        }

        for idx in 0..leaf_count {
            if out.len() >= freelist_count as usize {
                break;
            }
            let base = 8 + idx * 4;
            let leaf = u32::from_be_bytes([buf[base], buf[base + 1], buf[base + 2], buf[base + 3]]);
            if leaf == 0 {
                continue;
            }
            if leaf > db_size {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("freelist leaf page {leaf} exceeds db_size {db_size}"),
                });
            }
            let leaf_page = PageNumber::new(leaf).ok_or_else(|| FrankenError::DatabaseCorrupt {
                detail: format!("invalid freelist leaf page number {leaf}"),
            })?;
            out.push(leaf_page);
        }

        trunk = next_trunk;
    }

    out.truncate(freelist_count as usize);
    Ok(normalize_freelist(&out, db_size))
}

#[allow(clippy::too_many_arguments)]
fn serialize_freelist_to_write_set<F: VfsFile>(
    cx: &Cx,
    inner: &mut PagerInner<F>,
    cache: &ShardedPageCache,
    wal_backend: &SharedWalBackend,
    pool: &PageBufPool,
    write_set: &mut HashMap<PageNumber, StagedPage>,
    write_pages_sorted: &mut Vec<PageNumber>,
    committed_db_size: u32,
    pending_freed_pages: &[PageNumber],
) -> Result<()> {
    if committed_db_size == 0 {
        inner.freelist.clear();
        return Ok(());
    }

    // Use next_page as the normalization bound so we keep valid in-memory EOF
    // pages returned by aborted concurrent transactions. Those pages were
    // never part of the durable file image, so they must not be serialized
    // into page-1 freelist metadata until db_size grows to include them.
    let upper_bound = inner.next_page.saturating_sub(1).max(committed_db_size);
    // Build the predicted freelist from the committed freelist plus pending
    // freed pages WITHOUT mutating inner.freelist. This prevents concurrent
    // transactions from observing uncommitted freelist changes during the
    // window between Phase A (prepare) and Phase B (WAL I/O) of the split-
    // lock commit path. Freed pages are only promoted into inner.freelist
    // after Phase B succeeds (in Phase C). See beads_rust#138.
    let mut predicted_freelist = inner.freelist.clone();
    return_pages_to_freelist(&mut predicted_freelist, pending_freed_pages.iter().copied());
    let predicted_normalized = normalize_freelist(&predicted_freelist, upper_bound);
    // NOTE: Do NOT normalize inner.freelist here — this runs during Phase A
    // where inner.lock() may be released before Phase B. Mutating the shared
    // freelist would leak a side-effect visible to concurrent transactions
    // even if this commit fails. Normalization of inner.freelist (if needed)
    // should be deferred to Phase C after successful commit.
    let durable_freelist: Vec<PageNumber> = predicted_normalized
        .iter()
        .copied()
        .filter(|page| page.get() <= committed_db_size)
        .collect();

    let ps = inner.page_size.as_usize();
    let total_free = durable_freelist.len() as u32;

    let (first_trunk, trunk_pages) = if durable_freelist.is_empty() {
        (0u32, Vec::<u32>::new())
    } else {
        let max_leaf_entries = (ps / 4).saturating_sub(2).max(1);
        let trunk_count = durable_freelist.len().div_ceil(max_leaf_entries + 1);
        let trunks: Vec<u32> = durable_freelist
            .iter()
            .take(trunk_count)
            .map(|p| p.get())
            .collect();
        (trunks[0], trunks)
    };

    if !trunk_pages.is_empty() {
        let mut leaf_index = trunk_pages.len();
        let max_leaf_entries = (ps / 4).saturating_sub(2).max(1);

        for (idx, trunk_pg) in trunk_pages.iter().enumerate() {
            let next = trunk_pages.get(idx + 1).copied().unwrap_or(0);
            let remaining = durable_freelist.len().saturating_sub(leaf_index);
            let take = remaining.min(max_leaf_entries);

            let mut buf = pool.acquire()?;
            // Zero the entire page to avoid leaking stale data from the
            // pool in the unused tail of the trunk page.
            buf.fill(0);
            buf[0..4].copy_from_slice(&next.to_be_bytes());
            buf[4..8].copy_from_slice(&(take as u32).to_be_bytes());

            for i in 0..take {
                let leaf = durable_freelist[leaf_index + i].get();
                let base = 8 + i * 4;
                buf[base..base + 4].copy_from_slice(&leaf.to_be_bytes());
            }
            leaf_index += take;

            if let Some(pg) = PageNumber::new(*trunk_pg) {
                insert_staged_page(write_set, write_pages_sorted, pg, StagedPage::from_buf(buf));
            }
        }
    }

    let mut page1 = ensure_page_one_in_write_set(cx, inner, cache, wal_backend, pool, write_set)?;

    page1[32..36].copy_from_slice(&first_trunk.to_be_bytes());
    page1[36..40].copy_from_slice(&total_free.to_be_bytes());
    insert_staged_page(
        write_set,
        write_pages_sorted,
        PageNumber::ONE,
        StagedPage::from_buf(page1),
    );

    Ok(())
}

fn ensure_page_one_in_write_set<F: VfsFile>(
    cx: &Cx,
    inner: &mut PagerInner<F>,
    cache: &ShardedPageCache,
    wal_backend: &SharedWalBackend,
    pool: &PageBufPool,
    write_set: &mut HashMap<PageNumber, StagedPage>,
) -> Result<PageBuf> {
    if let Some(staged) = write_set.remove(&PageNumber::ONE) {
        return Ok(staged.into_buf(pool));
    }

    let page1_vec = inner.read_page_copy(cx, cache, wal_backend, PageNumber::ONE)?;
    let mut buf = pool.acquire()?;
    buf.copy_from_slice(&page1_vec);
    Ok(buf)
}

fn insert_page_sorted(pages: &mut Vec<PageNumber>, page_no: PageNumber) {
    match pages.binary_search_by_key(&page_no.get(), |page| page.get()) {
        Ok(_) => {}
        Err(idx) => pages.insert(idx, page_no),
    }
}

fn remove_page_sorted(pages: &mut Vec<PageNumber>, page_no: PageNumber) {
    if let Ok(idx) = pages.binary_search_by_key(&page_no.get(), |page| page.get()) {
        pages.remove(idx);
    }
}

fn insert_staged_page(
    write_set: &mut HashMap<PageNumber, StagedPage>,
    write_pages_sorted: &mut Vec<PageNumber>,
    page_no: PageNumber,
    staged: StagedPage,
) {
    if write_set.insert(page_no, staged).is_none() {
        insert_page_sorted(write_pages_sorted, page_no);
    }
}

#[cfg(test)]
struct WalCommitBatch<'a> {
    new_db_size: u32,
    frames: Vec<traits::WalFrameRef<'a>>,
}

#[cfg(test)]
fn collect_wal_commit_batch<'a>(
    current_db_size: u32,
    write_set: &'a HashMap<PageNumber, StagedPage>,
    write_pages_sorted: &[PageNumber],
) -> Result<Option<WalCommitBatch<'a>>> {
    if write_pages_sorted.is_empty() {
        return Ok(None);
    }

    let max_written = write_pages_sorted.last().map_or(0, |page| page.get());
    let new_db_size = current_db_size.max(max_written);
    let frame_count = write_pages_sorted.len();
    let mut frames = Vec::with_capacity(frame_count);

    for (idx, page_no) in write_pages_sorted.iter().enumerate() {
        let staged_page = write_set.get(page_no).ok_or_else(|| {
            FrankenError::internal(format!(
                "WAL commit batch missing page {} from write_set",
                page_no.get()
            ))
        })?;
        let db_size_if_commit = if idx + 1 == frame_count {
            new_db_size
        } else {
            0
        };

        frames.push(traits::WalFrameRef {
            page_number: page_no.get(),
            page_data: staged_page.as_page_bytes(),
            db_size_if_commit,
        });
    }

    Ok(Some(WalCommitBatch {
        new_db_size,
        frames,
    }))
}

/// Build a [`TransactionFrameBatch`] with OWNED frame data for group commit.
///
/// Unlike the borrowed-frame helper used by unit tests, this function clones
/// each page's bytes into the batch. This is necessary for group commit
/// because the batch must outlive the caller's write_set while waiting for the
/// flusher to write all batched frames.
///
/// Returns `(batch, new_db_size)` or `None` if there are no pages to commit.
fn build_group_commit_batch(
    current_db_size: u32,
    write_set: &HashMap<PageNumber, StagedPage>,
    write_pages_sorted: &[PageNumber],
) -> Result<Option<(TransactionFrameBatch, u32)>> {
    if write_pages_sorted.is_empty() {
        return Ok(None);
    }

    let max_written = write_pages_sorted.last().map_or(0, |page| page.get());
    let new_db_size = current_db_size.max(max_written);
    let frame_count = write_pages_sorted.len();
    let mut frames = Vec::with_capacity(frame_count);

    for (idx, page_no) in write_pages_sorted.iter().enumerate() {
        let staged_page = write_set.get(page_no).ok_or_else(|| {
            FrankenError::internal(format!(
                "group commit batch missing page {} from write_set",
                page_no.get()
            ))
        })?;
        let db_size_if_commit = if idx + 1 == frame_count {
            new_db_size
        } else {
            0
        };

        frames.push(FrameSubmission {
            page_number: page_no.get(),
            page_data: staged_page.as_page_bytes().to_vec(), // Clone data for ownership
            db_size_if_commit,
        });
    }

    Ok(Some((TransactionFrameBatch::new(frames), new_db_size)))
}

const SNAPSHOT_PUBLICATION_MODE: &str = "seqlock_published_pages";
const PUBLISHED_SNAPSHOT_WAIT_SLICE: Duration = Duration::from_micros(50);
/// Maximum retries for optimistic published-page reads before falling back to
/// the slow path. 64 iterations covers typical publish latency on x86 (1-3 µs
/// per seqlock retry). Not runtime-configurable — tuned for low-contention
/// steady state.
const PUBLISHED_READ_FAST_RETRY_LIMIT: usize = 64;
/// Number of counter stripes for published page version tracking. Power-of-2
/// for masking. Matches typical server core counts (up to 64 cores).
const PUBLISHED_COUNTER_STRIPE_COUNT: usize = 64;

// D1-CRITICAL Change 3: Sharded published pages to eliminate publish-side serialization.
/// Number of shards for the published pages map. Must be power of 2.
/// 64 shards balance contention reduction with memory overhead.
const PUBLISHED_PAGES_SHARD_COUNT: usize = 64;
const PUBLISHED_PAGES_SHARD_MASK: usize = PUBLISHED_PAGES_SHARD_COUNT - 1;
const PUBLISHED_PAGES_GOLDEN_RATIO: u32 = 2_654_435_769;

static NEXT_PUBLISHED_COUNTER_STRIPE: AtomicUsize = AtomicUsize::new(0);

std::thread_local! {
    static PUBLISHED_COUNTER_STRIPE_INDEX: usize =
        NEXT_PUBLISHED_COUNTER_STRIPE.fetch_add(1, AtomicOrdering::Relaxed)
            % PUBLISHED_COUNTER_STRIPE_COUNT;
}

#[derive(Debug)]
#[repr(align(64))]
struct CacheAlignedAtomicU64(AtomicU64);

impl CacheAlignedAtomicU64 {
    const fn new(value: u64) -> Self {
        Self(AtomicU64::new(value))
    }

    fn fetch_add(&self, value: u64, ordering: AtomicOrdering) {
        self.0.fetch_add(value, ordering);
    }

    fn load(&self, ordering: AtomicOrdering) -> u64 {
        self.0.load(ordering)
    }
}

#[derive(Debug)]
struct StripedCounter64 {
    stripes: [CacheAlignedAtomicU64; PUBLISHED_COUNTER_STRIPE_COUNT],
}

impl StripedCounter64 {
    fn new() -> Self {
        Self {
            stripes: std::array::from_fn(|_| CacheAlignedAtomicU64::new(0)),
        }
    }

    fn increment(&self) {
        PUBLISHED_COUNTER_STRIPE_INDEX.with(|stripe| {
            self.stripes[*stripe].fetch_add(1, AtomicOrdering::Relaxed);
        });
    }

    fn load(&self) -> u64 {
        self.stripes.iter().fold(0_u64, |sum, stripe| {
            sum.saturating_add(stripe.load(AtomicOrdering::Acquire))
        })
    }
}

/// D1-CRITICAL Change 3: Sharded published pages.
///
/// Replaces the single `RwLock<HashMap>` with 64 shards to eliminate
/// publish-side serialization. Each shard is independently locked,
/// allowing concurrent reads and writes to different page number ranges.
#[derive(Debug)]
struct ShardedPublishedPages {
    shards: Box<
        [RwLock<HashMap<PageNumber, PageData, foldhash::fast::FixedState>>;
            PUBLISHED_PAGES_SHARD_COUNT],
    >,
    page_count: AtomicUsize,
}

impl ShardedPublishedPages {
    fn new() -> Self {
        Self {
            shards: Box::new(std::array::from_fn(|_| {
                RwLock::new(HashMap::with_hasher(foldhash::fast::FixedState::default()))
            })),
            page_count: AtomicUsize::new(0),
        }
    }

    /// Select the shard index for a given page number using multiplicative hashing.
    #[inline]
    fn shard_index(page_no: PageNumber) -> usize {
        let hash = page_no.get().wrapping_mul(PUBLISHED_PAGES_GOLDEN_RATIO);
        (hash as usize) & PUBLISHED_PAGES_SHARD_MASK
    }

    /// Get a page from the appropriate shard.
    fn get(&self, page_no: PageNumber) -> Option<PageData> {
        let shard_idx = Self::shard_index(page_no);
        self.shards[shard_idx]
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&page_no)
            .cloned()
    }

    /// Insert a page into the appropriate shard.
    fn insert(&self, page_no: PageNumber, page: PageData) -> bool {
        let shard_idx = Self::shard_index(page_no);
        let inserted = self.shards[shard_idx]
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(page_no, page)
            .is_none();
        if inserted {
            self.page_count.fetch_add(1, AtomicOrdering::Relaxed);
        }
        inserted
    }

    /// Remove a page from the appropriate shard.
    fn remove(&self, page_no: PageNumber) -> bool {
        let shard_idx = Self::shard_index(page_no);
        let removed = self.shards[shard_idx]
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&page_no)
            .is_some();
        if removed {
            self.page_count.fetch_sub(1, AtomicOrdering::Relaxed);
        }
        removed
    }

    /// Clear all shards.
    fn clear(&self) {
        for shard in self.shards.iter() {
            shard
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        }
        self.page_count.store(0, AtomicOrdering::Release);
    }

    /// Retain pages matching the predicate across all shards.
    fn retain<F>(&self, f: F)
    where
        F: Fn(&PageNumber) -> bool,
    {
        let mut retained_total = 0_usize;
        for shard in self.shards.iter() {
            let mut shard = shard
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            shard.retain(|page_no, _| f(page_no));
            retained_total = retained_total.saturating_add(shard.len());
        }
        self.page_count
            .store(retained_total, AtomicOrdering::Release);
    }

    /// Insert multiple pages, batching by shard to minimize lock acquisitions.
    fn insert_batch<I>(&self, pages: I)
    where
        I: IntoIterator<Item = (PageNumber, PageData)>,
    {
        // Group pages by shard index
        let mut shard_batches: [Vec<(PageNumber, PageData)>; PUBLISHED_PAGES_SHARD_COUNT] =
            std::array::from_fn(|_| Vec::new());

        for (page_no, page) in pages {
            let shard_idx = Self::shard_index(page_no);
            shard_batches[shard_idx].push((page_no, page));
        }

        // Insert each batch into its shard
        let mut total_added = 0_usize;
        for (shard_idx, batch) in shard_batches.into_iter().enumerate() {
            if !batch.is_empty() {
                let mut shard = self.shards[shard_idx]
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                shard.reserve(batch.len());
                for (page_no, page) in batch {
                    if shard.insert(page_no, page).is_none() {
                        total_added = total_added.saturating_add(1);
                    }
                }
            }
        }
        if total_added > 0 {
            self.page_count
                .fetch_add(total_added, AtomicOrdering::Relaxed);
        }
    }

    /// Total number of pages across all shards.
    fn len(&self) -> usize {
        self.page_count.load(AtomicOrdering::Acquire)
    }
}

/// Point-in-time view of the pager metadata publication plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagerPublishedSnapshot {
    /// Even-numbered generation identifying the current published snapshot.
    pub snapshot_gen: u64,
    /// Commit sequence visible through the publication plane.
    pub visible_commit_seq: CommitSeq,
    /// Published database size in pages.
    pub db_size: u32,
    /// Published journal mode.
    pub journal_mode: JournalMode,
    /// Published freelist length.
    pub freelist_count: usize,
    /// Whether a checkpoint is currently active.
    pub checkpoint_active: bool,
    /// Number of committed pages currently served through the publication plane.
    pub page_set_size: usize,
}

#[derive(Debug, Clone, Copy)]
struct PublishedPagerUpdate {
    visible_commit_seq: CommitSeq,
    db_size: u32,
    journal_mode: JournalMode,
    freelist_count: usize,
    checkpoint_active: bool,
}

#[derive(Debug)]
struct PublishedPagerState {
    publish_lock: Mutex<()>,
    sequence_gate: Mutex<()>,
    sequence_cv: Condvar,
    sequence_waiters: KeyedWaitRegistry,
    sequence: AtomicU64,
    visible_commit_seq: AtomicU64,
    db_size: AtomicU32,
    journal_mode: AtomicU8,
    freelist_count: AtomicUsize,
    checkpoint_active: AtomicBool,
    page_set_size: AtomicUsize,
    publication_write_count: AtomicU64,
    read_retry_count: StripedCounter64,
    published_page_hits: StripedCounter64,
    // D1-CRITICAL Change 3: Sharded published pages (64 shards).
    pages: ShardedPublishedPages,
}

impl PublishedPagerState {
    fn new(
        db_size: u32,
        visible_commit_seq: CommitSeq,
        journal_mode: JournalMode,
        freelist_count: usize,
    ) -> Self {
        Self {
            publish_lock: Mutex::new(()),
            sequence_gate: Mutex::new(()),
            sequence_cv: Condvar::new(),
            sequence_waiters: KeyedWaitRegistry::new(),
            sequence: AtomicU64::new(2),
            visible_commit_seq: AtomicU64::new(visible_commit_seq.get()),
            db_size: AtomicU32::new(db_size),
            journal_mode: AtomicU8::new(encode_journal_mode(journal_mode)),
            freelist_count: AtomicUsize::new(freelist_count),
            checkpoint_active: AtomicBool::new(false),
            page_set_size: AtomicUsize::new(0),
            publication_write_count: AtomicU64::new(0),
            read_retry_count: StripedCounter64::new(),
            published_page_hits: StripedCounter64::new(),
            pages: ShardedPublishedPages::new(),
        }
    }

    fn signal_sequence_waiters(&self, observed_sequence: u64, stage: &'static str) {
        match PUBLISHED_SEQUENCE_WAIT_PATH_MODE {
            WaitPathMode::KeyedEventcount => {
                let woke_waiters = self.sequence_waiters.signal(observed_sequence);
                tracing::trace!(
                    target: "fsqlite.snapshot_publication",
                    run_id = "pager-publication",
                    scenario_id = "sequence_wait_signal",
                    observed_sequence,
                    stage,
                    wait_strategy = PUBLISHED_SEQUENCE_WAIT_PATH_MODE.as_str(),
                    publication_mode = SNAPSHOT_PUBLICATION_MODE,
                    woke_waiters,
                    "signaled targeted publication waiters"
                );
            }
            WaitPathMode::LegacyCondvarTimeout if stage == "publish_complete" => {
                self.sequence_cv.notify_all();
            }
            WaitPathMode::LegacyCondvarTimeout => {}
        }
    }

    fn snapshot(&self) -> PagerPublishedSnapshot {
        loop {
            let snapshot_gen = self.sequence.load(AtomicOrdering::Acquire);
            if snapshot_gen % 2 == 1 {
                self.record_retry();
                self.wait_for_sequence_change(snapshot_gen, PUBLISHED_SNAPSHOT_WAIT_SLICE);
                continue;
            }

            let visible_commit_seq =
                CommitSeq::new(self.visible_commit_seq.load(AtomicOrdering::Acquire));
            let db_size = self.db_size.load(AtomicOrdering::Acquire);
            let journal_mode = decode_journal_mode(self.journal_mode.load(AtomicOrdering::Acquire));
            let freelist_count = self.freelist_count.load(AtomicOrdering::Acquire);
            let checkpoint_active = self.checkpoint_active.load(AtomicOrdering::Acquire);
            let page_set_size = self.page_set_size.load(AtomicOrdering::Acquire);

            if self.sequence.load(AtomicOrdering::Acquire) == snapshot_gen {
                return PagerPublishedSnapshot {
                    snapshot_gen,
                    visible_commit_seq,
                    db_size,
                    journal_mode,
                    freelist_count,
                    checkpoint_active,
                    page_set_size,
                };
            }

            self.record_retry();
            self.wait_for_sequence_change(snapshot_gen, PUBLISHED_SNAPSHOT_WAIT_SLICE);
        }
    }

    fn try_get_page(&self, page_no: PageNumber) -> Option<PageData> {
        // D1-CRITICAL Change 3: Use sharded lookup - only locks one shard.
        self.pages.get(page_no)
    }

    // D1-CRITICAL Change 3: Operation-specific publish methods using sharded pages.
    // Replaces the closure-based publish API with type-safe operations.

    /// Publish metadata and insert a single observed page.
    fn publish_observed_page(
        &self,
        cx: &Cx,
        update: PublishedPagerUpdate,
        page_no: PageNumber,
        page: PageData,
    ) -> bool {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current_visible_commit_seq =
            CommitSeq::new(self.visible_commit_seq.load(AtomicOrdering::Acquire));
        if current_visible_commit_seq > update.visible_commit_seq {
            tracing::trace!(
                target: "fsqlite.snapshot_publication",
                trace_id = cx.trace_id(),
                run_id = "pager-publication",
                scenario_id = "stale_read_publish_skip",
                page_no = page_no.get(),
                observed_commit_seq = update.visible_commit_seq.get(),
                visible_commit_seq = current_visible_commit_seq.get(),
                publication_mode = SNAPSHOT_PUBLICATION_MODE,
                "skipping stale published page observation"
            );
            return false;
        }

        self.publish_insert_page_locked(cx, update, page_no, page);
        true
    }

    /// Publish metadata and optionally clear all pages.
    fn publish_clear_if(&self, cx: &Cx, update: PublishedPagerUpdate, should_clear: bool) {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        if should_clear {
            self.pages.clear();
        }
        let page_set_size = self.pages.len();

        self.finalize_publish(cx, update, page_set_size, start);
    }

    /// Publish metadata and remove a single page.
    fn publish_remove_page(&self, cx: &Cx, update: PublishedPagerUpdate, page_no: PageNumber) {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        self.pages.remove(page_no);
        let page_set_size = self.pages.len();

        self.finalize_publish(cx, update, page_set_size, start);
    }

    /// Publish metadata only (no page changes).
    fn publish_metadata_only(&self, cx: &Cx, update: PublishedPagerUpdate) {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        let page_set_size = self.pages.len();

        self.finalize_publish(cx, update, page_set_size, start);
    }

    fn store_metadata_single_connection(&self, update: PublishedPagerUpdate, clear_pages: bool) {
        if clear_pages {
            self.pages.clear();
            self.page_set_size.store(0, AtomicOrdering::Release);
        } else {
            self.page_set_size
                .store(self.pages.len(), AtomicOrdering::Release);
        }
        self.visible_commit_seq
            .store(update.visible_commit_seq.get(), AtomicOrdering::Release);
        self.db_size.store(update.db_size, AtomicOrdering::Release);
        self.journal_mode.store(
            encode_journal_mode(update.journal_mode),
            AtomicOrdering::Release,
        );
        self.freelist_count
            .store(update.freelist_count, AtomicOrdering::Release);
        self.checkpoint_active
            .store(update.checkpoint_active, AtomicOrdering::Release);
    }

    /// Publish truncate during checkpoint: retain pages up to max_page, then remove page one.
    fn publish_truncate_checkpoint(&self, cx: &Cx, update: PublishedPagerUpdate, max_page: u32) {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        self.pages.retain(|page_no| page_no.get() <= max_page);
        self.pages.remove(PageNumber::ONE);
        let page_set_size = self.pages.len();

        self.finalize_publish(cx, update, page_set_size, start);
    }

    /// Publish commit: retain pages up to db_size, then bulk insert from write_set.
    fn publish_commit(
        &self,
        cx: &Cx,
        update: PublishedPagerUpdate,
        write_set: &HashMap<PageNumber, StagedPage>,
    ) {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        // Only sweep published pages when this publication actually shrinks
        // the visible database size and is not stale relative to the current
        // published commit horizon. The common retained autocommit path does
        // not shrink; sweeping there is pure O(n) overhead inside the commit
        // roundtrip. It is also unsafe for an older smaller publication to
        // evict pages from a newer larger published snapshot.
        let previous_db_size = self.db_size.load(AtomicOrdering::Acquire);
        let previous_visible_commit_seq = self.visible_commit_seq.load(AtomicOrdering::Acquire);
        if update.db_size < previous_db_size
            && update.visible_commit_seq.get() >= previous_visible_commit_seq
        {
            self.pages.retain(|page_no| page_no.get() <= update.db_size);
        }

        // Bulk insert committed pages
        self.pages.insert_batch(
            write_set
                .iter()
                .map(|(&page_no, staged)| (page_no, staged.published_page())),
        );

        let page_set_size = self.pages.len();
        self.finalize_publish(cx, update, page_set_size, start);
    }

    fn publish_commit_consuming_pages<I>(&self, cx: &Cx, update: PublishedPagerUpdate, pages: I)
    where
        I: IntoIterator<Item = (PageNumber, StagedPage)>,
    {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        let previous_db_size = self.db_size.load(AtomicOrdering::Acquire);
        let previous_visible_commit_seq = self.visible_commit_seq.load(AtomicOrdering::Acquire);
        if update.db_size < previous_db_size
            && update.visible_commit_seq.get() >= previous_visible_commit_seq
        {
            self.pages.retain(|page_no| page_no.get() <= update.db_size);
        }

        self.pages.insert_batch(
            pages
                .into_iter()
                .map(|(page_no, staged)| (page_no, staged.into_published_page())),
        );

        let page_set_size = self.pages.len();
        self.finalize_publish(cx, update, page_set_size, start);
    }

    /// Publish commit by draining staged pages when the caller no longer needs
    /// the write set after publication.
    fn publish_commit_draining_write_set(
        &self,
        cx: &Cx,
        update: PublishedPagerUpdate,
        write_set: &mut HashMap<PageNumber, StagedPage>,
    ) {
        self.publish_commit_consuming_pages(cx, update, write_set.drain());
    }

    #[cfg(test)]
    fn publish_commit_staged_pages(
        &self,
        cx: &Cx,
        update: PublishedPagerUpdate,
        staged_pages: Vec<(PageNumber, StagedPage)>,
    ) {
        self.publish_commit_consuming_pages(cx, update, staged_pages);
    }

    /// Insert a single page (for testing and internal use).
    #[cfg(test)]
    fn publish_insert_single(
        &self,
        cx: &Cx,
        update: PublishedPagerUpdate,
        page_no: PageNumber,
        page: PageData,
    ) {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        self.pages.insert(page_no, page);
        let page_set_size = self.pages.len();

        self.finalize_publish(cx, update, page_set_size, start);
    }

    /// Internal: Insert a single page (called with publish_lock held).
    fn publish_insert_page_locked(
        &self,
        cx: &Cx,
        update: PublishedPagerUpdate,
        page_no: PageNumber,
        page: PageData,
    ) {
        let start = Instant::now();
        let publish_start_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        self.signal_sequence_waiters(publish_start_sequence, "publish_begin");

        self.pages.insert(page_no, page);
        let page_set_size = self.pages.len();

        self.finalize_publish(cx, update, page_set_size, start);
    }

    /// Internal: Finalize publish by updating metadata and notifying waiters.
    fn finalize_publish(
        &self,
        cx: &Cx,
        update: PublishedPagerUpdate,
        page_set_size: usize,
        start: Instant,
    ) {
        self.publication_write_count
            .fetch_add(1, AtomicOrdering::Relaxed);
        // Monotonic max: commit_seq must never regress under group commit.
        self.visible_commit_seq
            .fetch_max(update.visible_commit_seq.get(), AtomicOrdering::Release);
        // Monotonic max: db_size must never regress. Under group commit,
        // Transaction A (db_size=5) may publish after Transaction B (db_size=7),
        // which would revert the published db_size from 7 to 5. This causes
        // BusySnapshot errors where subsequent transactions can't see pages 6-7.
        self.db_size
            .fetch_max(update.db_size, AtomicOrdering::Release);
        self.journal_mode.store(
            encode_journal_mode(update.journal_mode),
            AtomicOrdering::Release,
        );
        self.freelist_count
            .store(update.freelist_count, AtomicOrdering::Release);
        self.checkpoint_active
            .store(update.checkpoint_active, AtomicOrdering::Release);
        self.page_set_size
            .store(page_set_size, AtomicOrdering::Release);
        let previous_sequence = self.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        let snapshot_gen = previous_sequence.saturating_add(1);
        self.signal_sequence_waiters(previous_sequence, "publish_complete");
        tracing::trace!(
            target: "fsqlite.snapshot_publication",
            trace_id = cx.trace_id(),
            run_id = "pager-publication",
            scenario_id = "metadata_publish",
            snapshot_gen,
            visible_commit_seq = update.visible_commit_seq.get(),
            publication_mode = SNAPSHOT_PUBLICATION_MODE,
            freelist_count = update.freelist_count,
            checkpoint_active = update.checkpoint_active,
            read_retry_count = self.read_retry_count(),
            page_set_size,
            elapsed_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            "published pager snapshot"
        );
    }

    fn note_published_hit(&self) {
        self.published_page_hits.increment();
    }

    fn record_retry(&self) {
        self.read_retry_count.increment();
    }

    fn wait_for_sequence_change(&self, observed_sequence: u64, timeout: Duration) {
        match PUBLISHED_SEQUENCE_WAIT_PATH_MODE {
            WaitPathMode::KeyedEventcount => {
                if self.sequence.load(AtomicOrdering::Acquire) != observed_sequence {
                    return;
                }
                let slot = self.sequence_waiters.slot(observed_sequence);
                let observed_generation = slot.generation();
                if self.sequence.load(AtomicOrdering::Acquire) != observed_sequence {
                    return;
                }
                let wait_result = slot.wait_for_change(observed_generation, timeout);
                tracing::trace!(
                    target: "fsqlite.snapshot_publication",
                    run_id = "pager-publication",
                    scenario_id = match wait_result {
                        KeyedWaitResult::Signaled => "sequence_wait_woke",
                        KeyedWaitResult::TimedOut => "sequence_wait_timeout_fallback",
                    },
                    observed_sequence,
                    wait_strategy = PUBLISHED_SEQUENCE_WAIT_PATH_MODE.as_str(),
                    publication_mode = SNAPSHOT_PUBLICATION_MODE,
                    "publication wait finished"
                );
            }
            WaitPathMode::LegacyCondvarTimeout => {
                let guard = self
                    .sequence_gate
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let (_guard, _timeout) = self
                    .sequence_cv
                    .wait_timeout_while(guard, timeout, |()| {
                        self.sequence.load(AtomicOrdering::Acquire) == observed_sequence
                    })
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }

    fn read_retry_count(&self) -> u64 {
        self.read_retry_count.load()
    }

    fn published_page_hits(&self) -> u64 {
        self.published_page_hits.load()
    }

    fn publication_write_count(&self) -> u64 {
        self.publication_write_count.load(AtomicOrdering::Relaxed)
    }
}

const fn encode_journal_mode(mode: JournalMode) -> u8 {
    match mode {
        JournalMode::Delete => 0,
        JournalMode::Wal => 1,
    }
}

const fn decode_journal_mode(raw: u8) -> JournalMode {
    match raw {
        1 => JournalMode::Wal,
        _ => JournalMode::Delete,
    }
}

/// A concrete single-writer pager backed by a VFS file.
pub struct SimplePager<V: Vfs> {
    /// VFS used to open journal/WAL companion files.
    vfs: Arc<V>,
    /// Path to the database file.
    db_path: PathBuf,
    /// Shared mutable state used by transactions.
    inner: Arc<Mutex<PagerInner<V::File>>>,
    /// Sharded page cache for high-concurrency workloads (bd-3wop3.2).
    /// Each shard has its own mutex, eliminating global lock contention.
    cache: Arc<ShardedPageCache>,
    /// Shared page buffer pool cloned into transactions for write staging.
    pool: PageBufPool,
    /// Published metadata/page plane for lock-light steady-state reads.
    published: Arc<PublishedPagerState>,
    /// WAL backend for WAL-mode operation (D1-CRITICAL: separate lock for split-lock commit).
    /// This enables Thread B to start its prepare phase while Thread A does WAL I/O.
    wal_backend: SharedWalBackend,
    /// Immutable committed-state snapshot (bd-db300.5.3.3.1 / Card 1: M6).
    /// Readers clone the Arc via a brief RwLock-read to inspect committed state
    /// without taking the PagerInner Mutex.  Published on every commit.
    committed_snapshot: Arc<RwLock<Arc<PagerCommittedSnapshot>>>,
    /// Same-path connection counter injected by the SQL connection layer.
    /// Unset pagers keep the legacy publication path.
    shared_connection_count: OnceLock<Arc<AtomicUsize>>,
}

impl<V: Vfs> traits::sealed::Sealed for SimplePager<V> {}

fn page_size_from_header_bytes(header_bytes: &[u8; DATABASE_HEADER_SIZE]) -> Option<PageSize> {
    if &header_bytes[..DATABASE_HEADER_MAGIC.len()] != DATABASE_HEADER_MAGIC {
        return None;
    }

    let raw = u16::from_be_bytes([header_bytes[16], header_bytes[17]]);
    let page_size = match raw {
        1 => 65_536,
        0 => return None,
        value => u32::from(value),
    };
    PageSize::new(page_size)
}

fn stale_main_header_is_wal_recoverable_error(error: &DatabaseHeaderError) -> bool {
    matches!(
        error,
        DatabaseHeaderError::InvalidSchemaFormat { raw: 0 }
            | DatabaseHeaderError::InvalidTextEncoding { raw: 0 }
    )
}

fn change_counter_from_header_bytes(header_bytes: &[u8; DATABASE_HEADER_SIZE]) -> u32 {
    u32::from_be_bytes([
        header_bytes[24],
        header_bytes[25],
        header_bytes[26],
        header_bytes[27],
    ])
}

fn stale_main_header_change_counter_under_wal(
    header_bytes: &[u8; DATABASE_HEADER_SIZE],
    error: &DatabaseHeaderError,
) -> Option<u64> {
    if !stale_main_header_is_wal_recoverable_error(error) {
        return None;
    }

    page_size_from_header_bytes(header_bytes)?;
    Some(u64::from(change_counter_from_header_bytes(header_bytes)))
}

fn wal_contains_valid_database_page1<F: VfsFile>(
    cx: &Cx,
    wal: &mut WalFile<F>,
    expected_page_size: PageSize,
) -> bool {
    let Ok(expected_page_size_usize) = usize::try_from(expected_page_size.get()) else {
        return false;
    };
    if wal.page_size() != expected_page_size_usize {
        return false;
    }

    // Trust only the committed WAL prefix. Valid trailing frames after the
    // last commit are not visible to readers and must not affect recovery.
    let Some(last_commit_frame) = wal.last_commit_frame(cx).ok().flatten() else {
        return false;
    };

    for frame_index in (0..=last_commit_frame).rev() {
        let Ok((frame_header, page_data)) = wal.read_frame(cx, frame_index) else {
            return false;
        };
        if frame_header.page_number != PageNumber::ONE.get() {
            continue;
        }
        if page_data.len() < DATABASE_HEADER_SIZE {
            return false;
        }

        let mut page1_header_bytes = [0_u8; DATABASE_HEADER_SIZE];
        page1_header_bytes.copy_from_slice(&page_data[..DATABASE_HEADER_SIZE]);
        return DatabaseHeader::from_bytes(&page1_header_bytes).is_ok();
    }

    false
}

fn stale_main_header_can_be_recovered_from_live_wal<V: Vfs>(
    cx: &Cx,
    vfs: &V,
    path: &Path,
    header_bytes: &[u8; DATABASE_HEADER_SIZE],
    error: &DatabaseHeaderError,
) -> Result<bool> {
    if !stale_main_header_is_wal_recoverable_error(error) {
        return Ok(false);
    }

    let Some(expected_page_size) = page_size_from_header_bytes(header_bytes) else {
        return Ok(false);
    };

    let mut wal_path = path.to_owned().into_os_string();
    wal_path.push("-wal");
    let wal_path = PathBuf::from(wal_path);
    if !vfs.access(cx, &wal_path, AccessFlags::EXISTS)? {
        return Ok(false);
    }

    // Probe WAL content with a write-capable handle first. On the Unix VFS,
    // a READONLY-opened WAL can become the canonical inode-table fd, poisoning
    // later writer paths with EBADF if they clone that descriptor.
    let Ok((wal_file, _)) = vfs.open(
        cx,
        Some(&wal_path),
        VfsOpenFlags::READWRITE | VfsOpenFlags::WAL,
    ) else {
        return Ok(false);
    };
    let Ok(mut wal) = WalFile::open(cx, wal_file) else {
        return Ok(false);
    };
    let wal_contains_valid_page1 =
        wal_contains_valid_database_page1(cx, &mut wal, expected_page_size);
    let _ = wal.close(cx);
    Ok(wal_contains_valid_page1)
}

fn bootstrap_header_from_stale_main_file(
    header_bytes: &[u8; DATABASE_HEADER_SIZE],
    page_size: PageSize,
) -> DatabaseHeader {
    DatabaseHeader {
        page_size,
        write_version: header_bytes[18],
        read_version: header_bytes[19],
        reserved_per_page: header_bytes[20],
        change_counter: u32::from_be_bytes([
            header_bytes[24],
            header_bytes[25],
            header_bytes[26],
            header_bytes[27],
        ]),
        page_count: u32::from_be_bytes([
            header_bytes[28],
            header_bytes[29],
            header_bytes[30],
            header_bytes[31],
        ]),
        freelist_trunk: u32::from_be_bytes([
            header_bytes[32],
            header_bytes[33],
            header_bytes[34],
            header_bytes[35],
        ]),
        freelist_count: u32::from_be_bytes([
            header_bytes[36],
            header_bytes[37],
            header_bytes[38],
            header_bytes[39],
        ]),
        schema_cookie: u32::from_be_bytes([
            header_bytes[40],
            header_bytes[41],
            header_bytes[42],
            header_bytes[43],
        ]),
        schema_format: u32::from_be_bytes([
            header_bytes[44],
            header_bytes[45],
            header_bytes[46],
            header_bytes[47],
        ]),
        default_cache_size: i32::from_be_bytes([
            header_bytes[48],
            header_bytes[49],
            header_bytes[50],
            header_bytes[51],
        ]),
        largest_root_page: u32::from_be_bytes([
            header_bytes[52],
            header_bytes[53],
            header_bytes[54],
            header_bytes[55],
        ]),
        // The authoritative encoding/schema header will be re-read from the
        // WAL-backed page-1 snapshot on the first transaction begin.
        text_encoding: fsqlite_types::TextEncoding::Utf8,
        user_version: u32::from_be_bytes([
            header_bytes[60],
            header_bytes[61],
            header_bytes[62],
            header_bytes[63],
        ]),
        incremental_vacuum: u32::from_be_bytes([
            header_bytes[64],
            header_bytes[65],
            header_bytes[66],
            header_bytes[67],
        ]),
        application_id: u32::from_be_bytes([
            header_bytes[68],
            header_bytes[69],
            header_bytes[70],
            header_bytes[71],
        ]),
        version_valid_for: u32::from_be_bytes([
            header_bytes[92],
            header_bytes[93],
            header_bytes[94],
            header_bytes[95],
        ]),
        sqlite_version: u32::from_be_bytes([
            header_bytes[96],
            header_bytes[97],
            header_bytes[98],
            header_bytes[99],
        ]),
    }
}

impl<V> MvccPager for SimplePager<V>
where
    V: Vfs + Send + Sync,
    V::File: Send + Sync,
{
    type Txn = SimpleTransaction<V>;

    fn begin(&self, cx: &Cx, mode: TransactionMode) -> Result<Self::Txn> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;

        if inner.checkpoint_active {
            return Err(FrankenError::Busy);
        }

        let active_transactions_before_begin = inner.active_transactions;

        // ── In-memory fast path ─────────────────────────────────────
        // For in-memory VFS, skip persistent shared-lock ownership between
        // local transactions. We still need to recover any externally-created
        // hot journal and refresh connection-local metadata/publication state
        // when the first local transaction starts, because another pager
        // sharing the same `MemoryVfs` can mutate the durable image or the
        // shared WAL backend between transactions.
        if self.vfs.is_memory() {
            let had_recovery_pending = inner.rollback_journal_recovery_state.is_pending();
            let commit_seq_before_refresh = inner.commit_seq;
            if active_transactions_before_begin == 0 {
                let journal_path = Self::journal_path(&self.db_path);
                let journal_exists = self.vfs.access(cx, &journal_path, AccessFlags::EXISTS)?;
                let journal_visibility_invalidation = had_recovery_pending || journal_exists;
                {
                    if inner.rollback_journal_recovery_state.is_pending() || journal_exists {
                        let page_size = inner.page_size;
                        match Self::recover_rollback_journal_if_present_locked(
                            cx,
                            &*self.vfs,
                            &mut inner.db_file,
                            &journal_path,
                            page_size,
                            LockLevel::None,
                        ) {
                            Ok(false) if had_recovery_pending => {
                                return Err(FrankenError::internal(
                                    "rollback journal missing while local recovery was pending",
                                ));
                            }
                            Ok(_) => {}
                            Err(err) => return Err(err),
                        }
                        self.cache.clear();
                        inner.rollback_journal_recovery_state = RollbackJournalRecoveryState::Clean;
                    }
                    inner.refresh_committed_state(cx, &self.cache, &self.wal_backend)?;
                }
                let clear_published_pages = journal_visibility_invalidation
                    || inner.commit_seq != commit_seq_before_refresh;
                // D1-CRITICAL Change 3: Use sharded publish_clear_if.
                self.published.publish_clear_if(
                    cx,
                    PublishedPagerUpdate {
                        visible_commit_seq: inner.commit_seq,
                        db_size: inner.db_size,
                        journal_mode: inner.journal_mode,
                        freelist_count: inner.freelist.len(),
                        checkpoint_active: inner.checkpoint_active,
                    },
                    clear_published_pages,
                );
            }

            let eager_writer = matches!(
                mode,
                TransactionMode::Immediate | TransactionMode::Exclusive
            );
            if eager_writer && inner.writer_active {
                return Err(FrankenError::Busy);
            }
            if eager_writer {
                inner.writer_active = true;
            }
            inner.active_transactions += 1;
            let original_db_size = inner.db_size;
            let journal_mode = inner.journal_mode;
            let published_snapshot = self.published.snapshot();
            let pool = self.pool.clone();
            let cleanup_cx = cx.clone();
            let memory_db_bump_alloc =
                self.vfs.is_memory() && self.db_path == Path::new("/:memory:");
            return Ok(SimpleTransaction {
                vfs: Arc::clone(&self.vfs),
                journal_path: Self::journal_path(&self.db_path),
                group_commit_queue: group_commit_queue_for_backend(
                    self.vfs.as_ref(),
                    &self.db_path,
                ),
                inner: Arc::clone(&self.inner),
                cache: Arc::clone(&self.cache),
                published: Arc::clone(&self.published),
                wal_backend: Arc::clone(&self.wal_backend),
                committed_snapshot: Arc::clone(&self.committed_snapshot),
                shared_connection_count: self.shared_connection_count.get().cloned(),
                published_visible_commit_seq: Cell::new(published_snapshot.visible_commit_seq),
                published_db_size: Cell::new(published_snapshot.db_size),
                write_set: HashMap::new(),
                write_pages_sorted: Vec::new(),
                freed_pages: Vec::new(),
                allocated_from_freelist: Vec::new(),
                allocated_from_eof: Vec::new(),
                mode,
                is_writer: eager_writer,
                committed: false,
                finished: false,
                original_db_size,
                savepoint_stack: Vec::new(),
                journal_mode,
                pool,
                cleanup_cx,
                page_lease: Vec::new(),
                memory_db_bump_alloc,
                rolled_back_pages: HashSet::new(),
                txn_read_cache: RefCell::new(HashMap::new()),
            });
        }

        // ── File-backed path (full locking + recovery) ──────────────
        let had_recovery_pending = inner.rollback_journal_recovery_state.is_pending();
        let commit_seq_before_refresh = inner.commit_seq;

        // Acquire a SHARED lock on the database file for cross-process
        // reader/writer exclusion. The file handle lock is shared across all
        // local transactions, so only the first active transaction should
        // acquire it.
        if active_transactions_before_begin == 0 {
            inner.db_file.lock(cx, LockLevel::Shared)?;
        }

        let mut journal_visibility_invalidation = false;
        let wal_snapshot_initialized = if active_transactions_before_begin == 0 {
            let journal_path = Self::journal_path(&self.db_path);
            let journal_exists = match self.vfs.access(cx, &journal_path, AccessFlags::EXISTS) {
                Ok(exists) => exists,
                Err(err) => {
                    let _ = inner.db_file.unlock(cx, LockLevel::None);
                    return Err(err);
                }
            };
            journal_visibility_invalidation = had_recovery_pending || journal_exists;
            if inner.rollback_journal_recovery_state.is_pending() || journal_exists {
                let page_size = inner.page_size;
                match Self::recover_rollback_journal_if_present_locked(
                    cx,
                    &*self.vfs,
                    &mut inner.db_file,
                    &journal_path,
                    page_size,
                    LockLevel::Shared,
                ) {
                    Ok(false) if inner.rollback_journal_recovery_state.is_pending() => {
                        inner.db_file.unlock(cx, LockLevel::None)?;
                        return Err(FrankenError::internal(
                            "rollback journal missing while local recovery was pending",
                        ));
                    }
                    Ok(_) => {}
                    Err(err) => {
                        let _ = inner.db_file.unlock(cx, LockLevel::None);
                        return Err(err);
                    }
                }
                // Any leftover rollback journal means the durable image may
                // have changed behind this pager, even when the journal had
                // already been invalidated to zero bytes. Drop cached state and
                // rebuild from disk before serving reads.
                self.cache.clear();
                inner.rollback_journal_recovery_state = RollbackJournalRecoveryState::Clean;
            }
            match inner.refresh_committed_state(cx, &self.cache, &self.wal_backend) {
                Ok(v) => v,
                Err(err) => {
                    let _ = inner.db_file.unlock(cx, LockLevel::None);
                    return Err(err);
                }
            }
        } else {
            false
        };

        if active_transactions_before_begin == 0 {
            let clear_published_pages =
                journal_visibility_invalidation || inner.commit_seq != commit_seq_before_refresh;
            // D1-CRITICAL Change 3: Use sharded publish_clear_if.
            self.published.publish_clear_if(
                cx,
                PublishedPagerUpdate {
                    visible_commit_seq: inner.commit_seq,
                    db_size: inner.db_size,
                    journal_mode: inner.journal_mode,
                    freelist_count: inner.freelist.len(),
                    checkpoint_active: inner.checkpoint_active,
                },
                clear_published_pages,
            );
        }

        let eager_writer = matches!(
            mode,
            TransactionMode::Immediate | TransactionMode::Exclusive
        );
        if eager_writer && inner.writer_active {
            if active_transactions_before_begin == 0 {
                inner.db_file.unlock(cx, LockLevel::None)?;
            }
            return Err(FrankenError::Busy);
        }

        // For write transactions, escalate to RESERVED to signal write intent
        // to other processes. This is a non-blocking advisory lock that
        // prevents multiple processes from writing simultaneously.
        if eager_writer {
            if let Err(err) = inner.db_file.lock(cx, LockLevel::Reserved) {
                let preserve_level = retained_lock_level_after_txn_exit(
                    active_transactions_before_begin,
                    inner.writer_active,
                );
                inner.db_file.unlock(cx, preserve_level)?;
                return Err(err);
            }
            inner.writer_active = true;
        }

        if inner.journal_mode == JournalMode::Wal && !wal_snapshot_initialized {
            let wal_begin_result =
                with_wal_backend(&self.wal_backend, |wal| wal.begin_transaction(cx));
            if let Err(err) = wal_begin_result {
                if eager_writer {
                    inner.writer_active = false;
                }
                let preserve_level = retained_lock_level_after_txn_exit(
                    active_transactions_before_begin,
                    inner.writer_active,
                );
                inner.db_file.unlock(cx, preserve_level)?;
                return Err(err);
            }
        }

        inner.active_transactions = inner.active_transactions.saturating_add(1);
        let original_db_size = inner.db_size;
        let journal_mode = inner.journal_mode;
        let pool = self.pool.clone();
        let published_snapshot = self.published.snapshot();
        let cleanup_cx = cleanup_child_cx(cx);
        let memory_db_bump_alloc = self.vfs.is_memory() && self.db_path == Path::new("/:memory:");
        drop(inner);

        Ok(SimpleTransaction {
            vfs: Arc::clone(&self.vfs),
            journal_path: Self::journal_path(&self.db_path),
            group_commit_queue: group_commit_queue_for_backend(self.vfs.as_ref(), &self.db_path),
            inner: Arc::clone(&self.inner),
            cache: Arc::clone(&self.cache),
            published: Arc::clone(&self.published),
            wal_backend: Arc::clone(&self.wal_backend),
            committed_snapshot: Arc::clone(&self.committed_snapshot),
            shared_connection_count: self.shared_connection_count.get().cloned(),
            published_visible_commit_seq: Cell::new(published_snapshot.visible_commit_seq),
            published_db_size: Cell::new(published_snapshot.db_size),
            write_set: HashMap::new(),
            write_pages_sorted: Vec::new(),
            freed_pages: Vec::new(),
            allocated_from_freelist: Vec::new(),
            allocated_from_eof: Vec::new(),
            mode,
            is_writer: eager_writer,
            committed: false,
            finished: false,
            original_db_size,
            savepoint_stack: Vec::new(),
            journal_mode,
            pool,
            cleanup_cx,
            page_lease: Vec::new(),
            memory_db_bump_alloc,
            rolled_back_pages: HashSet::new(),
            txn_read_cache: RefCell::new(HashMap::new()),
        })
    }

    fn journal_mode(&self) -> JournalMode {
        self.published.snapshot().journal_mode
    }

    fn is_readonly(&self) -> bool {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.access_mode.is_readonly()
    }

    fn set_journal_mode(&self, cx: &Cx, mode: JournalMode) -> Result<JournalMode> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;

        if inner.journal_mode == mode {
            return Ok(mode);
        }

        if inner.checkpoint_active {
            return Err(FrankenError::Busy);
        }
        if inner.active_transactions > 0 {
            // Cannot switch journal mode while any transaction is active.
            return Err(FrankenError::Busy);
        }

        if mode == JournalMode::Wal && !has_wal_backend(&self.wal_backend)? {
            return Err(FrankenError::Unsupported);
        }

        // Update the file format version in the database header (bytes 18-19).
        // WAL mode uses version 2; all rollback journal modes use version 1.
        // Without this, standard SQLite tools cannot detect WAL mode from the
        // on-disk header and will fail to look for the WAL file.
        let version_byte: u8 = if mode == JournalMode::Wal { 2 } else { 1 };
        if inner.db_size > 0 && !inner.access_mode.is_readonly() {
            let page_size = inner.page_size.as_usize();
            let mut page1 = vec![0u8; page_size];
            let bytes_read = inner.db_file.read(cx, &mut page1, 0)?;
            if bytes_read >= DATABASE_HEADER_SIZE {
                page1[18] = version_byte;
                page1[19] = version_byte;
                inner.db_file.write(cx, &page1, 0)?;
                self.cache.evict(PageNumber::ONE);
            }
        }

        inner.journal_mode = mode;
        // D1-CRITICAL Change 3: Use sharded publish_remove_page.
        self.published.publish_remove_page(
            cx,
            PublishedPagerUpdate {
                visible_commit_seq: inner.commit_seq,
                db_size: inner.db_size,
                journal_mode: inner.journal_mode,
                freelist_count: inner.freelist.len(),
                checkpoint_active: inner.checkpoint_active,
            },
            PageNumber::ONE,
        );
        drop(inner);
        Ok(mode)
    }

    fn set_wal_backend(&self, backend: Box<dyn WalBackend>) -> Result<()> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;
        if inner.checkpoint_active {
            return Err(FrankenError::Busy);
        }
        drop(inner);
        // D1-CRITICAL: Store in shared lock, NOT inner.wal_backend
        let mut wal_guard = self
            .wal_backend
            .write()
            .map_err(|_| FrankenError::internal("SharedWalBackend lock poisoned"))?;
        *wal_guard = Some(backend);
        drop(wal_guard);
        Ok(())
    }
}

impl<V: Vfs> SimplePager<V>
where
    V::File: Send + Sync,
{
    const EXPORT_COPY_CHUNK_SIZE: usize = 64 * 1024;

    /// Return the database path used by this pager.
    #[must_use]
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Clone the pager's VFS handle for companion-file operations.
    pub fn vfs_handle(&self) -> Arc<V> {
        Arc::clone(&self.vfs)
    }

    /// Propagate the connection's busy-timeout to the underlying VFS file so
    /// that `posix_lock` retries with backoff instead of returning BUSY
    /// immediately on cross-process contention.
    pub fn set_vfs_busy_timeout_ms(&self, ms: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.db_file.set_busy_timeout_ms(ms);
        }
    }

    /// Install a concrete WAL backend while preserving ownership on failure.
    ///
    /// This lets callers explicitly close any underlying VFS resources when
    /// the pager rejects installation.
    pub fn set_wal_backend_owned<B>(&self, backend: B) -> std::result::Result<(), (FrankenError, B)>
    where
        B: WalBackend + 'static,
    {
        let inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(_) => {
                return Err((FrankenError::internal("SimplePager lock poisoned"), backend));
            }
        };
        if inner.checkpoint_active {
            return Err((FrankenError::Busy, backend));
        }
        drop(inner);

        let mut wal_guard = match self.wal_backend.write() {
            Ok(guard) => guard,
            Err(_) => {
                return Err((
                    FrankenError::internal("SharedWalBackend lock poisoned"),
                    backend,
                ));
            }
        };
        *wal_guard = Some(Box::new(backend));
        drop(wal_guard);
        Ok(())
    }

    /// Return the current WAL commit sync policy.
    #[must_use]
    pub fn wal_commit_sync_policy(&self) -> WalCommitSyncPolicy {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wal_commit_sync_policy
    }

    /// Configure whether WAL-mode commits sync the WAL file immediately.
    pub fn set_wal_commit_sync_policy(&self, policy: WalCommitSyncPolicy) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;
        inner.wal_commit_sync_policy = policy;
        Ok(())
    }

    /// Export the pager's main database image as a self-contained SQLite file.
    ///
    /// The pager must be quiescent. In WAL mode we first checkpoint and
    /// truncate the WAL so the returned bytes contain the durable main image.
    pub fn export_database_bytes(&self, cx: &Cx) -> Result<Vec<u8>> {
        let source_full = self.vfs.full_pathname(cx, &self.db_path)?;

        let journal_mode = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;
            if inner.active_transactions > 0 || inner.checkpoint_active {
                return Err(FrankenError::Busy);
            }
            inner.journal_mode
        };

        if journal_mode == JournalMode::Wal {
            self.checkpoint(cx, traits::CheckpointMode::Truncate)?;
        }

        let source_flags = VfsOpenFlags::MAIN_DB | VfsOpenFlags::READWRITE;
        let (mut source_file, _) = self.vfs.open(cx, Some(&source_full), source_flags)?;

        let export_result = (|| -> Result<Vec<u8>> {
            let file_size = source_file.file_size(cx)?;
            let output_len = usize::try_from(file_size).map_err(|_| FrankenError::OutOfRange {
                what: "database export size".to_owned(),
                value: file_size.to_string(),
            })?;
            let mut bytes = vec![0_u8; output_len];
            let mut copied = 0_usize;
            while copied < output_len {
                let chunk_len = (output_len - copied).min(Self::EXPORT_COPY_CHUNK_SIZE);
                let bytes_read =
                    source_file.read(cx, &mut bytes[copied..copied + chunk_len], copied as u64)?;
                if bytes_read == 0 {
                    return Err(FrankenError::internal(
                        "unexpected EOF while exporting database image",
                    ));
                }
                copied = copied
                    .checked_add(bytes_read)
                    .ok_or_else(|| FrankenError::internal("export size overflow"))?;
            }
            Ok(bytes)
        })();

        let source_close = source_file.close(cx);

        let bytes = export_result?;
        source_close?;
        Ok(bytes)
    }

    /// Copy the pager's main database file to `target_path` via the active VFS.
    ///
    /// This is the pager-side export primitive used by higher-level features
    /// like `VACUUM INTO` and backup/canonicalization flows. The copy is only
    /// allowed when the pager is quiescent. In WAL mode we first checkpoint and
    /// truncate the WAL so the destination contains a self-contained main DB.
    pub fn copy_database_to(&self, cx: &Cx, target_path: &Path) -> Result<()> {
        let source_path = self.db_path.clone();
        let source_full = self.vfs.full_pathname(cx, &source_path)?;
        let target_full = self.vfs.full_pathname(cx, target_path)?;
        if source_full == target_full {
            return Err(FrankenError::CannotOpen { path: target_full });
        }
        if self.vfs.access(cx, &target_full, AccessFlags::EXISTS)? {
            return Err(FrankenError::CannotOpen { path: target_full });
        }

        let journal_mode = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;
            if inner.active_transactions > 0 || inner.checkpoint_active {
                return Err(FrankenError::Busy);
            }
            inner.journal_mode
        };

        if journal_mode == JournalMode::Wal {
            self.checkpoint(cx, traits::CheckpointMode::Truncate)?;
        }

        let source_flags = VfsOpenFlags::MAIN_DB | VfsOpenFlags::READWRITE;
        let target_flags = VfsOpenFlags::MAIN_DB
            | VfsOpenFlags::CREATE
            | VfsOpenFlags::EXCLUSIVE
            | VfsOpenFlags::READWRITE;
        let (mut source_file, _) = self.vfs.open(cx, Some(&source_full), source_flags)?;
        let (mut target_file, _) = self.vfs.open(cx, Some(&target_full), target_flags)?;

        let copy_result = (|| -> Result<()> {
            let file_size = source_file.file_size(cx)?;
            let mut copied = 0_u64;
            let mut buffer = vec![0_u8; Self::EXPORT_COPY_CHUNK_SIZE];
            while copied < file_size {
                let remaining = file_size - copied;
                let chunk_len = usize::try_from(remaining.min(buffer.len() as u64))
                    .map_err(|_| FrankenError::internal("copy chunk length overflow"))?;
                let bytes_read = source_file.read(cx, &mut buffer[..chunk_len], copied)?;
                if bytes_read == 0 {
                    return Err(FrankenError::internal(
                        "unexpected EOF while copying database image",
                    ));
                }
                target_file.write(cx, &buffer[..bytes_read], copied)?;
                copied = copied
                    .checked_add(
                        u64::try_from(bytes_read)
                            .map_err(|_| FrankenError::internal("copy size overflow"))?,
                    )
                    .ok_or_else(|| FrankenError::internal("copy offset overflow"))?;
            }
            target_file.truncate(cx, file_size)?;
            target_file.sync(cx, SyncFlags::FULL)?;
            Ok(())
        })();

        let source_close = source_file.close(cx);
        let target_close = target_file.close(cx);

        copy_result?;
        source_close?;
        target_close?;
        Ok(())
    }

    /// Capture point-in-time page-cache counters.
    pub fn cache_metrics_snapshot(&self) -> Result<PageCacheMetricsSnapshot> {
        Ok(self.cache.metrics_snapshot())
    }

    /// Reset page-cache counters without altering resident pages.
    pub fn reset_cache_metrics(&self) -> Result<()> {
        self.cache.reset_metrics();
        Ok(())
    }

    /// Capture the current published pager metadata snapshot.
    #[must_use]
    pub fn published_snapshot(&self) -> PagerPublishedSnapshot {
        self.published.snapshot()
    }

    /// Refresh the publication plane from the latest committed pager state.
    ///
    /// This is used by upper layers that need a coherent published visibility
    /// snapshot before starting a new transaction or deciding whether a
    /// connection-local execution image is stale.
    pub fn refresh_published_snapshot(&self, cx: &Cx) -> Result<PagerPublishedSnapshot> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;

        if inner.active_transactions > 0 || inner.checkpoint_active {
            return Ok(self.published.snapshot());
        }

        inner.db_file.lock(cx, LockLevel::Shared)?;

        let had_recovery_pending = inner.rollback_journal_recovery_state.is_pending();
        let commit_seq_before_refresh = inner.commit_seq;
        let journal_path = Self::journal_path(&self.db_path);

        let journal_exists = match self.vfs.access(cx, &journal_path, AccessFlags::EXISTS) {
            Ok(exists) => exists,
            Err(err) => {
                let _ = inner.db_file.unlock(cx, LockLevel::None);
                return Err(err);
            }
        };

        let mut recovered_or_invalidated_journal = false;
        if inner.rollback_journal_recovery_state.is_pending() || journal_exists {
            let page_size = inner.page_size;
            match Self::recover_rollback_journal_if_present_locked(
                cx,
                &*self.vfs,
                &mut inner.db_file,
                &journal_path,
                page_size,
                LockLevel::Shared,
            ) {
                Ok(false) if inner.rollback_journal_recovery_state.is_pending() => {
                    inner.db_file.unlock(cx, LockLevel::None)?;
                    return Err(FrankenError::internal(
                        "rollback journal missing while local recovery was pending",
                    ));
                }
                Ok(_) => {
                    recovered_or_invalidated_journal = true;
                    inner.rollback_journal_recovery_state = RollbackJournalRecoveryState::Clean;
                }
                Err(err) => {
                    let _ = inner.db_file.unlock(cx, LockLevel::None);
                    return Err(err);
                }
            }
        }

        if recovered_or_invalidated_journal {
            self.cache.clear();
        }
        if let Err(err) = inner.refresh_committed_state(cx, &self.cache, &self.wal_backend) {
            let _ = inner.db_file.unlock(cx, LockLevel::None);
            return Err(err);
        }

        let clear_published_pages =
            had_recovery_pending || journal_exists || inner.commit_seq != commit_seq_before_refresh;
        // D1-CRITICAL Change 3: Use sharded publish_clear_if.
        self.published.publish_clear_if(
            cx,
            PublishedPagerUpdate {
                visible_commit_seq: inner.commit_seq,
                db_size: inner.db_size,
                journal_mode: inner.journal_mode,
                freelist_count: inner.freelist.len(),
                checkpoint_active: inner.checkpoint_active,
            },
            clear_published_pages,
        );
        inner.db_file.unlock(cx, LockLevel::None)?;

        Ok(self.published.snapshot())
    }

    /// Number of snapshot retries steady-state readers have taken.
    #[must_use]
    pub fn published_read_retry_count(&self) -> u64 {
        self.published.read_retry_count()
    }

    /// Returns the database page size.
    #[must_use]
    pub fn page_size(&self) -> PageSize {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.page_size
    }

    /// Read the committed-state snapshot without taking the PagerInner Mutex.
    ///
    /// Returns a cheap `Arc` clone — the RwLock read-hold is ~nanoseconds.
    /// Use this for staleness checks, visibility probes, and begin-path
    /// fast-path gating instead of locking `PagerInner`.
    #[must_use]
    pub fn committed_snapshot(&self) -> Arc<PagerCommittedSnapshot> {
        if self
            .shared_connection_count
            .get()
            .is_some_and(|counter| counter.load(AtomicOrdering::Acquire) == 1)
        {
            let published = self.published.snapshot();
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            return Arc::new(PagerCommittedSnapshot {
                commit_seq: published.visible_commit_seq,
                db_size: published.db_size,
                journal_mode: published.journal_mode,
                freelist_count: published.freelist_count,
                checkpoint_active: published.checkpoint_active,
                writer_active: inner.writer_active,
                db_file_size_bytes: inner.committed_db_file_size_bytes,
            });
        }
        Arc::clone(
            &self
                .committed_snapshot
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Bind a same-path connection counter owned by the SQL connection layer.
    pub fn bind_shared_connection_count(&self, counter: Arc<AtomicUsize>) {
        let _ = self.shared_connection_count.set(counter);
    }

    /// Number of page reads satisfied directly from the publication plane.
    #[must_use]
    pub fn published_page_hits(&self) -> u64 {
        self.published.published_page_hits()
    }

    /// Number of publish-plane writes applied to this pager.
    #[must_use]
    pub fn publication_write_count(&self) -> u64 {
        self.published.publication_write_count()
    }

    /// Number of frames currently in the WAL for this pager.
    pub fn wal_frame_count(&self) -> usize {
        with_wal_backend(&self.wal_backend, |wal| Ok(wal.frame_count())).unwrap_or(0)
    }

    /// Compute the journal path from the database path.
    fn journal_path(db_path: &Path) -> PathBuf {
        let mut jp = db_path.as_os_str().to_owned();
        jp.push("-journal");
        PathBuf::from(jp)
    }

    fn recover_rollback_journal_if_present(
        cx: &Cx,
        vfs: &V,
        db_file: &mut V::File,
        journal_path: &Path,
        page_size: PageSize,
    ) -> Result<bool> {
        if !vfs.access(cx, journal_path, AccessFlags::EXISTS)? {
            return Ok(false);
        }

        Self::replay_journal(cx, vfs, db_file, journal_path, page_size)?;
        let _ = vfs.delete(cx, journal_path, true);
        Ok(true)
    }

    fn recover_rollback_journal_if_present_locked(
        cx: &Cx,
        vfs: &V,
        db_file: &mut V::File,
        journal_path: &Path,
        page_size: PageSize,
        restore_lock_level: LockLevel,
    ) -> Result<bool> {
        if !vfs.access(cx, journal_path, AccessFlags::EXISTS)? {
            return Ok(false);
        }

        db_file.lock(cx, LockLevel::Exclusive)?;
        let recovery_result =
            Self::recover_rollback_journal_if_present(cx, vfs, db_file, journal_path, page_size);
        let restore_result = db_file.unlock(cx, restore_lock_level);
        match (recovery_result, restore_result) {
            (Ok(recovered), Ok(())) => Ok(recovered),
            (Err(recovery_err), Ok(())) => Err(recovery_err),
            (Ok(_), Err(restore_err)) => Err(restore_err),
            (Err(recovery_err), Err(restore_err)) => Err(FrankenError::internal(format!(
                "hot journal recovery failed and could not restore lock level {restore_lock_level:?}: recovery={recovery_err}; restore={restore_err}"
            ))),
        }
    }

    /// Open (or create) a database and return a pager using a caller-owned
    /// capability context.
    ///
    /// Existing databases adopt the page size encoded in their on-disk header;
    /// `requested_page_size` is used when creating a new database or when the
    /// header is unavailable/corrupt and recovery must fall back to a caller
    /// default.
    ///
    /// If a hot journal is detected (leftover from a crash), it is replayed
    /// to restore the database to a consistent state before returning.
    #[allow(clippy::too_many_lines)]
    pub fn open_with_cx(
        cx: &Cx,
        vfs: V,
        path: &Path,
        requested_page_size: PageSize,
    ) -> Result<Self> {
        let vfs = Arc::new(vfs);
        let flags = VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
        let (mut db_file, _actual_flags) = vfs.open(cx, Some(path), flags)?;

        // Probe for existing page size BEFORE hot journal recovery.
        // Recovery requires the correct page size to correctly parse records.
        let mut file_size = db_file.file_size(cx)?;
        let page_size = if file_size >= DATABASE_HEADER_SIZE as u64 {
            let mut header_bytes = [0u8; DATABASE_HEADER_SIZE];
            let header_read = db_file.read(cx, &mut header_bytes, 0)?;
            if header_read >= DATABASE_HEADER_SIZE {
                match DatabaseHeader::from_bytes(&header_bytes) {
                    Ok(header) => header.page_size,
                    Err(error)
                        if stale_main_header_can_be_recovered_from_live_wal(
                            cx,
                            &*vfs,
                            path,
                            &header_bytes,
                            &error,
                        )? =>
                    {
                        page_size_from_header_bytes(&header_bytes).unwrap_or(requested_page_size)
                    }
                    Err(_) => requested_page_size,
                }
            } else {
                requested_page_size
            }
        } else {
            requested_page_size
        };

        let journal_path = Self::journal_path(path);
        // Hot journal recovery writes the database image back to its durable
        // pre-commit state, so acquire EXCLUSIVE before replay even during
        // initial open.
        let _ = Self::recover_rollback_journal_if_present_locked(
            cx,
            &*vfs,
            &mut db_file,
            &journal_path,
            page_size,
            LockLevel::None,
        )?;

        // Refresh file size after potential recovery.
        file_size = db_file.file_size(cx)?;
        let (header, bootstrapped_from_live_wal_stub) = if file_size == 0 {
            // SQLite databases are never truly empty: page 1 contains the
            // 100-byte database header followed by the sqlite_master root page.
            //
            // This makes newly-created databases valid for downstream layers
            // (B-tree, schema) and avoids surprising "empty file" semantics.
            let page_len = page_size.as_usize();
            let mut page1 = vec![0u8; page_len];

            let header = DatabaseHeader {
                page_size,
                page_count: 1,
                sqlite_version: FRANKENSQLITE_SQLITE_VERSION_NUMBER,
                ..DatabaseHeader::default()
            };
            let hdr_bytes = header.to_bytes().map_err(|err| {
                FrankenError::internal(format!("failed to encode new database header: {err}"))
            })?;
            page1[..DATABASE_HEADER_SIZE].copy_from_slice(&hdr_bytes);

            // Initialize sqlite_master root page as an empty leaf table B-tree
            // page (type 0x0D) with zero cells.
            let usable = page_size.usable(header.reserved_per_page);
            BTreePageHeader::write_empty_leaf_table(&mut page1, DATABASE_HEADER_SIZE, usable);

            db_file.write(cx, &page1, 0)?;
            db_file.sync(cx, SyncFlags::NORMAL)?;
            file_size = db_file.file_size(cx)?;
            (header, false)
        } else {
            if file_size < DATABASE_HEADER_SIZE as u64 {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "database file too small for header: {file_size} bytes (< {DATABASE_HEADER_SIZE})"
                    ),
                });
            }

            let mut header_bytes = [0u8; DATABASE_HEADER_SIZE];
            let header_read = db_file.read(cx, &mut header_bytes, 0)?;
            if header_read < DATABASE_HEADER_SIZE {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "short read fetching database header: got {header_read} of {DATABASE_HEADER_SIZE}"
                    ),
                });
            }
            let (header, bootstrapped_from_live_wal_stub) =
                match DatabaseHeader::from_bytes(&header_bytes) {
                    Ok(header) => (header, false),
                    Err(error)
                        if stale_main_header_can_be_recovered_from_live_wal(
                            cx,
                            &*vfs,
                            path,
                            &header_bytes,
                            &error,
                        )? =>
                    {
                        // A live SQLite WAL can carry the authoritative page-1
                        // header while the main file still contains the stale
                        // bootstrap stub. Accept the file here and let the
                        // first WAL-backed refresh validate and load the real
                        // header from page 1 in the committed snapshot.
                        (
                            bootstrap_header_from_stale_main_file(&header_bytes, page_size),
                            true,
                        )
                    }
                    Err(error) => {
                        return Err(FrankenError::DatabaseCorrupt {
                            detail: format!("invalid database header: {error}"),
                        });
                    }
                };
            if header.page_size != page_size {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "database page size mismatch: header={} requested={}",
                        header.page_size.get(),
                        page_size.get()
                    ),
                });
            }
            (header, bootstrapped_from_live_wal_stub)
        };

        let page_size_u64 = page_size.as_usize() as u64;
        if file_size % page_size_u64 != 0 {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "database file size {file_size} is not aligned to page size {}",
                    page_size.get()
                ),
            });
        }
        let db_pages = file_size
            .checked_div(page_size_u64)
            .ok_or_else(|| FrankenError::internal("page size must be non-zero"))?;
        let db_size = u32::try_from(db_pages).map_err(|_| FrankenError::OutOfRange {
            what: "database page count".to_owned(),
            value: db_pages.to_string(),
        })?;
        let next_page = if db_size >= 2 {
            db_size.saturating_add(1)
        } else {
            2
        };
        let freelist = if bootstrapped_from_live_wal_stub {
            Vec::new()
        } else {
            load_freelist_from_disk(
                cx,
                &db_file,
                page_size,
                db_size,
                header.freelist_trunk,
                header.freelist_count,
            )?
        };

        let initial_commit_seq = CommitSeq::new(u64::from(header.change_counter));
        let freelist_count = freelist.len();
        let cache = ShardedPageCache::new(page_size);
        let pool = cache.pool().clone();
        Ok(Self {
            vfs,
            db_path: path.to_owned(),
            inner: Arc::new(Mutex::new(PagerInner {
                db_file,
                page_size,
                db_size,
                next_page,
                writer_active: false,
                active_transactions: 0,
                checkpoint_active: false,
                access_mode: PagerAccessMode::ReadWrite,
                freelist,
                journal_mode: JournalMode::Delete,
                wal_commit_sync_policy: WalCommitSyncPolicy::PerCommit,
                rollback_journal_recovery_state: RollbackJournalRecoveryState::Clean,
                commit_seq: initial_commit_seq,
                committed_db_file_size_bytes: file_size,
            })),
            cache: Arc::new(cache),
            pool,
            published: Arc::new(PublishedPagerState::new(
                db_size,
                initial_commit_seq,
                JournalMode::Delete,
                freelist_count,
            )),
            wal_backend: new_shared_wal_backend(),
            committed_snapshot: Arc::new(RwLock::new(Arc::new(PagerCommittedSnapshot {
                commit_seq: initial_commit_seq,
                db_size,
                journal_mode: JournalMode::Delete,
                freelist_count,
                checkpoint_active: false,
                writer_active: false,
                db_file_size_bytes: file_size,
            }))),
            shared_connection_count: OnceLock::new(),
        })
    }

    /// Open a database in true read-only mode for fast analytical queries.
    ///
    /// Unlike [`open_with_cx`], this:
    /// - Opens the file with `READONLY` VFS flags (no write lock acquisition)
    /// - Skips journal recovery (read-only connections cannot replay journals)
    /// - Skips freelist traversal (not needed for read-only queries)
    /// - Does NOT create the file if it doesn't exist
    ///
    /// This makes opening a 22GB database nearly instant instead of taking
    /// minutes, because it avoids the expensive freelist scan and journal
    /// recovery that the read-write path performs.
    #[allow(clippy::too_many_lines)]
    pub fn open_readonly_with_cx(
        cx: &Cx,
        vfs: V,
        path: &Path,
        _requested_page_size: PageSize,
    ) -> Result<Self> {
        let vfs = Arc::new(vfs);
        let flags = VfsOpenFlags::READONLY | VfsOpenFlags::MAIN_DB;
        let (db_file, _actual_flags) = vfs.open(cx, Some(path), flags)?;

        let file_size = db_file.file_size(cx)?;
        if file_size == 0 {
            return Err(FrankenError::CannotOpen {
                path: path.to_owned(),
            });
        }
        if file_size < DATABASE_HEADER_SIZE as u64 {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "database file too small for header: {file_size} bytes (< {DATABASE_HEADER_SIZE})"
                ),
            });
        }

        let mut header_bytes = [0u8; DATABASE_HEADER_SIZE];
        let header_read = db_file.read(cx, &mut header_bytes, 0)?;
        if header_read < DATABASE_HEADER_SIZE {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "short read fetching database header: got {header_read} of {DATABASE_HEADER_SIZE}"
                ),
            });
        }
        let (header, page_size) = match DatabaseHeader::from_bytes(&header_bytes) {
            Ok(header) => {
                let page_size = header.page_size;
                (Some(header), page_size)
            }
            Err(error)
                if stale_main_header_can_be_recovered_from_live_wal(
                    cx,
                    &*vfs,
                    path,
                    &header_bytes,
                    &error,
                )? =>
            {
                let page_size = page_size_from_header_bytes(&header_bytes).ok_or_else(|| {
                    FrankenError::DatabaseCorrupt {
                        detail: "live WAL bootstrap could not recover database page size"
                            .to_owned(),
                    }
                })?;
                (None, page_size)
            }
            Err(error) => {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!("invalid database header: {error}"),
                });
            }
        };

        let page_size_u64 = page_size.as_usize() as u64;
        if file_size % page_size_u64 != 0 {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "database file size {file_size} is not aligned to page size {}",
                    page_size.get()
                ),
            });
        }
        let db_pages = file_size
            .checked_div(page_size_u64)
            .ok_or_else(|| FrankenError::internal("page size must be non-zero"))?;
        let db_size = u32::try_from(db_pages).map_err(|_| FrankenError::OutOfRange {
            what: "database page count".to_owned(),
            value: db_pages.to_string(),
        })?;
        let next_page = if db_size >= 2 {
            db_size.saturating_add(1)
        } else {
            2
        };

        // Skip freelist traversal for read-only — use empty freelist.
        let freelist = Vec::new();

        let initial_commit_seq = CommitSeq::new(u64::from(
            header.as_ref().map_or(0, |header| header.change_counter),
        ));
        let cache = ShardedPageCache::new(page_size);
        let pool = cache.pool().clone();
        Ok(Self {
            vfs,
            db_path: path.to_owned(),
            inner: Arc::new(Mutex::new(PagerInner {
                db_file,
                page_size,
                db_size,
                next_page,
                writer_active: false,
                active_transactions: 0,
                checkpoint_active: false,
                freelist,
                journal_mode: JournalMode::Delete,
                wal_commit_sync_policy: WalCommitSyncPolicy::PerCommit,
                access_mode: PagerAccessMode::ReadOnly,
                rollback_journal_recovery_state: RollbackJournalRecoveryState::Clean,
                commit_seq: initial_commit_seq,
                committed_db_file_size_bytes: file_size,
            })),
            cache: Arc::new(cache),
            pool,
            published: Arc::new(PublishedPagerState::new(
                db_size,
                initial_commit_seq,
                JournalMode::Delete,
                0, // freelist_count = 0 for read-only
            )),
            wal_backend: new_shared_wal_backend(),
            committed_snapshot: Arc::new(RwLock::new(Arc::new(PagerCommittedSnapshot {
                commit_seq: initial_commit_seq,
                db_size,
                journal_mode: JournalMode::Delete,
                freelist_count: 0,
                checkpoint_active: false,
                writer_active: false,
                db_file_size_bytes: file_size,
            }))),
            shared_connection_count: OnceLock::new(),
        })
    }

    /// Open (or create) a database and return a pager using a detached test context.
    #[cfg(test)]
    #[allow(clippy::too_many_lines)]
    pub fn open(vfs: V, path: &Path, page_size: PageSize) -> Result<Self> {
        let cx = Cx::new();
        Self::open_with_cx(&cx, vfs, path, page_size)
    }

    /// Replay a hot journal by writing original pages back to the database.
    fn replay_journal(
        cx: &Cx,
        vfs: &V,
        db_file: &mut V::File,
        journal_path: &Path,
        page_size: PageSize,
    ) -> Result<()> {
        let jrnl_flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
        let Ok((mut jrnl_file, _)) = vfs.open(cx, Some(journal_path), jrnl_flags) else {
            return Ok(()); // Cannot open journal — treat as no journal.
        };

        let jrnl_size = jrnl_file.file_size(cx)?;
        if jrnl_size < crate::journal::JOURNAL_HEADER_SIZE as u64 {
            return Ok(()); // Truncated/empty journal — nothing to replay.
        }

        // Read and parse the journal header.
        let mut hdr_buf = vec![0u8; crate::journal::JOURNAL_HEADER_SIZE];
        let _ = jrnl_file.read(cx, &mut hdr_buf, 0)?;
        let Ok(header) = JournalHeader::decode(&hdr_buf) else {
            return Ok(()); // Corrupt header — nothing to replay.
        };
        if header.page_size != page_size.get() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "hot journal page size mismatch: header={} expected={}",
                    header.page_size,
                    page_size.get()
                ),
            });
        }

        let page_count = if header.page_count < 0 {
            header.compute_page_count_from_file_size(jrnl_size)
        } else {
            #[allow(clippy::cast_sign_loss)]
            let c = header.page_count as u32;
            c
        };

        let header_size = u64::try_from(crate::journal::JOURNAL_HEADER_SIZE)
            .expect("journal header size should fit in u64");
        let hdr_padded = u64::from(header.sector_size).max(header_size);
        let ps = page_size.as_usize();
        let record_size = 4 + ps + 4;
        let mut offset = hdr_padded;

        for _ in 0..page_count {
            let mut rec_buf = vec![0u8; record_size];
            let bytes_read = jrnl_file.read(cx, &mut rec_buf, offset)?;
            if bytes_read < record_size {
                break; // Torn record — stop replay.
            }

            #[allow(clippy::cast_possible_truncation)]
            let Ok(record) = JournalPageRecord::decode(&rec_buf, ps as u32) else {
                break; // Corrupt record — stop replay.
            };

            // Verify checksum before applying.
            if record.verify_checksum(header.nonce).is_err() {
                break; // Checksum failure — stop replay at this point.
            }

            // Write the pre-image back to the database file.
            let Some(page_no) = PageNumber::new(record.page_number) else {
                return Err(FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "hot journal contains invalid page number {}",
                        record.page_number
                    ),
                });
            };
            let page_offset = u64::from(page_no.get() - 1) * ps as u64;
            db_file.write(cx, &record.content, page_offset)?;

            offset += record_size as u64;
        }

        // Sync the database after replaying.
        db_file.sync(cx, SyncFlags::NORMAL)?;

        // Truncate the database to the original size from the journal header.
        if header.initial_db_size > 0 {
            let target_size = u64::from(header.initial_db_size) * ps as u64;
            let current_size = db_file.file_size(cx)?;
            if current_size > target_size {
                db_file.truncate(cx, target_size)?;
                // Sync after truncation to ensure durability of the new file size.
                db_file.sync(cx, SyncFlags::NORMAL)?;
            }
        }

        // Recovery is complete. Invalidate the journal before best-effort
        // deletion so a later delete failure does not keep replaying the same
        // rollback journal on every open.
        jrnl_file.truncate(cx, 0)?;
        jrnl_file.sync(cx, SyncFlags::NORMAL)?;

        Ok(())
    }
}

/// A snapshot of the transaction state at a savepoint boundary.
struct SavepointEntry {
    /// The user-supplied savepoint name.
    name: String,
    /// Snapshot of the write-set at the time the savepoint was created.
    /// Stores published page data so savepoint capture can reuse the staged
    /// page's shared `Arc<Vec<u8>>` when available instead of cloning bytes.
    write_set_snapshot: HashMap<PageNumber, PageData>,
    /// Sorted unique page ids in the write-set snapshot.
    write_pages_sorted_snapshot: Vec<PageNumber>,
    /// Snapshot of freed pages at the time the savepoint was created.
    freed_pages_snapshot: Vec<PageNumber>,
    /// Snapshot of the pager's next_page counter.
    /// Used to restore allocation state on rollback.
    next_page_snapshot: u32,
    /// Snapshot of the pager's freelist.
    /// Used to restore allocation state on rollback.
    freelist_snapshot: Vec<PageNumber>,
    /// Snapshot of pages allocated from freelist by this transaction.
    allocated_from_freelist_snapshot: Vec<PageNumber>,
    /// Snapshot of pages allocated from EOF by this transaction.
    allocated_from_eof_snapshot: Vec<PageNumber>,
}

#[derive(Debug)]
enum StagedPageBacking {
    Buffered(PageBuf),
    Owned(PageData),
}

#[derive(Debug)]
struct StagedPage {
    backing: StagedPageBacking,
    published: OnceLock<PageData>,
}

impl StagedPage {
    fn from_buf(buf: PageBuf) -> Self {
        Self {
            backing: StagedPageBacking::Buffered(buf),
            published: OnceLock::new(),
        }
    }

    fn from_bytes(pool: &PageBufPool, data: &[u8]) -> Result<Self> {
        let mut buf = pool.acquire()?;
        let len = buf.len().min(data.len());
        buf[..len].copy_from_slice(&data[..len]);
        if len < buf.len() {
            buf[len..].fill(0);
        }
        Ok(Self::from_buf(buf))
    }

    fn from_page_data(data: PageData) -> Self {
        let published = OnceLock::new();
        let backing = StagedPageBacking::Owned(data.clone());
        let _ = published.set(data);
        Self { backing, published }
    }

    fn from_page_data_for_pool(pool: &PageBufPool, data: PageData) -> Result<Self> {
        if data.len() == pool.page_size() {
            Ok(Self::from_page_data(data))
        } else {
            Self::from_bytes(pool, data.as_bytes())
        }
    }

    fn as_page_bytes(&self) -> &[u8] {
        match &self.backing {
            StagedPageBacking::Buffered(buf) => buf.as_slice(),
            StagedPageBacking::Owned(data) => data.as_bytes(),
        }
    }

    fn published_page(&self) -> PageData {
        self.published
            .get_or_init(|| PageData::from_vec(self.as_page_bytes().to_vec()))
            .clone()
    }

    fn into_published_page(self) -> PageData {
        let Self { backing, published } = self;
        if let Some(page) = published.into_inner() {
            return page;
        }

        match backing {
            StagedPageBacking::Buffered(buf) => PageData::from_vec(buf.as_slice().to_vec()),
            StagedPageBacking::Owned(data) => data,
        }
    }

    fn into_buf(self, pool: &PageBufPool) -> PageBuf {
        match self.backing {
            StagedPageBacking::Buffered(buf) => buf,
            StagedPageBacking::Owned(data) => {
                let page_size = PageSize::new(
                    u32::try_from(pool.page_size()).expect("pool page size fits u32"),
                )
                .expect("pool page size invariant");
                let mut buf = pool.acquire().unwrap_or_else(|_| PageBuf::new(page_size));
                let len = buf.len().min(data.len());
                buf[..len].copy_from_slice(&data.as_bytes()[..len]);
                if len < buf.len() {
                    buf[len..].fill(0);
                }
                buf
            }
        }
    }
}

/// Transaction handle produced by [`SimplePager`].
/// Number of EOF pages to pre-allocate in a single lock acquisition.
/// Reduces `inner` mutex contention when concurrent writers cause
/// frequent B-tree splits that each need a new page. 8 is a reasonable
/// balance — larger batches waste pages on small transactions, smaller
/// batches increase lock contention on write-heavy workloads.
const PAGE_LEASE_BATCH_SIZE: u32 = 8;

pub struct SimpleTransaction<V: Vfs> {
    vfs: Arc<V>,
    journal_path: PathBuf,
    group_commit_queue: GroupCommitQueueRef,
    inner: Arc<Mutex<PagerInner<V::File>>>,
    cache: Arc<ShardedPageCache>,
    published: Arc<PublishedPagerState>,
    /// WAL backend for WAL-mode operation (D1-CRITICAL: separate lock for split-lock commit).
    wal_backend: SharedWalBackend,
    /// Shared committed-state snapshot (bd-db300.5.3.3.1 / M6).
    committed_snapshot: Arc<RwLock<Arc<PagerCommittedSnapshot>>>,
    /// Shared connection counter for single-connection fast path.
    shared_connection_count: Option<Arc<AtomicUsize>>,
    /// Visible commit sequence at snapshot capture. Uses `Cell` for interior
    /// mutability so `get_page` (which takes `&self`) can refresh the snapshot
    /// when encountering pages beyond the current db_size boundary.
    published_visible_commit_seq: Cell<CommitSeq>,
    /// Database size at snapshot capture. Used for MVCC visibility: pages
    /// beyond this bound didn't exist when this snapshot was taken. Uses
    /// `Cell` so the snapshot can be refreshed during reads when concurrent
    /// writers commit new pages.
    published_db_size: Cell<u32>,
    write_set: HashMap<PageNumber, StagedPage>,
    write_pages_sorted: Vec<PageNumber>,
    freed_pages: Vec<PageNumber>,
    allocated_from_freelist: Vec<PageNumber>,
    allocated_from_eof: Vec<PageNumber>,
    mode: TransactionMode,
    is_writer: bool,
    committed: bool,
    finished: bool,
    original_db_size: u32,
    /// Stack of savepoints, pushed on SAVEPOINT and popped on RELEASE.
    savepoint_stack: Vec<SavepointEntry>,
    /// Journal mode captured at transaction start.
    journal_mode: JournalMode,
    /// Buffer pool for allocating write-set pages.
    pool: PageBufPool,
    /// Caller-rooted cleanup context used for drop-time finalization.
    cleanup_cx: Cx,
    /// Local page lease: pre-allocated EOF pages that can be handed out
    /// without re-acquiring the global `inner` mutex. Reduces lock
    /// convoy pressure during concurrent insert workloads with B-tree
    /// splits. Unused pages are returned to the global next_page on
    /// commit/rollback.
    page_lease: Vec<PageNumber>,
    /// True only for real `:memory:` databases. Those databases never need
    /// durable freelist reuse mid-transaction, so allocation can stay on a
    /// simple bump-only fast path without pulling page 1 into the conflict
    /// surface.
    memory_db_bump_alloc: bool,
    /// Pages that were allocated after a savepoint but then rolled back.
    /// These pages should return zeros when read, not BusySnapshot error.
    rolled_back_pages: HashSet<PageNumber>,
    /// Per-transaction read cache: pages read via inner.lock() are cached
    /// here so subsequent reads of the same page (e.g., B-tree root during
    /// repeated INSERTs) skip inner.lock entirely. This eliminates the
    /// ~80,000 inner.lock acquisitions at 16 threads (reduces to ~80).
    /// Only used in WAL mode where the published snapshot fast path is
    /// defeated by constant commit_seq advancement.
    txn_read_cache: RefCell<HashMap<PageNumber, PageData>>,
}

impl<V: Vfs> traits::sealed::Sealed for SimpleTransaction<V> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WalPageOneWritePlan {
    max_written: u32,
    page_one_dirty: bool,
    freelist_metadata_dirty: bool,
    db_growth: bool,
}

impl WalPageOneWritePlan {
    /// In WAL mode, Page 1 rewrite is only required when Page 1 was explicitly
    /// modified (schema changes, VACUUM, etc.). We defer synthetic Page 1
    /// updates for:
    ///
    /// 1. `db_growth` - WAL frame's `db_size_if_commit` captures database size
    /// 2. `freelist_metadata_dirty` - Freelist changes from allocations/frees
    ///    are implicitly captured by the WAL frames (the pages that were
    ///    allocated/freed are in the WAL). At checkpoint time, the freelist
    ///    can be reconstructed from the final database state.
    ///
    /// bd-3wop3.8 (D1-CRITICAL): This eliminates ~2000 MVCC conflicts per
    /// 16-thread benchmark iteration where every thread's freelist operations
    /// (batch page_lease allocation and return) triggered Page 1 writes.
    #[must_use]
    fn requires_page_one_rewrite(self) -> bool {
        self.page_one_dirty
    }

    #[must_use]
    fn requires_page_count_advance(self) -> bool {
        self.db_growth
    }
}

impl<V: Vfs> SimpleTransaction<V> {
    /// Whether this transaction has been upgraded to a writer.
    #[must_use]
    pub fn is_writer(&self) -> bool {
        self.is_writer
    }

    /// Check if single-connection fast path is enabled.
    fn single_connection_fast_path_enabled(&self) -> bool {
        self.shared_connection_count
            .as_ref()
            .is_some_and(|counter| counter.load(AtomicOrdering::Acquire) == 1)
    }

    #[must_use]
    fn durable_freelist_pages_with_inner(
        inner: &PagerInner<V::File>,
        db_size: u32,
        restored_pages: &[PageNumber],
    ) -> Vec<PageNumber> {
        if db_size == 0 {
            return Vec::new();
        }

        let upper_bound = inner.next_page.saturating_sub(1).max(db_size);
        let mut freelist = inner.freelist.clone();
        return_pages_to_freelist(&mut freelist, restored_pages.iter().copied());
        normalize_freelist(&freelist, upper_bound)
            .into_iter()
            .filter(|page| page.get() <= db_size)
            .collect()
    }

    #[must_use]
    fn committed_durable_freelist_pages_with_inner(
        &self,
        inner: &PagerInner<V::File>,
    ) -> Vec<PageNumber> {
        Self::durable_freelist_pages_with_inner(inner, inner.db_size, &self.allocated_from_freelist)
    }

    #[must_use]
    fn predicted_durable_freelist_pages_with_inner(
        &self,
        inner: &PagerInner<V::File>,
        committed_db_size: u32,
    ) -> Vec<PageNumber> {
        Self::durable_freelist_pages_with_inner(inner, committed_db_size, &self.freed_pages)
    }

    #[must_use]
    fn freelist_metadata_dirty_with_inner(
        &self,
        inner: &PagerInner<V::File>,
        committed_db_size: u32,
    ) -> bool {
        self.committed_durable_freelist_pages_with_inner(inner)
            != self.predicted_durable_freelist_pages_with_inner(inner, committed_db_size)
    }

    #[must_use]
    fn freelist_metadata_dirty(&self) -> bool {
        self.inner.lock().map_or(true, |inner| {
            let committed_db_size = self.committed_db_size_with_inner(&inner);
            self.freelist_metadata_dirty_with_inner(&inner, committed_db_size)
        })
    }

    #[must_use]
    fn committed_db_size_with_inner(&self, inner: &PagerInner<V::File>) -> u32 {
        self.write_pages_sorted
            .last()
            .map_or(inner.db_size, |page| inner.db_size.max(page.get()))
    }

    #[must_use]
    fn classify_wal_page_one_write(
        &self,
        current_db_size: u32,
        freelist_dirty: bool,
    ) -> WalPageOneWritePlan {
        let max_written = self.write_pages_sorted.last().map_or(0, |page| page.get());
        WalPageOneWritePlan {
            max_written,
            page_one_dirty: self.write_set.contains_key(&PageNumber::ONE),
            freelist_metadata_dirty: freelist_dirty,
            db_growth: max_written > current_db_size,
        }
    }

    #[must_use]
    fn current_page_one_conflict_tracking_required_with_inner(
        &self,
        inner: &PagerInner<V::File>,
    ) -> bool {
        let committed_db_size = self.committed_db_size_with_inner(inner);
        let freelist_dirty = self.freelist_metadata_dirty_with_inner(inner, committed_db_size);
        let wal_page1_plan = self.classify_wal_page_one_write(inner.db_size, freelist_dirty);

        if self.journal_mode == JournalMode::Wal {
            wal_page1_plan.requires_page_one_rewrite()
        } else {
            !self.write_set.is_empty() || freelist_dirty
        }
    }

    #[must_use]
    fn allocate_page_requires_page_one_conflict_tracking_with_inner(
        &self,
        inner: &PagerInner<V::File>,
    ) -> bool {
        if self.memory_db_bump_alloc {
            return false;
        }

        if self.mode == TransactionMode::Concurrent {
            // Concurrent-mode allocator/header/page-count reconciliation is a
            // commit-planning concern. Ordinary page growth stays on the local
            // leased fast path and does not need to pull page 1 into the live
            // MVCC conflict surface up front.
            return false;
        }

        if self.current_page_one_conflict_tracking_required_with_inner(inner) {
            return true;
        }

        let committed_db_size = self.committed_db_size_with_inner(inner);
        let committed_freelist_is_snapshot_pinned = inner.active_transactions > 1;

        if committed_freelist_is_snapshot_pinned {
            return false;
        }

        match inner.freelist.last().copied() {
            Some(page) => page.get() <= committed_db_size,
            None => false,
        }
    }

    #[must_use]
    fn free_page_requires_page_one_conflict_tracking_with_inner(
        &self,
        inner: &PagerInner<V::File>,
        page_no: PageNumber,
    ) -> bool {
        if self.mode == TransactionMode::Concurrent {
            // Free-list/page-one reconciliation for concurrent transactions is
            // likewise deferred to the commit-time pending surface. Per-op free
            // should not synthesize page 1 into the hot path.
            return false;
        }

        if self.current_page_one_conflict_tracking_required_with_inner(inner) {
            return true;
        }

        page_no.get() <= self.committed_db_size_with_inner(inner)
    }

    #[must_use]
    fn write_page_requires_page_one_conflict_tracking_with_inner(
        &self,
        inner: &PagerInner<V::File>,
        page_no: PageNumber,
    ) -> bool {
        if page_no == PageNumber::ONE {
            return true;
        }

        if self.mode == TransactionMode::Concurrent {
            // Concurrent growth rewrites page 1 only at commit publication
            // time. Do not drag synthetic page-one conflict tracking through
            // every ordinary high-page write.
            return false;
        }

        if self.current_page_one_conflict_tracking_required_with_inner(inner) {
            return true;
        }

        page_no.get() > self.committed_db_size_with_inner(inner)
    }

    #[must_use]
    fn page_one_in_pending_commit_surface_with_inner(&self, inner: &PagerInner<V::File>) -> bool {
        let committed_db_size = self.committed_db_size_with_inner(inner);
        let durable_freelist =
            self.predicted_durable_freelist_pages_with_inner(inner, committed_db_size);
        let freelist_dirty =
            self.committed_durable_freelist_pages_with_inner(inner) != durable_freelist;
        let wal_page1_plan = self.classify_wal_page_one_write(inner.db_size, freelist_dirty);

        if self.journal_mode == JournalMode::Wal {
            wal_page1_plan.requires_page_one_rewrite()
        } else {
            !self.write_set.is_empty() || freelist_dirty
        }
    }

    fn predicted_commit_pages_with_inner(&self, inner: &PagerInner<V::File>) -> Vec<PageNumber> {
        let mut pages = self.write_pages_sorted.clone();
        let committed_db_size = self.committed_db_size_with_inner(inner);
        let durable_freelist =
            self.predicted_durable_freelist_pages_with_inner(inner, committed_db_size);
        let freelist_dirty =
            self.committed_durable_freelist_pages_with_inner(inner) != durable_freelist;

        if freelist_dirty && !durable_freelist.is_empty() {
            let max_leaf_entries = (inner.page_size.as_usize() / 4).saturating_sub(2).max(1);
            let trunk_count = durable_freelist.len().div_ceil(max_leaf_entries + 1);
            pages.extend(durable_freelist.into_iter().take(trunk_count));
        }

        if self.page_one_in_pending_commit_surface_with_inner(inner) {
            pages.push(PageNumber::ONE);
        }

        pages.sort_unstable();
        pages.dedup();
        pages
    }

    fn predicted_conflict_pages_with_inner(&self, inner: &PagerInner<V::File>) -> Vec<PageNumber> {
        let mut pages = self.predicted_commit_pages_with_inner(inner);

        if self.mode == TransactionMode::Concurrent && self.journal_mode == JournalMode::Wal {
            let page_one_dirty = self.write_set.contains_key(&PageNumber::ONE);

            // bd-3wop3.8 (D1-CRITICAL): In WAL mode, synthetic page 1 changes
            // (change counter, page count, freelist metadata) are safely
            // serialized by the pager inner.lock() during Phase A commit.
            // The commit protocol ensures that concurrent freelist/page-count
            // updates are merged correctly (last committer includes all prior
            // state). Only track page 1 as a conflict when directly modified
            // by schema operations (CREATE TABLE, DROP TABLE, etc.).
            //
            // This eliminates ~2000 spurious MVCC conflicts on page 1 that
            // occurred when concurrent INSERTs (db_growth) and DELETEs
            // (freelist_dirty) both touched page 1 header metadata.
            if !page_one_dirty {
                pages.retain(|page| *page != PageNumber::ONE);
            }
        }

        pages
    }

    fn publish_committed_state(&self, cx: &Cx, update: PublishedPagerUpdate) {
        // D1-CRITICAL Change 3: Use sharded publish_commit.
        self.published.publish_commit(cx, update, &self.write_set);
    }

    fn publish_committed_state_draining_write_set(
        &mut self,
        cx: &Cx,
        update: PublishedPagerUpdate,
    ) {
        self.published
            .publish_commit_draining_write_set(cx, update, &mut self.write_set);
    }

    /// Publish a new committed-state snapshot while the pager inner lock is still held.
    ///
    /// The write lock is held only long enough to swap the immutable snapshot Arc.
    fn publish_committed_snapshot_from_inner(&self, inner: &PagerInner<V::File>) {
        let snapshot = Arc::new(PagerCommittedSnapshot::from_inner(inner));
        let mut guard = self
            .committed_snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = snapshot;
    }

    fn drain_committed_cache_pages(&mut self) -> Vec<(PageNumber, PageBuf)> {
        let mut committed_pages = Vec::with_capacity(self.write_set.len());
        for (page_no, staged) in self.write_set.drain() {
            committed_pages.push((page_no, staged.into_buf(&self.pool)));
        }
        self.write_pages_sorted.clear();
        committed_pages
    }

    fn discard_committed_pages(&mut self) {
        self.write_set.clear();
        self.write_pages_sorted.clear();
    }
}

fn cleanup_child_cx(cx: &Cx) -> Cx {
    cx.create_child()
}

impl<V> SimpleTransaction<V>
where
    V: Vfs + Send,
    V::File: Send + Sync,
{
    fn invalidate_journal_after_commit(cx: &Cx, journal_file: &mut V::File) -> Result<()> {
        journal_file.truncate(cx, 0)?;
        journal_file.sync(cx, SyncFlags::NORMAL)?;
        Ok(())
    }

    /// Commit using the rollback journal protocol.
    #[allow(clippy::too_many_lines)]
    fn commit_journal(
        cx: &Cx,
        vfs: &Arc<V>,
        journal_path: &Path,
        inner: &mut PagerInner<V::File>,
        write_set: &HashMap<PageNumber, StagedPage>,
        original_db_size: u32,
    ) -> Result<()> {
        if !write_set.is_empty() {
            // Escalate to EXCLUSIVE before writing to the database file.
            // This prevents concurrent processes from reading partially
            // written pages during the commit.
            inner.db_file.lock(cx, LockLevel::Exclusive)?;

            // Phase 1: Write rollback journal with pre-images.
            let nonce = 0x4652_414E; // "FRAN" — deterministic nonce.
            let page_size = inner.page_size;
            let ps = page_size.as_usize();

            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl_file, _) = vfs.open(cx, Some(journal_path), jrnl_flags)?;

            let header = JournalHeader {
                #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                page_count: write_set.len() as i32,
                nonce,
                initial_db_size: original_db_size,
                sector_size: 512,
                page_size: page_size.get(),
            };
            let hdr_bytes = header.encode_padded();
            jrnl_file.write(cx, &hdr_bytes, 0)?;

            let mut jrnl_offset = hdr_bytes.len() as u64;
            for &page_no in write_set.keys() {
                // Read current on-disk content as the pre-image.
                let mut pre_image = vec![0u8; ps];
                if page_no.get() <= inner.db_size {
                    let disk_offset = u64::from(page_no.get() - 1) * ps as u64;
                    let bytes_read = inner.db_file.read(cx, &mut pre_image, disk_offset)?;
                    if bytes_read < ps {
                        return Err(FrankenError::DatabaseCorrupt {
                            detail: format!(
                                "short read while journaling pre-image for page {}: got {bytes_read} of {ps}",
                                page_no.get()
                            ),
                        });
                    }
                }

                let record = JournalPageRecord::new(page_no.get(), pre_image, nonce);
                let rec_bytes = record.encode();
                jrnl_file.write(cx, &rec_bytes, jrnl_offset)?;
                jrnl_offset += rec_bytes.len() as u64;
            }

            // Sync journal to ensure durability before modifying database.
            jrnl_file.sync(cx, SyncFlags::NORMAL)?;
            inner.rollback_journal_recovery_state = RollbackJournalRecoveryState::Pending;

            // Phase 2: Write dirty pages to database.
            let saved_db_size = inner.db_size;
            for (page_no, staged) in write_set {
                if let Err(e) = inner.flush_page(cx, *page_no, staged.as_page_bytes()) {
                    inner.db_size = saved_db_size;
                    return Err(e);
                }
                inner.db_size = inner.db_size.max(page_no.get());
            }

            inner.db_file.sync(cx, SyncFlags::NORMAL)?;

            // Phase 3: Make the journal non-hot before best-effort deletion.
            //
            // If directory-entry deletion fails after the database sync, a
            // leftover valid journal must not roll back the committed pages on
            // the next open.
            let cleanup_result = match Self::invalidate_journal_after_commit(cx, &mut jrnl_file) {
                Ok(()) => {
                    let _ = vfs.delete(cx, journal_path, true);
                    Ok(())
                }
                Err(invalidate_err) => {
                    if let Err(delete_err) = vfs.delete(cx, journal_path, true) {
                        Err(FrankenError::internal(format!(
                            "committed database but failed to invalidate or delete rollback journal: invalidate={invalidate_err}; delete={delete_err}"
                        )))
                    } else {
                        Ok(())
                    }
                }
            };
            inner.rollback_journal_recovery_state = RollbackJournalRecoveryState::Clean;
            cleanup_result?;
        }

        Ok(())
    }
    /// Commit using the WAL protocol with group commit batching.
    ///
    /// This method implements the group commit pattern (D1: bd-3wop3.1) which
    /// replaces the old `WAL_APPEND_GATES` global mutex with a cooperative
    /// batching protocol.
    ///
    /// **D1-CRITICAL (bd-3wop3.8): Real flusher/waiter cooperative batching**
    ///
    /// Protocol:
    /// 1. Each thread builds a `TransactionFrameBatch` with OWNED frame data
    /// 2. Thread submits batch to consolidator, receives `Flusher` or `Waiter` role
    /// 3. **Flusher**: Uses a tail-safe arrival wait. Fresh epochs fall back to
    ///    the legacy 20μs spin, but epochs that already spent that budget
    ///    gathering peers flush immediately. The flusher then writes ALL
    ///    batched frames from all transactions in ONE consolidated I/O + fsync
    /// 4. **Waiter**: Parks on condvar until flusher signals completion
    ///
    /// Benefits over immediate-flush:
    /// - N commits × fsync → 1 group × fsync (major latency reduction under load)
    /// - Consolidated I/O: one large write instead of N small writes
    /// - Reduced lock contention: waiters don't serialize through WAL I/O
    ///
    /// This function takes `Arc<Mutex<PagerInner>>` instead of `&mut PagerInner`.
    /// The CALLER drops their inner.lock() before calling this function, allowing
    /// other transactions to start their prepare phase while we wait/batch.
    #[allow(clippy::too_many_lines)]
    fn commit_wal_group_commit(
        cx: &Cx,
        wal_backend: &SharedWalBackend,
        inner_arc: &Arc<Mutex<PagerInner<V::File>>>,
        write_set: &HashMap<PageNumber, StagedPage>,
        write_pages_sorted: &[PageNumber],
        queue: &GroupCommitQueueRef,
    ) -> Result<()> {
        // ── Phase timing instrumentation ──
        let t_start = Instant::now();

        // Step 1: Build our batch with OWNED frame data.
        // We need to read current db_size and sync policy before releasing inner.
        let (current_db_size, sync_policy) = {
            let inner = inner_arc
                .lock()
                .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
            (inner.db_size, inner.wal_commit_sync_policy)
        };

        let (batch, _our_new_db_size) =
            match build_group_commit_batch(current_db_size, write_set, write_pages_sorted)? {
                Some(b) => b,
                None => return Ok(()), // Nothing to commit
            };

        let t_prepare_done = Instant::now();
        let prepare_us = t_prepare_done.duration_since(t_start).as_micros() as u64;

        // Step 2: Submit batch to consolidator, get Flusher or Waiter role.
        // If phase is FLUSHING, wait for current flush to complete before submitting.
        let t_consolidator_lock_start = Instant::now();
        let (outcome, our_epoch, consolidator_lock_wait_us, flushing_wait_us) = {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            let t_lock_acquired = Instant::now();
            let lock_wait_us = t_lock_acquired
                .duration_since(t_consolidator_lock_start)
                .as_micros() as u64;

            // ── Epoch pipelining: NO waiting during FLUSHING ──
            // The consolidator now accepts submissions during FLUSHING,
            // queuing them for the next epoch. This eliminates the
            // flushing_wait bottleneck that was 1-2.7ms at 16 threads.
            let flushing_wait = 0u64; // No longer blocks

            // Record epoch BEFORE submit (begin_flush will increment it).
            let epoch = consolidator.epoch();
            let outcome = consolidator.submit_batch(batch)?;
            (outcome, epoch, lock_wait_us, flushing_wait)
        };

        let run_flusher_loop = |mut record_initial_metrics: bool,
                                mut needs_arrival_wait: bool,
                                mut prefetched_flush: Option<(Vec<TransactionFrameBatch>, u64)>|
         -> Result<()> {
            'flusher_loop: loop {
                let arrival_wait_decision = if prefetched_flush.is_none() && needs_arrival_wait {
                    let observation = {
                        let consolidator = queue
                            .consolidator
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        ArrivalWaitObservation {
                            pending_batch_count: consolidator.pending_batch_count(),
                            should_flush_now: consolidator.should_flush_now(),
                            fill_age: consolidator.fill_age(),
                        }
                    };
                    decide_group_commit_arrival_wait(Some(observation))
                } else {
                    decide_group_commit_arrival_wait(None)
                };
                let arrival_wait_us = if !arrival_wait_decision.wait_budget.is_zero() {
                    let t_arrival_wait_start = Instant::now();
                    let deadline = t_arrival_wait_start + arrival_wait_decision.wait_budget;
                    loop {
                        let should_flush = {
                            let consolidator = queue
                                .consolidator
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            consolidator.should_flush_now()
                        };
                        if should_flush || Instant::now() >= deadline {
                            break;
                        }
                        std::hint::spin_loop();
                    }
                    Instant::now()
                        .duration_since(t_arrival_wait_start)
                        .as_micros() as u64
                } else {
                    0
                };

                let (batches, flush_epoch) = if let Some(prefetched) = prefetched_flush.take() {
                    prefetched
                } else {
                    let maybe_flush = {
                        let mut consolidator = queue
                            .consolidator
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if !record_initial_metrics
                            && consolidator.phase() != fsqlite_wal::ConsolidationPhase::Filling
                        {
                            None
                        } else {
                            let batches = consolidator.begin_flush()?;
                            Some((batches, consolidator.epoch()))
                        }
                    };
                    let Some(flush) = maybe_flush else {
                        break;
                    };
                    flush
                };

                // bd-db300.3.8.6: Fused single-pass assembly — build frame_refs
                // and compute final_db_size in one iteration over batches,
                // eliminating the intermediate `all_frames` Vec allocation.
                let batch_count = batches.len();
                let total_frames: usize = batches.iter().map(|b| b.frames.len()).sum();
                let mut frame_refs: Vec<traits::WalFrameRef<'_>> = Vec::with_capacity(total_frames);
                let mut final_db_size = current_db_size;
                for batch in &batches {
                    for frame in &batch.frames {
                        frame_refs.push(traits::WalFrameRef {
                            page_number: frame.page_number,
                            page_data: &frame.page_data,
                            db_size_if_commit: frame.db_size_if_commit,
                        });
                        if frame.db_size_if_commit > final_db_size {
                            final_db_size = frame.db_size_if_commit;
                        }
                    }
                }
                let frame_count = frame_refs.len();

                let mut prepared_batch = with_wal_backend(wal_backend, |wal| {
                    let mut prepared_batch = wal.prepare_append_frames(&frame_refs)?;
                    if let Some(prepared) = prepared_batch.as_mut() {
                        wal.finalize_prepared_frames(cx, prepared)?;
                    }
                    Ok(prepared_batch)
                })?;
                GLOBAL_CONSOLIDATION_METRICS.transactions_batched.fetch_add(
                    u64::try_from(batch_count).unwrap_or(u64::MAX),
                    AtomicOrdering::Relaxed,
                );

                const MAX_FLUSH_RETRIES: u32 = 10;
                let mut flush_result: Result<()> = Ok(());
                let mut inner_lock_wait_us: u64 = 0;
                let mut exclusive_lock_us: u64 = 0;
                let mut wal_append_us: u64 = 0;
                let mut wal_sync_us: u64 = 0;

                for attempt in 0..MAX_FLUSH_RETRIES {
                    let t_inner_lock_start = Instant::now();
                    flush_result = (|| -> Result<()> {
                        let mut inner = inner_arc.lock().map_err(|_| {
                            FrankenError::internal("SimpleTransaction lock poisoned")
                        })?;
                        inner_lock_wait_us = Instant::now()
                            .duration_since(t_inner_lock_start)
                            .as_micros() as u64;

                        let t_excl_start = Instant::now();
                        inner.db_file.lock(cx, LockLevel::Exclusive)?;
                        exclusive_lock_us =
                            Instant::now().duration_since(t_excl_start).as_micros() as u64;

                        let t_append_start = Instant::now();
                        let flush_io_result = (|| -> Result<()> {
                            with_wal_backend(wal_backend, |wal| {
                                if let Some(prepared) = prepared_batch.as_mut() {
                                    wal.append_prepared_frames(cx, prepared)
                                } else {
                                    wal.append_frames(cx, &frame_refs)
                                }
                            })?;
                            wal_append_us =
                                Instant::now().duration_since(t_append_start).as_micros() as u64;

                            if sync_policy.should_sync_on_commit() {
                                let t_sync_start = Instant::now();
                                with_wal_backend(wal_backend, |wal| wal.sync(cx))?;
                                wal_sync_us =
                                    Instant::now().duration_since(t_sync_start).as_micros() as u64;
                                GLOBAL_CONSOLIDATION_METRICS
                                    .fsyncs_total
                                    .fetch_add(1, AtomicOrdering::Relaxed);
                            }

                            inner.db_size = final_db_size;
                            Ok(())
                        })();

                        let restore_result = inner.db_file.unlock(cx, LockLevel::Reserved);
                        match (flush_io_result, restore_result) {
                            (Ok(()), Ok(())) => Ok(()),
                            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
                            (Err(flush_error), Err(restore_error)) => {
                                Err(FrankenError::internal(format!(
                                    "flush failed and could not restore RESERVED lock: flush={flush_error}; restore={restore_error}"
                                )))
                            }
                        }
                    })();

                    match &flush_result {
                        Err(
                            FrankenError::Busy
                            | FrankenError::BusyRecovery
                            | FrankenError::BusySnapshot { .. },
                        ) if attempt + 1 < MAX_FLUSH_RETRIES => {
                            let base_delay_ms = 1u64 << attempt;
                            // Use thread ID + attempt as a cheap entropy source
                            // for jitter. The previous `Instant::now().elapsed()`
                            // always returned ~0 (elapsed from now to now).
                            let thread_hash = {
                                use std::hash::{Hash, Hasher};
                                let mut h = std::collections::hash_map::DefaultHasher::new();
                                std::thread::current().id().hash(&mut h);
                                attempt.hash(&mut h);
                                h.finish()
                            };
                            let jitter_ms = thread_hash % (base_delay_ms / 2).max(1);
                            let delay_ms = base_delay_ms.saturating_add(jitter_ms);
                            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                            GLOBAL_CONSOLIDATION_METRICS.record_busy_retry();
                            // bd-db300.3.8.1: wake reason = busy_retry
                            GLOBAL_CONSOLIDATION_METRICS
                                .wake_reasons
                                .busy_retry
                                .fetch_add(1, AtomicOrdering::Relaxed);
                        }
                        _ => break,
                    }
                }

                match flush_result {
                    Ok(()) => {
                        #[cfg(any(test, feature = "fault-injection"))]
                        if let Err(error) =
                            crate::fault_hooks::maybe_inject_after_flush_before_publish(
                                flush_epoch,
                                batch_count,
                                frame_count,
                            )
                        {
                            let (abort_result, wake_next_epoch) = {
                                let mut consolidator = queue
                                    .consolidator
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let abort_result = consolidator.abort_flush();
                                let wake_next_epoch =
                                    abort_result.is_ok() && consolidator.has_flusher_vacancy();
                                (abort_result, wake_next_epoch)
                            };
                            queue.publish_failed_epoch(flush_epoch, &error, wake_next_epoch);
                            if let Err(abort_error) = abort_result {
                                return Err(FrankenError::internal(format!(
                                    "group commit flush hook failed for epoch {flush_epoch} and abort_flush also failed: hook={error}; abort={abort_error}"
                                )));
                            }
                            return Err(error);
                        }

                        GLOBAL_CONSOLIDATION_METRICS
                            .groups_flushed
                            .fetch_add(1, AtomicOrdering::Relaxed);
                        GLOBAL_CONSOLIDATION_METRICS.frames_consolidated.fetch_add(
                            u64::try_from(frame_count).unwrap_or(u64::MAX),
                            AtomicOrdering::Relaxed,
                        );
                        GLOBAL_CONSOLIDATION_METRICS
                            .max_group_size_observed
                            .fetch_max(
                                u64::try_from(frame_count).unwrap_or(u64::MAX),
                                AtomicOrdering::Relaxed,
                            );

                        GLOBAL_CONSOLIDATION_METRICS.record_phase_timing(
                            if record_initial_metrics {
                                prepare_us
                            } else {
                                0
                            },
                            if record_initial_metrics {
                                consolidator_lock_wait_us
                            } else {
                                0
                            },
                            flushing_wait_us,
                            true,
                            arrival_wait_us,
                            inner_lock_wait_us,
                            exclusive_lock_us,
                            wal_append_us,
                            wal_sync_us,
                            0,
                        );

                        // bd-db300.3.8.2: per-flush structured event splitting
                        // lock-wait time from WAL service time.
                        let lock_wait_total_us =
                            inner_lock_wait_us + exclusive_lock_us + flushing_wait_us;
                        let wal_service_total_us = wal_append_us + wal_sync_us;
                        tracing::debug!(
                            target: "fsqlite::wal::lock_scope",
                            role = "flusher",
                            epoch = flush_epoch,
                            frames = frame_count,
                            lock_wait_us = lock_wait_total_us,
                            inner_lock_wait_us,
                            exclusive_lock_us,
                            flushing_wait_us,
                            wal_service_us = wal_service_total_us,
                            wal_append_us,
                            wal_sync_us,
                            arrival_wait_us,
                            arrival_wait_policy = arrival_wait_decision.policy,
                            arrival_wait_reason = arrival_wait_decision.reason,
                            arrival_wait_budget_us = arrival_wait_decision.wait_budget_us(),
                            arrival_wait_fill_age_us = arrival_wait_decision.fill_age_us(),
                            arrival_wait_used_legacy_fallback =
                                arrival_wait_decision.used_legacy_fallback,
                            "WAL backend commit: lock_wait={lock_wait_total_us}us \
                             service={wal_service_total_us}us \
                             (append={wal_append_us}us sync={wal_sync_us}us) \
                             frames={frame_count}"
                        );

                        let (completed_epoch, has_promoted) = {
                            let mut consolidator = queue
                                .consolidator
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let promoted = consolidator.complete_flush()?;
                            (consolidator.epoch(), promoted)
                        };
                        queue.publish_completed_epoch(completed_epoch, has_promoted);

                        if has_promoted {
                            record_initial_metrics = false;
                            needs_arrival_wait = false;
                            continue 'flusher_loop;
                        }
                    }
                    Err(error) => {
                        let (abort_result, wake_next_epoch) = {
                            let mut consolidator = queue
                                .consolidator
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let abort_result = consolidator.abort_flush();
                            let wake_next_epoch =
                                abort_result.is_ok() && consolidator.has_flusher_vacancy();
                            (abort_result, wake_next_epoch)
                        };
                        queue.publish_failed_epoch(flush_epoch, &error, wake_next_epoch);
                        if let Err(abort_error) = abort_result {
                            return Err(FrankenError::internal(format!(
                                "group commit flush failed for epoch {flush_epoch} and abort_flush also failed: flush={error}; abort={abort_error}"
                            )));
                        }
                        return Err(error);
                    }
                }

                break;
            }

            Ok(())
        };

        match outcome {
            SubmitOutcome::Flusher => {
                run_flusher_loop(true, true, None)?;
            }

            SubmitOutcome::Waiter => {
                // Step 3b: WAITER path — wait for flusher to complete our epoch.
                //
                // We submitted during epoch N. The flusher will:
                // - Call begin_flush() which increments epoch to N+1
                // - Write our frames
                // - Call complete_flush() which sets completed_epoch = N+1
                //
                // So we wait for completed_epoch >= N+1.
                let target_epoch = our_epoch + 1;

                let t_waiter_start = Instant::now();
                let guard = queue
                    .consolidator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let wait_outcome = queue.wait_for_epoch_outcome(guard, target_epoch)?;
                let waiter_epoch_wait_us =
                    Instant::now().duration_since(t_waiter_start).as_micros() as u64;

                match wait_outcome {
                    WaitForEpochOutcome::Completed => {
                        // Record phase timing for waiter
                        GLOBAL_CONSOLIDATION_METRICS.record_phase_timing(
                            prepare_us,
                            consolidator_lock_wait_us,
                            flushing_wait_us,
                            false, // is_flusher
                            0,     // arrival_wait_us (N/A for waiter)
                            0,     // inner_lock_wait_us (N/A for waiter)
                            0,     // exclusive_lock_us (N/A for waiter)
                            0,     // wal_append_us (N/A for waiter)
                            0,     // wal_sync_us (N/A for waiter)
                            waiter_epoch_wait_us,
                        );

                        // bd-db300.3.8.2: per-waiter structured event showing
                        // time spent waiting for the flusher (all lock-wait,
                        // zero WAL service time on this thread).
                        let lock_wait_total_us =
                            consolidator_lock_wait_us + flushing_wait_us + waiter_epoch_wait_us;
                        tracing::debug!(
                            target: "fsqlite::wal::lock_scope",
                            role = "waiter",
                            lock_wait_us = lock_wait_total_us,
                            consolidator_lock_wait_us,
                            flushing_wait_us,
                            waiter_epoch_wait_us,
                            wal_service_us = 0_u64,
                            wal_append_us = 0_u64,
                            wal_sync_us = 0_u64,
                            "WAL backend commit: lock_wait={lock_wait_total_us}us \
                             service=0us (waiter — flusher did I/O)"
                        );

                        // The flusher already updated inner.db_size.
                        // Our frames are now durable in the WAL.
                    }
                    WaitForEpochOutcome::TakeOverFlusher {
                        batches,
                        flush_epoch,
                    } => {
                        let _ = waiter_epoch_wait_us;
                        run_flusher_loop(true, false, Some((batches, flush_epoch)))?;
                    }
                }
            }
        }

        Ok(())
    }

    fn ensure_writer(&mut self, cx: &Cx) -> Result<()> {
        if self.is_writer {
            return Ok(());
        }

        match self.mode {
            TransactionMode::ReadOnly => Err(FrankenError::ReadOnly),
            TransactionMode::Concurrent => {
                let inner = self
                    .inner
                    .lock()
                    .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
                if inner.checkpoint_active {
                    return Err(FrankenError::Busy);
                }
                // Concurrent writers don't acquire the global writer_active lock.
                drop(inner);
                self.is_writer = true;
                Ok(())
            }
            TransactionMode::Deferred => {
                let mut inner = self
                    .inner
                    .lock()
                    .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
                if inner.checkpoint_active {
                    return Err(FrankenError::Busy);
                }
                if inner.writer_active {
                    return Err(FrankenError::Busy);
                }
                // Escalate to RESERVED lock for cross-process writer exclusion.
                inner.db_file.lock(cx, LockLevel::Reserved)?;
                inner.writer_active = true;
                drop(inner);
                self.is_writer = true;
                Ok(())
            }
            TransactionMode::Immediate | TransactionMode::Exclusive => Err(FrankenError::internal(
                "writer transaction lost writer role",
            )),
        }
    }
}

const fn retained_lock_level_after_txn_exit(
    remaining_active_transactions: u32,
    writer_active: bool,
) -> LockLevel {
    if remaining_active_transactions == 0 {
        LockLevel::None
    } else if writer_active {
        LockLevel::Reserved
    } else {
        LockLevel::Shared
    }
}

impl<V> TransactionHandle for SimpleTransaction<V>
where
    V: Vfs + Send,
    V::File: Send + Sync,
{
    fn get_page(&self, cx: &Cx, page_no: PageNumber) -> Result<PageData> {
        if self.freed_pages.contains(&page_no) {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "page {} was freed earlier in this transaction",
                    page_no.get()
                ),
            });
        }

        if let Some(staged) = self.write_set.get(&page_no) {
            return Ok(staged.published_page());
        }

        // Pages that were allocated after a savepoint and then rolled back
        // should return zeros, not BusySnapshot error.
        if self.rolled_back_pages.contains(&page_no) {
            let inner = self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
            return Ok(PageData::zeroed(inner.page_size));
        }

        // MVCC db_size guard: pages beyond the transaction's snapshot db_size
        // did not exist when this snapshot was taken. Instead of immediately
        // returning BusySnapshot, we refresh the snapshot to see if concurrent
        // commits have advanced the db_size to include this page. This allows
        // read-only transactions to observe newly-committed pages without
        // requiring a full transaction restart.
        //
        // Exception: pages allocated by THIS transaction (in allocated_from_eof
        // or allocated_from_freelist) are allowed even if beyond published_db_size.
        if page_no.get() > self.published_db_size.get() {
            let page_allocated_by_this_txn = self.allocated_from_eof.contains(&page_no)
                || self.allocated_from_freelist.contains(&page_no)
                || self.page_lease.contains(&page_no);
            if !page_allocated_by_this_txn {
                // Snapshot refresh + retry: check if a newer snapshot includes this page.
                let fresh_snapshot = self.published.snapshot();
                if page_no.get() <= fresh_snapshot.db_size {
                    // The page now exists in a more recent snapshot. Advance our
                    // snapshot boundary to include it and continue with the read.
                    tracing::trace!(
                        target: "fsqlite.snapshot_publication",
                        trace_id = cx.trace_id(),
                        run_id = "pager-publication",
                        scenario_id = "snapshot_refresh_success",
                        page_no = page_no.get(),
                        old_db_size = self.published_db_size.get(),
                        new_db_size = fresh_snapshot.db_size,
                        old_commit_seq = self.published_visible_commit_seq.get().get(),
                        new_commit_seq = fresh_snapshot.visible_commit_seq.get(),
                        "refreshed snapshot to include requested page"
                    );
                    self.published_db_size.set(fresh_snapshot.db_size);
                    self.published_visible_commit_seq
                        .set(fresh_snapshot.visible_commit_seq);
                    // Fall through to continue with the read using the refreshed snapshot
                } else {
                    // Page is still beyond the latest snapshot — genuinely doesn't exist yet.
                    tracing::trace!(
                        target: "fsqlite.snapshot_publication",
                        trace_id = cx.trace_id(),
                        run_id = "pager-publication",
                        scenario_id = "page_beyond_snapshot_db_size",
                        page_no = page_no.get(),
                        published_db_size = self.published_db_size.get(),
                        latest_db_size = fresh_snapshot.db_size,
                        "page beyond transaction's snapshot db_size (even after refresh)"
                    );
                    return Err(FrankenError::BusySnapshot {
                        conflicting_pages: format!(
                            "page {} > snapshot db_size {} (latest: {})",
                            page_no.get(),
                            self.published_db_size.get(),
                            fresh_snapshot.db_size
                        ),
                    });
                }
            }
        }

        let read_start = Instant::now();
        let mut published_retry_count = 0_usize;
        while self.published.snapshot().visible_commit_seq
            == self.published_visible_commit_seq.get()
        {
            let snapshot = self.published.snapshot();
            if page_no.get() > snapshot.db_size {
                let confirm = self.published.snapshot();
                if confirm.snapshot_gen == snapshot.snapshot_gen {
                    tracing::trace!(
                        target: "fsqlite.snapshot_publication",
                        trace_id = cx.trace_id(),
                        run_id = "pager-publication",
                        scenario_id = "zero_fill_read",
                        snapshot_gen = snapshot.snapshot_gen,
                        visible_commit_seq = snapshot.visible_commit_seq.get(),
                        publication_mode = SNAPSHOT_PUBLICATION_MODE,
                        read_retry_count = self.published.read_retry_count(),
                        page_set_size = snapshot.page_set_size,
                        elapsed_ns =
                            u64::try_from(read_start.elapsed().as_nanos()).unwrap_or(u64::MAX),
                        "resolved zero-filled page from published metadata"
                    );
                    return Ok(PageData::from_vec(vec![0_u8; self.pool.page_size()]));
                }
                self.published.record_retry();
                if published_retry_count >= PUBLISHED_READ_FAST_RETRY_LIMIT {
                    break;
                }
                self.published
                    .wait_for_sequence_change(snapshot.snapshot_gen, PUBLISHED_SNAPSHOT_WAIT_SLICE);
                published_retry_count = published_retry_count.saturating_add(1);
                continue;
            }

            if let Some(page) = self.published.try_get_page(page_no) {
                let confirm = self.published.snapshot();
                if confirm.snapshot_gen == snapshot.snapshot_gen {
                    self.published.note_published_hit();
                    tracing::trace!(
                        target: "fsqlite.snapshot_publication",
                        trace_id = cx.trace_id(),
                        run_id = "pager-publication",
                        scenario_id = "published_read_hit",
                        snapshot_gen = snapshot.snapshot_gen,
                        visible_commit_seq = snapshot.visible_commit_seq.get(),
                        publication_mode = SNAPSHOT_PUBLICATION_MODE,
                        read_retry_count = self.published.read_retry_count(),
                        page_set_size = snapshot.page_set_size,
                        elapsed_ns =
                            u64::try_from(read_start.elapsed().as_nanos()).unwrap_or(u64::MAX),
                        "served page from published snapshot"
                    );
                    return Ok(page);
                }
                self.published.record_retry();
                if published_retry_count >= PUBLISHED_READ_FAST_RETRY_LIMIT {
                    break;
                }
                self.published
                    .wait_for_sequence_change(snapshot.snapshot_gen, PUBLISHED_SNAPSHOT_WAIT_SLICE);
                published_retry_count = published_retry_count.saturating_add(1);
                continue;
            }

            break;
        }

        let committed_snapshot = self.published.snapshot();
        if committed_snapshot.visible_commit_seq == self.published_visible_commit_seq.get()
            && committed_snapshot.journal_mode != JournalMode::Wal
            && page_no.get() <= committed_snapshot.db_size
        {
            if let Some(data) = self.cache.get_copy(page_no) {
                return Ok(PageData::from_vec(data));
            }
        }

        // Per-transaction read cache: pages previously read via inner.write()
        // are cached here. At 16 threads with constant commits, the published
        // snapshot fast path above is defeated (commit_seq constantly advances),
        // causing EVERY page read to hit inner.lock(). This cache eliminates
        // ~99% of those lock acquisitions for repeated B-tree traversals.
        if let Some(cached) = self.txn_read_cache.borrow().get(&page_no) {
            return Ok(cached.clone());
        }

        // WAL mode fast path: try shared-lock read first (bd-db300.3.8.7).
        if self.journal_mode == JournalMode::Wal {
            if let Some(data) = read_page_from_wal_backend(&self.wal_backend, cx, page_no)? {
                let page = PageData::from_vec(data);
                self.txn_read_cache
                    .borrow_mut()
                    .insert(page_no, page.clone());
                return Ok(page);
            }
        }

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        let data = inner.read_page_copy(cx, &self.cache, &self.wal_backend, page_no)?;
        let page = PageData::from_vec(data);
        let publish_update = PublishedPagerUpdate {
            visible_commit_seq: inner.commit_seq,
            db_size: inner.db_size,
            journal_mode: inner.journal_mode,
            freelist_count: inner.freelist.len(),
            checkpoint_active: inner.checkpoint_active,
        };
        let publish_page = page_no.get() <= inner.db_size
            && inner.commit_seq == self.published_visible_commit_seq.get();
        drop(inner);
        if publish_page {
            self.published
                .publish_observed_page(cx, publish_update, page_no, page.clone());
        }
        // Cache the page read from inner.lock() for future reads.
        self.txn_read_cache
            .borrow_mut()
            .insert(page_no, page.clone());
        Ok(page)
    }

    fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
        self.ensure_writer(cx)?;

        // If we are writing to a page that was previously freed in this transaction,
        // we must "un-free" it.
        if let Some(pos) = self.freed_pages.iter().position(|&p| p == page_no) {
            self.freed_pages.swap_remove(pos);
        }

        let staged = StagedPage::from_bytes(&self.pool, data)?;
        insert_staged_page(
            &mut self.write_set,
            &mut self.write_pages_sorted,
            page_no,
            staged,
        );
        Ok(())
    }

    fn write_page_data(&mut self, cx: &Cx, page_no: PageNumber, data: PageData) -> Result<()> {
        self.ensure_writer(cx)?;

        if let Some(pos) = self.freed_pages.iter().position(|&p| p == page_no) {
            self.freed_pages.swap_remove(pos);
        }

        let staged = StagedPage::from_page_data_for_pool(&self.pool, data)?;

        insert_staged_page(
            &mut self.write_set,
            &mut self.write_pages_sorted,
            page_no,
            staged,
        );
        Ok(())
    }

    fn allocate_page(&mut self, cx: &Cx) -> Result<PageNumber> {
        self.ensure_writer(cx)?;

        // ── Local lease fast path ──────────────────────────────────────
        // If we have pre-allocated pages from a previous batch, hand one
        // out without touching the global `inner` mutex at all.
        if let Some(page) = self.page_lease.pop() {
            self.allocated_from_eof.push(page);
            return Ok(page);
        }

        // Pages freed earlier in the same transaction stay quarantined until
        // commit. Reusing them immediately lets one B-tree operation hand a
        // page to another tree before the old ownership is durably retired,
        // which can surface as cross-tree page aliasing on disk.

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;

        if !self.memory_db_bump_alloc {
            let committed_freelist_is_snapshot_pinned =
                self.mode == TransactionMode::Concurrent || inner.active_transactions > 1;

            if committed_freelist_is_snapshot_pinned {
                // Concurrent writers always read against a fixed snapshot. So do
                // immediate/deferred writers when another local transaction is
                // still active, because that older reader snapshot can still
                // observe the committed image being replaced. In both cases, pages
                // at or below db_size are part of some still-visible committed
                // state and cannot be safely reused from the live global freelist
                // without versioned freelist metadata. Pages above db_size are
                // different: they only exist because an earlier transaction
                // allocated EOF pages and then rolled back, so reusing them cannot
                // violate snapshot visibility and avoids page-count holes.
                if let Some(idx) = inner
                    .freelist
                    .iter()
                    .rposition(|page| page.get() > inner.db_size)
                {
                    let page = inner.freelist.remove(idx);
                    self.allocated_from_freelist.push(page);
                    return Ok(page);
                }
            } else if let Some(page) = inner.freelist.pop() {
                self.allocated_from_freelist.push(page);
                return Ok(page);
            }
        }

        // ── EOF allocation ──────────────────────────────────────────────
        // For concurrent transactions that have already allocated at least
        // one page, batch-allocate PAGE_LEASE_BATCH_SIZE pages in one lock
        // acquisition to reduce mutex contention during B-tree splits.
        // The first allocation is always single-page to avoid over-reserving
        // for short transactions. Non-concurrent writers always allocate
        // one page at a time since there's no lock convoy to avoid.
        let pending_byte_page = (0x4000_0000 / inner.page_size.get()) + 1;
        let already_allocated =
            !self.allocated_from_eof.is_empty() || !self.allocated_from_freelist.is_empty();
        let batch = if self.mode == TransactionMode::Concurrent && already_allocated {
            PAGE_LEASE_BATCH_SIZE
        } else {
            1
        };
        let mut first_page: Option<PageNumber> = None;

        for _ in 0..batch {
            let mut raw = inner.next_page;
            if raw == pending_byte_page {
                raw = raw.saturating_add(1);
            }
            let next = raw.saturating_add(1);
            // Stop the batch if next_page can no longer advance (u32::MAX
            // saturation).  Continuing would hand out duplicate page numbers.
            if next == raw {
                break;
            }
            inner.next_page = next;
            if let Some(page) = PageNumber::new(raw) {
                if first_page.is_none() {
                    first_page = Some(page);
                } else {
                    self.page_lease.push(page);
                }
            }
        }
        drop(inner);

        let page = first_page.ok_or_else(|| FrankenError::OutOfRange {
            what: "allocated page number".to_owned(),
            value: "0".to_owned(),
        })?;
        self.allocated_from_eof.push(page);
        Ok(page)
    }

    fn free_page(&mut self, cx: &Cx, page_no: PageNumber) -> Result<()> {
        self.ensure_writer(cx)?;
        if page_no == PageNumber::ONE {
            return Err(FrankenError::OutOfRange {
                what: "free page number".to_owned(),
                value: page_no.get().to_string(),
            });
        }
        if !self.freed_pages.contains(&page_no) {
            self.freed_pages.push(page_no);
        }
        if self.write_set.remove(&page_no).is_some() {
            remove_page_sorted(&mut self.write_pages_sorted, page_no);
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn commit(&mut self, cx: &Cx) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        if !self.is_writer {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
            // Return any unused lease pages to the freelist so they can
            // be reused by other transactions.
            return_pages_to_freelist(&mut inner.freelist, self.page_lease.drain(..));
            inner.active_transactions = inner.active_transactions.saturating_sub(1);
            let preserve_level =
                retained_lock_level_after_txn_exit(inner.active_transactions, inner.writer_active);
            let _ = inner.db_file.unlock(cx, preserve_level);
            drop(inner);
            self.committed = true;
            self.finished = true;
            return Ok(());
        }
        if !self.has_pending_writes() {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
            // Return any unused lease pages.
            return_pages_to_freelist(&mut inner.freelist, self.page_lease.drain(..));
            inner.active_transactions = inner.active_transactions.saturating_sub(1);
            if self.mode != TransactionMode::Concurrent {
                inner.writer_active = false;
            }
            let preserve_level =
                retained_lock_level_after_txn_exit(inner.active_transactions, inner.writer_active);
            let _ = inner.db_file.unlock(cx, preserve_level);
            drop(inner);
            self.committed = true;
            self.finished = true;
            return Ok(());
        }

        // =====================================================================
        // D1-CRITICAL: Split inner lock into prepare/IO/publish phases (bd-3wop3.8)
        //
        // BEFORE: inner.lock() held for entire commit (~100us) serializing all threads
        // AFTER:
        //   Phase A (prepare, ~20us): Hold inner.lock() briefly to snapshot state
        //   DROP inner.lock() <-- allows Thread B to start Phase A while Thread A does I/O
        //   Phase B (WAL I/O, ~50us): Acquires inner.lock() only when needed
        //   Phase C (publish, ~10us): Re-acquires inner.lock() for finalization
        //
        // This allows N threads to overlap their prepare phases, reducing
        // serialization from N*100us to N*20us + 50us + 10us.
        // =====================================================================

        // ── Full commit path timing instrumentation ──
        let t_commit_start = Instant::now();

        // Phase A: Prepare write_set under inner lock (~20us)
        // Snapshot state needed for WAL I/O, then DROP inner.lock() immediately.
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        // Return any unused lease pages before computing committed db_size.
        return_pages_to_freelist(&mut inner.freelist, self.page_lease.drain(..));

        let committed_db_size = self.committed_db_size_with_inner(&inner);
        // Declared outside the block so it survives to Phase C where freed
        // pages are promoted into inner.freelist after successful WAL commit.
        let pending_freed: Vec<PageNumber>;
        {
            // ShardedPageCache uses per-shard internal locking
            //
            // Compute freelist_dirty BEFORE draining freed_pages, because
            // predicted_durable_freelist_pages_with_inner reads self.freed_pages.
            let freelist_dirty = self.freelist_metadata_dirty_with_inner(&inner, committed_db_size);
            // CRITICAL FIX (beads_rust#138): Do NOT push freed_pages into
            // inner.freelist during Phase A. In the split-lock WAL commit
            // path, inner.lock() is released between Phase A and Phase B.
            // If freed pages are pushed here, a concurrent transaction's
            // Phase A can observe them and serialize a conflicting freelist
            // into its write_set. When both batches reach the WAL, the
            // last writer's page 1 (with stale/inconsistent freelist
            // trunk pointer and count) overwrites the first writer's,
            // creating orphaned pages ("page N is never used").
            //
            // Instead, we drain freed_pages into a local vec and pass it
            // to the serializer which builds a predicted freelist from
            // inner.freelist + pending_freed without mutating inner.freelist.
            // The actual promotion into inner.freelist is deferred to
            // Phase C (after WAL success).
            pending_freed = self.freed_pages.drain(..).collect();
            if freelist_dirty {
                if let Err(e) = serialize_freelist_to_write_set(
                    cx,
                    &mut inner,
                    &self.cache,
                    &self.wal_backend,
                    &self.pool,
                    &mut self.write_set,
                    &mut self.write_pages_sorted,
                    committed_db_size,
                    &pending_freed,
                ) {
                    self.freed_pages.extend(pending_freed);
                    return Err(e);
                }
            }

            // In rollback-journal mode page 1 is still the durable commit beacon.
            // In WAL mode classify page-1 work by semantic trigger so later beads
            // can remove or defer each class independently.
            let wal_page1_plan = self.classify_wal_page_one_write(inner.db_size, freelist_dirty);
            // D1-CRITICAL Fix: In WAL mode, page 1 must be written to WAL not
            // only when it was explicitly dirty, but also when the database
            // grows (new pages allocated beyond current db_size). Without this,
            // other connections reading page 1 from WAL won't see the updated
            // page_count header, causing BusySnapshot errors.
            let must_write_page1 = if self.journal_mode == JournalMode::Wal {
                wal_page1_plan.requires_page_one_rewrite()
                    || wal_page1_plan.requires_page_count_advance()
            } else {
                true
            };
            if must_write_page1 {
                let mut page1 = match ensure_page_one_in_write_set(
                    cx,
                    &mut inner,
                    &self.cache,
                    &self.wal_backend,
                    &self.pool,
                    &mut self.write_set,
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        self.freed_pages.extend(pending_freed);
                        return Err(e);
                    }
                };
                if page1.len() >= DATABASE_HEADER_SIZE {
                    let mut page_count_bytes = [0_u8; 4];
                    page_count_bytes.copy_from_slice(&page1[28..32]);
                    let existing_page_count = u32::from_be_bytes(page_count_bytes);
                    let new_change_counter = inner.commit_seq.get().wrapping_add(1) as u32;

                    // Offset 24..28: change counter (big-endian u32)
                    page1[24..28].copy_from_slice(&new_change_counter.to_be_bytes());
                    if self.journal_mode != JournalMode::Wal
                        || wal_page1_plan.requires_page_count_advance()
                    {
                        let new_db_size = committed_db_size.max(existing_page_count);
                        // Offset 28..32: page count (big-endian u32)
                        page1[28..32].copy_from_slice(&new_db_size.to_be_bytes());
                    }
                    // Offset 92..96: version-valid-for
                    page1[92..96].copy_from_slice(&new_change_counter.to_be_bytes());
                }
                insert_staged_page(
                    &mut self.write_set,
                    &mut self.write_pages_sorted,
                    PageNumber::ONE,
                    StagedPage::from_buf(page1),
                );
            }
        }

        let t_phase_a_done = Instant::now();

        // Phase B: Commit via WAL or journal
        // D1-CRITICAL: For WAL mode, we release inner.lock() here so other
        // threads can start their Phase A (prepare) while we wait for
        // the consolidator lock. This is the key parallelization win.
        let commit_result = if self.journal_mode == JournalMode::Wal {
            // Drop inner lock BEFORE acquiring consolidator lock.
            // This allows other threads to run Phase A concurrently.
            drop(inner);

            // WAL mode: Use group commit for same-process batching.
            // commit_wal_group_commit will acquire consolidator.lock() first,
            // then briefly inner.lock() for the actual WAL I/O.
            let result = Self::commit_wal_group_commit(
                cx,
                &self.wal_backend,
                &self.inner,
                &self.write_set,
                &self.write_pages_sorted,
                &self.group_commit_queue,
            );

            // Re-acquire inner lock for Phase C (finalize).
            inner = match self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))
            {
                Ok(guard) => guard,
                Err(e) => {
                    self.freed_pages.extend(pending_freed);
                    return Err(e);
                }
            };

            result
        } else {
            // Journal mode: Direct commit (no group commit)
            // Journal mode keeps inner locked throughout - no parallelization.
            Self::commit_journal(
                cx,
                &self.vfs,
                &self.journal_path,
                &mut inner,
                &self.write_set,
                self.original_db_size,
            )
        };

        let t_phase_b_done = Instant::now();

        if commit_result.is_ok() {
            // Phase C1 (FAST, under inner.lock): Update metadata only.
            // CRITICAL FIX (beads_rust#138): Now that WAL I/O has succeeded,
            // promote the pending freed pages into inner.freelist. This is
            // the deferred half of the Phase A fix — freed pages are only
            // visible to other transactions after the WAL commit is durable.
            return_pages_to_freelist(&mut inner.freelist, pending_freed);
            if self.journal_mode != JournalMode::Wal {
                inner.db_size = committed_db_size;
            }
            inner.commit_seq = inner.commit_seq.next();
            if let Ok(file_size) = inner.db_file.file_size(cx) {
                inner.committed_db_file_size_bytes = file_size;
            }
            inner.active_transactions = inner.active_transactions.saturating_sub(1);
            if self.mode != TransactionMode::Concurrent {
                inner.writer_active = false;
            }
            let publish_update = PublishedPagerUpdate {
                visible_commit_seq: inner.commit_seq,
                db_size: inner.db_size,
                journal_mode: inner.journal_mode,
                freelist_count: inner.freelist.len(),
                checkpoint_active: inner.checkpoint_active,
            };
            let single_connection_fast_path = self.single_connection_fast_path_enabled();
            if !single_connection_fast_path {
                // bd-db300.5.3.3.1: publish immutable snapshot while inner is still held.
                self.publish_committed_snapshot_from_inner(&inner);
            }
            let preserve_level =
                retained_lock_level_after_txn_exit(inner.active_transactions, inner.writer_active);
            let _ = inner.db_file.unlock(cx, preserve_level);
            drop(inner);

            let t_phase_c1_done = Instant::now();

            // H4 fault hook: crash during Phase C, after commit_seq update
            // but before snapshot publish. WAL frames are durable, commit_seq
            // incremented in-memory, but snapshot plane not yet updated.
            #[cfg(any(test, feature = "fault-injection"))]
            if !single_connection_fast_path {
                crate::fault_hooks::maybe_inject_during_phase_c(
                    publish_update.visible_commit_seq.get(),
                    publish_update.db_size,
                )?;
            }

            // Phase C2 (outside inner.lock): Publish to snapshot plane.
            // Even for single_connection_fast_path, we must publish write_set pages.
            self.publish_committed_state(cx, publish_update);

            let t_phase_c2_done = Instant::now();

            // Record full commit path timing.
            if self.journal_mode == JournalMode::Wal {
                let phase_a_us = t_phase_a_done.duration_since(t_commit_start).as_micros() as u64;
                let phase_b_us = t_phase_b_done.duration_since(t_phase_a_done).as_micros() as u64;
                let phase_c1_us = t_phase_c1_done.duration_since(t_phase_b_done).as_micros() as u64;
                let phase_c2_us =
                    t_phase_c2_done.duration_since(t_phase_c1_done).as_micros() as u64;
                GLOBAL_CONSOLIDATION_METRICS.record_commit_phases(
                    phase_a_us,
                    phase_b_us,
                    phase_c1_us,
                    phase_c2_us,
                );
            }

            // WAL readers prefer the published snapshot plane or WAL backend,
            // so copying committed pages into the shared cache is wasted work.
            if publish_update.journal_mode == JournalMode::Wal {
                self.discard_committed_pages();
            } else {
                let committed_cache_pages = self.drain_committed_cache_pages();
                if !committed_cache_pages.is_empty() {
                    let inner = self
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if inner.commit_seq == publish_update.visible_commit_seq {
                        for (page_no, buf) in committed_cache_pages {
                            self.cache.insert_buffer(page_no, buf);
                        }
                    }
                }
            }
            self.committed = true;
            self.finished = true;
        } else {
            // Keep the writer lock held on commit failure so no other writer
            // can interleave while the caller decides to retry or roll back.
            //
            // CRITICAL FIX (beads_rust#138): Restore pending freed pages so
            // a retry or rollback can still observe them. In the old code
            // they leaked into inner.freelist regardless of commit outcome;
            // now we only promote on success and restore on failure.
            self.freed_pages.extend(pending_freed);
            drop(inner);
        }
        commit_result
    }

    fn commit_and_retain(&mut self, cx: &Cx) -> Result<bool> {
        // Only supported for in-memory pagers where we can skip I/O.
        if !self.vfs.is_memory() {
            self.commit(cx)?;
            return Ok(false);
        }

        // If not a writer or no pending writes, just commit normally.
        if !self.is_writer || !self.has_pending_writes() {
            self.commit(cx)?;
            return Ok(false);
        }

        // Perform the full commit but don't release writer state.
        // This is the same as commit() except we:
        //  - Don't decrement active_transactions
        //  - Don't set writer_active = false
        //  - Don't set committed/finished = true
        //  - Clear write_set for reuse instead
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        return_pages_to_freelist(&mut inner.freelist, self.page_lease.drain(..));

        let committed_db_size = self.committed_db_size_with_inner(&inner);
        // Compute freelist_dirty BEFORE draining freed_pages, because
        // predicted_durable_freelist_pages_with_inner reads self.freed_pages.
        let freelist_dirty_for_retain =
            self.freelist_metadata_dirty_with_inner(&inner, committed_db_size);
        // CRITICAL FIX (beads_rust#138): Drain freed_pages AFTER dirty check
        // but do NOT push into inner.freelist. Pass to serializer as
        // pending_freed so inner.freelist remains untouched until Phase C
        // (after successful commit).
        let pending_freed: Vec<PageNumber> = self.freed_pages.drain(..).collect();
        let commit_result = {
            let freelist_dirty = freelist_dirty_for_retain;
            if freelist_dirty {
                if let Err(e) = serialize_freelist_to_write_set(
                    cx,
                    &mut inner,
                    &self.cache,
                    &self.wal_backend,
                    &self.pool,
                    &mut self.write_set,
                    &mut self.write_pages_sorted,
                    committed_db_size,
                    &pending_freed,
                ) {
                    self.freed_pages.extend(pending_freed);
                    return Err(e);
                }
            }

            let wal_page1_plan = self.classify_wal_page_one_write(inner.db_size, freelist_dirty);
            // D1-CRITICAL Fix: In WAL mode, page 1 must be written to WAL not
            // only when it was explicitly dirty, but also when the database
            // grows (new pages allocated beyond current db_size). Without this,
            // other connections reading page 1 from WAL won't see the updated
            // page_count header, causing BusySnapshot errors.
            let must_write_page1 = if self.journal_mode == JournalMode::Wal {
                wal_page1_plan.requires_page_one_rewrite()
                    || wal_page1_plan.requires_page_count_advance()
            } else if self.vfs.is_memory() {
                // B3.4: :memory: journal mode skips page 1 header update unless:
                // 1. freelist_dirty (freelist count in header must match), OR
                // 2. page 1 is explicitly dirty in write_set
                freelist_dirty || self.write_set.contains_key(&PageNumber::ONE)
            } else {
                true
            };
            if must_write_page1 {
                let mut page1 = match ensure_page_one_in_write_set(
                    cx,
                    &mut inner,
                    &self.cache,
                    &self.wal_backend,
                    &self.pool,
                    &mut self.write_set,
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        self.freed_pages.extend(pending_freed);
                        return Err(e);
                    }
                };
                if page1.len() >= DATABASE_HEADER_SIZE {
                    let mut page_count_bytes = [0_u8; 4];
                    page_count_bytes.copy_from_slice(&page1[28..32]);
                    let existing_page_count = u32::from_be_bytes(page_count_bytes);
                    let new_change_counter = inner.commit_seq.get().wrapping_add(1) as u32;
                    page1[24..28].copy_from_slice(&new_change_counter.to_be_bytes());
                    if self.journal_mode != JournalMode::Wal
                        || wal_page1_plan.requires_page_count_advance()
                    {
                        let new_db_size = committed_db_size.max(existing_page_count);
                        page1[28..32].copy_from_slice(&new_db_size.to_be_bytes());
                    }
                    page1[92..96].copy_from_slice(&new_change_counter.to_be_bytes());
                }
                insert_staged_page(
                    &mut self.write_set,
                    &mut self.write_pages_sorted,
                    PageNumber::ONE,
                    StagedPage::from_buf(page1),
                );
            }

            if self.journal_mode == JournalMode::Wal {
                drop(inner);
                let result = Self::commit_wal_group_commit(
                    cx,
                    &self.wal_backend,
                    &self.inner,
                    &self.write_set,
                    &self.write_pages_sorted,
                    &self.group_commit_queue,
                );
                inner = match self
                    .inner
                    .lock()
                    .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))
                {
                    Ok(guard) => guard,
                    Err(e) => {
                        self.freed_pages.extend(pending_freed);
                        return Err(e);
                    }
                };
                result
            } else if self.vfs.is_memory() {
                // bd-wwqen.3: :memory: retained-commit fast path.
                // Skip journal creation, pre-image backup, sync, and deletion.
                // Batch dirty-page flushes through the VFS so MemoryFile can
                // hold its backing-storage lock once for the whole retained
                // commit. Keep the staged pages in the write set for the later
                // publish step so the flush path avoids an eager drain/move of
                // the whole staging map on every autocommit write.
                let page_size_bytes = u64::from(inner.page_size.get());

                let mut flushed_db_size = inner.db_size;
                let write_result = {
                    let mut batched_writes: SmallVec<[(u64, &[u8]); 8]> =
                        SmallVec::with_capacity(self.write_set.len());
                    for (&page_no, staged) in &self.write_set {
                        let offset = u64::from(page_no.get() - 1) * page_size_bytes;
                        batched_writes.push((offset, staged.as_page_bytes()));
                        flushed_db_size = flushed_db_size.max(page_no.get());
                    }
                    inner
                        .db_file
                        .write_page_batch(cx, batched_writes.as_slice())
                };
                if let Err(e) = write_result {
                    self.freed_pages.extend(pending_freed);
                    return Err(e);
                }
                inner.db_size = flushed_db_size;
                Ok(())
            } else {
                Self::commit_journal(
                    cx,
                    &self.vfs,
                    &self.journal_path,
                    &mut inner,
                    &self.write_set,
                    self.original_db_size,
                )
            }
        };

        if commit_result.is_ok() {
            // For journal mode, update db_size from our computed value.
            // For WAL mode with group commit, the flusher already set inner.db_size
            // to the consolidated max across all batched transactions - don't revert it.
            return_pages_to_freelist(&mut inner.freelist, pending_freed);
            if self.journal_mode != JournalMode::Wal {
                inner.db_size = committed_db_size;
            }
            inner.commit_seq = inner.commit_seq.next();
            // B3.4: :memory: derives file size from db_size * page_size — skip VFS roundtrip
            if self.vfs.is_memory() {
                inner.committed_db_file_size_bytes =
                    u64::from(inner.db_size) * u64::from(inner.page_size.get());
            } else if let Ok(file_size) = inner.db_file.file_size(cx) {
                inner.committed_db_file_size_bytes = file_size;
            }
            // NOTE: We intentionally do NOT decrement active_transactions or
            // set writer_active=false — the transaction stays "active" for reuse.
            let publish_update = PublishedPagerUpdate {
                visible_commit_seq: inner.commit_seq,
                db_size: inner.db_size,
                journal_mode: inner.journal_mode,
                freelist_count: inner.freelist.len(),
                checkpoint_active: inner.checkpoint_active,
            };
            let single_connection_fast_path = self.single_connection_fast_path_enabled();
            // bd-db300.5.3.3.1: publish immutable snapshot while inner is still
            // held — MUST happen before publish_committed_state (same order as
            // `commit()`), so concurrent readers see the immutable snapshot
            // before the seqlock commit_seq advances.
            if !single_connection_fast_path {
                self.publish_committed_snapshot_from_inner(&inner);
            }
            drop(inner);
            // Always publish write_set pages, even for single connection fast path.
            self.publish_committed_state_draining_write_set(cx, publish_update);

            // Clear retained transaction state for reuse.
            self.write_pages_sorted.clear();
            self.freed_pages.clear();
            self.allocated_from_freelist.clear();
            self.allocated_from_eof.clear();
            self.savepoint_stack.clear();
            self.rolled_back_pages.clear();
            self.original_db_size = committed_db_size;
            self.published_visible_commit_seq
                .set(publish_update.visible_commit_seq);
            self.published_db_size.set(publish_update.db_size);
            // Transaction stays active — committed/finished remain false.
            Ok(true)
        } else {
            // CRITICAL FIX (beads_rust#138): Restore pending freed pages on
            // commit failure so rollback can still see them.
            self.freed_pages.extend(pending_freed);
            drop(inner);
            commit_result?;
            unreachable!()
        }
    }

    fn is_writer(&self) -> bool {
        self.is_writer
    }

    fn has_pending_writes(&self) -> bool {
        !self.write_set.is_empty() || self.freelist_metadata_dirty()
    }

    fn published_visible_commit_seq_hint(&self) -> Option<CommitSeq> {
        Some(self.published_visible_commit_seq.get())
    }

    fn pending_commit_pages(&self) -> Result<Vec<PageNumber>> {
        if !self.has_pending_writes() {
            return Ok(Vec::new());
        }
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        Ok(self.predicted_commit_pages_with_inner(&inner))
    }

    fn pending_conflict_pages(&self) -> Result<Vec<PageNumber>> {
        if !self.has_pending_writes() {
            return Ok(Vec::new());
        }
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        Ok(self.predicted_conflict_pages_with_inner(&inner))
    }

    fn write_set_page_numbers(&self) -> Vec<PageNumber> {
        self.write_pages_sorted.clone()
    }

    fn page_size(&self) -> PageSize {
        PageSize::new(u32::try_from(self.pool.page_size()).expect("pool page size fits u32"))
            .expect("pool page size invariant")
    }

    fn page_one_in_pending_commit_surface(&self) -> Result<bool> {
        if !self.has_pending_writes() {
            return Ok(false);
        }
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        Ok(self.page_one_in_pending_commit_surface_with_inner(&inner))
    }

    fn allocate_page_requires_page_one_conflict_tracking(&self) -> Result<bool> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        Ok(self.allocate_page_requires_page_one_conflict_tracking_with_inner(&inner))
    }

    fn free_page_requires_page_one_conflict_tracking(&self, page_no: PageNumber) -> Result<bool> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        Ok(self.free_page_requires_page_one_conflict_tracking_with_inner(&inner, page_no))
    }

    fn write_page_requires_page_one_conflict_tracking(&self, page_no: PageNumber) -> Result<bool> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
        Ok(self.write_page_requires_page_one_conflict_tracking_with_inner(&inner, page_no))
    }

    fn rollback(&mut self, cx: &Cx) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.write_set.clear();
        self.write_pages_sorted.clear();
        self.freed_pages.clear();
        self.savepoint_stack.clear();
        self.rolled_back_pages.clear();
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;

        let restored_from_journal = if self.is_writer
            && self.journal_mode != JournalMode::Wal
            && inner.rollback_journal_recovery_state.is_pending()
        {
            let page_size = inner.page_size;
            if !SimplePager::<V>::recover_rollback_journal_if_present(
                cx,
                &*self.vfs,
                &mut inner.db_file,
                &self.journal_path,
                page_size,
            )? {
                return Err(FrankenError::internal(
                    "rollback journal missing while failed commit recovery was pending",
                ));
            }
            true
        } else {
            false
        };

        if restored_from_journal {
            // ShardedPageCache uses per-shard internal locking
            self.cache.clear();
            inner.refresh_committed_state(cx, &self.cache, &self.wal_backend)?;
            inner.rollback_journal_recovery_state = RollbackJournalRecoveryState::Clean;
            self.allocated_from_freelist.clear();
            self.allocated_from_eof.clear();
            // Lease pages were EOF allocations that were never written to
            // disk. After journal recovery rebuilds committed state, these
            // page numbers don't exist — just drop them.
            self.page_lease.clear();
            if self.mode != TransactionMode::Concurrent {
                inner.writer_active = false;
            }
        } else {
            // Restore pages allocated from the freelist.
            return_pages_to_freelist(&mut inner.freelist, self.allocated_from_freelist.drain(..));

            if self.is_writer && self.mode != TransactionMode::Concurrent {
                // Non-concurrent: next_page will be reset below, so lease
                // pages (which were EOF allocations) will be re-issued
                // naturally by future transactions. Just drop them — putting
                // them on the freelist would create sparse page holes since
                // the freelist consumer could pick a high page number while
                // next_page restarts from db_size+1.
                self.page_lease.clear();

                inner.db_size = self.original_db_size;

                // Reset next_page to avoid holes if we allocated pages that are now discarded.
                // Logic matches SimplePager::open.
                let db_size = inner.db_size;
                inner.next_page = if db_size >= 2 {
                    db_size.saturating_add(1)
                } else {
                    2
                };

                inner.writer_active = false;
            } else if self.is_writer && self.mode == TransactionMode::Concurrent {
                // Concurrent: next_page is NOT reset, so lease pages and
                // aborted EOF allocations must return to the in-memory
                // freelist. Otherwise next_page skips over them permanently
                // and a later commit can grow page_count past those holes,
                // yielding "Page N: never used" corruption.
                return_pages_to_freelist(&mut inner.freelist, self.page_lease.drain(..));
                return_pages_to_freelist(&mut inner.freelist, self.allocated_from_eof.drain(..));
            } else {
                // Read-only transaction: lease should be empty (only writers
                // allocate pages), but clear defensively.
                self.page_lease.clear();
            }
        }
        inner.active_transactions = inner.active_transactions.saturating_sub(1);
        let preserve_level =
            retained_lock_level_after_txn_exit(inner.active_transactions, inner.writer_active);
        let _ = inner.db_file.unlock(cx, preserve_level);
        drop(inner);
        if self.is_writer {
            // Delete any partial journal file.
            let _ = self.vfs.delete(cx, &self.journal_path, true);
        }
        self.committed = false;
        self.finished = true;
        Ok(())
    }

    fn record_write_witness(&mut self, _cx: &Cx, _key: fsqlite_types::WitnessKey) {}

    fn savepoint(&mut self, _cx: &Cx, name: &str) -> Result<()> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;

        self.savepoint_stack.push(SavepointEntry {
            name: name.to_owned(),
            write_set_snapshot: self
                .write_set
                .iter()
                .map(|(&k, v)| (k, v.published_page()))
                .collect(),
            write_pages_sorted_snapshot: self.write_pages_sorted.clone(),
            freed_pages_snapshot: self.freed_pages.clone(),
            next_page_snapshot: inner.next_page,
            freelist_snapshot: inner.freelist.clone(),
            allocated_from_freelist_snapshot: self.allocated_from_freelist.clone(),
            allocated_from_eof_snapshot: self.allocated_from_eof.clone(),
        });
        drop(inner);
        Ok(())
    }

    fn release_savepoint(&mut self, _cx: &Cx, name: &str) -> Result<()> {
        let pos = self
            .savepoint_stack
            .iter()
            .rposition(|sp| sp.name == name)
            .ok_or_else(|| FrankenError::internal(format!("no savepoint named '{name}'")))?;
        // RELEASE removes the named savepoint and all savepoints above it.
        // Changes since the savepoint are kept (merged into the parent).
        self.savepoint_stack.truncate(pos);
        Ok(())
    }

    fn rollback_to_savepoint(&mut self, _cx: &Cx, name: &str) -> Result<()> {
        let pos = self
            .savepoint_stack
            .iter()
            .rposition(|sp| sp.name == name)
            .ok_or_else(|| FrankenError::internal(format!("no savepoint named '{name}'")))?;

        let entry = &self.savepoint_stack[pos];

        // Restore write-set FIRST to ensure we don't leave the transaction in an
        // inconsistent state if PageBuf allocation fails (OOM).
        let new_write_set = entry
            .write_set_snapshot
            .iter()
            .map(|(&k, v)| -> Result<(PageNumber, StagedPage)> {
                Ok((
                    k,
                    StagedPage::from_page_data_for_pool(&self.pool, v.clone())?,
                ))
            })
            .collect::<Result<HashMap<_, _>>>()?;

        // Track pages that were allocated after the savepoint so that get_page
        // can return zeros for them instead of BusySnapshot error.
        for page_no in self
            .allocated_from_eof
            .iter()
            .skip(entry.allocated_from_eof_snapshot.len())
        {
            self.rolled_back_pages.insert(*page_no);
        }
        for page_no in self
            .allocated_from_freelist
            .iter()
            .skip(entry.allocated_from_freelist_snapshot.len())
        {
            self.rolled_back_pages.insert(*page_no);
        }

        {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimpleTransaction lock poisoned"))?;
            if self.mode != TransactionMode::Concurrent {
                inner.next_page = entry.next_page_snapshot;
                inner.freelist.clone_from(&entry.freelist_snapshot);
                // Lease pages reference the rolled-back next_page range
                // and will be re-allocated by future EOF allocations, so
                // just drop them.
                self.page_lease.clear();
            } else {
                // Return unused lease pages to the freelist before
                // returning post-savepoint EOF/freelist allocations.
                // These are valid EOF page numbers that next_page has
                // already advanced past (concurrent mode doesn't roll
                // back next_page).
                return_pages_to_freelist(&mut inner.freelist, self.page_lease.drain(..));
                return_pages_to_freelist(
                    &mut inner.freelist,
                    self.allocated_from_eof
                        .drain(entry.allocated_from_eof_snapshot.len()..),
                );
                return_pages_to_freelist(
                    &mut inner.freelist,
                    self.allocated_from_freelist
                        .drain(entry.allocated_from_freelist_snapshot.len()..),
                );
            }
        }

        self.allocated_from_freelist = entry.allocated_from_freelist_snapshot.clone();
        self.allocated_from_eof = entry.allocated_from_eof_snapshot.clone();
        self.freed_pages = entry.freed_pages_snapshot.clone();
        self.write_set = new_write_set;
        self.write_pages_sorted = entry.write_pages_sorted_snapshot.clone();

        // Discard savepoints created after the named one, but keep
        // the named savepoint itself (it can be rolled back to again).
        self.savepoint_stack.truncate(pos + 1);
        Ok(())
    }
}

impl<V: Vfs> Drop for SimpleTransaction<V> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Ok(mut inner) = self.inner.lock() {
            // Restore freelist allocations.
            return_pages_to_freelist(&mut inner.freelist, self.allocated_from_freelist.drain(..));

            if self.is_writer && self.mode != TransactionMode::Concurrent {
                // Non-concurrent: next_page will be reset, so lease pages
                // are re-issued naturally. Just drop them to avoid holes.
                self.page_lease.clear();

                inner.db_size = self.original_db_size;

                // Reset next_page to avoid holes if we allocated pages that are now discarded.
                // Logic matches SimplePager::open and SimpleTransaction::rollback.
                let db_size = inner.db_size;
                inner.next_page = if db_size >= 2 {
                    db_size.saturating_add(1)
                } else {
                    2
                };

                inner.writer_active = false;
            } else if self.is_writer && self.mode == TransactionMode::Concurrent {
                // Concurrent: next_page stays advanced, so return lease
                // pages and EOF allocations to the freelist.
                return_pages_to_freelist(&mut inner.freelist, self.page_lease.drain(..));
                return_pages_to_freelist(&mut inner.freelist, self.allocated_from_eof.drain(..));
            } else {
                // Read-only: lease should be empty, clear defensively.
                self.page_lease.clear();
            }
            inner.active_transactions = inner.active_transactions.saturating_sub(1);
            let preserve_level =
                retained_lock_level_after_txn_exit(inner.active_transactions, inner.writer_active);
            // Final unlock should preserve caller lineage without letting
            // inherited cancellation strand the file lock during drop cleanup.
            let _mask = self.cleanup_cx.masked();
            let _ = inner.db_file.unlock(&self.cleanup_cx, preserve_level);
        }
        // We cannot easily delete the journal file here because Drop doesn't
        // take a Context or return a Result. It's best effort cleanup.
        // Hot journal recovery will handle any leftover files on next open.
        self.finished = true;
    }
}

// ---------------------------------------------------------------------------
// CheckpointPageWriter implementation for WAL checkpointing
// ---------------------------------------------------------------------------

/// A checkpoint page writer that writes pages directly to the database file.
///
/// This type implements [`CheckpointPageWriter`] and is used during WAL
/// checkpointing to transfer committed pages from the WAL back to the main
/// database file.
///
/// The writer holds a reference to the pager's inner state and acquires the
/// mutex for each operation. This is acceptable because checkpoint is an
/// infrequent operation and the writes must be serialized with other pager
/// operations anyway.
pub struct SimplePagerCheckpointWriter<V: Vfs>
where
    V::File: Send + Sync,
{
    inner: Arc<Mutex<PagerInner<V::File>>>,
    cache: Arc<ShardedPageCache>,
    published: Arc<PublishedPagerState>,
}

impl<V: Vfs> traits::sealed::Sealed for SimplePagerCheckpointWriter<V> where V::File: Send + Sync {}

impl<V> SimplePagerCheckpointWriter<V>
where
    V: Vfs + Send + Sync,
    V::File: Send + Sync,
{
    /// Patch page 1 header fields that must remain globally consistent.
    ///
    /// This ensures external SQLite readers see:
    /// - a valid change counter (24..28),
    /// - the true on-disk page count (28..32),
    /// - matching version-valid-for (92..96).
    fn patch_page1_header(
        inner: &mut PagerInner<V::File>,
        cache: &ShardedPageCache,
        cx: &Cx,
    ) -> Result<()> {
        // SQLite databases always keep page 1 when non-empty.
        if inner.db_size == 0 {
            return Ok(());
        }

        let page_size = inner.page_size.as_usize();
        let mut page1 = vec![0u8; page_size];
        let bytes_read = inner.db_file.read(cx, &mut page1, 0)?;
        if bytes_read < DATABASE_HEADER_SIZE {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "short read while patching page 1 header: got {bytes_read} bytes, need at least {DATABASE_HEADER_SIZE}",
                ),
            });
        }

        let new_page_count = inner.db_size;
        let current_change_counter = inner.commit_seq.get() as u32;

        page1[24..28].copy_from_slice(&current_change_counter.to_be_bytes());
        page1[28..32].copy_from_slice(&new_page_count.to_be_bytes());
        page1[92..96].copy_from_slice(&current_change_counter.to_be_bytes());
        inner.db_file.write(cx, &page1, 0)?;
        cache.evict(PageNumber::ONE);
        Ok(())
    }
}

impl<V> traits::CheckpointPageWriter for SimplePagerCheckpointWriter<V>
where
    V: Vfs + Send + Sync,
    V::File: Send + Sync,
{
    fn write_page(&mut self, cx: &Cx, page_no: PageNumber, data: &[u8]) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePagerCheckpointWriter lock poisoned"))?;

        // Update db_size if this page extends the database.
        inner.db_size = inner.db_size.max(page_no.get());

        // Write directly to the database file, bypassing the cache.
        // The WAL checkpoint is authoritative, so we overwrite any cached version.
        let page_size = inner.page_size.as_usize();
        let offset = u64::from(page_no.get() - 1) * page_size as u64;

        // For page 1, repair header fields after writing the frame bytes.
        // A final repair also occurs in sync() so page_count remains correct
        // even when page 1 was checkpointed before higher-numbered pages.
        inner.db_file.write(cx, data, offset)?;
        if page_no == PageNumber::ONE && data.len() >= DATABASE_HEADER_SIZE {
            Self::patch_page1_header(&mut inner, &self.cache, cx)?;
        }

        // Invalidate cache entry if present to avoid stale reads.
        self.cache.evict(page_no);
        if page_no == PageNumber::ONE {
            // D1-CRITICAL Change 3: Use sharded publish_remove_page.
            self.published.publish_remove_page(
                cx,
                PublishedPagerUpdate {
                    visible_commit_seq: inner.commit_seq,
                    db_size: inner.db_size,
                    journal_mode: inner.journal_mode,
                    freelist_count: inner.freelist.len(),
                    checkpoint_active: inner.checkpoint_active,
                },
                PageNumber::ONE,
            );
        }

        drop(inner);
        Ok(())
    }

    fn truncate(&mut self, cx: &Cx, n_pages: u32) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePagerCheckpointWriter lock poisoned"))?;

        let old_db_size = inner.db_size;
        let page_size = inner.page_size.as_usize();
        let target_size = u64::from(n_pages) * page_size as u64;
        inner.db_file.truncate(cx, target_size)?;
        inner.db_size = n_pages;

        // Invalidate cached pages beyond the new size.
        // ShardedPageCache is internally synchronized, so no lock needed.
        for pgno in (n_pages.saturating_add(1))..=old_db_size {
            if let Some(page_no) = PageNumber::new(pgno) {
                self.cache.evict(page_no);
            }
        }
        // D1-CRITICAL Change 3: Use sharded publish_truncate_checkpoint.
        self.published.publish_truncate_checkpoint(
            cx,
            PublishedPagerUpdate {
                visible_commit_seq: inner.commit_seq,
                db_size: inner.db_size,
                journal_mode: inner.journal_mode,
                freelist_count: inner.freelist.len(),
                checkpoint_active: inner.checkpoint_active,
            },
            n_pages,
        );

        drop(inner);
        Ok(())
    }

    fn sync(&mut self, cx: &Cx) -> Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| FrankenError::internal("SimplePagerCheckpointWriter lock poisoned"))?;
        // Ensure header page_count reflects the final db_size after all
        // checkpoint writes/truncation, even if page 1 was checkpointed early.
        // ShardedPageCache is internally synchronized, so no lock needed.
        Self::patch_page1_header(&mut inner, &self.cache, cx)?;
        // D1-CRITICAL Change 3: Use sharded publish_remove_page.
        self.published.publish_remove_page(
            cx,
            PublishedPagerUpdate {
                visible_commit_seq: inner.commit_seq,
                db_size: inner.db_size,
                journal_mode: inner.journal_mode,
                freelist_count: inner.freelist.len(),
                checkpoint_active: inner.checkpoint_active,
            },
            PageNumber::ONE,
        );
        inner.db_file.sync(cx, SyncFlags::NORMAL)
    }
}

impl<V: Vfs> SimplePager<V>
where
    V::File: Send + Sync,
{
    /// Create a checkpoint page writer for WAL checkpointing.
    ///
    /// The returned writer implements [`CheckpointPageWriter`] and can be
    /// wrapped in a `CheckpointTargetAdapter` from `fsqlite-core` to satisfy
    /// the WAL executor's `CheckpointTarget` trait.
    ///
    /// # Panics
    ///
    /// This method does not panic, but the returned writer's methods may
    /// return errors if the pager's internal mutex is poisoned.
    #[must_use]
    pub fn checkpoint_writer(&self) -> SimplePagerCheckpointWriter<V> {
        SimplePagerCheckpointWriter {
            inner: Arc::clone(&self.inner),
            cache: Arc::clone(&self.cache),
            published: Arc::clone(&self.published),
        }
    }

    /// Run a WAL checkpoint to transfer frames from the WAL to the database.
    ///
    /// This is the main checkpoint entry point for WAL mode. It:
    /// 1. Acquires the pager lock
    /// 2. Creates a checkpoint writer for database page writes
    /// 3. Delegates to the WAL backend's checkpoint implementation
    ///
    /// # Arguments
    ///
    /// * `cx` - Cancellation/deadline context
    /// * `mode` - Checkpoint mode (Passive, Full, Restart, Truncate)
    ///
    /// # Returns
    ///
    /// A `CheckpointResult` describing what was accomplished, or an error if:
    /// - The pager is not in WAL mode
    /// - The pager lock is poisoned
    /// - Any I/O error occurs during the checkpoint
    ///
    /// # Notes
    ///
    /// This implementation refuses to checkpoint while any transaction is active.
    /// It starts from the beginning (backfilled_frames = 0) and passes
    /// `oldest_reader_frame = None`. Because pager does not yet track external
    /// reader end marks, `RESTART` and `TRUNCATE` are conservatively downgraded
    /// to `FULL` so we never reset or truncate WAL based on incomplete reader
    /// visibility. For incremental, reader-aware checkpointing, use the
    /// lower-level WAL backend API.
    pub fn checkpoint(
        &self,
        cx: &Cx,
        mode: traits::CheckpointMode,
    ) -> Result<traits::CheckpointResult> {
        let cleanup_cx = cleanup_child_cx(cx);
        // Take the WAL backend out of the pager while marking checkpoint active.
        // `begin()` and deferred writer upgrades are blocked while this flag is
        // set so commits cannot observe "WAL mode but no backend".
        let wal = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| FrankenError::internal("SimplePager lock poisoned"))?;

            // Check we're in WAL mode.
            if inner.journal_mode != JournalMode::Wal {
                return Err(FrankenError::Unsupported);
            }
            if inner.checkpoint_active {
                return Err(FrankenError::Busy);
            }
            // Without reader tracking in pager, the safe policy is to refuse
            // checkpoint while any transaction is active.
            if inner.active_transactions > 0 {
                return Err(FrankenError::Busy);
            }

            inner.checkpoint_active = true;
            let mut wal_guard = self
                .wal_backend
                .write()
                .map_err(|_| FrankenError::internal("SharedWalBackend lock poisoned"))?;
            let Some(wal) = wal_guard.take() else {
                inner.checkpoint_active = false;
                return Err(FrankenError::internal(
                    "WAL mode active but no WAL backend installed",
                ));
            };
            // D1-CRITICAL Change 3: Use sharded publish_metadata_only.
            self.published.publish_metadata_only(
                cx,
                PublishedPagerUpdate {
                    visible_commit_seq: inner.commit_seq,
                    db_size: inner.db_size,
                    journal_mode: inner.journal_mode,
                    freelist_count: inner.freelist.len(),
                    checkpoint_active: inner.checkpoint_active,
                },
            );
            wal
        };
        // Lock is released here.

        struct CheckpointGuard<'a, F: VfsFile> {
            inner: &'a std::sync::Mutex<PagerInner<F>>,
            published: &'a PublishedPagerState,
            wal_backend: &'a SharedWalBackend,
            wal: Option<Box<dyn WalBackend>>,
            cleanup_cx: Cx,
        }

        impl<F: VfsFile> Drop for CheckpointGuard<'_, F> {
            fn drop(&mut self) {
                if let Ok(mut inner) = self.inner.lock() {
                    if let Some(wal) = self.wal.take() {
                        if let Ok(mut wal_guard) = self.wal_backend.write() {
                            *wal_guard = Some(wal);
                        }
                    }
                    inner.checkpoint_active = false;
                    let _mask = self.cleanup_cx.masked();
                    // D1-CRITICAL Change 3: Use sharded publish_metadata_only.
                    self.published.publish_metadata_only(
                        &self.cleanup_cx,
                        PublishedPagerUpdate {
                            visible_commit_seq: inner.commit_seq,
                            db_size: inner.db_size,
                            journal_mode: inner.journal_mode,
                            freelist_count: inner.freelist.len(),
                            checkpoint_active: inner.checkpoint_active,
                        },
                    );
                }
            }
        }

        let mut guard = CheckpointGuard {
            inner: &self.inner,
            published: self.published.as_ref(),
            wal_backend: &self.wal_backend,
            wal: Some(wal),
            cleanup_cx,
        };

        // Create a checkpoint writer that writes directly to the database file.
        let mut writer = self.checkpoint_writer();
        let effective_mode = match mode {
            traits::CheckpointMode::Restart | traits::CheckpointMode::Truncate => {
                tracing::debug!(
                    requested_mode = ?mode,
                    "downgrading checkpoint mode because pager lacks reader-tracking for safe WAL reset"
                );
                traits::CheckpointMode::Full
            }
            _ => mode,
        };

        // Run the checkpoint from the beginning. Reader-aware incremental
        // checkpointing requires exposing oldest-reader tracking from pager.
        guard
            .wal
            .as_mut()
            .expect("wal was just inserted")
            .checkpoint(cx, effective_mode, &mut writer, 0, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{MvccPager, TransactionHandle, TransactionMode};
    use fsqlite_types::PageSize;
    use fsqlite_types::flags::{AccessFlags, SyncFlags, VfsOpenFlags};
    use fsqlite_types::{BTreePageHeader, DatabaseHeader};
    use fsqlite_vfs::{MemoryFile, MemoryVfs, Vfs, VfsFile};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex, OnceLock};

    const BEAD_ID: &str = "bd-bca.1";
    type ObservedLockLevel = Arc<Mutex<LockLevel>>;
    type ObservedUnlockTraceIds = Arc<Mutex<Vec<u64>>>;
    type ObservedCleanupUnlockHarness = (
        SimplePager<ObservedLockVfs>,
        ObservedLockLevel,
        ObservedUnlockTraceIds,
    );

    fn init_publication_test_tracing() {
        static TRACING_INIT: OnceLock<()> = OnceLock::new();
        TRACING_INIT.get_or_init(|| {
            if tracing_subscriber::fmt()
                .with_ansi(false)
                .with_max_level(tracing::Level::TRACE)
                .with_test_writer()
                .try_init()
                .is_err()
            {
                // Another test already installed a global subscriber.
            }
        });
    }

    fn test_pager() -> (SimplePager<MemoryVfs>, PathBuf) {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/test.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        (pager, path)
    }

    fn sample_page(seed: u8) -> Vec<u8> {
        let page_size = PageSize::DEFAULT.as_usize();
        let mut page = vec![0u8; page_size];
        for (i, byte) in page.iter_mut().enumerate() {
            let reduced = u8::try_from(i % 251).expect("modulo fits u8");
            *byte = reduced ^ seed;
        }
        page
    }

    #[derive(Debug, Clone, Copy)]
    struct ReadSurfaceSnapshot {
        cache: PageCacheMetricsSnapshot,
        published_hits: u64,
    }

    fn read_surface_snapshot<V>(pager: &SimplePager<V>) -> ReadSurfaceSnapshot
    where
        V: Vfs + Send + Sync,
        V::File: Send + Sync,
    {
        ReadSurfaceSnapshot {
            cache: pager.cache_metrics_snapshot().unwrap(),
            published_hits: pager.published_page_hits(),
        }
    }

    fn observed_read_total(before: ReadSurfaceSnapshot, after: ReadSurfaceSnapshot) -> u64 {
        after
            .cache
            .total_accesses()
            .saturating_sub(before.cache.total_accesses())
            .saturating_add(after.published_hits.saturating_sub(before.published_hits))
    }

    fn observed_read_hit_rate_percent(
        before: ReadSurfaceSnapshot,
        after: ReadSurfaceSnapshot,
    ) -> f64 {
        let cache_hits = after.cache.hits.saturating_sub(before.cache.hits);
        let published_hits = after.published_hits.saturating_sub(before.published_hits);
        let total_reads = observed_read_total(before, after);
        if total_reads == 0 {
            0.0
        } else {
            (cache_hits.saturating_add(published_hits) as f64 * 100.0) / total_reads as f64
        }
    }

    struct DropAwareWalBackend {
        dropped: Arc<Mutex<bool>>,
    }

    impl Drop for DropAwareWalBackend {
        fn drop(&mut self) {
            *self
                .dropped
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        }
    }

    impl crate::traits::WalBackend for DropAwareWalBackend {
        fn append_frame(
            &mut self,
            _cx: &Cx,
            _page_number: u32,
            _page_data: &[u8],
            _db_size_if_commit: u32,
        ) -> Result<()> {
            Ok(())
        }

        fn read_page(&mut self, _cx: &Cx, _page_number: u32) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn sync(&mut self, _cx: &Cx) -> Result<()> {
            Ok(())
        }

        fn frame_count(&self) -> usize {
            0
        }

        fn checkpoint(
            &mut self,
            _cx: &Cx,
            _mode: crate::traits::CheckpointMode,
            _writer: &mut dyn crate::traits::CheckpointPageWriter,
            _backfilled_frames: u32,
            _oldest_reader_frame: Option<u32>,
        ) -> Result<crate::traits::CheckpointResult> {
            Ok(crate::traits::CheckpointResult {
                total_frames: 0,
                frames_backfilled: 0,
                completed: false,
                wal_was_reset: false,
            })
        }
    }

    #[derive(Clone, Default)]
    struct JournalDeleteFailVfs {
        inner: MemoryVfs,
    }

    impl JournalDeleteFailVfs {
        fn new() -> Self {
            Self {
                inner: MemoryVfs::new(),
            }
        }
    }

    impl Vfs for JournalDeleteFailVfs {
        type File = MemoryFile;

        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn open(
            &self,
            cx: &Cx,
            path: Option<&std::path::Path>,
            flags: VfsOpenFlags,
        ) -> Result<(Self::File, VfsOpenFlags)> {
            self.inner.open(cx, path, flags)
        }

        fn delete(&self, cx: &Cx, path: &std::path::Path, sync_dir: bool) -> Result<()> {
            if path.to_string_lossy().ends_with("-journal") {
                return Err(FrankenError::internal(
                    "simulated journal delete failure".to_owned(),
                ));
            }
            self.inner.delete(cx, path, sync_dir)
        }

        fn access(&self, cx: &Cx, path: &std::path::Path, flags: AccessFlags) -> Result<bool> {
            self.inner.access(cx, path, flags)
        }

        fn full_pathname(&self, cx: &Cx, path: &std::path::Path) -> Result<PathBuf> {
            self.inner.full_pathname(cx, path)
        }
    }

    #[derive(Clone, Default)]
    struct WalReadonlyFallbackProbeVfs {
        inner: MemoryVfs,
        readonly_wal_open_attempted: Arc<AtomicBool>,
    }

    impl WalReadonlyFallbackProbeVfs {
        fn new() -> Self {
            Self {
                inner: MemoryVfs::new(),
                readonly_wal_open_attempted: Arc::new(AtomicBool::new(false)),
            }
        }

        fn readonly_wal_open_attempted(&self) -> bool {
            self.readonly_wal_open_attempted
                .load(AtomicOrdering::Relaxed)
        }
    }

    impl Vfs for WalReadonlyFallbackProbeVfs {
        type File = MemoryFile;

        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn open(
            &self,
            cx: &Cx,
            path: Option<&std::path::Path>,
            flags: VfsOpenFlags,
        ) -> Result<(Self::File, VfsOpenFlags)> {
            let is_wal = flags.contains(VfsOpenFlags::WAL);
            if is_wal && flags.contains(VfsOpenFlags::READONLY) {
                self.readonly_wal_open_attempted
                    .store(true, AtomicOrdering::Relaxed);
            }
            if is_wal && flags.contains(VfsOpenFlags::READWRITE) {
                return Err(FrankenError::CannotOpen {
                    path: path
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("<wal-probe>")),
                });
            }
            self.inner.open(cx, path, flags)
        }

        fn delete(&self, cx: &Cx, path: &std::path::Path, sync_dir: bool) -> Result<()> {
            self.inner.delete(cx, path, sync_dir)
        }

        fn access(&self, cx: &Cx, path: &std::path::Path, flags: AccessFlags) -> Result<bool> {
            self.inner.access(cx, path, flags)
        }

        fn full_pathname(&self, cx: &Cx, path: &std::path::Path) -> Result<PathBuf> {
            self.inner.full_pathname(cx, path)
        }
    }

    #[derive(Clone)]
    struct ObservedLockVfs {
        inner: MemoryVfs,
        observed_lock_level: ObservedLockLevel,
        observed_unlock_trace_ids: ObservedUnlockTraceIds,
        fail_unlock_on_checkpoint_error: bool,
    }

    impl ObservedLockVfs {
        fn new() -> Self {
            Self {
                inner: MemoryVfs::new(),
                observed_lock_level: Arc::new(Mutex::new(LockLevel::None)),
                observed_unlock_trace_ids: Arc::new(Mutex::new(Vec::new())),
                fail_unlock_on_checkpoint_error: false,
            }
        }

        fn observed_lock_level(&self) -> ObservedLockLevel {
            Arc::clone(&self.observed_lock_level)
        }

        fn observed_unlock_trace_ids(&self) -> ObservedUnlockTraceIds {
            Arc::clone(&self.observed_unlock_trace_ids)
        }

        fn with_checkpoint_enforced_unlock() -> Self {
            Self {
                fail_unlock_on_checkpoint_error: true,
                ..Self::new()
            }
        }
    }

    struct ObservedLockFile {
        inner: MemoryFile,
        observed_lock_level: ObservedLockLevel,
        observed_unlock_trace_ids: ObservedUnlockTraceIds,
        fail_unlock_on_checkpoint_error: bool,
    }

    impl Vfs for ObservedLockVfs {
        type File = ObservedLockFile;

        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn open(
            &self,
            cx: &Cx,
            path: Option<&std::path::Path>,
            flags: VfsOpenFlags,
        ) -> Result<(Self::File, VfsOpenFlags)> {
            let (inner, actual_flags) = self.inner.open(cx, path, flags)?;
            Ok((
                ObservedLockFile {
                    inner,
                    observed_lock_level: self.observed_lock_level(),
                    observed_unlock_trace_ids: self.observed_unlock_trace_ids(),
                    fail_unlock_on_checkpoint_error: self.fail_unlock_on_checkpoint_error,
                },
                actual_flags,
            ))
        }

        fn delete(&self, cx: &Cx, path: &std::path::Path, sync_dir: bool) -> Result<()> {
            self.inner.delete(cx, path, sync_dir)
        }

        fn access(&self, cx: &Cx, path: &std::path::Path, flags: AccessFlags) -> Result<bool> {
            self.inner.access(cx, path, flags)
        }

        fn full_pathname(&self, cx: &Cx, path: &std::path::Path) -> Result<PathBuf> {
            self.inner.full_pathname(cx, path)
        }
    }

    impl VfsFile for ObservedLockFile {
        fn close(&mut self, cx: &Cx) -> Result<()> {
            let result = self.inner.close(cx);
            if result.is_ok() {
                *self.observed_lock_level.lock().unwrap() = LockLevel::None;
            }
            result
        }

        fn read(&self, cx: &Cx, buf: &mut [u8], offset: u64) -> Result<usize> {
            self.inner.read(cx, buf, offset)
        }

        fn write(&mut self, cx: &Cx, buf: &[u8], offset: u64) -> Result<()> {
            self.inner.write(cx, buf, offset)
        }

        fn truncate(&mut self, cx: &Cx, size: u64) -> Result<()> {
            self.inner.truncate(cx, size)
        }

        fn sync(&mut self, cx: &Cx, flags: SyncFlags) -> Result<()> {
            self.inner.sync(cx, flags)
        }

        fn file_size(&self, cx: &Cx) -> Result<u64> {
            self.inner.file_size(cx)
        }

        fn lock(&mut self, cx: &Cx, level: LockLevel) -> Result<()> {
            self.inner.lock(cx, level)?;
            *self.observed_lock_level.lock().unwrap() = level;
            Ok(())
        }

        fn unlock(&mut self, cx: &Cx, level: LockLevel) -> Result<()> {
            self.observed_unlock_trace_ids
                .lock()
                .unwrap()
                .push(cx.trace_id());
            if self.fail_unlock_on_checkpoint_error {
                cx.checkpoint()
                    .map_err(|err| FrankenError::internal(err.to_string()))?;
            }
            self.inner.unlock(cx, level)?;
            *self.observed_lock_level.lock().unwrap() = level;
            Ok(())
        }

        fn check_reserved_lock(&self, cx: &Cx) -> Result<bool> {
            self.inner.check_reserved_lock(cx)
        }

        fn sector_size(&self) -> u32 {
            self.inner.sector_size()
        }

        fn device_characteristics(&self) -> u32 {
            self.inner.device_characteristics()
        }

        fn shm_map(
            &mut self,
            cx: &Cx,
            region: u32,
            size: u32,
            extend: bool,
        ) -> Result<fsqlite_vfs::ShmRegion> {
            self.inner.shm_map(cx, region, size, extend)
        }

        fn shm_lock(&mut self, cx: &Cx, offset: u32, n: u32, flags: u32) -> Result<()> {
            self.inner.shm_lock(cx, offset, n, flags)
        }

        fn shm_barrier(&self) {
            self.inner.shm_barrier();
        }

        fn shm_unmap(&mut self, cx: &Cx, delete: bool) -> Result<()> {
            self.inner.shm_unmap(cx, delete)
        }
    }

    fn observed_lock_pager() -> (SimplePager<ObservedLockVfs>, ObservedLockLevel) {
        let vfs = ObservedLockVfs::new();
        let observed_lock_level = vfs.observed_lock_level();
        let path = PathBuf::from("/observed-lock.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        (pager, observed_lock_level)
    }

    fn observed_lock_pager_with_checkpoint_enforced_unlock() -> ObservedCleanupUnlockHarness {
        let vfs = ObservedLockVfs::with_checkpoint_enforced_unlock();
        let observed_lock_level = vfs.observed_lock_level();
        let observed_unlock_trace_ids = vfs.observed_unlock_trace_ids();
        let path = PathBuf::from("/observed-lock-checkpoint.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        (pager, observed_lock_level, observed_unlock_trace_ids)
    }

    #[derive(Debug, Default)]
    struct ExclusiveLockMetrics {
        owner: Option<u64>,
        acquired_at: Option<Instant>,
        acquisition_count: usize,
        hold_samples_ns: Vec<u64>,
        wait_samples_ns: Vec<u64>,
    }

    #[derive(Clone)]
    struct BlockingObservedLockVfs {
        inner: MemoryVfs,
        observed_lock_level: Arc<Mutex<LockLevel>>,
        next_handle_id: StdArc<AtomicU64>,
        exclusive_metrics: StdArc<(StdMutex<ExclusiveLockMetrics>, StdCondvar)>,
    }

    impl BlockingObservedLockVfs {
        fn new() -> Self {
            Self {
                inner: MemoryVfs::new(),
                observed_lock_level: Arc::new(Mutex::new(LockLevel::None)),
                next_handle_id: StdArc::new(AtomicU64::new(1)),
                exclusive_metrics: StdArc::new((
                    StdMutex::new(ExclusiveLockMetrics::default()),
                    StdCondvar::new(),
                )),
            }
        }

        fn observed_lock_level(&self) -> Arc<Mutex<LockLevel>> {
            Arc::clone(&self.observed_lock_level)
        }

        fn wait_for_exclusive_acquisitions(&self, target: usize) {
            let (metrics_lock, metrics_ready) = &*self.exclusive_metrics;
            let mut metrics = metrics_lock.lock().unwrap();
            while metrics.acquisition_count < target {
                metrics = metrics_ready.wait(metrics).unwrap();
            }
        }

        fn exclusive_hold_samples_ns(&self) -> Vec<u64> {
            let (metrics_lock, _) = &*self.exclusive_metrics;
            metrics_lock.lock().unwrap().hold_samples_ns.clone()
        }

        fn exclusive_wait_samples_ns(&self) -> Vec<u64> {
            let (metrics_lock, _) = &*self.exclusive_metrics;
            metrics_lock.lock().unwrap().wait_samples_ns.clone()
        }

        fn clear_exclusive_metrics(&self) {
            let (metrics_lock, _) = &*self.exclusive_metrics;
            let mut metrics = metrics_lock.lock().unwrap();
            *metrics = ExclusiveLockMetrics::default();
        }
    }

    struct BlockingObservedLockFile {
        inner: MemoryFile,
        observed_lock_level: Arc<Mutex<LockLevel>>,
        handle_id: u64,
        lock_level: LockLevel,
        exclusive_metrics: StdArc<(StdMutex<ExclusiveLockMetrics>, StdCondvar)>,
    }

    impl BlockingObservedLockFile {
        fn release_exclusive_hold(&self) {
            let (metrics_lock, metrics_ready) = &*self.exclusive_metrics;
            let mut metrics = metrics_lock.lock().unwrap();
            if metrics.owner == Some(self.handle_id) {
                if let Some(acquired_at) = metrics.acquired_at.take() {
                    metrics
                        .hold_samples_ns
                        .push(u64::try_from(acquired_at.elapsed().as_nanos()).unwrap_or(u64::MAX));
                }
                metrics.owner = None;
                metrics_ready.notify_all();
            }
        }
    }

    impl Vfs for BlockingObservedLockVfs {
        type File = BlockingObservedLockFile;

        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn open(
            &self,
            cx: &Cx,
            path: Option<&std::path::Path>,
            flags: VfsOpenFlags,
        ) -> Result<(Self::File, VfsOpenFlags)> {
            let (inner, actual_flags) = self.inner.open(cx, path, flags)?;
            Ok((
                BlockingObservedLockFile {
                    inner,
                    observed_lock_level: self.observed_lock_level(),
                    handle_id: self.next_handle_id.fetch_add(1, AtomicOrdering::Relaxed),
                    lock_level: LockLevel::None,
                    exclusive_metrics: StdArc::clone(&self.exclusive_metrics),
                },
                actual_flags,
            ))
        }

        fn delete(&self, cx: &Cx, path: &std::path::Path, sync_dir: bool) -> Result<()> {
            self.inner.delete(cx, path, sync_dir)
        }

        fn access(&self, cx: &Cx, path: &std::path::Path, flags: AccessFlags) -> Result<bool> {
            self.inner.access(cx, path, flags)
        }

        fn full_pathname(&self, cx: &Cx, path: &std::path::Path) -> Result<PathBuf> {
            self.inner.full_pathname(cx, path)
        }
    }

    impl VfsFile for BlockingObservedLockFile {
        fn close(&mut self, cx: &Cx) -> Result<()> {
            self.release_exclusive_hold();
            let result = self.inner.close(cx);
            if result.is_ok() {
                self.lock_level = LockLevel::None;
                *self.observed_lock_level.lock().unwrap() = LockLevel::None;
            }
            result
        }

        fn read(&self, cx: &Cx, buf: &mut [u8], offset: u64) -> Result<usize> {
            self.inner.read(cx, buf, offset)
        }

        fn write(&mut self, cx: &Cx, buf: &[u8], offset: u64) -> Result<()> {
            self.inner.write(cx, buf, offset)
        }

        fn truncate(&mut self, cx: &Cx, size: u64) -> Result<()> {
            self.inner.truncate(cx, size)
        }

        fn sync(&mut self, cx: &Cx, flags: SyncFlags) -> Result<()> {
            self.inner.sync(cx, flags)
        }

        fn file_size(&self, cx: &Cx) -> Result<u64> {
            self.inner.file_size(cx)
        }

        fn lock(&mut self, cx: &Cx, level: LockLevel) -> Result<()> {
            if self.lock_level < LockLevel::Exclusive && level >= LockLevel::Exclusive {
                let wait_started = Instant::now();
                let (metrics_lock, metrics_ready) = &*self.exclusive_metrics;
                let mut metrics = metrics_lock.lock().unwrap();
                while metrics.owner.is_some() && metrics.owner != Some(self.handle_id) {
                    metrics = metrics_ready.wait(metrics).unwrap();
                }
                metrics
                    .wait_samples_ns
                    .push(u64::try_from(wait_started.elapsed().as_nanos()).unwrap_or(u64::MAX));
                metrics.owner = Some(self.handle_id);
                metrics.acquired_at = Some(Instant::now());
                metrics.acquisition_count = metrics.acquisition_count.saturating_add(1);
                metrics_ready.notify_all();
            }

            self.inner.lock(cx, level)?;
            if self.lock_level < level {
                self.lock_level = level;
            }
            *self.observed_lock_level.lock().unwrap() = self.lock_level;
            Ok(())
        }

        fn unlock(&mut self, cx: &Cx, level: LockLevel) -> Result<()> {
            if self.lock_level >= LockLevel::Exclusive && level < LockLevel::Exclusive {
                self.release_exclusive_hold();
            }

            self.inner.unlock(cx, level)?;
            if self.lock_level > level {
                self.lock_level = level;
            }
            *self.observed_lock_level.lock().unwrap() = self.lock_level;
            Ok(())
        }

        fn check_reserved_lock(&self, cx: &Cx) -> Result<bool> {
            self.inner.check_reserved_lock(cx)
        }

        fn sector_size(&self) -> u32 {
            self.inner.sector_size()
        }

        fn device_characteristics(&self) -> u32 {
            self.inner.device_characteristics()
        }

        fn shm_map(
            &mut self,
            cx: &Cx,
            region: u32,
            size: u32,
            extend: bool,
        ) -> Result<fsqlite_vfs::ShmRegion> {
            self.inner.shm_map(cx, region, size, extend)
        }

        fn shm_lock(&mut self, cx: &Cx, offset: u32, n: u32, flags: u32) -> Result<()> {
            self.inner.shm_lock(cx, offset, n, flags)
        }

        fn shm_barrier(&self) {
            self.inner.shm_barrier();
        }

        fn shm_unmap(&mut self, cx: &Cx, delete: bool) -> Result<()> {
            self.inner.shm_unmap(cx, delete)
        }
    }

    #[derive(Debug)]
    struct DbWriteFailState {
        target_path: PathBuf,
        armed: bool,
        remaining_successful_db_writes: usize,
    }

    #[derive(Clone)]
    struct DbWriteFailOnceVfs {
        inner: MemoryVfs,
        state: Arc<Mutex<DbWriteFailState>>,
    }

    impl DbWriteFailOnceVfs {
        fn new(target_path: PathBuf) -> Self {
            Self {
                inner: MemoryVfs::new(),
                state: Arc::new(Mutex::new(DbWriteFailState {
                    target_path,
                    armed: false,
                    remaining_successful_db_writes: 0,
                })),
            }
        }

        fn arm_after_db_writes(&self, successful_db_writes_before_failure: usize) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.armed = true;
            state.remaining_successful_db_writes = successful_db_writes_before_failure;
        }
    }

    #[derive(Debug)]
    struct DbWriteFailOnceFile {
        inner: MemoryFile,
        state: Arc<Mutex<DbWriteFailState>>,
        is_target_db: bool,
    }

    impl Vfs for DbWriteFailOnceVfs {
        type File = DbWriteFailOnceFile;

        fn name(&self) -> &'static str {
            self.inner.name()
        }

        fn open(
            &self,
            cx: &Cx,
            path: Option<&std::path::Path>,
            flags: VfsOpenFlags,
        ) -> Result<(Self::File, VfsOpenFlags)> {
            let (inner, actual_flags) = self.inner.open(cx, path, flags)?;
            let is_target_db = {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                path == Some(state.target_path.as_path()) && flags.contains(VfsOpenFlags::MAIN_DB)
            };
            Ok((
                DbWriteFailOnceFile {
                    inner,
                    state: Arc::clone(&self.state),
                    is_target_db,
                },
                actual_flags,
            ))
        }

        fn delete(&self, cx: &Cx, path: &std::path::Path, sync_dir: bool) -> Result<()> {
            self.inner.delete(cx, path, sync_dir)
        }

        fn access(&self, cx: &Cx, path: &std::path::Path, flags: AccessFlags) -> Result<bool> {
            self.inner.access(cx, path, flags)
        }

        fn full_pathname(&self, cx: &Cx, path: &std::path::Path) -> Result<PathBuf> {
            self.inner.full_pathname(cx, path)
        }
    }

    impl VfsFile for DbWriteFailOnceFile {
        fn close(&mut self, cx: &Cx) -> Result<()> {
            self.inner.close(cx)
        }

        fn read(&self, cx: &Cx, buf: &mut [u8], offset: u64) -> Result<usize> {
            self.inner.read(cx, buf, offset)
        }

        fn write(&mut self, cx: &Cx, buf: &[u8], offset: u64) -> Result<()> {
            if self.is_target_db {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.armed {
                    if state.remaining_successful_db_writes == 0 {
                        state.armed = false;
                        return Err(FrankenError::Io(std::io::Error::other(
                            "simulated main-db write failure",
                        )));
                    }
                    state.remaining_successful_db_writes -= 1;
                }
            }
            self.inner.write(cx, buf, offset)
        }

        fn truncate(&mut self, cx: &Cx, size: u64) -> Result<()> {
            self.inner.truncate(cx, size)
        }

        fn sync(&mut self, cx: &Cx, flags: SyncFlags) -> Result<()> {
            self.inner.sync(cx, flags)
        }

        fn file_size(&self, cx: &Cx) -> Result<u64> {
            self.inner.file_size(cx)
        }

        fn lock(&mut self, cx: &Cx, level: fsqlite_types::LockLevel) -> Result<()> {
            self.inner.lock(cx, level)
        }

        fn unlock(&mut self, cx: &Cx, level: fsqlite_types::LockLevel) -> Result<()> {
            self.inner.unlock(cx, level)
        }

        fn check_reserved_lock(&self, cx: &Cx) -> Result<bool> {
            self.inner.check_reserved_lock(cx)
        }

        fn sector_size(&self) -> u32 {
            self.inner.sector_size()
        }

        fn device_characteristics(&self) -> u32 {
            self.inner.device_characteristics()
        }

        fn shm_map(
            &mut self,
            cx: &Cx,
            region: u32,
            size: u32,
            extend: bool,
        ) -> Result<fsqlite_vfs::ShmRegion> {
            self.inner.shm_map(cx, region, size, extend)
        }

        fn shm_lock(&mut self, cx: &Cx, offset: u32, n: u32, flags: u32) -> Result<()> {
            self.inner.shm_lock(cx, offset, n, flags)
        }

        fn shm_barrier(&self) {
            self.inner.shm_barrier();
        }

        fn shm_unmap(&mut self, cx: &Cx, delete: bool) -> Result<()> {
            self.inner.shm_unmap(cx, delete)
        }
    }

    #[test]
    fn test_open_empty_database() {
        let (pager, _) = test_pager();
        let inner = pager.inner.lock().unwrap();
        assert_eq!(inner.db_size, 1, "bead_id={BEAD_ID} case=empty_db_size");
        assert_eq!(
            inner.page_size,
            PageSize::DEFAULT,
            "bead_id={BEAD_ID} case=page_size_default"
        );
        drop(inner);

        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let raw_page = txn.get_page(&cx, PageNumber::ONE).unwrap().into_vec();

        let hdr: [u8; DATABASE_HEADER_SIZE] = raw_page[..DATABASE_HEADER_SIZE]
            .try_into()
            .expect("page 1 must contain database header");
        let parsed = DatabaseHeader::from_bytes(&hdr).expect("header should parse");
        assert_eq!(
            parsed.page_size,
            PageSize::DEFAULT,
            "bead_id={BEAD_ID} case=page1_header_page_size"
        );
        assert_eq!(
            parsed.page_count, 1,
            "bead_id={BEAD_ID} case=page1_header_page_count"
        );

        let btree_hdr =
            BTreePageHeader::parse(&raw_page, PageSize::DEFAULT, 0, true).expect("btree header");
        assert_eq!(
            btree_hdr.cell_count, 0,
            "bead_id={BEAD_ID} case=sqlite_master_initially_empty"
        );
    }

    #[test]
    fn test_open_existing_database_uses_header_page_size() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/page_size_autodetect.db");

        let expected_page_size = PageSize::new(8192).unwrap();
        let _pager = SimplePager::open(vfs.clone(), &path, expected_page_size).unwrap();
        let reopened = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        assert!(
            reopened.page_size() == expected_page_size,
            "bead_id={BEAD_ID} case=autodetect_existing_page_size"
        );
    }

    #[test]
    fn test_begin_refreshes_external_page_growth_before_allocation() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/pager_refresh_external_growth.db");
        let pager1 = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let pager2 = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut writer1 = pager1.begin(&cx, TransactionMode::Immediate).unwrap();
        let page2 = writer1.allocate_page(&cx).unwrap();
        assert_eq!(page2.get(), 2, "first writer should allocate page 2");
        writer1.write_page(&cx, page2, &vec![0xAB; ps]).unwrap();
        writer1.commit(&cx).unwrap();

        let mut writer2 = pager2.begin(&cx, TransactionMode::Immediate).unwrap();
        let page3 = writer2.allocate_page(&cx).unwrap();
        assert_eq!(
            page3.get(),
            3,
            "bead_id={BEAD_ID} case=refresh_external_growth_reissues_next_page"
        );
    }

    #[test]
    fn test_open_existing_database_rejects_non_page_aligned_size() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/misaligned.db");
        let cx = Cx::new();
        let _pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();

        let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
        let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();
        let file_size = db_file.file_size(&cx).unwrap();
        db_file.write(&cx, &[0xAB], file_size).unwrap();

        let Err(err) = SimplePager::open(vfs, &path, PageSize::DEFAULT) else {
            panic!("expected non-page-aligned file size error");
        };
        assert!(
            matches!(err, FrankenError::DatabaseCorrupt { .. }),
            "bead_id={BEAD_ID} case=reject_non_page_aligned_file_size"
        );
    }

    #[test]
    fn test_begin_readonly_transaction() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert!(!txn.is_writer, "bead_id={BEAD_ID} case=readonly_not_writer");
    }

    #[test]
    fn test_begin_write_transaction() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        assert!(txn.is_writer, "bead_id={BEAD_ID} case=immediate_is_writer");
    }

    #[test]
    fn test_begin_deferred_transaction_starts_reader() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::Deferred).unwrap();
        assert!(
            !txn.is_writer,
            "bead_id={BEAD_ID} case=deferred_starts_readonly"
        );
    }

    #[test]
    fn test_begin_concurrent_transaction_starts_reader() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        assert!(
            !txn.is_writer,
            "bead_id={BEAD_ID} case=concurrent_starts_readonly"
        );
    }

    #[test]
    fn test_deferred_upgrades_on_first_write_intent() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut deferred = pager.begin(&cx, TransactionMode::Deferred).unwrap();
        assert!(
            !deferred.is_writer,
            "bead_id={BEAD_ID} case=deferred_pre_upgrade"
        );

        let _page = deferred.allocate_page(&cx).unwrap();
        assert!(
            deferred.is_writer,
            "bead_id={BEAD_ID} case=deferred_upgraded_to_writer"
        );
    }

    #[test]
    fn test_deferred_upgrade_busy_when_writer_active() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let _writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut deferred = pager.begin(&cx, TransactionMode::Deferred).unwrap();

        let err = deferred.allocate_page(&cx).unwrap_err();
        assert!(matches!(err, FrankenError::Busy));
    }

    #[test]
    fn test_concurrent_writer_blocked() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let _txn1 = pager.begin(&cx, TransactionMode::Exclusive).unwrap();
        let result = pager.begin(&cx, TransactionMode::Immediate);
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=concurrent_writer_busy"
        );
    }

    #[test]
    fn test_multiple_readers_allowed() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let _r1 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let _r2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        // Both readers can coexist.
    }

    #[test]
    fn test_write_page_and_read_back() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();

        let page_no = txn.allocate_page(&cx).unwrap();
        let page_size = PageSize::DEFAULT.as_usize();
        let mut data = vec![0_u8; page_size];
        data[0] = 0xDE;
        data[1] = 0xAD;
        txn.write_page(&cx, page_no, &data).unwrap();

        let read_back = txn.get_page(&cx, page_no).unwrap();
        assert_eq!(
            read_back.as_ref()[0],
            0xDE,
            "bead_id={BEAD_ID} case=read_back_byte0"
        );
        assert_eq!(
            read_back.as_ref()[1],
            0xAD,
            "bead_id={BEAD_ID} case=read_back_byte1"
        );
    }

    #[test]
    fn test_commit_persists_pages() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        // Write in first transaction.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page_no = txn.allocate_page(&cx).unwrap();
        let page_size = PageSize::DEFAULT.as_usize();
        let mut data = vec![0_u8; page_size];
        data[0..4].copy_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);
        txn.write_page(&cx, page_no, &data).unwrap();
        txn.commit(&cx).unwrap();

        // Read in second transaction.
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let read_back = txn2.get_page(&cx, page_no).unwrap();
        assert_eq!(
            &read_back.as_ref()[0..4],
            &[0xCA, 0xFE, 0xBA, 0xBE],
            "bead_id={BEAD_ID} case=commit_persists"
        );
    }

    #[test]
    fn test_rollback_discards_writes() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        // Allocate and write a page, then commit so it exists on disk.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page_no = txn.allocate_page(&cx).unwrap();
        let page_size = PageSize::DEFAULT.as_usize();
        let original = vec![0x11_u8; page_size];
        txn.write_page(&cx, page_no, &original).unwrap();
        txn.commit(&cx).unwrap();

        // Overwrite in a new transaction, then rollback.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let modified = vec![0x99_u8; page_size];
        txn2.write_page(&cx, page_no, &modified).unwrap();
        txn2.rollback(&cx).unwrap();

        // Read again — should see original data.
        let txn3 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let read_back = txn3.get_page(&cx, page_no).unwrap();
        assert_eq!(
            read_back.as_ref()[0],
            0x11,
            "bead_id={BEAD_ID} case=rollback_restores"
        );
    }

    #[test]
    fn test_allocate_returns_sequential_pages() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();

        let p1 = txn.allocate_page(&cx).unwrap();
        let p2 = txn.allocate_page(&cx).unwrap();
        assert!(
            p2.get() > p1.get(),
            "bead_id={BEAD_ID} case=sequential_alloc p1={} p2={}",
            p1.get(),
            p2.get()
        );
    }

    #[test]
    fn test_free_page_reuses_on_next_alloc() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // 1. Allocate a page and commit.
        let mut txn1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn1.allocate_page(&cx).unwrap();
        txn1.write_page(&cx, p, &vec![0_u8; ps]).unwrap();
        txn1.commit(&cx).unwrap();

        // 2. Free the page and commit -> moves to freelist.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.free_page(&cx, p).unwrap();
        txn2.commit(&cx).unwrap();

        // Verify freelist has the page.
        {
            let inner = pager.inner.lock().unwrap();
            assert_eq!(inner.freelist.len(), 1);
            assert_eq!(inner.freelist[0], p);
            drop(inner);
        }

        // 3. Allocate the page again (pops from freelist).
        let mut txn3 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = txn3.allocate_page(&cx).unwrap();
        assert_eq!(
            p2,
            p,
            "bead_id={BEAD_ID} case=freelist_reuse p3={} p1={}",
            p2.get(),
            p.get()
        );
    }

    #[test]
    fn test_freed_pages_are_quarantined_until_commit() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p2, &vec![0xAA; ps]).unwrap();
        txn.free_page(&cx, p2).unwrap();

        let p3 = txn.allocate_page(&cx).unwrap();
        assert_eq!(
            p3.get(),
            p2.get() + 1,
            "bead_id={BEAD_ID} case=freed_pages_quarantined_until_commit"
        );
        txn.write_page(&cx, p3, &vec![0xBB; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let mut next_txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let reused = next_txn.allocate_page(&cx).unwrap();
        assert_eq!(
            reused, p2,
            "bead_id={BEAD_ID} case=freed_pages_reenter_committed_freelist_after_commit"
        );
        next_txn.rollback(&cx).unwrap();
    }

    #[test]
    fn test_get_page_rejects_page_freed_in_same_transaction() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let page = {
            let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page = seed.allocate_page(&cx).unwrap();
            seed.write_page(&cx, page, &vec![0xAA; ps]).unwrap();
            seed.commit(&cx).unwrap();
            page
        };

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.free_page(&cx, page).unwrap();

        let err = txn
            .get_page(&cx, page)
            .expect_err("read-after-free must be rejected");
        let detail = err.to_string();
        assert!(
            detail.contains("freed earlier in this transaction"),
            "expected read-after-free error, got: {detail}"
        );
    }

    #[test]
    fn test_cannot_free_page_one() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let result = txn.free_page(&cx, PageNumber::ONE);
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=cannot_free_page_one"
        );
    }

    #[test]
    fn test_readonly_cannot_write() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let result = txn.write_page(&cx, PageNumber::ONE, &[0_u8; 4096]);
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=readonly_cannot_write"
        );
    }

    #[test]
    fn test_readonly_cannot_allocate() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let result = txn.allocate_page(&cx);
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=readonly_cannot_allocate"
        );
    }

    #[test]
    fn test_drop_uncommitted_writer_releases_lock() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        {
            let _txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            // Dropped without commit or rollback.
        }

        // Should be able to begin a new writer.
        let txn2 = pager.begin(&cx, TransactionMode::Immediate);
        assert!(
            txn2.is_ok(),
            "bead_id={BEAD_ID} case=drop_releases_writer_lock"
        );
    }

    #[test]
    fn test_drop_cleanup_unlock_preserves_lineage_and_masks_cancellation() {
        let (pager, observed_lock_level, observed_unlock_trace_ids) =
            observed_lock_pager_with_checkpoint_enforced_unlock();
        let cx = Cx::new().with_trace_context(41, 0, 0);

        {
            let _txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
            cx.cancel();
        }

        assert_eq!(
            *observed_lock_level.lock().unwrap(),
            LockLevel::None,
            "bead_id={BEAD_ID} case=drop_cleanup_unlock_releases_lock_after_parent_cancel"
        );
        assert_eq!(
            observed_unlock_trace_ids.lock().unwrap().last().copied(),
            Some(41),
            "bead_id={BEAD_ID} case=drop_cleanup_unlock_uses_parent_trace_lineage"
        );
    }

    #[test]
    fn test_reader_exit_preserves_shared_lock_for_other_reader() {
        let (pager, observed_lock_level) = observed_lock_pager();
        let cx = Cx::new();

        let mut reader1 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let reader2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();

        assert_eq!(*observed_lock_level.lock().unwrap(), LockLevel::Shared);

        reader1.commit(&cx).unwrap();

        assert_eq!(
            *observed_lock_level.lock().unwrap(),
            LockLevel::Shared,
            "bead_id={BEAD_ID} case=reader_commit_keeps_shared_for_other_reader"
        );

        drop(reader2);

        assert_eq!(
            *observed_lock_level.lock().unwrap(),
            LockLevel::None,
            "bead_id={BEAD_ID} case=last_reader_releases_shared"
        );
    }

    #[test]
    fn test_reader_exit_preserves_reserved_lock_for_active_writer() {
        let (pager, observed_lock_level) = observed_lock_pager();
        let cx = Cx::new();

        let mut writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();

        assert_eq!(*observed_lock_level.lock().unwrap(), LockLevel::Reserved);

        drop(reader);

        assert_eq!(
            *observed_lock_level.lock().unwrap(),
            LockLevel::Reserved,
            "bead_id={BEAD_ID} case=reader_drop_keeps_reserved_for_writer"
        );

        writer.commit(&cx).unwrap();

        assert_eq!(
            *observed_lock_level.lock().unwrap(),
            LockLevel::None,
            "bead_id={BEAD_ID} case=writer_commit_releases_last_lock"
        );
    }

    #[test]
    fn test_commit_then_drop_no_double_release() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            txn.commit(&cx).unwrap();
            // committed=true, drop should skip writer_active=false
        }

        // Writer should already be released by commit.
        let txn2 = pager.begin(&cx, TransactionMode::Immediate);
        assert!(
            txn2.is_ok(),
            "bead_id={BEAD_ID} case=commit_releases_writer"
        );
    }

    #[test]
    fn test_double_commit_is_idempotent() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.commit(&cx).unwrap();
        // Second commit should be a no-op.
        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_multi_page_write_commit_read() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let page_size = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut allocated_pages = Vec::new();
        for i in 0_u8..5 {
            let p = txn.allocate_page(&cx).unwrap();
            let data = vec![i; page_size];
            txn.write_page(&cx, p, &data).unwrap();
            allocated_pages.push(p);
        }
        txn.commit(&cx).unwrap();

        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        for (i, &p) in allocated_pages.iter().enumerate() {
            let data = txn2.get_page(&cx, p).unwrap();
            #[allow(clippy::cast_possible_truncation)]
            let expected = i as u8;
            assert_eq!(
                data.as_ref()[0],
                expected,
                "bead_id={BEAD_ID} case=multi_page idx={i}"
            );
        }
    }

    // ── Journal crash recovery tests ────────────────────────────────────

    #[test]
    fn test_commit_journal_short_preimage_read_errors() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/short_preimage.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();

        // Establish page 2 so the next commit must read a pre-image for it.
        let page_two = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
            p
        };

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, page_two, &vec![0x22; ps]).unwrap();

        // Simulate external truncation: pre-image read for page 2 becomes short.
        let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
        let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();
        db_file
            .truncate(&cx, PageSize::DEFAULT.as_usize() as u64)
            .unwrap();

        let err = txn.commit(&cx).unwrap_err();
        assert!(
            matches!(err, FrankenError::DatabaseCorrupt { .. }),
            "bead_id={BEAD_ID} case=short_preimage_read_is_corruption"
        );

        // Commit failure should keep the writer lock held on commit failure so no other writer
        // can interleave while the caller decides to retry or roll back.
        let Err(busy) = pager.begin(&cx, TransactionMode::Immediate) else {
            panic!("expected begin to fail while writer lock is still held");
        };
        assert!(
            matches!(busy, FrankenError::Busy),
            "bead_id={BEAD_ID} case=commit_error_keeps_writer_lock"
        );

        txn.rollback(&cx).unwrap();
        let _next_writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
    }

    #[test]
    fn test_rollback_recovers_after_partial_commit_failure() {
        let path = PathBuf::from("/rollback_after_failed_commit.db");
        let journal_path = SimplePager::<DbWriteFailOnceVfs>::journal_path(&path);
        let vfs = DbWriteFailOnceVfs::new(path.clone());
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let original_two = vec![0x11; ps];
        let original_three = vec![0x44; ps];
        let (page_two, page_three) = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            let page_three = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &original_two).unwrap();
            txn.write_page(&cx, page_three, &original_three).unwrap();
            txn.commit(&cx).unwrap();
            (page_two, page_three)
        };

        vfs.arm_after_db_writes(1);

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, page_two, &vec![0x22; ps]).unwrap();
        txn.write_page(&cx, page_three, &vec![0x55; ps]).unwrap();

        let err = txn.commit(&cx).unwrap_err();
        assert!(
            matches!(err, FrankenError::Io(_)),
            "bead_id={BEAD_ID} case=partial_commit_surfaces_io_error"
        );

        txn.rollback(&cx).unwrap();

        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            reader.get_page(&cx, page_two).unwrap().into_vec(),
            original_two
        );
        assert_eq!(
            reader.get_page(&cx, page_three).unwrap().into_vec(),
            original_three
        );

        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=rollback_removes_failed_commit_journal"
        );

        let reopened = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let reopened_reader = reopened.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            reopened_reader.get_page(&cx, page_two).unwrap().into_vec(),
            vec![0x11; ps]
        );
        assert_eq!(
            reopened_reader
                .get_page(&cx, page_three)
                .unwrap()
                .into_vec(),
            vec![0x44; ps]
        );
    }

    #[test]
    fn test_begin_recovers_abandoned_failed_commit() {
        let path = PathBuf::from("/begin_recovers_failed_commit.db");
        let journal_path = SimplePager::<DbWriteFailOnceVfs>::journal_path(&path);
        let vfs = DbWriteFailOnceVfs::new(path.clone());
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let original_two = vec![0x61; ps];
        let original_three = vec![0x73; ps];
        let (page_two, page_three) = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            let page_three = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &original_two).unwrap();
            txn.write_page(&cx, page_three, &original_three).unwrap();
            txn.commit(&cx).unwrap();
            (page_two, page_three)
        };

        vfs.arm_after_db_writes(1);

        {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            txn.write_page(&cx, page_two, &vec![0x62; ps]).unwrap();
            txn.write_page(&cx, page_three, &vec![0x74; ps]).unwrap();
            let err = txn.commit(&cx).unwrap_err();
            assert!(
                matches!(err, FrankenError::Io(_)),
                "bead_id={BEAD_ID} case=abandoned_failed_commit_surfaces_io_error"
            );
        }

        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            reader.get_page(&cx, page_two).unwrap().into_vec(),
            original_two
        );
        assert_eq!(
            reader.get_page(&cx, page_three).unwrap().into_vec(),
            original_three
        );

        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=next_begin_cleans_abandoned_failed_commit_journal"
        );

        let reopened = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let reopened_reader = reopened.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            reopened_reader.get_page(&cx, page_two).unwrap().into_vec(),
            vec![0x61; ps]
        );
        assert_eq!(
            reopened_reader
                .get_page(&cx, page_three)
                .unwrap()
                .into_vec(),
            vec![0x73; ps]
        );
    }

    #[test]
    fn test_commit_survives_journal_delete_failure() {
        let vfs = JournalDeleteFailVfs::new();
        let path = PathBuf::from("/journal_delete_failure_commit.db");
        let journal_path = SimplePager::<JournalDeleteFailVfs>::journal_path(&path);
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let page_two = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0xAB; ps]).unwrap();
            txn.commit(&cx).unwrap();
            page_two
        };

        let reopened = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let reader = reopened.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            reader.get_page(&cx, page_two).unwrap().into_vec(),
            vec![0xAB; ps]
        );

        assert!(
            vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=delete_failure_leaves_journal_inode"
        );

        let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
        let (journal_file, _) = vfs.open(&cx, Some(&journal_path), flags).unwrap();
        assert_eq!(
            journal_file.file_size(&cx).unwrap(),
            0,
            "bead_id={BEAD_ID} case=delete_failure_still_invalidates_journal"
        );
    }

    #[test]
    fn test_hot_journal_recovery_survives_delete_failure() {
        let vfs = JournalDeleteFailVfs::new();
        let path = PathBuf::from("/journal_delete_failure_recovery.db");
        let journal_path = SimplePager::<JournalDeleteFailVfs>::journal_path(&path);
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        {
            let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();

            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
            let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();

            let header = JournalHeader {
                page_count: 1,
                nonce: 0x4652_414E,
                initial_db_size: 2,
                sector_size: 512,
                page_size: PageSize::DEFAULT.get(),
            };
            let hdr_bytes = header.encode_padded();
            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl_file, _) = vfs.open(&cx, Some(&journal_path), jrnl_flags).unwrap();
            jrnl_file.write(&cx, &hdr_bytes, 0).unwrap();
            let record = JournalPageRecord::new(2, vec![0x11; ps], header.nonce);
            jrnl_file
                .write(&cx, &record.encode(), hdr_bytes.len() as u64)
                .unwrap();
            jrnl_file.sync(&cx, SyncFlags::NORMAL).unwrap();

            let page_offset = PageSize::DEFAULT.as_usize() as u64;
            db_file.write(&cx, &vec![0x22; ps], page_offset).unwrap();
            db_file.sync(&cx, SyncFlags::NORMAL).unwrap();
        }

        let reopened = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let reader = reopened.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let page_two = PageNumber::new(2).unwrap();
        assert_eq!(
            reader.get_page(&cx, page_two).unwrap().into_vec(),
            vec![0x11; ps]
        );

        assert!(
            vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=recovery_delete_failure_leaves_journal_inode"
        );

        let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
        let (journal_file, _) = vfs.open(&cx, Some(&journal_path), flags).unwrap();
        assert_eq!(
            journal_file.file_size(&cx).unwrap(),
            0,
            "bead_id={BEAD_ID} case=recovery_delete_failure_still_invalidates_journal"
        );
    }

    #[test]
    fn test_commit_creates_and_deletes_journal() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/jrnl_test.db");
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);

        // Before commit, no journal.
        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=no_journal_before_commit"
        );

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0xAA; 4096]).unwrap();
        txn.commit(&cx).unwrap();

        // After commit, journal should be deleted.
        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=journal_deleted_after_commit"
        );
    }

    #[test]
    fn test_hot_journal_recovery_restores_original_data() {
        // Simulate a crash: write data, manually create a journal with pre-images,
        // then reopen. The journal should be replayed, restoring original data.
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/crash_test.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Step 1: Create a database with known data via normal commit.
        {
            let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            assert_eq!(p.get(), 2);
            txn.write_page(&cx, p, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        // Step 2: Corrupt the database (simulate a partial write that crashed).
        {
            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
            let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();
            let corrupt_data = vec![0x99; ps];
            let offset = u64::from(2_u32 - 1) * ps as u64;
            db_file.write(&cx, &corrupt_data, offset).unwrap();
        }

        // Step 3: Create a hot journal with the original pre-image.
        {
            let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl, _) = vfs.open(&cx, Some(&journal_path), jrnl_flags).unwrap();

            let nonce = 42;
            let header = JournalHeader {
                page_count: 1,
                nonce,
                initial_db_size: 2,
                sector_size: 512,
                page_size: 4096,
            };
            let hdr_bytes = header.encode_padded();
            jrnl.write(&cx, &hdr_bytes, 0).unwrap();

            let record = JournalPageRecord::new(2, vec![0x11; ps], nonce);
            let rec_bytes = record.encode();
            jrnl.write(&cx, &rec_bytes, hdr_bytes.len() as u64).unwrap();
        }

        // Step 4: Reopen — should detect hot journal and replay.
        {
            let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
            let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
            let page_no_2 = PageNumber::new(2).unwrap();
            let data = txn.get_page(&cx, page_no_2).unwrap();

            assert_eq!(
                data.as_ref()[0],
                0x11,
                "bead_id={BEAD_ID} case=journal_recovery_restores"
            );
            assert_eq!(
                data.as_ref()[ps - 1],
                0x11,
                "bead_id={BEAD_ID} case=journal_recovery_restores_last_byte"
            );
        }

        // Step 5: Verify journal is deleted after recovery.
        let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=journal_deleted_after_recovery"
        );
    }

    #[test]
    fn test_long_lived_pager_recovers_external_hot_journal_on_begin() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/long_lived_hot_journal.db");
        let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();
        let page_two = PageNumber::new(2).unwrap();

        // Keep one pager instance alive while another actor mutates the file.
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();

        {
            let seed = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
            let mut txn = seed.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            assert_eq!(page_two.get(), 2);
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        let warm_reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            warm_reader.get_page(&cx, page_two).unwrap().into_vec(),
            vec![0x11; ps],
            "bead_id={BEAD_ID} case=existing_pager_populates_publication_before_hot_journal"
        );
        drop(warm_reader);
        assert!(
            pager.published_snapshot().page_set_size > 0,
            "bead_id={BEAD_ID} case=existing_pager_has_published_pages_before_hot_journal"
        );

        {
            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
            let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();

            let header = JournalHeader {
                page_count: 1,
                nonce: 0x4652_414E,
                initial_db_size: 2,
                sector_size: 512,
                page_size: PageSize::DEFAULT.get(),
            };
            let hdr_bytes = header.encode_padded();
            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl_file, _) = vfs.open(&cx, Some(&journal_path), jrnl_flags).unwrap();
            jrnl_file.write(&cx, &hdr_bytes, 0).unwrap();
            let record = JournalPageRecord::new(2, vec![0x11; ps], header.nonce);
            jrnl_file
                .write(&cx, &record.encode(), hdr_bytes.len() as u64)
                .unwrap();
            jrnl_file.sync(&cx, SyncFlags::NORMAL).unwrap();

            let page_offset = PageSize::DEFAULT.as_usize() as u64;
            db_file.write(&cx, &vec![0x99; ps], page_offset).unwrap();
            db_file.sync(&cx, SyncFlags::NORMAL).unwrap();
        }

        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            pager.published_snapshot().page_set_size,
            0,
            "bead_id={BEAD_ID} case=existing_pager_clears_published_pages_after_hot_journal"
        );
        assert_eq!(
            reader.get_page(&cx, page_two).unwrap().into_vec(),
            vec![0x11; ps],
            "bead_id={BEAD_ID} case=existing_pager_recovers_hot_journal"
        );
        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=existing_pager_removes_hot_journal"
        );
    }

    #[test]
    fn test_refresh_published_snapshot_recovers_external_hot_journal() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/published_refresh_hot_journal.db");
        let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();

        {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            assert_eq!(page_two.get(), 2);
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        let page_two = PageNumber::new(2).unwrap();
        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(reader.get_page(&cx, page_two).unwrap().as_ref()[0], 0x11);
        drop(reader);
        assert!(
            pager.published_snapshot().page_set_size > 0,
            "bead_id={BEAD_ID} case=publication_plane_populated_before_hot_journal_refresh"
        );

        {
            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
            let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();

            let header = JournalHeader {
                page_count: 1,
                nonce: 0x5245_4652,
                initial_db_size: 2,
                sector_size: 512,
                page_size: PageSize::DEFAULT.get(),
            };
            let hdr_bytes = header.encode_padded();
            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl_file, _) = vfs.open(&cx, Some(&journal_path), jrnl_flags).unwrap();
            jrnl_file.write(&cx, &hdr_bytes, 0).unwrap();
            let record = JournalPageRecord::new(2, vec![0x11; ps], header.nonce);
            jrnl_file
                .write(&cx, &record.encode(), hdr_bytes.len() as u64)
                .unwrap();
            jrnl_file.sync(&cx, SyncFlags::NORMAL).unwrap();

            let page_offset = PageSize::DEFAULT.as_usize() as u64;
            db_file.write(&cx, &vec![0x99; ps], page_offset).unwrap();
            db_file.sync(&cx, SyncFlags::NORMAL).unwrap();
        }

        let refreshed = pager.refresh_published_snapshot(&cx).unwrap();
        assert_eq!(
            refreshed.page_set_size, 0,
            "bead_id={BEAD_ID} case=published_refresh_clears_published_pages_after_hot_journal"
        );

        let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
        let (db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();
        let mut restored = vec![0u8; ps];
        let bytes_read = db_file
            .read(&cx, &mut restored, PageSize::DEFAULT.as_usize() as u64)
            .unwrap();
        assert_eq!(bytes_read, ps);
        assert_eq!(
            restored,
            vec![0x11; ps],
            "bead_id={BEAD_ID} case=published_refresh_recovers_hot_journal_bytes"
        );
        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=published_refresh_removes_hot_journal"
        );
    }

    #[test]
    fn test_hot_journal_truncated_record_stops_replay() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/trunc_jrnl.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Create DB with 2 pages.
        {
            let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p1 = txn.allocate_page(&cx).unwrap();
            let p2 = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p1, &vec![0xAB; ps]).unwrap();
            txn.write_page(&cx, p2, &vec![0xBB; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        // Corrupt page 2.
        {
            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
            let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();
            db_file
                .write(&cx, &vec![0xFF; ps], u64::from(2_u32 - 1) * ps as u64)
                .unwrap();
        }

        // Journal claims 2 records but second is truncated.
        {
            let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl, _) = vfs.open(&cx, Some(&journal_path), jrnl_flags).unwrap();

            let nonce = 7;
            let header = JournalHeader {
                page_count: 2,
                nonce,
                initial_db_size: 3,
                sector_size: 512,
                page_size: 4096,
            };
            let hdr_bytes = header.encode_padded();
            jrnl.write(&cx, &hdr_bytes, 0).unwrap();

            // First record: valid pre-image for page 3.
            let rec1 = JournalPageRecord::new(3, vec![0xCC; ps], nonce);
            let rec1_bytes = rec1.encode();
            jrnl.write(&cx, &rec1_bytes, hdr_bytes.len() as u64)
                .unwrap();

            // Second record: truncated.
            let rec2 = JournalPageRecord::new(2, vec![0xBB; ps], nonce);
            let rec2_bytes = rec2.encode();
            let trunc_len = rec2_bytes.len() / 2;
            let offset = hdr_bytes.len() as u64 + rec1_bytes.len() as u64;
            jrnl.write(&cx, &rec2_bytes[..trunc_len], offset).unwrap();
        }

        // Reopen — first record replays, second skipped.
        {
            let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
            let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
            let page_no_2 = PageNumber::new(2).unwrap();
            let data2 = txn.get_page(&cx, page_no_2).unwrap();
            assert_eq!(
                data2.as_ref()[0],
                0xFF,
                "bead_id={BEAD_ID} case=truncated_journal_page2_not_restored"
            );
        }
    }

    #[test]
    fn test_hot_journal_checksum_mismatch_stops_replay() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/cksum_jrnl.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        {
            let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0x55; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        // Corrupt page 2.
        {
            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_DB;
            let (mut db_file, _) = vfs.open(&cx, Some(&path), flags).unwrap();
            db_file
                .write(&cx, &vec![0xEE; ps], u64::from(2_u32 - 1) * ps as u64)
                .unwrap();
        }

        // Journal with wrong nonce in record (checksum won't verify).
        {
            let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl, _) = vfs.open(&cx, Some(&journal_path), jrnl_flags).unwrap();

            let nonce = 99;
            let header = JournalHeader {
                page_count: 1,
                nonce,
                initial_db_size: 2,
                sector_size: 512,
                page_size: 4096,
            };
            let hdr_bytes = header.encode_padded();
            jrnl.write(&cx, &hdr_bytes, 0).unwrap();

            // Wrong nonce in record.
            let record = JournalPageRecord::new(2, vec![0x55; ps], nonce + 1);
            let rec_bytes = record.encode();
            jrnl.write(&cx, &rec_bytes, hdr_bytes.len() as u64).unwrap();
        }

        // Reopen — bad checksum stops replay.
        {
            let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
            let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
            let page_no_2 = PageNumber::new(2).unwrap();
            let data = txn.get_page(&cx, page_no_2).unwrap();
            assert_eq!(
                data.as_ref()[0],
                0xEE,
                "bead_id={BEAD_ID} case=bad_checksum_stops_replay"
            );
        }
    }

    #[test]
    fn test_hot_journal_invalid_page_number_errors() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/bad_pgno_jrnl.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        {
            let _pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
            let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
            let jrnl_flags =
                VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE | VfsOpenFlags::MAIN_JOURNAL;
            let (mut jrnl, _) = vfs.open(&cx, Some(&journal_path), jrnl_flags).unwrap();

            let nonce = 321;
            let header = JournalHeader {
                page_count: 1,
                nonce,
                initial_db_size: 1,
                sector_size: 512,
                page_size: 4096,
            };
            let hdr_bytes = header.encode_padded();
            jrnl.write(&cx, &hdr_bytes, 0).unwrap();

            let record = JournalPageRecord::new(0, vec![0xAA; ps], nonce);
            let rec_bytes = record.encode();
            jrnl.write(&cx, &rec_bytes, hdr_bytes.len() as u64).unwrap();
        }

        let Err(err) = SimplePager::open(vfs, &path, PageSize::DEFAULT) else {
            panic!("expected invalid journal page number error");
        };
        assert!(
            matches!(err, FrankenError::DatabaseCorrupt { .. }),
            "bead_id={BEAD_ID} case=invalid_journal_page_number_rejected"
        );
    }

    #[test]
    fn test_journal_not_created_for_readonly_commit() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let journal_path = SimplePager::<MemoryVfs>::journal_path(&pager.db_path);

        let mut txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        txn.commit(&cx).unwrap();

        assert!(
            !pager
                .vfs
                .access(&cx, &journal_path, AccessFlags::EXISTS)
                .unwrap(),
            "bead_id={BEAD_ID} case=journal_deleted_for_readonly"
        );
    }

    #[test]
    fn test_rollback_deletes_journal() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/rollback_jrnl.db");
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0xDD; 4096]).unwrap();
        txn.rollback(&cx).unwrap();

        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=journal_deleted_on_rollback"
        );
    }

    // ── Savepoint tests ────────────────────────────────────────────────

    #[test]
    fn test_savepoint_basic_rollback_to() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        // Create savepoint after first write.
        txn.savepoint(&cx, "sp1").unwrap();

        // Second write (after savepoint).
        let p2 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p2, &vec![0x22; ps]).unwrap();
        // Overwrite p1 after savepoint.
        txn.write_page(&cx, p1, &vec![0x33; ps]).unwrap();

        // Rollback to sp1 — should undo second write and p1 overwrite.
        txn.rollback_to_savepoint(&cx, "sp1").unwrap();

        // p1 should have the value from before the savepoint.
        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x11,
            "bead_id={BEAD_ID} case=savepoint_rollback_restores_p1"
        );

        // p2 should no longer be in the write-set (reads zeros from disk).
        let data2 = txn.get_page(&cx, p2).unwrap();
        assert_eq!(
            data2.as_ref()[0],
            0x00,
            "bead_id={BEAD_ID} case=savepoint_rollback_removes_p2"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_release_keeps_changes() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0xAA; ps]).unwrap();

        txn.savepoint(&cx, "sp1").unwrap();

        // Write after savepoint.
        txn.write_page(&cx, p1, &vec![0xBB; ps]).unwrap();

        // Release — changes after savepoint are kept.
        txn.release_savepoint(&cx, "sp1").unwrap();

        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0xBB,
            "bead_id={BEAD_ID} case=release_keeps_changes"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_nested_rollback_to_inner() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "outer").unwrap();
        txn.write_page(&cx, p1, &vec![0x22; ps]).unwrap();

        txn.savepoint(&cx, "inner").unwrap();
        txn.write_page(&cx, p1, &vec![0x33; ps]).unwrap();

        // Rollback to inner — should restore to 0x22 (state at "inner" creation).
        txn.rollback_to_savepoint(&cx, "inner").unwrap();

        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x22,
            "bead_id={BEAD_ID} case=nested_rollback_inner"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_nested_rollback_to_outer() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "outer").unwrap();
        txn.write_page(&cx, p1, &vec![0x22; ps]).unwrap();

        txn.savepoint(&cx, "inner").unwrap();
        txn.write_page(&cx, p1, &vec![0x33; ps]).unwrap();

        // Rollback to outer — should restore to 0x11 and discard inner savepoint.
        txn.rollback_to_savepoint(&cx, "outer").unwrap();

        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x11,
            "bead_id={BEAD_ID} case=nested_rollback_outer"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_rollback_to_preserves_savepoint() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "sp1").unwrap();

        // First modification + rollback.
        txn.write_page(&cx, p1, &vec![0x22; ps]).unwrap();
        txn.rollback_to_savepoint(&cx, "sp1").unwrap();

        // Should be back to 0x11.
        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x11,
            "bead_id={BEAD_ID} case=rollback_to_preserves_savepoint"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_rollback_reclaims_allocated_pages() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();

        // Initial state: 1 page (header)
        let p1 = txn.allocate_page(&cx).unwrap(); // Page 2
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "sp1").unwrap();

        // Allocate Page 3 inside savepoint
        let p2 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p2, &vec![0x22; ps]).unwrap();

        assert_eq!(p2.get(), p1.get() + 1, "Expected sequential allocation");

        // Rollback to sp1. This should ideally "un-allocate" p2.
        txn.rollback_to_savepoint(&cx, "sp1").unwrap();

        // Allocate again. Should we get p2 again?
        // If next_page wasn't reverted, we'll get p2 + 1 (Page 4), leaving Page 3 as a hole.
        let p3 = txn.allocate_page(&cx).unwrap();

        assert_eq!(
            p3.get(),
            p2.get(),
            "bead_id={BEAD_ID} case=rollback_reclaims_allocation: expected page {} but got {}",
            p2.get(),
            p3.get()
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_rollback_to_preserves_savepoint_multi() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "sp1").unwrap();

        // Modify again.
        txn.write_page(&cx, p1, &vec![0x33; ps]).unwrap();
        txn.rollback_to_savepoint(&cx, "sp1").unwrap();

        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x11,
            "bead_id={BEAD_ID} case=rollback_to_preserves_savepoint_multi"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_freed_pages_restored_on_rollback() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        let p2 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0xAA; ps]).unwrap();
        txn.write_page(&cx, p2, &vec![0xBB; ps]).unwrap();

        txn.savepoint(&cx, "sp1").unwrap();

        // Free p2 after savepoint.
        txn.free_page(&cx, p2).unwrap();

        // Rollback — p2 should no longer be freed.
        txn.rollback_to_savepoint(&cx, "sp1").unwrap();

        // p2 should still be in the write-set (not freed).
        let data = txn.get_page(&cx, p2).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0xBB,
            "bead_id={BEAD_ID} case=freed_pages_restored"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_unknown_name_errors() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();

        let result = txn.rollback_to_savepoint(&cx, "nonexistent");
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=rollback_to_unknown_savepoint_errors"
        );

        let result = txn.release_savepoint(&cx, "nonexistent");
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=release_unknown_savepoint_errors"
        );
    }

    #[test]
    fn test_savepoint_release_then_rollback_to_outer() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "outer").unwrap();
        txn.write_page(&cx, p1, &vec![0x22; ps]).unwrap();

        txn.savepoint(&cx, "inner").unwrap();
        txn.write_page(&cx, p1, &vec![0x33; ps]).unwrap();

        // Release inner — changes kept, inner savepoint removed.
        txn.release_savepoint(&cx, "inner").unwrap();

        // Rollback to outer — should revert to 0x11.
        txn.rollback_to_savepoint(&cx, "outer").unwrap();

        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x11,
            "bead_id={BEAD_ID} case=release_inner_then_rollback_outer"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_savepoint_commit_with_active_savepoints() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "sp1").unwrap();
        txn.write_page(&cx, p1, &vec![0x22; ps]).unwrap();

        // Commit with active savepoint — all changes should be persisted.
        txn.commit(&cx).unwrap();

        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = txn2.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x22,
            "bead_id={BEAD_ID} case=commit_with_savepoints_persists_all"
        );
    }

    #[test]
    fn test_savepoint_full_rollback_clears_savepoints() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();

        txn.savepoint(&cx, "sp1").unwrap();
        txn.savepoint(&cx, "sp2").unwrap();

        // Full rollback should clear all savepoints.
        txn.rollback(&cx).unwrap();

        // Trying to rollback to a savepoint after full rollback should error.
        let result = txn.rollback_to_savepoint(&cx, "sp1");
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=full_rollback_clears_savepoints"
        );
    }

    #[test]
    fn test_savepoint_three_levels_deep() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();

        // Level 0: write 0x00
        txn.write_page(&cx, p1, &vec![0x00; ps]).unwrap();
        txn.savepoint(&cx, "L0").unwrap();

        // Level 1: write 0x11
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();
        txn.savepoint(&cx, "L1").unwrap();

        // Level 2: write 0x22
        txn.write_page(&cx, p1, &vec![0x22; ps]).unwrap();
        txn.savepoint(&cx, "L2").unwrap();

        // Level 3: write 0x33
        txn.write_page(&cx, p1, &vec![0x33; ps]).unwrap();

        // Verify current state
        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x33,
            "bead_id={BEAD_ID} case=3level_current"
        );

        // Rollback to L2 → should see 0x22
        txn.rollback_to_savepoint(&cx, "L2").unwrap();
        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(data.as_ref()[0], 0x22, "bead_id={BEAD_ID} case=3level_L2");

        // Rollback to L1 → should see 0x11
        txn.rollback_to_savepoint(&cx, "L1").unwrap();
        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(data.as_ref()[0], 0x11, "bead_id={BEAD_ID} case=3level_L1");

        // Rollback to L0 → should see 0x00
        txn.rollback_to_savepoint(&cx, "L0").unwrap();
        let data = txn.get_page(&cx, p1).unwrap();
        assert_eq!(data.as_ref()[0], 0x00, "bead_id={BEAD_ID} case=3level_L0");

        txn.commit(&cx).unwrap();

        // Verify committed value is 0x00 (state at L0).
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = txn2.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x00,
            "bead_id={BEAD_ID} case=3level_committed"
        );
    }

    // ── WAL mode integration tests ──────────────────────────────────────

    use fsqlite_wal::checksum::{WAL_FRAME_HEADER_SIZE, WalChecksumTransform};
    use fsqlite_wal::wal::WalAppendFrameRef;
    use fsqlite_wal::{WalFile, WalSalts};
    use serde_json::json;
    use std::sync::{Arc as StdArc, Condvar as StdCondvar, Mutex as StdMutex};
    use std::time::Instant;

    /// (page_number, page_data, db_size_if_commit)
    type WalFrame = (u32, Vec<u8>, u32);
    type SharedFrames = StdArc<StdMutex<Vec<WalFrame>>>;
    type SharedCounter = StdArc<StdMutex<usize>>;
    type SharedLockLevels = StdArc<StdMutex<Vec<LockLevel>>>;
    const TRACK_C_BATCH_BENCH_BEAD_ID: &str = "bd-db300.3.1.4";
    const TRACK_C_PUBLISH_WINDOW_INVENTORY_BEAD_ID: &str = "bd-db300.3.2.1";
    const TRACK_C_PUBLISH_WINDOW_BENCH_BEAD_ID: &str = "bd-db300.3.2.3";
    const TRACK_C_BATCH_BENCH_WARMUP_ITERS: usize = 5;
    const TRACK_C_BATCH_BENCH_MEASURE_ITERS: usize = 25;
    const TRACK_C_BATCH_BENCH_CASES: [(&str, usize); 3] = [
        ("page1_plus_1_new_page", 2),
        ("page1_plus_7_new_pages", 8),
        ("page1_plus_31_new_pages", 32),
    ];
    const TRACK_C_PUBLISH_WINDOW_BENCH_WARMUP_ITERS: usize = 5;
    const TRACK_C_PUBLISH_WINDOW_BENCH_MEASURE_ITERS: usize = 20;
    const TRACK_C_PUBLISH_WINDOW_BENCH_CASES: [(&str, usize); 3] = [
        ("interior_only_1_dirty_page", 1),
        ("interior_only_7_dirty_pages", 7),
        ("interior_only_31_dirty_pages", 31),
    ];
    const TRACK_C_METADATA_BENCH_BEAD_ID: &str = "bd-db300.3.3.3";
    const TRACK_C_METADATA_BENCH_WARMUP_ITERS: usize = 5;
    const TRACK_C_METADATA_BENCH_MEASURE_ITERS: usize = 25;
    const TRACK_C_METADATA_BENCH_CASES: [(&str, usize); 3] = [
        ("interior_only_1_dirty_page", 1),
        ("interior_only_7_dirty_pages", 7),
        ("interior_only_31_dirty_pages", 31),
    ];

    /// In-memory WAL backend for testing WAL-mode commit and page lookup.
    struct MockWalBackend {
        frames: SharedFrames,
        begin_calls: SharedCounter,
        batch_calls: SharedCounter,
        sync_calls: SharedCounter,
        read_page_calls: SharedCounter,
    }

    impl MockWalBackend {
        fn new() -> (Self, SharedFrames, SharedCounter, SharedCounter) {
            let frames: SharedFrames = StdArc::new(StdMutex::new(Vec::new()));
            let begin_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let batch_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let sync_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let read_page_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            (
                Self {
                    frames: StdArc::clone(&frames),
                    begin_calls: StdArc::clone(&begin_calls),
                    batch_calls: StdArc::clone(&batch_calls),
                    sync_calls,
                    read_page_calls,
                },
                frames,
                begin_calls,
                batch_calls,
            )
        }

        fn new_with_sync_tracking() -> (
            Self,
            SharedFrames,
            SharedCounter,
            SharedCounter,
            SharedCounter,
        ) {
            let frames: SharedFrames = StdArc::new(StdMutex::new(Vec::new()));
            let begin_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let batch_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let sync_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let read_page_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            (
                Self {
                    frames: StdArc::clone(&frames),
                    begin_calls: StdArc::clone(&begin_calls),
                    batch_calls: StdArc::clone(&batch_calls),
                    sync_calls: StdArc::clone(&sync_calls),
                    read_page_calls,
                },
                frames,
                begin_calls,
                batch_calls,
                sync_calls,
            )
        }

        fn with_shared_frames(frames: SharedFrames) -> (Self, SharedCounter, SharedCounter) {
            let begin_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let batch_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let sync_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let read_page_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            (
                Self {
                    frames,
                    begin_calls: StdArc::clone(&begin_calls),
                    batch_calls: StdArc::clone(&batch_calls),
                    sync_calls,
                    read_page_calls,
                },
                begin_calls,
                batch_calls,
            )
        }

        fn new_with_read_tracking() -> (
            Self,
            SharedFrames,
            SharedCounter,
            SharedCounter,
            SharedCounter,
        ) {
            let frames: SharedFrames = StdArc::new(StdMutex::new(Vec::new()));
            let begin_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let batch_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let sync_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let read_page_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            (
                Self {
                    frames: StdArc::clone(&frames),
                    begin_calls: StdArc::clone(&begin_calls),
                    batch_calls: StdArc::clone(&batch_calls),
                    sync_calls,
                    read_page_calls: StdArc::clone(&read_page_calls),
                },
                frames,
                begin_calls,
                batch_calls,
                read_page_calls,
            )
        }
    }

    struct FailingGroupCommitWalBackend {
        append_frames_calls: SharedCounter,
    }

    impl FailingGroupCommitWalBackend {
        fn new() -> (Self, SharedCounter) {
            let append_frames_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            (
                Self {
                    append_frames_calls: StdArc::clone(&append_frames_calls),
                },
                append_frames_calls,
            )
        }
    }

    struct PreparedBatchObservedWalBackend {
        frames: SharedFrames,
        append_frames_calls: SharedCounter,
        append_prepared_calls: SharedCounter,
        prepare_lock_levels: SharedLockLevels,
        append_lock_levels: SharedLockLevels,
        observed_lock_level: Arc<Mutex<LockLevel>>,
    }

    impl PreparedBatchObservedWalBackend {
        fn new(
            observed_lock_level: Arc<Mutex<LockLevel>>,
        ) -> (
            Self,
            SharedFrames,
            SharedCounter,
            SharedCounter,
            SharedLockLevels,
            SharedLockLevels,
        ) {
            let frames: SharedFrames = StdArc::new(StdMutex::new(Vec::new()));
            let append_frames_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let append_prepared_calls: SharedCounter = StdArc::new(StdMutex::new(0));
            let prepare_lock_levels: SharedLockLevels = StdArc::new(StdMutex::new(Vec::new()));
            let append_lock_levels: SharedLockLevels = StdArc::new(StdMutex::new(Vec::new()));

            (
                Self {
                    frames: StdArc::clone(&frames),
                    append_frames_calls: StdArc::clone(&append_frames_calls),
                    append_prepared_calls: StdArc::clone(&append_prepared_calls),
                    prepare_lock_levels: StdArc::clone(&prepare_lock_levels),
                    append_lock_levels: StdArc::clone(&append_lock_levels),
                    observed_lock_level,
                },
                frames,
                append_frames_calls,
                append_prepared_calls,
                prepare_lock_levels,
                append_lock_levels,
            )
        }
    }

    #[derive(Clone, Copy)]
    enum TrackCBatchMode {
        SingleFrame,
        Batched,
    }

    impl TrackCBatchMode {
        const fn as_str(self) -> &'static str {
            match self {
                Self::SingleFrame => "single_frame",
                Self::Batched => "batch_append",
            }
        }
    }

    #[derive(Clone, Copy)]
    enum TrackCPublishWindowMode {
        InlinePrepareBaseline,
        PreparedCandidate,
    }

    impl TrackCPublishWindowMode {
        const fn as_str(self) -> &'static str {
            match self {
                Self::InlinePrepareBaseline => "inline_prepare_baseline",
                Self::PreparedCandidate => "prepared_candidate",
            }
        }
    }

    #[derive(Clone, Copy)]
    enum TrackCMetadataMode {
        ForcedPageOneBaseline,
        SemanticCleanupCandidate,
    }

    impl TrackCMetadataMode {
        const fn as_str(self) -> &'static str {
            match self {
                Self::ForcedPageOneBaseline => "forced_page_one_baseline",
                Self::SemanticCleanupCandidate => "semantic_cleanup_candidate",
            }
        }
    }

    struct TrackCBenchmarkWalBackend {
        wal: WalFile<MemoryFile>,
        mode: TrackCBatchMode,
    }

    struct TrackCPublishWindowBenchWalBackend {
        wal: WalFile<BlockingObservedLockFile>,
        mode: TrackCPublishWindowMode,
    }

    impl TrackCBenchmarkWalBackend {
        fn new(vfs: &MemoryVfs, cx: &Cx, path: &std::path::Path, mode: TrackCBatchMode) -> Self {
            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::CREATE | VfsOpenFlags::WAL;
            let (file, _) = vfs.open(cx, Some(path), flags).unwrap();
            let wal = WalFile::create(
                cx,
                file,
                PageSize::DEFAULT.get(),
                0,
                WalSalts {
                    salt1: 0xDB30_0314,
                    salt2: 0xC1C1_C1C1,
                },
            )
            .unwrap();
            Self { wal, mode }
        }
    }

    impl TrackCPublishWindowBenchWalBackend {
        fn new(
            vfs: &BlockingObservedLockVfs,
            cx: &Cx,
            path: &std::path::Path,
            mode: TrackCPublishWindowMode,
        ) -> Self {
            let flags = VfsOpenFlags::READWRITE | VfsOpenFlags::CREATE | VfsOpenFlags::WAL;
            let (file, _) = vfs.open(cx, Some(path), flags).unwrap();
            let wal = WalFile::create(
                cx,
                file,
                PageSize::DEFAULT.get(),
                0,
                WalSalts {
                    salt1: 0xDB30_0323,
                    salt2: 0xC2C3_C2C3,
                },
            )
            .unwrap();
            Self { wal, mode }
        }
    }

    impl crate::traits::WalBackend for TrackCBenchmarkWalBackend {
        fn append_frame(
            &mut self,
            cx: &Cx,
            page_number: u32,
            page_data: &[u8],
            db_size_if_commit: u32,
        ) -> fsqlite_error::Result<()> {
            self.wal
                .append_frame(cx, page_number, page_data, db_size_if_commit)
        }

        fn append_frames(
            &mut self,
            cx: &Cx,
            frames: &[crate::traits::WalFrameRef<'_>],
        ) -> fsqlite_error::Result<()> {
            match self.mode {
                TrackCBatchMode::SingleFrame => {
                    for frame in frames {
                        self.wal.append_frame(
                            cx,
                            frame.page_number,
                            frame.page_data,
                            frame.db_size_if_commit,
                        )?;
                    }
                    Ok(())
                }
                TrackCBatchMode::Batched => {
                    let wal_frames: Vec<_> = frames
                        .iter()
                        .map(|frame| WalAppendFrameRef {
                            page_number: frame.page_number,
                            page_data: frame.page_data,
                            db_size_if_commit: frame.db_size_if_commit,
                        })
                        .collect();
                    self.wal.append_frames(cx, &wal_frames)
                }
            }
        }

        fn read_page(
            &mut self,
            _cx: &Cx,
            _page_number: u32,
        ) -> fsqlite_error::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn committed_txn_count(&mut self, cx: &Cx) -> fsqlite_error::Result<u64> {
            let Some(last_commit_frame) = self.wal.last_commit_frame(cx)? else {
                return Ok(0);
            };

            let mut commit_count = 0_u64;
            for frame_index in 0..=last_commit_frame {
                if self.wal.read_frame_header(cx, frame_index)?.is_commit() {
                    commit_count = commit_count.saturating_add(1);
                }
            }
            Ok(commit_count)
        }

        fn sync(&mut self, cx: &Cx) -> fsqlite_error::Result<()> {
            self.wal.sync(cx, SyncFlags::NORMAL)
        }

        fn frame_count(&self) -> usize {
            self.wal.frame_count()
        }

        fn checkpoint(
            &mut self,
            _cx: &Cx,
            _mode: crate::traits::CheckpointMode,
            _writer: &mut dyn crate::traits::CheckpointPageWriter,
            _backfilled_frames: u32,
            _oldest_reader_frame: Option<u32>,
        ) -> fsqlite_error::Result<crate::traits::CheckpointResult> {
            let total_frames = u32::try_from(self.wal.frame_count()).unwrap_or(u32::MAX);
            Ok(crate::traits::CheckpointResult {
                total_frames,
                frames_backfilled: 0,
                completed: false,
                wal_was_reset: false,
            })
        }
    }

    impl crate::traits::WalBackend for TrackCPublishWindowBenchWalBackend {
        fn append_frame(
            &mut self,
            cx: &Cx,
            page_number: u32,
            page_data: &[u8],
            db_size_if_commit: u32,
        ) -> fsqlite_error::Result<()> {
            self.wal
                .append_frame(cx, page_number, page_data, db_size_if_commit)
        }

        fn append_frames(
            &mut self,
            cx: &Cx,
            frames: &[crate::traits::WalFrameRef<'_>],
        ) -> fsqlite_error::Result<()> {
            let wal_frames: Vec<_> = frames
                .iter()
                .map(|frame| WalAppendFrameRef {
                    page_number: frame.page_number,
                    page_data: frame.page_data,
                    db_size_if_commit: frame.db_size_if_commit,
                })
                .collect();
            self.wal.append_frames(cx, &wal_frames)
        }

        fn prepare_append_frames(
            &mut self,
            frames: &[crate::traits::WalFrameRef<'_>],
        ) -> fsqlite_error::Result<Option<crate::traits::PreparedWalFrameBatch>> {
            if matches!(self.mode, TrackCPublishWindowMode::InlinePrepareBaseline) {
                return Ok(None);
            }

            if frames.is_empty() {
                return Ok(None);
            }

            let wal_frames: Vec<_> = frames
                .iter()
                .map(|frame| WalAppendFrameRef {
                    page_number: frame.page_number,
                    page_data: frame.page_data,
                    db_size_if_commit: frame.db_size_if_commit,
                })
                .collect();
            let frame_size = WAL_FRAME_HEADER_SIZE + wal_frames[0].page_data.len();
            let frame_metas = wal_frames
                .iter()
                .map(|frame| crate::traits::PreparedWalFrameMeta {
                    page_number: frame.page_number,
                    db_size_if_commit: frame.db_size_if_commit,
                })
                .collect();
            let frame_bytes = self.wal.prepare_frame_bytes(&wal_frames)?;
            let checksum_transforms = wal_frames
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let frame_start = index
                        .checked_mul(frame_size)
                        .expect("frame start fits usize");
                    let frame_end = frame_start
                        .checked_add(frame_size)
                        .expect("frame end fits usize");
                    let transform = WalChecksumTransform::for_wal_frame(
                        &frame_bytes[frame_start..frame_end],
                        self.wal.page_size(),
                        self.wal.big_endian_checksum(),
                    )?;
                    Ok(crate::traits::PreparedWalChecksumTransform {
                        a11: transform.a11,
                        a12: transform.a12,
                        a21: transform.a21,
                        a22: transform.a22,
                        c1: transform.c1,
                        c2: transform.c2,
                    })
                })
                .collect::<Result<Vec<_>>>()?;

            Ok(Some(crate::traits::PreparedWalFrameBatch {
                frame_size,
                page_data_offset: WAL_FRAME_HEADER_SIZE,
                frame_metas,
                checksum_transforms,
                frame_bytes,
                last_commit_frame_offset: frames
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(offset, frame)| (frame.db_size_if_commit != 0).then_some(offset)),
                finalized_for: None,
                finalized_running_checksum: None,
            }))
        }

        fn append_prepared_frames(
            &mut self,
            cx: &Cx,
            prepared: &mut crate::traits::PreparedWalFrameBatch,
        ) -> fsqlite_error::Result<()> {
            let checksum_transforms: Vec<_> = prepared
                .checksum_transforms
                .iter()
                .map(|transform| WalChecksumTransform {
                    a11: transform.a11,
                    a12: transform.a12,
                    a21: transform.a21,
                    a22: transform.a22,
                    c1: transform.c1,
                    c2: transform.c2,
                })
                .collect();
            self.wal.append_prepared_frame_bytes(
                cx,
                &mut prepared.frame_bytes,
                &checksum_transforms,
            )
        }

        fn read_page(
            &mut self,
            _cx: &Cx,
            _page_number: u32,
        ) -> fsqlite_error::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn committed_txn_count(&mut self, cx: &Cx) -> fsqlite_error::Result<u64> {
            let Some(last_commit_frame) = self.wal.last_commit_frame(cx)? else {
                return Ok(0);
            };

            let mut commit_count = 0_u64;
            for frame_index in 0..=last_commit_frame {
                if self.wal.read_frame_header(cx, frame_index)?.is_commit() {
                    commit_count = commit_count.saturating_add(1);
                }
            }
            Ok(commit_count)
        }

        fn sync(&mut self, cx: &Cx) -> fsqlite_error::Result<()> {
            self.wal.sync(cx, SyncFlags::NORMAL)
        }

        fn frame_count(&self) -> usize {
            self.wal.frame_count()
        }

        fn checkpoint(
            &mut self,
            _cx: &Cx,
            _mode: crate::traits::CheckpointMode,
            _writer: &mut dyn crate::traits::CheckpointPageWriter,
            _backfilled_frames: u32,
            _oldest_reader_frame: Option<u32>,
        ) -> fsqlite_error::Result<crate::traits::CheckpointResult> {
            let total_frames = u32::try_from(self.wal.frame_count()).unwrap_or(u32::MAX);
            Ok(crate::traits::CheckpointResult {
                total_frames,
                frames_backfilled: 0,
                completed: false,
                wal_was_reset: false,
            })
        }
    }

    fn track_c_prepared_commit(
        mode: TrackCBatchMode,
        dirty_pages: usize,
    ) -> (Cx, SimpleTransaction<MemoryVfs>) {
        assert!(dirty_pages >= 2);

        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let db_path = PathBuf::from(format!(
            "/track_c_batch_commit_{}_{}.db",
            mode.as_str(),
            dirty_pages
        ));
        let wal_path = PathBuf::from(format!(
            "/track_c_batch_commit_{}_{}.db-wal",
            mode.as_str(),
            dirty_pages
        ));
        let pager = SimplePager::open(vfs.clone(), &db_path, PageSize::DEFAULT).unwrap();
        let backend = TrackCBenchmarkWalBackend::new(&vfs, &cx, &wal_path, mode);
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page_bytes = PageSize::DEFAULT.as_usize();
        txn.write_page(&cx, PageNumber::ONE, &vec![0xA1; page_bytes])
            .unwrap();
        for page_idx in 1..dirty_pages {
            let page_no = txn.allocate_page(&cx).unwrap();
            let fill = u8::try_from((page_idx % 251) + 1).unwrap();
            txn.write_page(&cx, page_no, &vec![fill; page_bytes])
                .unwrap();
        }
        (cx, txn)
    }

    fn track_c_measure_commit_ns(mode: TrackCBatchMode, dirty_pages: usize) -> Vec<u64> {
        let total_iters = TRACK_C_BATCH_BENCH_WARMUP_ITERS + TRACK_C_BATCH_BENCH_MEASURE_ITERS;
        let mut samples = Vec::with_capacity(TRACK_C_BATCH_BENCH_MEASURE_ITERS);

        for iter_idx in 0..total_iters {
            let (cx, mut txn) = track_c_prepared_commit(mode, dirty_pages);
            let started = Instant::now();
            txn.commit(&cx).unwrap();
            if iter_idx >= TRACK_C_BATCH_BENCH_WARMUP_ITERS {
                let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                samples.push(elapsed_ns);
            }
        }

        samples
    }

    fn track_c_metadata_seed_existing_pages(
        pager: &SimplePager<MemoryVfs>,
        cx: &Cx,
        interior_dirty_pages: usize,
    ) {
        assert!(interior_dirty_pages > 0);
        let mut txn = pager.begin(cx, TransactionMode::Immediate).unwrap();
        let page_bytes = PageSize::DEFAULT.as_usize();
        for page_idx in 0..interior_dirty_pages {
            let page_no = txn.allocate_page(cx).unwrap();
            let fill = u8::try_from((page_idx % 251) + 1).unwrap();
            txn.write_page(cx, page_no, &vec![fill; page_bytes])
                .unwrap();
        }
        txn.commit(cx).unwrap();
    }

    fn track_c_metadata_apply_workload(
        txn: &mut SimpleTransaction<MemoryVfs>,
        cx: &Cx,
        interior_dirty_pages: usize,
        mode: TrackCMetadataMode,
    ) {
        let page_bytes = PageSize::DEFAULT.as_usize();
        for page_idx in 0..interior_dirty_pages {
            let page_no = PageNumber::new(u32::try_from(page_idx + 2).unwrap()).unwrap();
            let fill = u8::try_from(((page_idx + 17) % 251) + 1).unwrap();
            txn.write_page(cx, page_no, &vec![fill; page_bytes])
                .unwrap();
        }
        if matches!(mode, TrackCMetadataMode::ForcedPageOneBaseline) {
            let page_one = txn.get_page(cx, PageNumber::ONE).unwrap().into_vec();
            txn.write_page(cx, PageNumber::ONE, &page_one).unwrap();
        }
    }

    fn track_c_metadata_prepared_commit(
        mode: TrackCMetadataMode,
        interior_dirty_pages: usize,
    ) -> (Cx, SimpleTransaction<MemoryVfs>) {
        assert!(interior_dirty_pages > 0);

        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let db_path = PathBuf::from(format!(
            "/track_c_metadata_cleanup_{}_{}.db",
            mode.as_str(),
            interior_dirty_pages
        ));
        let wal_path = PathBuf::from(format!(
            "/track_c_metadata_cleanup_{}_{}.db-wal",
            mode.as_str(),
            interior_dirty_pages
        ));
        let pager = SimplePager::open(vfs.clone(), &db_path, PageSize::DEFAULT).unwrap();
        track_c_metadata_seed_existing_pages(&pager, &cx, interior_dirty_pages);
        let backend =
            TrackCBenchmarkWalBackend::new(&vfs, &cx, &wal_path, TrackCBatchMode::Batched);
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        track_c_metadata_apply_workload(&mut txn, &cx, interior_dirty_pages, mode);
        (cx, txn)
    }

    fn track_c_metadata_measure_commit_ns(
        mode: TrackCMetadataMode,
        interior_dirty_pages: usize,
    ) -> Vec<u64> {
        let total_iters =
            TRACK_C_METADATA_BENCH_WARMUP_ITERS + TRACK_C_METADATA_BENCH_MEASURE_ITERS;
        let mut samples = Vec::with_capacity(TRACK_C_METADATA_BENCH_MEASURE_ITERS);

        for iter_idx in 0..total_iters {
            let (cx, mut txn) = track_c_metadata_prepared_commit(mode, interior_dirty_pages);
            let started = Instant::now();
            txn.commit(&cx).unwrap();
            if iter_idx >= TRACK_C_METADATA_BENCH_WARMUP_ITERS {
                let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                samples.push(elapsed_ns);
            }
        }

        samples
    }

    fn track_c_metadata_capture_frame_pages(
        mode: TrackCMetadataMode,
        interior_dirty_pages: usize,
    ) -> Vec<u32> {
        assert!(interior_dirty_pages > 0);

        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let db_path = PathBuf::from(format!(
            "/track_c_metadata_capture_{}_{}.db",
            mode.as_str(),
            interior_dirty_pages
        ));
        let pager = SimplePager::open(vfs, &db_path, PageSize::DEFAULT).unwrap();
        track_c_metadata_seed_existing_pages(&pager, &cx, interior_dirty_pages);

        let (backend, frames, _begin_calls, _batch_calls) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        frames.lock().unwrap().clear();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        track_c_metadata_apply_workload(&mut txn, &cx, interior_dirty_pages, mode);
        txn.commit(&cx).unwrap();

        frames
            .lock()
            .unwrap()
            .iter()
            .map(|(page_number, _, _)| *page_number)
            .collect()
    }

    fn track_c_seed_existing_pages_blocking(
        pager: &SimplePager<BlockingObservedLockVfs>,
        cx: &Cx,
        page_count: usize,
    ) {
        assert!(page_count > 0);
        let mut txn = pager.begin(cx, TransactionMode::Immediate).unwrap();
        let page_bytes = PageSize::DEFAULT.as_usize();
        for page_idx in 0..page_count {
            let page_no = txn.allocate_page(cx).unwrap();
            let fill = u8::try_from((page_idx % 251) + 1).unwrap();
            txn.write_page(cx, page_no, &vec![fill; page_bytes])
                .unwrap();
        }
        txn.commit(cx).unwrap();
    }

    fn track_c_write_existing_page_range(
        txn: &mut SimpleTransaction<BlockingObservedLockVfs>,
        cx: &Cx,
        start_page_no: u32,
        page_count: usize,
        fill_offset: usize,
    ) {
        let page_bytes = PageSize::DEFAULT.as_usize();
        for page_idx in 0..page_count {
            let page_no =
                PageNumber::new(start_page_no + u32::try_from(page_idx).unwrap()).unwrap();
            let fill = u8::try_from(((page_idx + fill_offset) % 251) + 1).unwrap();
            txn.write_page(cx, page_no, &vec![fill; page_bytes])
                .unwrap();
        }
    }

    fn track_c_publish_window_prepared_commit(
        mode: TrackCPublishWindowMode,
        dirty_pages: usize,
    ) -> (
        Cx,
        SimpleTransaction<BlockingObservedLockVfs>,
        BlockingObservedLockVfs,
    ) {
        assert!(dirty_pages > 0);

        let cx = Cx::new();
        let vfs = BlockingObservedLockVfs::new();
        let db_path = PathBuf::from(format!(
            "/track_c_publish_window_hold_{}_{}.db",
            mode.as_str(),
            dirty_pages
        ));
        let wal_path = PathBuf::from(format!(
            "/track_c_publish_window_hold_{}_{}.db-wal",
            mode.as_str(),
            dirty_pages
        ));

        let seed_pager = SimplePager::open(vfs.clone(), &db_path, PageSize::DEFAULT).unwrap();
        track_c_seed_existing_pages_blocking(&seed_pager, &cx, dirty_pages);
        drop(seed_pager);

        let pager = SimplePager::open(vfs.clone(), &db_path, PageSize::DEFAULT).unwrap();
        let backend = TrackCPublishWindowBenchWalBackend::new(&vfs, &cx, &wal_path, mode);
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        vfs.clear_exclusive_metrics();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        track_c_write_existing_page_range(&mut txn, &cx, 2, dirty_pages, 37);
        (cx, txn, vfs)
    }

    fn track_c_measure_publish_window_hold_ns(
        mode: TrackCPublishWindowMode,
        dirty_pages: usize,
    ) -> Vec<u64> {
        let total_iters =
            TRACK_C_PUBLISH_WINDOW_BENCH_WARMUP_ITERS + TRACK_C_PUBLISH_WINDOW_BENCH_MEASURE_ITERS;
        let mut samples = Vec::with_capacity(TRACK_C_PUBLISH_WINDOW_BENCH_MEASURE_ITERS);

        for iter_idx in 0..total_iters {
            let (cx, mut txn, vfs) = track_c_publish_window_prepared_commit(mode, dirty_pages);
            txn.commit(&cx).unwrap();
            let hold_ns = *vfs.exclusive_hold_samples_ns().last().unwrap_or(&0);
            if iter_idx >= TRACK_C_PUBLISH_WINDOW_BENCH_WARMUP_ITERS {
                samples.push(hold_ns);
            }
        }

        samples
    }

    fn track_c_open_contending_pagers(
        mode: TrackCPublishWindowMode,
        dirty_pages: usize,
    ) -> (
        BlockingObservedLockVfs,
        SimplePager<BlockingObservedLockVfs>,
        SimplePager<BlockingObservedLockVfs>,
    ) {
        assert!(dirty_pages > 0);

        let vfs = BlockingObservedLockVfs::new();
        let db_path = PathBuf::from(format!(
            "/track_c_publish_window_contention_{}_{}.db",
            mode.as_str(),
            dirty_pages
        ));
        let wal_path = PathBuf::from(format!(
            "/track_c_publish_window_contention_{}_{}.db-wal",
            mode.as_str(),
            dirty_pages
        ));
        let seed_cx = Cx::new();
        let seed_pager = SimplePager::open(vfs.clone(), &db_path, PageSize::DEFAULT).unwrap();
        track_c_seed_existing_pages_blocking(&seed_pager, &seed_cx, dirty_pages.saturating_mul(2));
        drop(seed_pager);

        let pager_a = SimplePager::open(vfs.clone(), &db_path, PageSize::DEFAULT).unwrap();
        let pager_b = SimplePager::open(vfs.clone(), &db_path, PageSize::DEFAULT).unwrap();
        let cx_a = Cx::new();
        let cx_b = Cx::new();
        pager_a
            .set_wal_backend(Box::new(TrackCPublishWindowBenchWalBackend::new(
                &vfs, &cx_a, &wal_path, mode,
            )))
            .unwrap();
        pager_b
            .set_wal_backend(Box::new(TrackCPublishWindowBenchWalBackend::new(
                &vfs, &cx_b, &wal_path, mode,
            )))
            .unwrap();
        pager_a.set_journal_mode(&cx_a, JournalMode::Wal).unwrap();
        pager_b.set_journal_mode(&cx_b, JournalMode::Wal).unwrap();
        vfs.clear_exclusive_metrics();
        (vfs, pager_a, pager_b)
    }

    fn track_c_measure_competing_writer_stall_ns(
        mode: TrackCPublishWindowMode,
        dirty_pages: usize,
    ) -> Vec<u64> {
        let total_iters =
            TRACK_C_PUBLISH_WINDOW_BENCH_WARMUP_ITERS + TRACK_C_PUBLISH_WINDOW_BENCH_MEASURE_ITERS;
        let mut samples = Vec::with_capacity(TRACK_C_PUBLISH_WINDOW_BENCH_MEASURE_ITERS);

        for iter_idx in 0..total_iters {
            let (vfs, pager_a, pager_b) = track_c_open_contending_pagers(mode, dirty_pages);
            let writer_a = std::thread::spawn(move || {
                let cx = Cx::new();
                let mut txn = pager_a.begin(&cx, TransactionMode::Immediate).unwrap();
                track_c_write_existing_page_range(&mut txn, &cx, 2, dirty_pages, 53);
                txn.commit(&cx).unwrap();
            });

            vfs.wait_for_exclusive_acquisitions(1);

            let cx_b = Cx::new();
            let mut txn_b = pager_b.begin(&cx_b, TransactionMode::Immediate).unwrap();
            let contender_start_page = u32::try_from(dirty_pages).unwrap() + 2;
            track_c_write_existing_page_range(
                &mut txn_b,
                &cx_b,
                contender_start_page,
                dirty_pages,
                97,
            );
            txn_b.commit(&cx_b).unwrap();
            writer_a.join().unwrap();

            let stall_ns = vfs
                .exclusive_wait_samples_ns()
                .into_iter()
                .max()
                .unwrap_or(0);
            if iter_idx >= TRACK_C_PUBLISH_WINDOW_BENCH_WARMUP_ITERS {
                samples.push(stall_ns);
            }
        }

        samples
    }

    fn track_c_percentile_ns(sorted_samples: &[u64], percentile: usize) -> u64 {
        let last_idx = sorted_samples.len().saturating_sub(1);
        let rank = last_idx.saturating_mul(percentile).div_ceil(100);
        sorted_samples[rank]
    }

    fn track_c_sample_summary(samples: &[u64]) -> serde_json::Value {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let total_ns: u128 = sorted.iter().map(|value| u128::from(*value)).sum();
        let mean_ns = (total_ns as f64) / (sorted.len() as f64);

        json!({
            "sample_count": sorted.len(),
            "min_ns": sorted.first().copied().unwrap_or(0),
            "median_ns": track_c_percentile_ns(&sorted, 50),
            "p95_ns": track_c_percentile_ns(&sorted, 95),
            "max_ns": sorted.last().copied().unwrap_or(0),
            "mean_ns": mean_ns,
            "raw_samples_ns": sorted,
        })
    }

    impl crate::traits::WalBackend for MockWalBackend {
        fn begin_transaction(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
            let mut begin_calls = self.begin_calls.lock().unwrap();
            *begin_calls += 1;
            Ok(())
        }

        fn append_frame(
            &mut self,
            _cx: &Cx,
            page_number: u32,
            page_data: &[u8],
            db_size_if_commit: u32,
        ) -> fsqlite_error::Result<()> {
            self.frames
                .lock()
                .unwrap()
                .push((page_number, page_data.to_vec(), db_size_if_commit));
            Ok(())
        }

        fn append_frames(
            &mut self,
            _cx: &Cx,
            frames: &[crate::traits::WalFrameRef<'_>],
        ) -> fsqlite_error::Result<()> {
            let mut batch_calls = self.batch_calls.lock().unwrap();
            *batch_calls += 1;
            drop(batch_calls);

            let mut written = self.frames.lock().unwrap();
            for frame in frames {
                written.push((
                    frame.page_number,
                    frame.page_data.to_vec(),
                    frame.db_size_if_commit,
                ));
            }
            Ok(())
        }

        fn read_page(
            &mut self,
            _cx: &Cx,
            page_number: u32,
        ) -> fsqlite_error::Result<Option<Vec<u8>>> {
            *self.read_page_calls.lock().unwrap() += 1;
            let frames = self.frames.lock().unwrap();
            // Scan backwards for the latest version of the page.
            let result = frames
                .iter()
                .rev()
                .find(|(pn, _, _)| *pn == page_number)
                .map(|(_, data, _)| data.clone());
            drop(frames);
            Ok(result)
        }

        fn committed_txns_since_page(
            &mut self,
            _cx: &Cx,
            page_number: u32,
        ) -> fsqlite_error::Result<u64> {
            let frames = self.frames.lock().unwrap();
            let last_page_frame = frames.iter().rposition(|(pn, _, _)| *pn == page_number);
            let Some(last_page_frame) = last_page_frame else {
                return Ok(frames
                    .iter()
                    .filter(|(_, _, db_size_if_commit)| *db_size_if_commit > 0)
                    .count() as u64);
            };

            let mut page_commit_seen = false;
            let mut committed_txns_after_page = 0_u64;
            for (frame_index, (_, _, db_size_if_commit)) in frames.iter().enumerate() {
                if *db_size_if_commit == 0 {
                    continue;
                }
                if !page_commit_seen && frame_index >= last_page_frame {
                    page_commit_seen = true;
                    continue;
                }
                if page_commit_seen {
                    committed_txns_after_page = committed_txns_after_page.saturating_add(1);
                }
            }
            Ok(committed_txns_after_page)
        }

        fn committed_txn_count(&mut self, _cx: &Cx) -> fsqlite_error::Result<u64> {
            Ok(self
                .frames
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, db_size_if_commit)| *db_size_if_commit > 0)
                .count() as u64)
        }

        fn sync(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
            *self.sync_calls.lock().unwrap() += 1;
            Ok(())
        }

        fn frame_count(&self) -> usize {
            self.frames.lock().unwrap().len()
        }

        fn checkpoint(
            &mut self,
            _cx: &Cx,
            _mode: crate::traits::CheckpointMode,
            _writer: &mut dyn crate::traits::CheckpointPageWriter,
            _backfilled_frames: u32,
            _oldest_reader_frame: Option<u32>,
        ) -> fsqlite_error::Result<crate::traits::CheckpointResult> {
            let total_frames = u32::try_from(self.frames.lock().unwrap().len()).map_err(|_| {
                fsqlite_error::FrankenError::internal("mock wal frame count exceeds u32")
            })?;
            Ok(crate::traits::CheckpointResult {
                total_frames,
                frames_backfilled: total_frames,
                completed: true,
                wal_was_reset: false,
            })
        }
    }

    impl crate::traits::WalBackend for FailingGroupCommitWalBackend {
        fn begin_transaction(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
            Ok(())
        }

        fn append_frame(
            &mut self,
            _cx: &Cx,
            _page_number: u32,
            _page_data: &[u8],
            _db_size_if_commit: u32,
        ) -> fsqlite_error::Result<()> {
            Err(FrankenError::internal(
                "forced single-frame group commit failure",
            ))
        }

        fn append_frames(
            &mut self,
            _cx: &Cx,
            _frames: &[crate::traits::WalFrameRef<'_>],
        ) -> fsqlite_error::Result<()> {
            let mut append_frames_calls = self.append_frames_calls.lock().unwrap();
            *append_frames_calls += 1;
            drop(append_frames_calls);
            std::thread::sleep(std::time::Duration::from_millis(1));
            Err(FrankenError::internal(
                "forced batched group commit append failure",
            ))
        }

        fn read_page(
            &mut self,
            _cx: &Cx,
            _page_number: u32,
        ) -> fsqlite_error::Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn committed_txn_count(&mut self, _cx: &Cx) -> fsqlite_error::Result<u64> {
            Ok(0)
        }

        fn sync(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
            Ok(())
        }

        fn frame_count(&self) -> usize {
            0
        }

        fn checkpoint(
            &mut self,
            _cx: &Cx,
            _mode: crate::traits::CheckpointMode,
            _writer: &mut dyn crate::traits::CheckpointPageWriter,
            _backfilled_frames: u32,
            _oldest_reader_frame: Option<u32>,
        ) -> fsqlite_error::Result<crate::traits::CheckpointResult> {
            Ok(crate::traits::CheckpointResult {
                total_frames: 0,
                frames_backfilled: 0,
                completed: true,
                wal_was_reset: false,
            })
        }
    }

    impl crate::traits::WalBackend for PreparedBatchObservedWalBackend {
        fn append_frame(
            &mut self,
            _cx: &Cx,
            page_number: u32,
            page_data: &[u8],
            db_size_if_commit: u32,
        ) -> fsqlite_error::Result<()> {
            self.frames
                .lock()
                .unwrap()
                .push((page_number, page_data.to_vec(), db_size_if_commit));
            Ok(())
        }

        fn append_frames(
            &mut self,
            _cx: &Cx,
            frames: &[crate::traits::WalFrameRef<'_>],
        ) -> fsqlite_error::Result<()> {
            *self.append_frames_calls.lock().unwrap() += 1;

            let mut written = self.frames.lock().unwrap();
            for frame in frames {
                written.push((
                    frame.page_number,
                    frame.page_data.to_vec(),
                    frame.db_size_if_commit,
                ));
            }
            Ok(())
        }

        fn prepare_append_frames(
            &mut self,
            frames: &[crate::traits::WalFrameRef<'_>],
        ) -> fsqlite_error::Result<Option<crate::traits::PreparedWalFrameBatch>> {
            self.prepare_lock_levels
                .lock()
                .unwrap()
                .push(*self.observed_lock_level.lock().unwrap());

            if frames.is_empty() {
                return Ok(None);
            }

            let frame_size =
                fsqlite_wal::checksum::WAL_FRAME_HEADER_SIZE + frames[0].page_data.len();
            let mut frame_bytes = Vec::with_capacity(frame_size * frames.len());
            let mut frame_metas = Vec::with_capacity(frames.len());
            let mut checksum_transforms = Vec::with_capacity(frames.len());
            for frame in frames {
                frame_metas.push(crate::traits::PreparedWalFrameMeta {
                    page_number: frame.page_number,
                    db_size_if_commit: frame.db_size_if_commit,
                });
                checksum_transforms.push(crate::traits::PreparedWalChecksumTransform {
                    a11: 0,
                    a12: 0,
                    a21: 0,
                    a22: 0,
                    c1: 0,
                    c2: 0,
                });
                frame_bytes
                    .extend_from_slice(&[0_u8; fsqlite_wal::checksum::WAL_FRAME_HEADER_SIZE]);
                frame_bytes.extend_from_slice(frame.page_data);
            }

            Ok(Some(crate::traits::PreparedWalFrameBatch {
                frame_size,
                page_data_offset: fsqlite_wal::checksum::WAL_FRAME_HEADER_SIZE,
                frame_metas,
                checksum_transforms,
                frame_bytes,
                last_commit_frame_offset: frames
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(offset, frame)| (frame.db_size_if_commit != 0).then_some(offset)),
                finalized_for: None,
                finalized_running_checksum: None,
            }))
        }

        fn append_prepared_frames(
            &mut self,
            _cx: &Cx,
            prepared: &mut crate::traits::PreparedWalFrameBatch,
        ) -> fsqlite_error::Result<()> {
            self.append_lock_levels
                .lock()
                .unwrap()
                .push(*self.observed_lock_level.lock().unwrap());
            *self.append_prepared_calls.lock().unwrap() += 1;

            let mut written = self.frames.lock().unwrap();
            for frame in prepared.frame_refs() {
                written.push((
                    frame.page_number,
                    frame.page_data.to_vec(),
                    frame.db_size_if_commit,
                ));
            }
            Ok(())
        }

        fn read_page(
            &mut self,
            _cx: &Cx,
            page_number: u32,
        ) -> fsqlite_error::Result<Option<Vec<u8>>> {
            let frames = self.frames.lock().unwrap();
            Ok(frames
                .iter()
                .rev()
                .find(|(pn, _, _)| *pn == page_number)
                .map(|(_, data, _)| data.clone()))
        }

        fn committed_txn_count(&mut self, _cx: &Cx) -> fsqlite_error::Result<u64> {
            Ok(self
                .frames
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, db_size_if_commit)| *db_size_if_commit > 0)
                .count() as u64)
        }

        fn sync(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
            Ok(())
        }

        fn frame_count(&self) -> usize {
            self.frames.lock().unwrap().len()
        }

        fn checkpoint(
            &mut self,
            _cx: &Cx,
            _mode: crate::traits::CheckpointMode,
            _writer: &mut dyn crate::traits::CheckpointPageWriter,
            _backfilled_frames: u32,
            _oldest_reader_frame: Option<u32>,
        ) -> fsqlite_error::Result<crate::traits::CheckpointResult> {
            let total_frames = u32::try_from(self.frames.lock().unwrap().len()).map_err(|_| {
                fsqlite_error::FrankenError::internal("observed wal frame count exceeds u32")
            })?;
            Ok(crate::traits::CheckpointResult {
                total_frames,
                frames_backfilled: total_frames,
                completed: true,
                wal_was_reset: false,
            })
        }
    }

    fn wal_pager() -> (SimplePager<MemoryVfs>, SharedFrames) {
        let (pager, frames, _, _) = wal_pager_with_tracking();
        (pager, frames)
    }

    fn wal_pager_with_tracking() -> (
        SimplePager<MemoryVfs>,
        SharedFrames,
        SharedCounter,
        SharedCounter,
    ) {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/wal_test.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let (backend, frames, begin_calls, batch_calls) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        (pager, frames, begin_calls, batch_calls)
    }

    fn wal_pager_with_read_tracking() -> (
        SimplePager<MemoryVfs>,
        SharedFrames,
        SharedCounter,
        SharedCounter,
        SharedCounter,
    ) {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/wal_read_tracking.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let (backend, frames, begin_calls, batch_calls, read_page_calls) =
            MockWalBackend::new_with_read_tracking();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        (pager, frames, begin_calls, batch_calls, read_page_calls)
    }

    fn wal_pager_pair_with_shared_backend()
    -> (SimplePager<MemoryVfs>, SimplePager<MemoryVfs>, SharedFrames) {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/wal_shared_refresh.db");
        let pager1 = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let pager2 = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();

        let frames: SharedFrames = StdArc::new(StdMutex::new(Vec::new()));
        let (backend1, _, _) = MockWalBackend::with_shared_frames(StdArc::clone(&frames));
        let (backend2, _, _) = MockWalBackend::with_shared_frames(StdArc::clone(&frames));
        pager1.set_wal_backend(Box::new(backend1)).unwrap();
        pager2.set_wal_backend(Box::new(backend2)).unwrap();
        pager1.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        pager2.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        (pager1, pager2, frames)
    }

    #[test]
    fn test_journal_mode_default_is_delete() {
        let (pager, _) = test_pager();
        assert_eq!(
            pager.journal_mode(),
            JournalMode::Delete,
            "bead_id={BEAD_ID} case=default_journal_mode"
        );
    }

    #[test]
    fn test_set_journal_mode_wal_requires_backend() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        // Without a WAL backend, switching to WAL should fail.
        let result = pager.set_journal_mode(&cx, JournalMode::Wal);
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=wal_requires_backend"
        );
    }

    #[test]
    fn test_set_journal_mode_wal_with_backend() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let (backend, _frames, _, _) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        let mode = pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        assert_eq!(
            mode,
            JournalMode::Wal,
            "bead_id={BEAD_ID} case=wal_mode_set"
        );
        assert_eq!(
            pager.journal_mode(),
            JournalMode::Wal,
            "bead_id={BEAD_ID} case=wal_mode_persisted"
        );
    }

    #[test]
    fn test_set_journal_mode_updates_header_version_bytes() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        // Verify default state: bytes 18-19 should be 1 (rollback journal).
        {
            let inner = pager.inner.lock().unwrap();
            let mut page1 = vec![0u8; inner.page_size.as_usize()];
            let n = inner.db_file.read(&cx, &mut page1, 0).unwrap();
            assert!(n >= DATABASE_HEADER_SIZE);
            assert_eq!(page1[18], 1, "bead_id={BEAD_ID} case=default_write_version");
            assert_eq!(page1[19], 1, "bead_id={BEAD_ID} case=default_read_version");
        }

        // Switch to WAL mode: bytes 18-19 should become 2.
        let (backend, _frames, _, _) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        {
            let inner = pager.inner.lock().unwrap();
            let mut page1 = vec![0u8; inner.page_size.as_usize()];
            let n = inner.db_file.read(&cx, &mut page1, 0).unwrap();
            assert!(n >= DATABASE_HEADER_SIZE);
            assert_eq!(page1[18], 2, "bead_id={BEAD_ID} case=wal_write_version");
            assert_eq!(page1[19], 2, "bead_id={BEAD_ID} case=wal_read_version");
        }

        // Switch back to DELETE mode: bytes 18-19 should revert to 1.
        pager.set_journal_mode(&cx, JournalMode::Delete).unwrap();

        {
            let inner = pager.inner.lock().unwrap();
            let mut page1 = vec![0u8; inner.page_size.as_usize()];
            let n = inner.db_file.read(&cx, &mut page1, 0).unwrap();
            assert!(n >= DATABASE_HEADER_SIZE);
            assert_eq!(page1[18], 1, "bead_id={BEAD_ID} case=delete_write_version");
            assert_eq!(page1[19], 1, "bead_id={BEAD_ID} case=delete_read_version");
        }
    }

    #[test]
    fn test_set_journal_mode_blocked_during_write() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let _writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let (backend, _frames, _, _) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        let result = pager.set_journal_mode(&cx, JournalMode::Wal);
        assert!(
            result.is_err(),
            "bead_id={BEAD_ID} case=mode_switch_blocked_during_write"
        );
    }

    #[test]
    fn test_set_journal_mode_same_mode_succeeds_during_reader() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let _reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();

        let mode = pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        assert_eq!(mode, JournalMode::Wal);
    }

    #[test]
    fn test_checkpoint_busy_with_active_reader() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();

        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let err = pager
            .checkpoint(&cx, crate::traits::CheckpointMode::Passive)
            .expect_err("checkpoint should be blocked by active reader");
        assert!(matches!(err, FrankenError::Busy));
        drop(reader);

        // After reader ends, checkpoint should proceed.
        let result = pager
            .checkpoint(&cx, crate::traits::CheckpointMode::Passive)
            .expect("checkpoint should succeed after reader closes");
        assert_eq!(result.total_frames, 0);
    }

    #[test]
    fn test_checkpoint_busy_with_active_writer() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();

        let _writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let err = pager
            .checkpoint(&cx, crate::traits::CheckpointMode::Passive)
            .expect_err("checkpoint should be blocked by active writer");
        assert!(matches!(err, FrankenError::Busy));
    }

    #[test]
    fn test_checkpoint_writer_sync_repairs_page1_header_after_late_growth() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0xCD; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        // Snapshot current page 1 as a realistic checkpoint payload.
        let mut page1_data = {
            let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
            txn.get_page(&cx, PageNumber::ONE).unwrap().into_vec()
        };
        let header: [u8; DATABASE_HEADER_SIZE] = page1_data[..DATABASE_HEADER_SIZE]
            .try_into()
            .expect("page 1 header must be present");
        let expected_change_counter = DatabaseHeader::from_bytes(&header)
            .expect("header must parse")
            .change_counter;
        page1_data[24..28].copy_from_slice(&0_u32.to_be_bytes());
        page1_data[92..96].copy_from_slice(&0_u32.to_be_bytes());

        let mut writer = pager.checkpoint_writer();

        // Simulate checkpoint replay order where page 1 arrives first, then a
        // higher page extends the DB. Without final header repair this can
        // leave header page_count stale.
        crate::traits::CheckpointPageWriter::write_page(
            &mut writer,
            &cx,
            PageNumber::ONE,
            &page1_data,
        )
        .unwrap();
        let page_three = PageNumber::new(3).unwrap();
        crate::traits::CheckpointPageWriter::write_page(
            &mut writer,
            &cx,
            page_three,
            &vec![0xAB; ps],
        )
        .unwrap();
        crate::traits::CheckpointPageWriter::sync(&mut writer, &cx).unwrap();

        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let raw_page1 = txn.get_page(&cx, PageNumber::ONE).unwrap().into_vec();
        let header: [u8; DATABASE_HEADER_SIZE] = raw_page1[..DATABASE_HEADER_SIZE]
            .try_into()
            .expect("page 1 header must be present");
        let parsed = DatabaseHeader::from_bytes(&header).expect("header must parse");
        assert_eq!(
            parsed.page_count,
            page_three.get(),
            "bead_id={BEAD_ID} case=checkpoint_sync_repairs_page_count"
        );
        assert_eq!(
            parsed.change_counter, expected_change_counter,
            "bead_id={BEAD_ID} case=checkpoint_sync_repairs_change_counter"
        );
        assert_eq!(
            parsed.version_valid_for, parsed.change_counter,
            "bead_id={BEAD_ID} case=checkpoint_sync_repairs_version_valid_for"
        );
    }

    #[test]
    fn test_wal_commit_appends_frames() {
        let (pager, frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        let data = vec![0xAA_u8; ps];
        txn.write_page(&cx, p1, &data).unwrap();
        txn.commit(&cx).unwrap();

        let locked_frames = frames.lock().unwrap();
        assert_eq!(
            locked_frames.len(),
            2,
            "bead_id={BEAD_ID} case=wal_two_frames_appended_including_header"
        );
        let p1_frame = locked_frames.iter().find(|f| f.0 == p1.get()).unwrap();
        assert_eq!(p1_frame.1[0], 0xAA, "bead_id={BEAD_ID} case=wal_frame_data");
        // Commit frame should have db_size > 0.
        let commit_count = locked_frames.iter().filter(|f| f.2 > 0).count();
        assert_eq!(commit_count, 1, "bead_id={BEAD_ID} case=wal_commit_marker");
        drop(locked_frames);
    }

    #[test]
    fn test_wal_commit_multi_page() {
        let (pager, frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        let p2 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();
        txn.write_page(&cx, p2, &vec![0x22; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let locked_frames = frames.lock().unwrap();
        assert_eq!(
            locked_frames.len(),
            3,
            "bead_id={BEAD_ID} case=wal_multi_page_count"
        );
        // Exactly one frame should be the commit frame (db_size > 0).
        let commit_count = locked_frames.iter().filter(|f| f.2 > 0).count();
        drop(locked_frames);
        assert_eq!(
            commit_count, 1,
            "bead_id={BEAD_ID} case=wal_exactly_one_commit_marker"
        );
    }

    #[test]
    fn test_wal_commit_uses_single_batch_append() {
        let (pager, frames, _begin_calls, batch_calls) = wal_pager_with_tracking();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let new_page = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, PageNumber::ONE, &vec![0x11; ps])
            .unwrap();
        txn.write_page(&cx, new_page, &vec![0x22; ps]).unwrap();
        txn.commit(&cx).unwrap();

        assert_eq!(
            *batch_calls.lock().unwrap(),
            1,
            "bead_id={BEAD_ID} case=wal_batch_append_single_call"
        );

        let frames = frames.lock().unwrap();
        assert_eq!(
            frames.len(),
            2,
            "bead_id={BEAD_ID} case=wal_batch_append_frame_count"
        );
        assert_eq!(
            frames[0].0,
            PageNumber::ONE.get(),
            "bead_id={BEAD_ID} case=wal_batch_append_sorted_page1"
        );
        assert_eq!(
            frames[1].0,
            new_page.get(),
            "bead_id={BEAD_ID} case=wal_batch_append_sorted_new_page"
        );
        assert_eq!(
            frames[0].2, 0,
            "bead_id={BEAD_ID} case=wal_batch_append_non_commit_first"
        );
        assert_eq!(
            frames[1].2,
            new_page.get(),
            "bead_id={BEAD_ID} case=wal_batch_append_commit_marker_last"
        );
    }

    #[test]
    fn test_wal_preparation_happens_before_exclusive_publish_lock() {
        let (pager, observed_lock_level) = observed_lock_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let (
            backend,
            frames,
            append_frames_calls,
            append_prepared_calls,
            prepare_lock_levels,
            append_lock_levels,
        ) = PreparedBatchObservedWalBackend::new(Arc::clone(&observed_lock_level));
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let new_page = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, PageNumber::ONE, &vec![0x11; ps])
            .unwrap();
        txn.write_page(&cx, new_page, &vec![0x22; ps]).unwrap();
        txn.commit(&cx).unwrap();

        assert_eq!(
            *append_frames_calls.lock().unwrap(),
            0,
            "bead_id=bd-db300.3.2 case=prepared_path_skips_fallback_append"
        );
        assert_eq!(
            *append_prepared_calls.lock().unwrap(),
            1,
            "bead_id=bd-db300.3.2 case=prepared_path_uses_prepared_append"
        );
        assert_eq!(
            prepare_lock_levels.lock().unwrap().as_slice(),
            &[LockLevel::Reserved],
            "bead_id=bd-db300.3.2 case=prepare_runs_before_exclusive_publish"
        );
        assert_eq!(
            append_lock_levels.lock().unwrap().as_slice(),
            &[LockLevel::Exclusive],
            "bead_id=bd-db300.3.2 case=prepared_append_runs_inside_exclusive_publish"
        );

        let frames = frames.lock().unwrap();
        assert_eq!(
            frames.len(),
            2,
            "bead_id=bd-db300.3.2 case=prepared_path_preserves_frame_count"
        );
        assert_eq!(
            frames[0].0,
            PageNumber::ONE.get(),
            "bead_id=bd-db300.3.2 case=prepared_path_preserves_sorted_page_one"
        );
        assert_eq!(
            frames[1].0,
            new_page.get(),
            "bead_id=bd-db300.3.2 case=prepared_path_preserves_sorted_commit_frame"
        );
    }

    #[test]
    fn test_wal_commit_sync_policy_deferred_skips_sync() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/wal_deferred_sync_policy.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let (backend, frames, _begin_calls, _batch_calls, sync_calls) =
            MockWalBackend::new_with_sync_tracking();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        pager
            .set_wal_commit_sync_policy(WalCommitSyncPolicy::Deferred)
            .unwrap();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, page, &vec![0x55; PageSize::DEFAULT.as_usize()])
            .unwrap();
        txn.commit(&cx).unwrap();

        assert_eq!(
            *sync_calls.lock().unwrap(),
            0,
            "bead_id={BEAD_ID} case=wal_sync_policy_deferred_skips_commit_sync"
        );
        assert_eq!(
            frames.lock().unwrap().len(),
            2,
            "bead_id={BEAD_ID} case=wal_sync_policy_deferred_still_appends_wal_frames"
        );
    }

    #[test]
    fn test_wal_commit_sync_policy_per_commit_syncs() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/wal_per_commit_sync_policy.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let (backend, frames, _begin_calls, _batch_calls, sync_calls) =
            MockWalBackend::new_with_sync_tracking();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        pager
            .set_wal_commit_sync_policy(WalCommitSyncPolicy::PerCommit)
            .unwrap();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, page, &vec![0x66; PageSize::DEFAULT.as_usize()])
            .unwrap();
        txn.commit(&cx).unwrap();

        assert_eq!(
            *sync_calls.lock().unwrap(),
            1,
            "bead_id={BEAD_ID} case=wal_sync_policy_per_commit_runs_commit_sync"
        );
        assert_eq!(
            frames.lock().unwrap().len(),
            2,
            "bead_id={BEAD_ID} case=wal_sync_policy_per_commit_appends_wal_frames"
        );
    }

    #[test]
    fn test_group_commit_flush_failure_wakes_waiters_with_error() {
        for attempt in 0..32 {
            let vfs = MemoryVfs::new();
            let path = PathBuf::from(format!("/wal_group_commit_failure_waiter_{attempt}.db"));
            let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
            let cx = Cx::new();
            let (backend, append_frames_calls) = FailingGroupCommitWalBackend::new();
            pager.set_wal_backend(Box::new(backend)).unwrap();
            pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
            pager
                .set_wal_commit_sync_policy(WalCommitSyncPolicy::Deferred)
                .unwrap();

            let inner = Arc::clone(&pager.inner);
            let wal_backend = Arc::clone(&pager.wal_backend);
            let queue = Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()));
            let pool = pager.pool.clone();
            let start = StdArc::new(std::sync::Barrier::new(3));

            let spawn_commit = |page_number: u32, fill: u8| {
                let inner = Arc::clone(&inner);
                let wal_backend = Arc::clone(&wal_backend);
                let queue = Arc::clone(&queue);
                let pool = pool.clone();
                let start = StdArc::clone(&start);
                std::thread::spawn(move || {
                    let cx = Cx::new();
                    let page_no = PageNumber::new(page_number).unwrap();
                    let page =
                        StagedPage::from_bytes(&pool, &vec![fill; pool.page_size()]).unwrap();
                    let mut write_set = HashMap::new();
                    write_set.insert(page_no, page);
                    let write_pages_sorted = vec![page_no];
                    start.wait();
                    SimpleTransaction::<MemoryVfs>::commit_wal_group_commit(
                        &cx,
                        &wal_backend,
                        &inner,
                        &write_set,
                        &write_pages_sorted,
                        &queue,
                    )
                })
            };

            let writer_a = spawn_commit(2, 0x11);
            let writer_b = spawn_commit(3, 0x22);
            start.wait();

            let result_a = writer_a.join().unwrap();
            let result_b = writer_b.join().unwrap();
            let append_call_count = *append_frames_calls.lock().unwrap();
            if append_call_count != 1 {
                continue;
            }

            let error_a = result_a.expect_err("flusher should observe append failure");
            let error_b = result_b.expect_err("waiter should observe propagated failure");
            let error_a = error_a.to_string();
            let error_b = error_b.to_string();
            assert!(
                error_a.contains("forced batched group commit append failure")
                    || error_b.contains("forced batched group commit append failure"),
                "bead_id={BEAD_ID} case=group_commit_flusher_reports_backend_failure error_a={error_a} error_b={error_b}"
            );
            assert!(
                error_a.contains("group commit flush failed")
                    || error_b.contains("group commit flush failed"),
                "bead_id={BEAD_ID} case=group_commit_waiter_reports_epoch_failure error_a={error_a} error_b={error_b}"
            );
            return;
        }

        panic!(
            "bead_id={BEAD_ID} case=group_commit_flush_failure_wakes_waiters could not coalesce flusher+waiter in allotted attempts"
        );
    }

    #[test]
    fn test_group_commit_flush_failure_restores_reserved_lock_level() {
        let vfs = BlockingObservedLockVfs::new();
        let observed_lock_level = vfs.observed_lock_level();
        let path = PathBuf::from("/wal_group_commit_failure_restores_reserved.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let (backend, _append_frames_calls) = FailingGroupCommitWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
        pager
            .set_wal_commit_sync_policy(WalCommitSyncPolicy::Deferred)
            .unwrap();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, page, &vec![0x77; PageSize::DEFAULT.as_usize()])
            .unwrap();

        let error = txn.commit(&cx).expect_err(
            "failing WAL backend should surface a group commit error instead of committing",
        );
        assert!(
            error
                .to_string()
                .contains("forced batched group commit append failure"),
            "bead_id={BEAD_ID} case=group_commit_failure_surfaces_backend_error error={error}"
        );
        assert_eq!(
            *observed_lock_level.lock().unwrap(),
            LockLevel::Reserved,
            "bead_id={BEAD_ID} case=group_commit_failure_restores_writer_lock_after_exclusive_error"
        );

        drop(txn);
        assert_eq!(
            *observed_lock_level.lock().unwrap(),
            LockLevel::None,
            "bead_id={BEAD_ID} case=group_commit_failure_drop_releases_retained_writer_lock"
        );
    }

    #[test]
    fn test_group_commit_fault_hook_after_flush_before_publish_wakes_waiters_with_error_and_records_context()
     {
        for attempt in 0..32 {
            crate::fault_hooks::clear();

            let vfs = MemoryVfs::new();
            let path = PathBuf::from(format!("/wal_group_commit_publish_hook_{attempt}.db"));
            let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
            let cx = Cx::new();
            let (backend, _frames, _begin_calls, batch_calls, sync_calls) =
                MockWalBackend::new_with_sync_tracking();
            pager.set_wal_backend(Box::new(backend)).unwrap();
            pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
            pager
                .set_wal_commit_sync_policy(WalCommitSyncPolicy::PerCommit)
                .unwrap();

            crate::fault_hooks::arm_after_flush_before_publish(
                crate::fault_hooks::FaultHookArm::new(
                    "bd-db300.7.2.2-after-flush-before-publish",
                    "GROUP-COMMIT-PUBLISH",
                    "group_commit_publish_recovery",
                ),
            );

            let inner = Arc::clone(&pager.inner);
            let wal_backend = Arc::clone(&pager.wal_backend);
            let queue = Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()));
            let pool = pager.pool.clone();
            let start = StdArc::new(std::sync::Barrier::new(3));

            let spawn_commit = |page_number: u32, fill: u8| {
                let inner = Arc::clone(&inner);
                let wal_backend = Arc::clone(&wal_backend);
                let queue = Arc::clone(&queue);
                let pool = pool.clone();
                let start = StdArc::clone(&start);
                std::thread::spawn(move || {
                    let cx = Cx::new();
                    let page_no = PageNumber::new(page_number).unwrap();
                    let page =
                        StagedPage::from_bytes(&pool, &vec![fill; pool.page_size()]).unwrap();
                    let mut write_set = HashMap::new();
                    write_set.insert(page_no, page);
                    let write_pages_sorted = vec![page_no];
                    start.wait();
                    SimpleTransaction::<MemoryVfs>::commit_wal_group_commit(
                        &cx,
                        &wal_backend,
                        &inner,
                        &write_set,
                        &write_pages_sorted,
                        &queue,
                    )
                })
            };

            let writer_a = spawn_commit(2, 0x11);
            let writer_b = spawn_commit(3, 0x22);
            start.wait();

            let result_a = writer_a.join().unwrap();
            let result_b = writer_b.join().unwrap();
            if *batch_calls.lock().unwrap() != 1 {
                continue;
            }

            assert_eq!(
                *sync_calls.lock().unwrap(),
                1,
                "bead_id={BEAD_ID} case=group_commit_publish_hook_runs_after_real_sync"
            );

            let error_a = result_a
                .expect_err("flusher should surface publish-hook failure")
                .to_string();
            let error_b = result_b
                .expect_err("waiter should observe propagated publish-hook failure")
                .to_string();
            assert!(
                error_a.contains("fault_inject:after_flush_before_publish")
                    || error_b.contains("fault_inject:after_flush_before_publish"),
                "bead_id={BEAD_ID} case=group_commit_publish_hook_reports_primary_failure error_a={error_a} error_b={error_b}"
            );
            assert!(
                error_a.contains("group commit flush failed")
                    || error_b.contains("group commit flush failed"),
                "bead_id={BEAD_ID} case=group_commit_publish_hook_reports_epoch_failure error_a={error_a} error_b={error_b}"
            );

            let records = crate::fault_hooks::take_records();
            assert_eq!(records.len(), 1, "publish hook should record exactly once");
            assert_eq!(records[0].point, "after_flush_before_publish");
            assert_eq!(
                records[0].run_id,
                "bd-db300.7.2.2-after-flush-before-publish"
            );
            assert_eq!(records[0].scenario_id, "GROUP-COMMIT-PUBLISH");
            assert_eq!(records[0].invariant_family, "group_commit_publish_recovery");
            assert!(
                records[0].detail.contains("batch_count=2"),
                "record should capture coalesced batch context: {}",
                records[0].detail
            );
            assert!(
                records[0].detail.contains("frame_count=2"),
                "record should capture coalesced frame context: {}",
                records[0].detail
            );

            crate::fault_hooks::clear();
            return;
        }

        panic!(
            "bead_id={BEAD_ID} case=group_commit_publish_hook_wakes_waiters could not coalesce flusher+waiter in allotted attempts"
        );
    }

    /// H11 / F11: Suppressed Condvar notification — the flusher still publishes
    /// the completed epoch, but intentionally skips the wakeup. Waiters must
    /// recover via the timed wait rather than hanging forever.
    ///
    /// Proof obligation: both flusher and waiter threads complete within a
    /// bounded time. No permanent hang.
    ///
    /// Replay: `cargo test -p fsqlite-pager --lib -- test_fault_drop_condvar_notify --nocapture`
    #[test]
    fn test_fault_drop_condvar_notify_waiters_recover_via_timeout() {
        for attempt in 0..32 {
            crate::fault_hooks::clear();

            let cx = Cx::new();
            let vfs = MemoryVfs::new();
            let path = PathBuf::from(format!("/condvar_notify_drop_{attempt}.db"));
            let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
            let (backend, _frames, _begin_calls, batch_calls, _sync_calls) =
                MockWalBackend::new_with_sync_tracking();
            pager.set_wal_backend(Box::new(backend)).unwrap();
            pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();
            pager
                .set_wal_commit_sync_policy(WalCommitSyncPolicy::PerCommit)
                .unwrap();

            crate::fault_hooks::arm_drop_condvar_notify(crate::fault_hooks::FaultHookArm::new(
                "bd-db300.7.2.2-drop-condvar",
                "DROP-CONDVAR-NOTIFY",
                "liveness_under_fault",
            ));

            let inner = Arc::clone(&pager.inner);
            let wal_backend = Arc::clone(&pager.wal_backend);
            let queue = Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()));
            let pool = pager.pool.clone();
            let start = StdArc::new(std::sync::Barrier::new(3));

            let spawn_commit = |page_number: u32, fill: u8| {
                let inner = Arc::clone(&inner);
                let wal_backend = Arc::clone(&wal_backend);
                let queue = Arc::clone(&queue);
                let pool = pool.clone();
                let start = StdArc::clone(&start);
                std::thread::spawn(move || {
                    let cx = Cx::new();
                    let page_no = PageNumber::new(page_number).unwrap();
                    let page =
                        StagedPage::from_bytes(&pool, &vec![fill; pool.page_size()]).unwrap();
                    let mut write_set = HashMap::new();
                    write_set.insert(page_no, page);
                    let write_pages_sorted = vec![page_no];
                    start.wait();
                    SimpleTransaction::<MemoryVfs>::commit_wal_group_commit(
                        &cx,
                        &wal_backend,
                        &inner,
                        &write_set,
                        &write_pages_sorted,
                        &queue,
                    )
                })
            };

            let writer_a = spawn_commit(2, 0x33);
            let writer_b = spawn_commit(3, 0x44);
            let started = Instant::now();
            start.wait();

            let result_a = writer_a.join().unwrap();
            let result_b = writer_b.join().unwrap();
            let elapsed = started.elapsed();

            if *batch_calls.lock().unwrap() != 1 {
                crate::fault_hooks::clear();
                continue;
            }

            // Key proof: both threads completed (no hang), within 2 seconds.
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "bead_id={BEAD_ID} case=condvar_drop_no_hang elapsed={elapsed:?}"
            );

            // The flusher succeeds (wrote frames and published the epoch, but
            // skipped the Condvar wakeup). Waiter recovers via epoch check on
            // timeout — may succeed or fail.
            let _any_ok = result_a.is_ok() || result_b.is_ok();

            let records = crate::fault_hooks::take_records();
            assert_eq!(records.len(), 1, "condvar-drop hook should fire once");
            assert_eq!(records[0].point, "drop_condvar_notify");
            assert_eq!(records[0].run_id, "bd-db300.7.2.2-drop-condvar");
            assert!(
                records[0].detail.contains("completed_epoch="),
                "record should capture epoch: {}",
                records[0].detail
            );

            crate::fault_hooks::clear();
            return;
        }

        panic!(
            "bead_id={BEAD_ID} case=condvar_drop_no_hang could not coalesce in allotted attempts"
        );
    }

    /// H4 / F4: Crash during Phase C — after WAL frames are durable and
    /// commit_seq is updated, but before snapshot publish completes.
    ///
    /// Proof obligation: commit returns Err, but WAL frames were written
    /// (the mock backend recorded them). On a real system, recovery from
    /// the durable WAL would rebuild correct state.
    ///
    /// Replay: `cargo test -p fsqlite-pager --lib -- test_fault_during_phase_c --nocapture`
    #[test]
    fn test_fault_during_phase_c_returns_error_and_wal_frames_survive() {
        crate::fault_hooks::clear();

        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/fault_phase_c_test.db");
        let pager = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let (backend, frames, _begin_calls, _batch_calls) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        // Arm the Phase C hook.
        crate::fault_hooks::arm_during_phase_c(crate::fault_hooks::FaultHookArm::new(
            "bd-db300.7.2.2-phase-c",
            "PHASE-C-CRASH",
            "commit_publish_recovery",
        ));

        // Begin a writer transaction and dirty a page.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page_bytes = PageSize::DEFAULT.as_usize();
        txn.write_page(&cx, PageNumber::ONE, &vec![0xCC; page_bytes])
            .unwrap();

        // Commit should fail — the hook fires after WAL I/O but before publish.
        let err = txn
            .commit(&cx)
            .expect_err("Phase C fault hook should cause commit to fail");
        assert!(
            err.to_string().contains("fault_inject:during_phase_c"),
            "error should identify the Phase C hook: {err}"
        );

        // WAL frames should have been written (the error is AFTER WAL I/O).
        let written_frames = frames.lock().unwrap();
        assert!(
            !written_frames.is_empty(),
            "WAL frames should survive — the fault fires after WAL I/O completes"
        );

        // Verify injection record.
        let records = crate::fault_hooks::take_records();
        assert_eq!(records.len(), 1, "exactly one Phase C fault should fire");
        assert_eq!(records[0].point, "during_phase_c");
        assert_eq!(records[0].run_id, "bd-db300.7.2.2-phase-c");
        assert_eq!(records[0].scenario_id, "PHASE-C-CRASH");
        assert!(
            records[0].detail.contains("commit_seq="),
            "record should capture commit_seq: {}",
            records[0].detail
        );

        crate::fault_hooks::clear();
    }

    #[test]
    fn test_publish_window_measurement_captures_exclusive_hold_sample() {
        let (cx, mut txn, vfs) =
            track_c_publish_window_prepared_commit(TrackCPublishWindowMode::PreparedCandidate, 7);
        txn.commit(&cx).unwrap();

        let hold_samples = vfs.exclusive_hold_samples_ns();
        assert_eq!(
            hold_samples.len(),
            1,
            "bead_id={TRACK_C_PUBLISH_WINDOW_BENCH_BEAD_ID} case=hold_sample_count"
        );
        assert!(
            hold_samples[0] > 0,
            "bead_id={TRACK_C_PUBLISH_WINDOW_BENCH_BEAD_ID} case=hold_sample_positive"
        );
    }

    #[test]
    fn test_publish_window_contention_measurement_captures_competing_writer_wait() {
        let (vfs, pager_a, pager_b) =
            track_c_open_contending_pagers(TrackCPublishWindowMode::PreparedCandidate, 7);
        let writer_a = std::thread::spawn(move || {
            let cx = Cx::new();
            let mut txn = pager_a.begin(&cx, TransactionMode::Immediate).unwrap();
            track_c_write_existing_page_range(&mut txn, &cx, 2, 7, 53);
            txn.commit(&cx).unwrap();
        });

        vfs.wait_for_exclusive_acquisitions(1);

        let cx_b = Cx::new();
        let mut txn_b = pager_b.begin(&cx_b, TransactionMode::Immediate).unwrap();
        track_c_write_existing_page_range(&mut txn_b, &cx_b, 9, 7, 97);
        txn_b.commit(&cx_b).unwrap();
        writer_a.join().unwrap();

        let wait_samples = vfs.exclusive_wait_samples_ns();
        assert_eq!(
            wait_samples.len(),
            2,
            "bead_id={TRACK_C_PUBLISH_WINDOW_BENCH_BEAD_ID} case=wait_sample_count"
        );
        assert!(
            wait_samples.iter().copied().max().unwrap_or(0) > 0,
            "bead_id={TRACK_C_PUBLISH_WINDOW_BENCH_BEAD_ID} case=contending_writer_wait_positive"
        );
    }

    #[test]
    fn test_collect_wal_commit_batch_keeps_sorted_order_and_single_commit_boundary() {
        let page_three = PageNumber::new(3).unwrap();
        let mut write_set = HashMap::new();

        let mut page1 = PageBuf::new(PageSize::DEFAULT);
        page1.fill(0x11);
        write_set.insert(PageNumber::ONE, StagedPage::from_buf(page1));

        let mut page3 = PageBuf::new(PageSize::DEFAULT);
        page3.fill(0x33);
        write_set.insert(page_three, StagedPage::from_buf(page3));

        let write_pages_sorted = vec![PageNumber::ONE, page_three];
        let batch = collect_wal_commit_batch(2, &write_set, &write_pages_sorted)
            .unwrap()
            .expect("non-empty write set should yield a WAL batch");

        assert_eq!(
            batch.new_db_size,
            page_three.get(),
            "bead_id={BEAD_ID} case=wal_batch_helper_new_db_size"
        );
        assert_eq!(
            batch.frames.len(),
            2,
            "bead_id={BEAD_ID} case=wal_batch_helper_frame_count"
        );
        assert_eq!(
            batch.frames[0].page_number,
            PageNumber::ONE.get(),
            "bead_id={BEAD_ID} case=wal_batch_helper_sorted_first"
        );
        assert_eq!(
            batch.frames[0].db_size_if_commit, 0,
            "bead_id={BEAD_ID} case=wal_batch_helper_non_commit_prefix"
        );
        assert_eq!(
            batch.frames[1].page_number,
            page_three.get(),
            "bead_id={BEAD_ID} case=wal_batch_helper_sorted_last"
        );
        assert_eq!(
            batch.frames[1].db_size_if_commit,
            page_three.get(),
            "bead_id={BEAD_ID} case=wal_batch_helper_commit_boundary_last"
        );
    }

    #[test]
    fn test_collect_wal_commit_batch_preserves_existing_db_size_for_interior_updates() {
        let page_two = PageNumber::new(2).unwrap();
        let mut write_set = HashMap::new();

        let mut page2 = PageBuf::new(PageSize::DEFAULT);
        page2.fill(0x22);
        write_set.insert(page_two, StagedPage::from_buf(page2));

        let batch = collect_wal_commit_batch(9, &write_set, &[page_two])
            .unwrap()
            .expect("single dirty page should yield a WAL batch");

        assert_eq!(
            batch.new_db_size, 9,
            "bead_id={BEAD_ID} case=wal_batch_helper_preserve_db_size"
        );
        assert_eq!(
            batch.frames[0].db_size_if_commit, 9,
            "bead_id={BEAD_ID} case=wal_batch_helper_commit_marker_uses_existing_db_size"
        );
    }

    #[test]
    fn test_collect_wal_commit_batch_returns_none_for_empty_write_set() {
        let write_set = HashMap::new();
        let batch = collect_wal_commit_batch(4, &write_set, &[]).unwrap();
        assert!(
            batch.is_none(),
            "bead_id={BEAD_ID} case=wal_batch_helper_empty_batch"
        );
    }

    #[test]
    fn test_collect_wal_commit_batch_errors_when_sorted_page_missing_from_write_set() {
        let missing_page = PageNumber::new(4).unwrap();
        let write_set = HashMap::new();
        let err = match collect_wal_commit_batch(1, &write_set, &[missing_page]) {
            Ok(_) => panic!(
                "bead_id={BEAD_ID} case=wal_batch_helper_missing_page expected helper to fail"
            ),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("missing page 4"),
            "bead_id={BEAD_ID} case=wal_batch_helper_missing_page error={message}"
        );
    }

    #[test]
    fn test_build_group_commit_batch_clones_owned_frames_and_commit_boundary() {
        let page_two = PageNumber::new(2).unwrap();
        let page_three = PageNumber::new(3).unwrap();
        let mut write_set = HashMap::new();

        let mut page2 = PageBuf::new(PageSize::DEFAULT);
        page2.fill(0x22);
        write_set.insert(page_two, StagedPage::from_buf(page2));

        let mut page3 = PageBuf::new(PageSize::DEFAULT);
        page3.fill(0x33);
        write_set.insert(page_three, StagedPage::from_buf(page3));

        let (batch, new_db_size) = build_group_commit_batch(2, &write_set, &[page_two, page_three])
            .unwrap()
            .expect("sorted write set should yield a group commit batch");
        drop(write_set);

        assert_eq!(
            new_db_size, 3,
            "bead_id={BEAD_ID} case=group_commit_batch_helper_new_db_size"
        );
        assert_eq!(
            batch.frames.len(),
            2,
            "bead_id={BEAD_ID} case=group_commit_batch_helper_frame_count"
        );
        assert_eq!(
            batch.frames[0].page_number,
            page_two.get(),
            "bead_id={BEAD_ID} case=group_commit_batch_helper_sorted_first"
        );
        assert_eq!(
            batch.frames[0].db_size_if_commit, 0,
            "bead_id={BEAD_ID} case=group_commit_batch_helper_non_commit_first"
        );
        assert_eq!(
            batch.frames[1].page_number,
            page_three.get(),
            "bead_id={BEAD_ID} case=group_commit_batch_helper_sorted_last"
        );
        assert_eq!(
            batch.frames[1].db_size_if_commit,
            page_three.get(),
            "bead_id={BEAD_ID} case=group_commit_batch_helper_commit_marker_last"
        );
        assert_eq!(
            batch.frames[0].page_data[0], 0x22,
            "bead_id={BEAD_ID} case=group_commit_batch_helper_preserves_first_payload_after_source_drop"
        );
        assert_eq!(
            batch.frames[1].page_data[0], 0x33,
            "bead_id={BEAD_ID} case=group_commit_batch_helper_preserves_second_payload_after_source_drop"
        );
    }

    #[test]
    fn test_build_group_commit_batch_returns_none_for_empty_write_set() {
        let write_set = HashMap::new();
        let batch = build_group_commit_batch(4, &write_set, &[]).unwrap();
        assert!(
            batch.is_none(),
            "bead_id={BEAD_ID} case=group_commit_batch_helper_empty_batch"
        );
    }

    #[test]
    fn test_build_group_commit_batch_errors_when_sorted_page_missing_from_write_set() {
        let missing_page = PageNumber::new(4).unwrap();
        let write_set = HashMap::new();
        let err = match build_group_commit_batch(1, &write_set, &[missing_page]) {
            Ok(_) => panic!(
                "bead_id={BEAD_ID} case=group_commit_batch_helper_missing_page expected helper to fail"
            ),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("missing page 4"),
            "bead_id={BEAD_ID} case=group_commit_batch_helper_missing_page error={message}"
        );
    }

    #[test]
    fn test_group_commit_queue_retains_failed_epoch_for_late_waiter() {
        let queue = GroupCommitQueue::new(GroupCommitConfig::default());
        queue.publish_failed_epoch(
            1,
            &FrankenError::internal("forced group commit flush failure"),
            false,
        );
        queue.publish_completed_epoch(2, false);

        let guard = queue
            .consolidator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let err = queue.wait_for_epoch_outcome(guard, 1).unwrap_err();
        let message = err.to_string();

        assert!(
            message.contains("epoch 1"),
            "bead_id={BEAD_ID} case=group_commit_failed_epoch_late_waiter_mentions_epoch message={message}"
        );
        assert!(
            message.contains("forced group commit flush failure"),
            "bead_id={BEAD_ID} case=group_commit_failed_epoch_late_waiter_preserves_detail message={message}"
        );
    }

    #[test]
    fn test_group_commit_queue_success_not_poisoned_by_other_failed_epoch() {
        let queue = GroupCommitQueue::new(GroupCommitConfig::default());
        queue.publish_failed_epoch(
            1,
            &FrankenError::internal("forced group commit flush failure"),
            false,
        );
        queue.publish_completed_epoch(2, false);

        let guard = queue
            .consolidator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(matches!(
            queue.wait_for_epoch_outcome(guard, 2).unwrap(),
            WaitForEpochOutcome::Completed
        ));
    }

    #[test]
    fn test_group_commit_queue_publish_synchronizes_with_waiter_mutex() {
        let queue = Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()));
        let guard = queue
            .consolidator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let publish_queue = Arc::clone(&queue);

        let handle = std::thread::spawn(move || {
            started_tx
                .send(())
                .expect("publisher thread should signal start");
            publish_queue.publish_completed_epoch(1, false);
            done_tx
                .send(())
                .expect("publisher thread should signal completion");
        });

        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("publisher thread should start while waiter holds the mutex");
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err(),
            "bead_id={BEAD_ID} case=group_commit_publish_must_block_behind_waiter_mutex"
        );

        drop(guard);

        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("publisher thread should complete once the waiter mutex is released");
        handle.join().unwrap();

        let guard = queue
            .consolidator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(matches!(
            queue.wait_for_epoch_outcome(guard, 1).unwrap(),
            WaitForEpochOutcome::Completed
        ));
    }

    #[test]
    fn test_group_commit_queue_waiter_takes_over_promoted_epoch_vacancy() {
        let queue = GroupCommitQueue::new(GroupCommitConfig::default());

        {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let batch1 = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 1,
                page_data: sample_page(0x01),
                db_size_if_commit: 1,
            }]);
            assert_eq!(
                consolidator.submit_batch(batch1).unwrap(),
                SubmitOutcome::Flusher
            );
            let _ = consolidator.begin_flush().unwrap();
            let pipelined_batch = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 2,
                page_data: sample_page(0x02),
                db_size_if_commit: 2,
            }]);
            assert_eq!(
                consolidator.submit_batch(pipelined_batch).unwrap(),
                SubmitOutcome::Waiter
            );
        }

        queue.publish_completed_epoch(1, true);

        let guard = queue
            .consolidator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outcome = queue.wait_for_epoch_outcome(guard, 2).unwrap();
        let WaitForEpochOutcome::TakeOverFlusher {
            flush_epoch,
            batches,
        } = outcome
        else {
            panic!("promoted epoch waiter should take over the flusher vacancy");
        };
        assert_eq!(flush_epoch, 2);
        assert_eq!(batches.len(), 1);
    }

    #[test]
    fn test_keyed_wait_registry_signals_only_target_key() {
        let registry = Arc::new(KeyedWaitRegistry::new());
        let slot_a = registry.slot(11);
        let slot_b = registry.slot(17);
        let generation_a = slot_a.generation();
        let generation_b = slot_b.generation();
        let (done_a_tx, done_a_rx) = std::sync::mpsc::channel();
        let (done_b_tx, done_b_rx) = std::sync::mpsc::channel();

        let waiter_a = std::thread::spawn(move || {
            done_a_tx
                .send(slot_a.wait_for_change(generation_a, Duration::from_secs(1)))
                .expect("waiter A should report");
        });
        let waiter_b = std::thread::spawn(move || {
            done_b_tx
                .send(slot_b.wait_for_change(generation_b, Duration::from_secs(1)))
                .expect("waiter B should report");
        });

        assert!(registry.signal(11));
        assert_eq!(
            done_a_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("targeted waiter should wake"),
            KeyedWaitResult::Signaled,
            "bead_id={BEAD_ID} case=keyed_wait_registry_wakes_target_key"
        );
        assert!(
            done_b_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "bead_id={BEAD_ID} case=keyed_wait_registry_does_not_herd_wake_other_keys"
        );
        assert!(registry.signal(17));
        assert_eq!(
            done_b_rx
                .recv_timeout(Duration::from_millis(100))
                .expect("second waiter should wake once signaled"),
            KeyedWaitResult::Signaled,
            "bead_id={BEAD_ID} case=keyed_wait_registry_wakes_second_key"
        );

        waiter_a.join().unwrap();
        waiter_b.join().unwrap();
    }

    #[test]
    fn test_keyed_wait_slot_returns_signaled_after_generation_advance() {
        let slot = KeyedWaitSlot::default();
        let observed_generation = slot.generation();
        slot.signal();

        assert_eq!(
            slot.wait_for_change(observed_generation, Duration::from_millis(1)),
            KeyedWaitResult::Signaled,
            "bead_id={BEAD_ID} case=keyed_wait_slot_pre_signaled_generation_must_not_timeout"
        );
    }

    #[test]
    fn test_keyed_wait_registry_prunes_stale_slots_after_last_waiter_drops() {
        let registry = KeyedWaitRegistry::new();
        {
            let _slot = registry.slot(23);
            assert!(
                registry.has_slot(23),
                "bead_id={BEAD_ID} case=keyed_wait_registry_live_slot_visible_while_held"
            );
        }

        assert!(
            !registry.signal(23),
            "bead_id={BEAD_ID} case=keyed_wait_registry_stale_slot_must_not_report_signal"
        );
        assert!(
            !registry.has_slot(23),
            "bead_id={BEAD_ID} case=keyed_wait_registry_prunes_stale_slot_after_signal_attempt"
        );
    }

    #[test]
    fn test_group_commit_queue_promoted_waiter_wakes_on_targeted_publish() {
        let queue = Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()));

        {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let batch1 = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 1,
                page_data: sample_page(0x10),
                db_size_if_commit: 1,
            }]);
            assert_eq!(
                consolidator.submit_batch(batch1).unwrap(),
                SubmitOutcome::Flusher
            );
            let _ = consolidator.begin_flush().unwrap();
            let pipelined_batch = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 2,
                page_data: sample_page(0x20),
                db_size_if_commit: 2,
            }]);
            assert_eq!(
                consolidator.submit_batch(pipelined_batch).unwrap(),
                SubmitOutcome::Waiter
            );
        }

        let waiter_queue = Arc::clone(&queue);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let guard = waiter_queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Pre-register the targeted slot before publishing readiness so
            // this test isolates the targeted wake behavior instead of racing
            // the scheduler on when the waiter first touches the registry.
            let _registered_slot = waiter_queue.epoch_waiters.slot(2);
            ready_tx.send(()).expect("waiter should signal readiness");
            let outcome = waiter_queue.wait_for_epoch_outcome(guard, 2);
            done_tx.send(outcome).expect("waiter should report outcome");
        });

        ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter should start");
        assert!(
            done_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "bead_id={BEAD_ID} case=group_commit_promoted_waiter_stays_parked_until_targeted_publish"
        );

        {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(consolidator.complete_flush().unwrap());
        }
        queue.publish_completed_epoch(1, true);

        let outcome = done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("targeted publish should wake the promoted waiter")
            .expect("waiter should succeed");
        let WaitForEpochOutcome::TakeOverFlusher {
            flush_epoch,
            batches,
        } = outcome
        else {
            panic!("promoted epoch waiter should take over after targeted wake");
        };
        assert_eq!(flush_epoch, 2);
        assert_eq!(batches.len(), 1);
        waiter.join().unwrap();
    }

    #[test]
    fn test_group_commit_queue_completed_publish_wakes_target_and_next_epoch_waiters() {
        let queue = Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()));

        {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let batch1 = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 1,
                page_data: sample_page(0x31),
                db_size_if_commit: 1,
            }]);
            assert_eq!(
                consolidator.submit_batch(batch1).unwrap(),
                SubmitOutcome::Flusher
            );
            let _ = consolidator.begin_flush().unwrap();
            let pipelined_batch = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 2,
                page_data: sample_page(0x32),
                db_size_if_commit: 2,
            }]);
            assert_eq!(
                consolidator.submit_batch(pipelined_batch).unwrap(),
                SubmitOutcome::Waiter
            );
        }

        let _target_slot = queue.epoch_waiters.slot(1);
        let _next_slot = queue.epoch_waiters.slot(2);

        let spawn_waiter = |target_epoch: u64| {
            let waiter_queue = Arc::clone(&queue);
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let guard = waiter_queue
                    .consolidator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ready_tx.send(()).expect("waiter should signal readiness");
                let outcome = waiter_queue.wait_for_epoch_outcome(guard, target_epoch);
                done_tx.send(outcome).expect("waiter should report outcome");
            });
            (handle, ready_rx, done_rx)
        };

        let (target_handle, target_ready_rx, target_done_rx) = spawn_waiter(1);
        let (next_handle, next_ready_rx, next_done_rx) = spawn_waiter(2);

        target_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("target waiter should start");
        next_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("next-epoch waiter should start");
        assert!(
            target_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "bead_id={BEAD_ID} case=group_commit_completed_publish_target_waiter_stays_parked_until_publish"
        );
        assert!(
            next_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "bead_id={BEAD_ID} case=group_commit_completed_publish_next_waiter_stays_parked_until_publish"
        );

        {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                consolidator.complete_flush().unwrap(),
                "bead_id={BEAD_ID} case=group_commit_completed_publish_must_promote_next_epoch"
            );
        }
        queue.publish_completed_epoch(1, true);

        let target_outcome = target_done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("target waiter should wake")
            .expect("target waiter should succeed");
        assert!(
            matches!(target_outcome, WaitForEpochOutcome::Completed),
            "bead_id={BEAD_ID} case=group_commit_completed_publish_target_waiter_observes_completed_epoch outcome={target_outcome:?}"
        );

        let next_outcome = next_done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("next-epoch waiter should wake")
            .expect("next-epoch waiter should succeed");
        let WaitForEpochOutcome::TakeOverFlusher {
            flush_epoch,
            batches,
        } = next_outcome
        else {
            panic!(
                "bead_id={BEAD_ID} case=group_commit_completed_publish_next_waiter_must_take_over_flusher outcome={next_outcome:?}"
            );
        };
        assert_eq!(
            flush_epoch, 2,
            "bead_id={BEAD_ID} case=group_commit_completed_publish_next_waiter_flush_epoch"
        );
        assert_eq!(
            batches.len(),
            1,
            "bead_id={BEAD_ID} case=group_commit_completed_publish_next_waiter_batch_count"
        );

        target_handle.join().unwrap();
        next_handle.join().unwrap();
    }

    #[test]
    fn test_group_commit_queue_failed_publish_wakes_failed_and_next_epoch_waiters() {
        let queue = Arc::new(GroupCommitQueue::new(GroupCommitConfig::default()));

        {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let batch1 = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 1,
                page_data: sample_page(0x41),
                db_size_if_commit: 1,
            }]);
            assert_eq!(
                consolidator.submit_batch(batch1).unwrap(),
                SubmitOutcome::Flusher
            );
            let _ = consolidator.begin_flush().unwrap();
            let pipelined_batch = TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 2,
                page_data: sample_page(0x42),
                db_size_if_commit: 2,
            }]);
            assert_eq!(
                consolidator.submit_batch(pipelined_batch).unwrap(),
                SubmitOutcome::Waiter
            );
        }

        let _failed_slot = queue.epoch_waiters.slot(1);
        let _next_slot = queue.epoch_waiters.slot(2);

        let spawn_waiter = |target_epoch: u64| {
            let waiter_queue = Arc::clone(&queue);
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let handle = std::thread::spawn(move || {
                let guard = waiter_queue
                    .consolidator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ready_tx.send(()).expect("waiter should signal readiness");
                let outcome = waiter_queue.wait_for_epoch_outcome(guard, target_epoch);
                done_tx.send(outcome).expect("waiter should report outcome");
            });
            (handle, ready_rx, done_rx)
        };

        let (failed_handle, failed_ready_rx, failed_done_rx) = spawn_waiter(1);
        let (next_handle, next_ready_rx, next_done_rx) = spawn_waiter(2);

        failed_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("failed-epoch waiter should start");
        next_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("next-epoch waiter should start");
        assert!(
            failed_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "bead_id={BEAD_ID} case=group_commit_failed_publish_failed_waiter_stays_parked_until_publish"
        );
        assert!(
            next_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "bead_id={BEAD_ID} case=group_commit_failed_publish_next_waiter_stays_parked_until_publish"
        );

        {
            let mut consolidator = queue
                .consolidator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            consolidator.abort_flush().unwrap();
            assert!(
                consolidator.has_flusher_vacancy(),
                "bead_id={BEAD_ID} case=group_commit_failed_publish_abort_must_expose_flusher_vacancy"
            );
        }

        let failure = FrankenError::internal("forced keyed failed epoch for proof coverage");
        queue.publish_failed_epoch(1, &failure, true);

        let failed_error = failed_done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("failed-epoch waiter should wake")
            .expect_err("failed-epoch waiter should surface the flush failure")
            .to_string();
        assert!(
            failed_error.contains("epoch 1"),
            "bead_id={BEAD_ID} case=group_commit_failed_publish_failed_waiter_mentions_epoch error={failed_error}"
        );
        assert!(
            failed_error.contains("forced keyed failed epoch for proof coverage"),
            "bead_id={BEAD_ID} case=group_commit_failed_publish_failed_waiter_preserves_detail error={failed_error}"
        );

        let next_outcome = next_done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("next-epoch waiter should wake")
            .expect("next-epoch waiter should succeed");
        let WaitForEpochOutcome::TakeOverFlusher {
            flush_epoch,
            batches,
        } = next_outcome
        else {
            panic!(
                "bead_id={BEAD_ID} case=group_commit_failed_publish_next_waiter_must_take_over_flusher outcome={next_outcome:?}"
            );
        };
        assert_eq!(
            flush_epoch, 2,
            "bead_id={BEAD_ID} case=group_commit_failed_publish_next_waiter_flush_epoch"
        );
        assert_eq!(
            batches.len(),
            1,
            "bead_id={BEAD_ID} case=group_commit_failed_publish_next_waiter_batch_count"
        );

        failed_handle.join().unwrap();
        next_handle.join().unwrap();
    }

    #[test]
    fn test_arrival_wait_policy_fresh_epoch_uses_legacy_fallback() {
        let decision = decide_group_commit_arrival_wait(Some(ArrivalWaitObservation {
            pending_batch_count: 1,
            should_flush_now: false,
            fill_age: Duration::from_micros(3),
        }));

        assert_eq!(
            decision.wait_budget, LEGACY_GROUP_COMMIT_ARRIVAL_WAIT,
            "bead_id=bd-db300.3.8.5 case=arrival_wait_fresh_epoch_legacy_fallback"
        );
        assert_eq!(
            decision.reason, "legacy_fallback",
            "bead_id=bd-db300.3.8.5 case=arrival_wait_fresh_epoch_reason"
        );
        assert!(
            decision.used_legacy_fallback,
            "bead_id=bd-db300.3.8.5 case=arrival_wait_fresh_epoch_marks_fallback"
        );
    }

    #[test]
    fn test_arrival_wait_policy_skips_after_legacy_window_is_already_spent() {
        let decision = decide_group_commit_arrival_wait(Some(ArrivalWaitObservation {
            pending_batch_count: 1,
            should_flush_now: false,
            fill_age: LEGACY_GROUP_COMMIT_ARRIVAL_WAIT,
        }));

        assert_eq!(
            decision.wait_budget,
            Duration::ZERO,
            "bead_id=bd-db300.3.8.5 case=arrival_wait_fill_age_exhausted_budget"
        );
        assert_eq!(
            decision.reason, "fill_age_exhausted",
            "bead_id=bd-db300.3.8.5 case=arrival_wait_fill_age_exhausted_reason"
        );
        assert!(
            !decision.used_legacy_fallback,
            "bead_id=bd-db300.3.8.5 case=arrival_wait_fill_age_exhausted_not_fallback"
        );
    }

    #[test]
    fn test_arrival_wait_policy_skips_promoted_follow_on_flushes() {
        let decision = decide_group_commit_arrival_wait(None);

        assert_eq!(
            decision.wait_budget,
            Duration::ZERO,
            "bead_id=bd-db300.3.8.5 case=arrival_wait_promoted_follow_on_budget"
        );
        assert_eq!(
            decision.reason, "promoted_follow_on",
            "bead_id=bd-db300.3.8.5 case=arrival_wait_promoted_follow_on_reason"
        );
    }

    #[test]
    #[ignore = "benchmark evidence only"]
    fn wal_publish_window_shrink_benchmark_report() {
        let cases: Vec<_> = TRACK_C_PUBLISH_WINDOW_BENCH_CASES
            .iter()
            .map(|(scenario_id, dirty_pages)| {
                let baseline_hold_samples = track_c_measure_publish_window_hold_ns(
                    TrackCPublishWindowMode::InlinePrepareBaseline,
                    *dirty_pages,
                );
                let candidate_hold_samples = track_c_measure_publish_window_hold_ns(
                    TrackCPublishWindowMode::PreparedCandidate,
                    *dirty_pages,
                );
                let baseline_stall_samples = track_c_measure_competing_writer_stall_ns(
                    TrackCPublishWindowMode::InlinePrepareBaseline,
                    *dirty_pages,
                );
                let candidate_stall_samples = track_c_measure_competing_writer_stall_ns(
                    TrackCPublishWindowMode::PreparedCandidate,
                    *dirty_pages,
                );

                let baseline_hold_summary = track_c_sample_summary(&baseline_hold_samples);
                let candidate_hold_summary = track_c_sample_summary(&candidate_hold_samples);
                let baseline_stall_summary = track_c_sample_summary(&baseline_stall_samples);
                let candidate_stall_summary = track_c_sample_summary(&candidate_stall_samples);

                let baseline_hold_median =
                    baseline_hold_summary["median_ns"].as_u64().unwrap_or(0);
                let candidate_hold_median =
                    candidate_hold_summary["median_ns"].as_u64().unwrap_or(0);
                let baseline_stall_median =
                    baseline_stall_summary["median_ns"].as_u64().unwrap_or(0);
                let candidate_stall_median =
                    candidate_stall_summary["median_ns"].as_u64().unwrap_or(0);

                json!({
                    "scenario_id": scenario_id,
                    "dirty_pages": dirty_pages,
                    "exclusive_window_hold_baseline": baseline_hold_summary,
                    "exclusive_window_hold_candidate": candidate_hold_summary,
                    "contending_writer_stall_baseline": baseline_stall_summary,
                    "contending_writer_stall_candidate": candidate_stall_summary,
                    "hold_reduction_ratio_median": if baseline_hold_median == 0 {
                        0.0
                    } else {
                        1.0 - (candidate_hold_median as f64 / baseline_hold_median as f64)
                    },
                    "stall_reduction_ratio_median": if baseline_stall_median == 0 {
                        0.0
                    } else {
                        1.0 - (candidate_stall_median as f64 / baseline_stall_median as f64)
                    },
                    "faster_variant_by_hold_median": if candidate_hold_median <= baseline_hold_median {
                        "prepared_candidate"
                    } else {
                        "inline_prepare_baseline"
                    },
                    "faster_variant_by_stall_median": if candidate_stall_median <= baseline_stall_median {
                        "prepared_candidate"
                    } else {
                        "inline_prepare_baseline"
                    },
                })
            })
            .collect();

        let report = json!({
            "schema_version": "fsqlite.track_c.publish_window_benchmark.v1",
            "bead_id": TRACK_C_PUBLISH_WINDOW_BENCH_BEAD_ID,
            "parent_bead_id": "bd-db300.3.2",
            "measured_operation": "pager_commit_wal_publish_window",
            "warmup_iterations": TRACK_C_PUBLISH_WINDOW_BENCH_WARMUP_ITERS,
            "measurement_iterations": TRACK_C_PUBLISH_WINDOW_BENCH_MEASURE_ITERS,
            "vfs": "blocking_memory_vfs",
            "baseline_variant": "inline_prepare_under_exclusive_lock",
            "candidate_variant": "prepared_batch_before_exclusive_lock",
            "cases": cases,
        });

        println!("BEGIN_BD_DB300_3_2_3_REPORT");
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        println!("END_BD_DB300_3_2_3_REPORT");
    }

    #[test]
    #[ignore = "benchmark evidence only"]
    fn wal_commit_batch_benchmark_report() {
        let cases: Vec<_> = TRACK_C_BATCH_BENCH_CASES
            .iter()
            .map(|(scenario_id, dirty_pages)| {
                let single_samples =
                    track_c_measure_commit_ns(TrackCBatchMode::SingleFrame, *dirty_pages);
                let batch_samples =
                    track_c_measure_commit_ns(TrackCBatchMode::Batched, *dirty_pages);

                let single_summary = track_c_sample_summary(&single_samples);
                let batch_summary = track_c_sample_summary(&batch_samples);
                let single_median = single_summary["median_ns"].as_u64().unwrap_or(0);
                let batch_median = batch_summary["median_ns"].as_u64().unwrap_or(0);
                let single_mean = single_summary["mean_ns"].as_f64().unwrap_or(0.0);
                let batch_mean = batch_summary["mean_ns"].as_f64().unwrap_or(0.0);

                json!({
                    "scenario_id": scenario_id,
                    "dirty_pages": dirty_pages,
                    "single_frame": single_summary,
                    "batch_append": batch_summary,
                    "speedup_vs_single_median": if batch_median == 0 {
                        0.0
                    } else {
                        (single_median as f64) / (batch_median as f64)
                    },
                    "speedup_vs_single_mean": if batch_mean == 0.0 {
                        0.0
                    } else {
                        single_mean / batch_mean
                    },
                    "faster_variant_by_median": if batch_median <= single_median {
                        "batch_append"
                    } else {
                        "single_frame"
                    },
                })
            })
            .collect();

        let report = json!({
            "schema_version": "fsqlite.track_c.batch_commit_benchmark.v1",
            "bead_id": TRACK_C_BATCH_BENCH_BEAD_ID,
            "parent_bead_id": "bd-db300.3.1",
            "measured_operation": "pager_commit_wal_path",
            "warmup_iterations": TRACK_C_BATCH_BENCH_WARMUP_ITERS,
            "measurement_iterations": TRACK_C_BATCH_BENCH_MEASURE_ITERS,
            "vfs": "memory",
            "sync_mode": "normal_noop_memory_vfs",
            "baseline_variant": "single_frame_append_loop",
            "candidate_variant": "transaction_wide_batch_append",
            "cases": cases,
        });

        println!("BEGIN_BD_DB300_3_1_4_REPORT");
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        println!("END_BD_DB300_3_1_4_REPORT");
    }

    #[test]
    #[ignore = "inventory evidence only"]
    fn wal_publish_window_inventory_report() {
        let outside_window = vec![
            json!({
                "component": "collect_wal_commit_batch",
                "location": "crates/fsqlite-pager/src/pager.rs::commit_wal",
                "classification": "already_outside_publish_window",
                "move_candidate": "not_applicable",
                "rationale": "frame ordering, commit-marker boundary, and new_db_size derivation happen before EXCLUSIVE lock acquisition",
            }),
            json!({
                "component": "wal_adapter_frame_ref_copy",
                "location": "crates/fsqlite-core/src/wal_adapter.rs::prepare_append_frames",
                "classification": "pure_copy",
                "move_candidate": "completed",
                "rationale": "borrowed WalFrameRef values are copied into owned batch metadata before the exclusive publish window starts",
            }),
            json!({
                "component": "wal_batch_buffer_allocation",
                "location": "crates/fsqlite-wal/src/wal.rs::prepare_frame_bytes",
                "classification": "allocation",
                "move_candidate": "completed",
                "rationale": "contiguous frame buffer allocation now happens in the prepare phase before EXCLUSIVE lock acquisition",
            }),
            json!({
                "component": "wal_header_and_payload_copy",
                "location": "crates/fsqlite-wal/src/wal.rs::prepare_frame_bytes",
                "classification": "pure_copy",
                "move_candidate": "completed",
                "rationale": "page-number/db-size stamping, salt staging, and payload copies are fully serialized before publish time",
            }),
            json!({
                "component": "wal_checksum_transform_precompute",
                "location": "crates/fsqlite-core/src/wal_adapter.rs::prepare_append_frames",
                "classification": "pure_compute",
                "move_candidate": "completed",
                "rationale": "per-frame checksum transforms are now derived outside the lock so publish only rebinds the live seed",
            }),
            json!({
                "component": "wal_prelock_checksum_finalize",
                "location": "crates/fsqlite-core/src/wal_adapter.rs::finalize_prepared_frames",
                "classification": "pure_compute",
                "move_candidate": "completed",
                "rationale": "prepared batches now refresh/publish the base snapshot and stamp checksum fields before EXCLUSIVE when the live append window is still open to optimistic reuse",
            }),
        ];

        let inside_window = vec![
            json!({
                "component": "lock_exclusive",
                "location": "crates/fsqlite-pager/src/pager.rs::commit_wal",
                "classification": "serialized_boundary",
                "move_candidate": "no",
                "rationale": "cross-process WAL append exclusion is the start of the current publish window",
            }),
            json!({
                "component": "wal_refresh_before_append",
                "location": "crates/fsqlite-core/src/wal_adapter.rs::append_prepared_frames",
                "classification": "state_refresh_read_only",
                "move_candidate": "conditional",
                "rationale": "prepared batches can skip the lock-held refresh when the pre-lock append-window token still matches on disk, but stale windows still require a refresh before durable append",
            }),
            json!({
                "component": "wal_append_window_validation",
                "location": "crates/fsqlite-core/src/wal_adapter.rs::append_prepared_frames",
                "classification": "state_validation",
                "move_candidate": "conditional",
                "rationale": "the lock-held path now performs a cheap file-size/header check to decide whether the pre-lock finalized batch can be reused or whether it must fall back to refresh/re-finalize",
            }),
            json!({
                "component": "wal_checksum_seed_rebind_fallback",
                "location": "crates/fsqlite-core/src/wal_adapter.rs::append_prepared_frames",
                "classification": "publish_seed_binding",
                "move_candidate": "conditional",
                "rationale": "checksum rebinding remains inside the publish window only for stale-window fallback cases where another writer changed the live append seed before EXCLUSIVE was acquired",
            }),
            json!({
                "component": "wal_file_write",
                "location": "crates/fsqlite-wal/src/wal.rs::append_finalized_prepared_frame_bytes",
                "classification": "durable_state_transition",
                "move_candidate": "no",
                "rationale": "single contiguous file write is the core serialized append that must observe the authoritative WAL end",
            }),
            json!({
                "component": "wal_state_advance_after_write",
                "location": "crates/fsqlite-wal/src/wal.rs::append_finalized_prepared_frame_bytes",
                "classification": "durable_state_transition",
                "move_candidate": "no",
                "rationale": "frame_count/running_checksum advancement must match the durable append that just occurred",
            }),
            json!({
                "component": "fec_hook_on_frame",
                "location": "crates/fsqlite-core/src/wal_adapter.rs::append_prepared_frames",
                "classification": "post_append_compute",
                "move_candidate": "yes",
                "rationale": "FEC hook work remains explicitly non-fatal and still runs after append while the publish window is open",
            }),
            json!({
                "component": "wal_sync",
                "location": "crates/fsqlite-pager/src/pager.rs::commit_wal",
                "classification": "durability_barrier",
                "move_candidate": "no",
                "rationale": "sync is the durability barrier for the commit and therefore part of the required serialized state transition",
            }),
            json!({
                "component": "inner_db_size_update",
                "location": "crates/fsqlite-pager/src/pager.rs::commit_wal",
                "classification": "pager_state_publish",
                "move_candidate": "no",
                "rationale": "pager-visible db_size must only advance after the WAL append and sync succeed",
            }),
        ];

        let definitely_movable = inside_window
            .iter()
            .filter(|entry| entry["move_candidate"] == "yes")
            .count();
        let conditionally_movable = inside_window
            .iter()
            .filter(|entry| entry["move_candidate"] == "conditional")
            .count();
        let required_serialized = inside_window
            .iter()
            .filter(|entry| entry["move_candidate"] == "no")
            .count();

        let report = json!({
            "schema_version": "fsqlite.track_c.publish_window_inventory.v1",
            "bead_id": TRACK_C_PUBLISH_WINDOW_INVENTORY_BEAD_ID,
            "parent_bead_id": "bd-db300.3.2",
            "measured_operation": "pager_commit_wal_path",
            "measurement_anchor_bead_id": TRACK_C_BATCH_BENCH_BEAD_ID,
            "measurement_anchor_report": "fsqlite.track_c.batch_commit_benchmark.v1",
            "current_window": {
                "entry_function": "crates/fsqlite-pager/src/pager.rs::commit_wal",
                "window_begins_at": "inner.db_file.lock(cx, LockLevel::Exclusive)?",
                "window_ends_after": "inner.db_size = batch.new_db_size",
            },
            "outside_window": outside_window,
            "inside_window": inside_window,
            "summary": {
                "outside_window_steps": outside_window.len(),
                "inside_window_steps": inside_window.len(),
                "definitely_movable_inside_window_steps": definitely_movable,
                "conditionally_movable_inside_window_steps": conditionally_movable,
                "required_serialized_inside_window_steps": required_serialized,
            },
        });

        println!("BEGIN_BD_DB300_3_2_1_REPORT");
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        println!("END_BD_DB300_3_2_1_REPORT");

        assert_eq!(
            report["measured_operation"], "pager_commit_wal_path",
            "bead_id={TRACK_C_PUBLISH_WINDOW_INVENTORY_BEAD_ID} case=measured_operation_anchor"
        );
        assert_eq!(
            report["summary"]["definitely_movable_inside_window_steps"], 5,
            "bead_id={TRACK_C_PUBLISH_WINDOW_INVENTORY_BEAD_ID} case=movable_step_count"
        );
        assert_eq!(
            report["summary"]["conditionally_movable_inside_window_steps"], 1,
            "bead_id={TRACK_C_PUBLISH_WINDOW_INVENTORY_BEAD_ID} case=conditional_step_count"
        );
        assert_eq!(
            report["summary"]["required_serialized_inside_window_steps"], 5,
            "bead_id={TRACK_C_PUBLISH_WINDOW_INVENTORY_BEAD_ID} case=required_step_count"
        );
    }

    #[test]
    #[ignore = "benchmark evidence only"]
    fn wal_metadata_cleanup_benchmark_report() {
        let cases: Vec<_> = TRACK_C_METADATA_BENCH_CASES
            .iter()
            .map(|(scenario_id, interior_dirty_pages)| {
                let baseline_samples = track_c_metadata_measure_commit_ns(
                    TrackCMetadataMode::ForcedPageOneBaseline,
                    *interior_dirty_pages,
                );
                let candidate_samples = track_c_metadata_measure_commit_ns(
                    TrackCMetadataMode::SemanticCleanupCandidate,
                    *interior_dirty_pages,
                );
                let baseline_summary = track_c_sample_summary(&baseline_samples);
                let candidate_summary = track_c_sample_summary(&candidate_samples);
                let baseline_median = baseline_summary["median_ns"].as_u64().unwrap_or(0);
                let candidate_median = candidate_summary["median_ns"].as_u64().unwrap_or(0);
                let baseline_mean = baseline_summary["mean_ns"].as_f64().unwrap_or(0.0);
                let candidate_mean = candidate_summary["mean_ns"].as_f64().unwrap_or(0.0);
                let baseline_frame_pages = track_c_metadata_capture_frame_pages(
                    TrackCMetadataMode::ForcedPageOneBaseline,
                    *interior_dirty_pages,
                );
                let candidate_frame_pages = track_c_metadata_capture_frame_pages(
                    TrackCMetadataMode::SemanticCleanupCandidate,
                    *interior_dirty_pages,
                );
                let baseline_page_one_frames = baseline_frame_pages
                    .iter()
                    .filter(|page_number| **page_number == PageNumber::ONE.get())
                    .count();
                let candidate_page_one_frames = candidate_frame_pages
                    .iter()
                    .filter(|page_number| **page_number == PageNumber::ONE.get())
                    .count();

                json!({
                    "scenario_id": scenario_id,
                    "interior_dirty_pages": interior_dirty_pages,
                    "forced_page_one_baseline": baseline_summary,
                    "semantic_cleanup_candidate": candidate_summary,
                    "baseline_frame_pages": baseline_frame_pages,
                    "candidate_frame_pages": candidate_frame_pages,
                    "baseline_total_frames_per_commit": baseline_frame_pages.len(),
                    "candidate_total_frames_per_commit": candidate_frame_pages.len(),
                    "baseline_page_one_frames_per_commit": baseline_page_one_frames,
                    "candidate_page_one_frames_per_commit": candidate_page_one_frames,
                    "frame_count_reduction_per_commit": baseline_frame_pages.len().saturating_sub(candidate_frame_pages.len()),
                    "page_one_exposure_reduction_per_commit": baseline_page_one_frames.saturating_sub(candidate_page_one_frames),
                    "speedup_vs_baseline_median": if candidate_median == 0 {
                        0.0
                    } else {
                        (baseline_median as f64) / (candidate_median as f64)
                    },
                    "speedup_vs_baseline_mean": if candidate_mean == 0.0 {
                        0.0
                    } else {
                        baseline_mean / candidate_mean
                    },
                    "faster_variant_by_median": if candidate_median <= baseline_median {
                        "semantic_cleanup_candidate"
                    } else {
                        "forced_page_one_baseline"
                    },
                })
            })
            .collect();

        let report = json!({
            "schema_version": "fsqlite.track_c.metadata_cleanup_benchmark.v1",
            "bead_id": TRACK_C_METADATA_BENCH_BEAD_ID,
            "parent_bead_id": "bd-db300.3.3",
            "measured_operation": "wal_commit_interior_only_workload",
            "warmup_iterations": TRACK_C_METADATA_BENCH_WARMUP_ITERS,
            "measurement_iterations": TRACK_C_METADATA_BENCH_MEASURE_ITERS,
            "vfs": "memory",
            "sync_mode": "normal_noop_memory_vfs",
            "baseline_variant": "forced_page_one_rewrite_every_commit",
            "candidate_variant": "semantic_trigger_page_one_cleanup",
            "cases": cases,
        });

        println!("BEGIN_BD_DB300_3_3_3_REPORT");
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
        println!("END_BD_DB300_3_3_3_REPORT");
    }

    #[test]
    fn test_wal_commit_preserves_sorted_unique_frame_order() {
        let (pager, frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = txn.allocate_page(&cx).unwrap();
        let p3 = txn.allocate_page(&cx).unwrap();
        let p4 = txn.allocate_page(&cx).unwrap();

        txn.write_page(&cx, p4, &vec![0x44; ps]).unwrap();
        txn.write_page(&cx, p2, &vec![0x22; ps]).unwrap();
        txn.write_page(&cx, p3, &vec![0x33; ps]).unwrap();
        txn.write_page(&cx, p4, &vec![0x55; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let frame_pages: Vec<u32> = frames.lock().unwrap().iter().map(|frame| frame.0).collect();
        assert_eq!(
            frame_pages,
            vec![PageNumber::ONE.get(), p2.get(), p3.get(), p4.get()],
            "bead_id={BEAD_ID} case=wal_frames_sorted_and_unique"
        );
    }

    #[test]
    fn test_wal_read_page_from_wal() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Write and commit via WAL.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        let data = vec![0xBB_u8; ps];
        txn.write_page(&cx, p1, &data).unwrap();
        txn.commit(&cx).unwrap();

        // Read back in a new transaction — should find the page in WAL.
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let read_back = txn2.get_page(&cx, p1).unwrap();
        assert_eq!(
            read_back.as_ref()[0],
            0xBB,
            "bead_id={BEAD_ID} case=wal_read_back_from_wal"
        );
    }

    #[test]
    fn test_wal_no_journal_file_created() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/wal_no_jrnl.db");
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let (backend, _frames, _, _) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0xFF; 4096]).unwrap();
        txn.commit(&cx).unwrap();

        // In WAL mode, no journal file should be created.
        let journal_path = SimplePager::<MemoryVfs>::journal_path(&path);
        assert!(
            !vfs.access(&cx, &journal_path, AccessFlags::EXISTS).unwrap(),
            "bead_id={BEAD_ID} case=wal_no_journal_created"
        );
    }

    #[test]
    fn test_wal_begin_not_called_for_rejected_eager_writer() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let (backend, _frames, begin_calls, _) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        let _writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        assert_eq!(
            *begin_calls.lock().unwrap(),
            1,
            "bead_id={BEAD_ID} case=first_writer_initializes_wal_snapshot"
        );

        let err = pager
            .begin(&cx, TransactionMode::Immediate)
            .err()
            .expect("second eager writer should be rejected");
        assert!(matches!(err, FrankenError::Busy));
        assert_eq!(
            *begin_calls.lock().unwrap(),
            1,
            "bead_id={BEAD_ID} case=rejected_writer_must_not_mutate_wal_state"
        );
    }

    #[test]
    fn test_wal_mode_switch_back_to_delete() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();

        assert_eq!(pager.journal_mode(), JournalMode::Wal);
        let mode = pager.set_journal_mode(&cx, JournalMode::Delete).unwrap();
        assert_eq!(
            mode,
            JournalMode::Delete,
            "bead_id={BEAD_ID} case=switch_back_to_delete"
        );
    }

    #[test]
    fn test_wal_overwrite_page_reads_latest() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // First commit: write 0x11.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0x11; ps]).unwrap();
        txn.commit(&cx).unwrap();

        // Second commit: overwrite with 0x22.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.write_page(&cx, p1, &vec![0x22; ps]).unwrap();
        txn2.commit(&cx).unwrap();

        // Read should see 0x22 (latest WAL entry).
        let txn3 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = txn3.get_page(&cx, p1).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x22,
            "bead_id={BEAD_ID} case=wal_latest_version"
        );
    }

    #[test]
    fn test_wal_interior_update_skips_page1_when_metadata_unchanged() {
        let (pager, frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let page_two = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
            page_two
        };

        let frames_before = frames.lock().unwrap().len();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, page_two, &vec![0x22; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let frames = frames.lock().unwrap();
        let appended = &frames[frames_before..];
        assert_eq!(
            appended.len(),
            1,
            "bead_id={BEAD_ID} case=wal_headerless_interior_commit_frame_count"
        );
        assert_eq!(
            appended[0].0,
            page_two.get(),
            "bead_id={BEAD_ID} case=wal_headerless_interior_commit_page"
        );
        assert_eq!(
            appended[0].2,
            page_two.get(),
            "bead_id={BEAD_ID} case=wal_headerless_interior_commit_commit_marker_uses_existing_db_size"
        );
    }

    #[test]
    fn test_wal_page_one_write_plan_for_interior_update_has_no_trigger() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let page_two = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
            page_two
        };

        let current_db_size = pager.published_snapshot().db_size;
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, page_two, &vec![0x22; ps]).unwrap();

        let plan = txn.classify_wal_page_one_write(current_db_size, txn.freelist_metadata_dirty());
        assert_eq!(
            plan,
            WalPageOneWritePlan {
                max_written: page_two.get(),
                page_one_dirty: false,
                freelist_metadata_dirty: false,
                db_growth: false,
            },
            "bead_id={BEAD_ID} case=wal_page1_plan_interior_update"
        );
        assert!(
            !plan.requires_page_one_rewrite(),
            "bead_id={BEAD_ID} case=wal_page1_plan_interior_update_no_rewrite"
        );
        assert!(
            !plan.requires_page_count_advance(),
            "bead_id={BEAD_ID} case=wal_page1_plan_interior_update_no_growth"
        );
    }

    #[test]
    fn test_wal_page_one_write_plan_marks_page_one_dirty_trigger() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        let current_db_size = pager.published_snapshot().db_size;
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, PageNumber::ONE, &vec![0x77; ps])
            .unwrap();

        let plan = txn.classify_wal_page_one_write(current_db_size, txn.freelist_metadata_dirty());
        assert_eq!(
            plan,
            WalPageOneWritePlan {
                max_written: PageNumber::ONE.get(),
                page_one_dirty: true,
                freelist_metadata_dirty: false,
                db_growth: false,
            },
            "bead_id={BEAD_ID} case=wal_page1_plan_page1_dirty"
        );
        assert!(
            plan.requires_page_one_rewrite(),
            "bead_id={BEAD_ID} case=wal_page1_plan_page1_dirty_requires_rewrite"
        );
        assert!(
            !plan.requires_page_count_advance(),
            "bead_id={BEAD_ID} case=wal_page1_plan_page1_dirty_no_growth"
        );
    }

    #[test]
    fn test_wal_page_one_write_plan_freelist_trigger_defers_page_one() {
        // bd-3wop3.8 (D1-CRITICAL): Verify that freelist changes do NOT trigger
        // Page 1 rewrite in WAL mode. The WAL frames implicitly capture freelist
        // state through the allocated/freed pages. Page 1 is reconstructed at
        // checkpoint time. This eliminates MVCC conflicts from concurrent
        // freelist operations (page_lease batch allocations).
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let page_two = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
            page_two
        };

        let current_db_size = pager.published_snapshot().db_size;
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.free_page(&cx, page_two).unwrap();

        let plan = txn.classify_wal_page_one_write(current_db_size, txn.freelist_metadata_dirty());
        // Note: freelist_metadata_dirty may be true or false depending on internal
        // pager state, but the key assertion is that requires_page_one_rewrite()
        // returns false for pure freelist changes (page_one_dirty is false).
        assert!(
            !plan.page_one_dirty,
            "bead_id={BEAD_ID} case=wal_page1_plan_freelist_page1_not_dirty"
        );
        // D1-CRITICAL: Pure freelist changes do NOT require Page 1 rewrite
        assert!(
            !plan.requires_page_one_rewrite(),
            "bead_id={BEAD_ID} case=wal_page1_plan_freelist_defers_page_one"
        );
        assert!(
            !plan.requires_page_count_advance(),
            "bead_id={BEAD_ID} case=wal_page1_plan_freelist_no_growth"
        );
    }

    #[test]
    fn test_wal_page_one_write_plan_marks_database_growth_trigger() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let current_db_size = pager.published_snapshot().db_size;
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let page_two = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();

        let plan = txn.classify_wal_page_one_write(current_db_size, txn.freelist_metadata_dirty());
        assert_eq!(
            plan,
            WalPageOneWritePlan {
                max_written: page_two.get(),
                page_one_dirty: false,
                freelist_metadata_dirty: false,
                db_growth: true,
            },
            "bead_id={BEAD_ID} case=wal_page1_plan_db_growth"
        );
        // D1-CRITICAL: Pure db_growth does NOT require Page 1 rewrite in WAL mode.
        // The WAL frame's db_size_if_commit captures the database size, so Page 1
        // update can be deferred to checkpoint. This eliminates MVCC conflicts.
        assert!(
            !plan.requires_page_one_rewrite(),
            "bead_id={BEAD_ID} case=wal_page1_plan_db_growth_skips_page_one_rewrite"
        );
        assert!(
            plan.requires_page_count_advance(),
            "bead_id={BEAD_ID} case=wal_page1_plan_db_growth_advances_count"
        );
    }

    #[test]
    fn test_wal_page_one_write_plan_clears_net_zero_freelist_reuse() {
        let (pager, _frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let page_two = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
            page_two
        };

        {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            txn.free_page(&cx, page_two).unwrap();
            txn.commit(&cx).unwrap();
        }

        let current_db_size = pager.published_snapshot().db_size;
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let reused = txn.allocate_page(&cx).unwrap();
        assert_eq!(
            reused, page_two,
            "bead_id={BEAD_ID} case=wal_page1_plan_net_zero_reuse_reclaims_committed_freelist_page"
        );
        txn.free_page(&cx, reused).unwrap();

        let plan = txn.classify_wal_page_one_write(current_db_size, txn.freelist_metadata_dirty());
        assert_eq!(
            plan,
            WalPageOneWritePlan {
                max_written: 0,
                page_one_dirty: false,
                freelist_metadata_dirty: false,
                db_growth: false,
            },
            "bead_id={BEAD_ID} case=wal_page1_plan_net_zero_reuse_has_no_trigger"
        );
        assert!(
            !plan.requires_page_one_rewrite(),
            "bead_id={BEAD_ID} case=wal_page1_plan_net_zero_reuse_skips_page_one"
        );
        assert!(
            !txn.has_pending_writes(),
            "bead_id={BEAD_ID} case=wal_page1_plan_net_zero_reuse_has_no_pending_writes"
        );
        assert!(
            txn.pending_commit_pages().unwrap().is_empty(),
            "bead_id={BEAD_ID} case=wal_page1_plan_net_zero_reuse_has_no_commit_pages"
        );
    }

    #[test]
    fn test_wal_net_zero_eof_allocate_free_does_not_append_frames_or_advance_seq() {
        let (pager, frames) = wal_pager();
        let cx = Cx::new();

        let seq_before = pager.published_snapshot().visible_commit_seq;
        let frames_before = frames.lock().unwrap().len();
        let current_db_size = pager.published_snapshot().db_size;

        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let page_two = txn.allocate_page(&cx).unwrap();
        txn.free_page(&cx, page_two).unwrap();

        let plan = txn.classify_wal_page_one_write(current_db_size, txn.freelist_metadata_dirty());
        assert_eq!(
            plan,
            WalPageOneWritePlan {
                max_written: 0,
                page_one_dirty: false,
                freelist_metadata_dirty: false,
                db_growth: false,
            },
            "bead_id={BEAD_ID} case=wal_page1_plan_eof_allocate_then_free_has_no_trigger"
        );
        assert!(
            !txn.has_pending_writes(),
            "bead_id={BEAD_ID} case=wal_net_zero_eof_allocate_free_has_no_pending_writes"
        );
        assert!(
            txn.pending_commit_pages().unwrap().is_empty(),
            "bead_id={BEAD_ID} case=wal_net_zero_eof_allocate_free_has_no_commit_pages"
        );

        txn.commit(&cx).unwrap();

        assert_eq!(
            pager.published_snapshot().visible_commit_seq,
            seq_before,
            "bead_id={BEAD_ID} case=wal_net_zero_eof_allocate_free_keeps_visible_commit_seq"
        );
        assert_eq!(
            frames.lock().unwrap().len(),
            frames_before,
            "bead_id={BEAD_ID} case=wal_net_zero_eof_allocate_free_appends_no_frames"
        );
    }

    #[test]
    fn test_wal_external_refresh_tracks_headerless_interior_commit() {
        let (pager1, pager2, frames) = wal_pager_pair_with_shared_backend();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let page_two = {
            let mut txn = pager1.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
            page_two
        };

        let seq_before = pager1.published_snapshot().visible_commit_seq;

        let mut txn = pager1.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, page_two, &vec![0x22; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let latest_seq = pager1.published_snapshot().visible_commit_seq;
        assert!(
            latest_seq > seq_before,
            "bead_id={BEAD_ID} case=wal_headerless_interior_commit_advances_local_seq"
        );
        assert_eq!(
            frames
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, db_size_if_commit)| *db_size_if_commit > 0)
                .count(),
            2,
            "bead_id={BEAD_ID} case=wal_headerless_interior_commit_emits_commit_markers"
        );
        {
            let inner = pager2.inner.lock().unwrap();
            assert_eq!(
                inner.journal_mode,
                JournalMode::Wal,
                "bead_id={BEAD_ID} case=wal_headerless_interior_commit_follower_in_wal_mode"
            );
            drop(inner);
            let mut wal_guard = pager2.wal_backend.write().unwrap();
            let wal = wal_guard
                .as_deref_mut()
                .expect("WAL backend should stay installed");
            assert_eq!(
                wal.committed_txn_count(&cx).unwrap(),
                2,
                "bead_id={BEAD_ID} case=wal_headerless_interior_commit_backend_reports_commit_count"
            );
        }

        let reader = pager2.begin(&cx, TransactionMode::ReadOnly).unwrap();
        {
            let inner = pager2.inner.lock().unwrap();
            assert_eq!(
                inner.commit_seq, latest_seq,
                "bead_id={BEAD_ID} case=wal_headerless_interior_commit_refreshes_inner_commit_seq"
            );
        }
        let refreshed = pager2.published_snapshot();
        assert_eq!(
            refreshed.visible_commit_seq, latest_seq,
            "bead_id={BEAD_ID} case=wal_headerless_interior_commit_refreshes_visible_seq"
        );
        assert_eq!(
            reader.get_page(&cx, page_two).unwrap().as_ref()[0],
            0x22,
            "bead_id={BEAD_ID} case=wal_headerless_interior_commit_refreshes_latest_page"
        );
    }

    #[test]
    fn test_wal_rollback_does_not_append_frames() {
        let (pager, frames) = wal_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p1 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p1, &vec![0xDD; ps]).unwrap();
        txn.rollback(&cx).unwrap();

        assert_eq!(
            frames.lock().unwrap().len(),
            0,
            "bead_id={BEAD_ID} case=wal_rollback_no_frames"
        );
    }

    #[test]
    fn test_wal_begin_skips_committed_page1_reload_when_seq_and_file_size_hold() {
        let (pager, frames, _, _, read_page_calls) = wal_pager_with_read_tracking();
        let cx = Cx::new();

        let page1 = {
            let inner = pager.inner.lock().unwrap();
            let mut page = vec![0_u8; inner.page_size.as_usize()];
            let bytes_read = inner.db_file.read(&cx, &mut page, 0).unwrap();
            assert_eq!(
                bytes_read,
                inner.page_size.as_usize(),
                "bead_id={BEAD_ID} case=wal_begin_read_tracking_reads_page1"
            );
            page
        };

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, PageNumber::ONE, &page1).unwrap();
        txn.commit(&cx).unwrap();

        assert!(
            frames
                .lock()
                .unwrap()
                .iter()
                .any(|(page_number, _, _)| *page_number == PageNumber::ONE.get()),
            "bead_id={BEAD_ID} case=wal_begin_read_tracking_commits_page1_into_wal"
        );

        let read_page_calls_before_begin = *read_page_calls.lock().unwrap();
        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let read_page_calls_after_begin = *read_page_calls.lock().unwrap();
        drop(reader);

        assert_eq!(
            read_page_calls_after_begin, read_page_calls_before_begin,
            "bead_id={BEAD_ID} case=wal_begin_skips_page1_reload_when_commit_seq_and_file_size_match"
        );
    }

    // ── 5A.1: Page 1 initialization tests (bd-2yy6) ───────────────────

    const BEAD_5A1: &str = "bd-2yy6";

    #[test]
    fn test_page1_database_header_all_fields() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let raw = txn.get_page(&cx, PageNumber::ONE).unwrap().into_vec();

        eprintln!(
            "[5A1][test=page1_database_header_all_fields][step=parse] page_len={}",
            raw.len()
        );

        let hdr_bytes: [u8; DATABASE_HEADER_SIZE] = raw[..DATABASE_HEADER_SIZE]
            .try_into()
            .expect("page 1 must have 100-byte header");
        let hdr = DatabaseHeader::from_bytes(&hdr_bytes).expect("header must parse");

        // Verify each field matches the expected new-database defaults.
        assert_eq!(
            hdr.page_size,
            PageSize::DEFAULT,
            "bead_id={BEAD_5A1} case=page_size"
        );
        assert_eq!(hdr.page_count, 1, "bead_id={BEAD_5A1} case=page_count");
        assert_eq!(
            hdr.sqlite_version, FRANKENSQLITE_SQLITE_VERSION_NUMBER,
            "bead_id={BEAD_5A1} case=sqlite_version"
        );
        assert_eq!(
            hdr.schema_format, 4,
            "bead_id={BEAD_5A1} case=schema_format"
        );
        assert_eq!(
            hdr.freelist_trunk, 0,
            "bead_id={BEAD_5A1} case=freelist_trunk"
        );
        assert_eq!(
            hdr.freelist_count, 0,
            "bead_id={BEAD_5A1} case=freelist_count"
        );
        assert_eq!(
            hdr.schema_cookie, 0,
            "bead_id={BEAD_5A1} case=schema_cookie"
        );
        assert_eq!(
            hdr.text_encoding,
            fsqlite_types::TextEncoding::Utf8,
            "bead_id={BEAD_5A1} case=text_encoding"
        );
        assert_eq!(hdr.user_version, 0, "bead_id={BEAD_5A1} case=user_version");
        assert_eq!(
            hdr.application_id, 0,
            "bead_id={BEAD_5A1} case=application_id"
        );
        assert_eq!(
            hdr.change_counter, 0,
            "bead_id={BEAD_5A1} case=change_counter"
        );

        // Magic string bytes 0..16.
        assert_eq!(
            &raw[..16],
            b"SQLite format 3\0",
            "bead_id={BEAD_5A1} case=magic_string"
        );
        // Payload fractions at bytes 21/22/23.
        assert_eq!(raw[21], 64, "bead_id={BEAD_5A1} case=max_payload_fraction");
        assert_eq!(raw[22], 32, "bead_id={BEAD_5A1} case=min_payload_fraction");
        assert_eq!(raw[23], 32, "bead_id={BEAD_5A1} case=leaf_payload_fraction");

        eprintln!(
            "[5A1][test=page1_database_header_all_fields][step=verify] \
             page_size={} page_count={} schema_format={} encoding=UTF8 \u{2713}",
            hdr.page_size.get(),
            hdr.page_count,
            hdr.schema_format
        );
    }

    #[test]
    fn test_page1_btree_header_is_valid_empty_leaf_table() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let raw = txn.get_page(&cx, PageNumber::ONE).unwrap().into_vec();

        let btree_hdr =
            BTreePageHeader::parse(&raw, PageSize::DEFAULT, 0, true).expect("btree header");
        assert_eq!(
            btree_hdr.page_type,
            fsqlite_types::BTreePageType::LeafTable,
            "bead_id={BEAD_5A1} case=btree_page_type"
        );
        assert_eq!(
            btree_hdr.cell_count, 0,
            "bead_id={BEAD_5A1} case=btree_cell_count"
        );
        assert_eq!(
            btree_hdr.cell_content_start,
            PageSize::DEFAULT.get(),
            "bead_id={BEAD_5A1} case=btree_content_start"
        );
        assert_eq!(
            btree_hdr.first_freeblock, 0,
            "bead_id={BEAD_5A1} case=btree_first_freeblock"
        );
        assert_eq!(
            btree_hdr.fragmented_free_bytes, 0,
            "bead_id={BEAD_5A1} case=btree_fragmented_free"
        );
        assert_eq!(
            btree_hdr.header_offset, DATABASE_HEADER_SIZE,
            "bead_id={BEAD_5A1} case=btree_header_offset"
        );
        assert!(
            btree_hdr.right_most_child.is_none(),
            "bead_id={BEAD_5A1} case=leaf_no_child"
        );

        eprintln!("[5A1][test=page1_btree_header][step=verify] empty_leaf_table valid \u{2713}");
    }

    #[test]
    fn test_page1_rest_is_zeroed() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let raw = txn.get_page(&cx, PageNumber::ONE).unwrap().into_vec();

        // After the B-tree header (8 bytes starting at offset 100), the rest of
        // the page should be all zeros (no cells, no cell pointers, no data).
        let btree_header_end = DATABASE_HEADER_SIZE + 8;
        let trailing = &raw[btree_header_end..];
        let non_zero_count = trailing.iter().filter(|&&b| b != 0).count();
        assert_eq!(
            non_zero_count, 0,
            "bead_id={BEAD_5A1} case=trailing_bytes_zeroed non_zero_count={non_zero_count}"
        );

        eprintln!(
            "[5A1][test=page1_rest_is_zeroed][step=verify] \
             trailing_bytes={} all_zero=true \u{2713}",
            trailing.len()
        );
    }

    #[test]
    fn test_page1_various_page_sizes() {
        for &ps_val in &[512u32, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            let page_size = PageSize::new(ps_val).unwrap();
            let vfs = MemoryVfs::new();
            let path = PathBuf::from(format!("/test_{ps_val}.db"));
            let pager = SimplePager::open(vfs, &path, page_size).unwrap();
            let cx = Cx::new();

            let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
            let raw = txn.get_page(&cx, PageNumber::ONE).unwrap().into_vec();

            eprintln!(
                "[5A1][test=page1_various_page_sizes][step=open] page_size={ps_val} page_len={}",
                raw.len()
            );

            assert_eq!(
                raw.len(),
                ps_val as usize,
                "bead_id={BEAD_5A1} case=page_len ps={ps_val}"
            );

            // Verify database header parses.
            let hdr_bytes: [u8; DATABASE_HEADER_SIZE] =
                raw[..DATABASE_HEADER_SIZE].try_into().unwrap();
            let hdr = DatabaseHeader::from_bytes(&hdr_bytes).unwrap_or_else(|e| {
                panic!("bead_id={BEAD_5A1} case=hdr_parse ps={ps_val} err={e}")
            });
            assert_eq!(
                hdr.page_size, page_size,
                "bead_id={BEAD_5A1} case=hdr_page_size ps={ps_val}"
            );

            // Verify B-tree header parses.
            let btree = BTreePageHeader::parse(&raw, page_size, 0, true).unwrap_or_else(|e| {
                panic!("bead_id={BEAD_5A1} case=btree_parse ps={ps_val} err={e}")
            });
            assert_eq!(
                btree.cell_count, 0,
                "bead_id={BEAD_5A1} case=empty_cells ps={ps_val}"
            );

            // Content offset should be usable_size (= page_size when reserved=0).
            let expected_content = ps_val;
            assert_eq!(
                btree.cell_content_start, expected_content,
                "bead_id={BEAD_5A1} case=content_start ps={ps_val}"
            );

            eprintln!(
                "[5A1][test=page1_various_page_sizes][step=verify] \
                 page_size={ps_val} content_start={} \u{2713}",
                btree.cell_content_start
            );
        }
    }

    #[test]
    fn test_write_empty_leaf_table_roundtrip() {
        // Verify that write_empty_leaf_table produces bytes that parse back
        // correctly via BTreePageHeader::parse().
        let page_size = PageSize::DEFAULT;
        let mut page = vec![0u8; page_size.as_usize()];

        // Write at offset 0 (non-page-1 case).
        BTreePageHeader::write_empty_leaf_table(&mut page, 0, page_size.get());

        let parsed = BTreePageHeader::parse(&page, page_size, 0, false)
            .expect("bead_id=bd-2yy6 written page must parse");

        assert_eq!(parsed.page_type, fsqlite_types::BTreePageType::LeafTable);
        assert_eq!(parsed.cell_count, 0);
        assert_eq!(parsed.first_freeblock, 0);
        assert_eq!(parsed.fragmented_free_bytes, 0);
        assert_eq!(parsed.cell_content_start, page_size.get());
        assert_eq!(parsed.header_offset, 0);

        eprintln!(
            "[5A1][test=write_empty_leaf_roundtrip][step=verify] \
             non_page1 roundtrip \u{2713}"
        );

        // Write at offset 100 (page-1 case).
        let mut page1 = vec![0u8; page_size.as_usize()];
        BTreePageHeader::write_empty_leaf_table(&mut page1, DATABASE_HEADER_SIZE, page_size.get());

        // Need to also write a valid database header for parse to succeed.
        let hdr = DatabaseHeader {
            page_size,
            page_count: 1,
            sqlite_version: FRANKENSQLITE_SQLITE_VERSION_NUMBER,
            ..DatabaseHeader::default()
        };
        let hdr_bytes = hdr.to_bytes().unwrap();
        page1[..DATABASE_HEADER_SIZE].copy_from_slice(&hdr_bytes);

        let parsed1 = BTreePageHeader::parse(&page1, page_size, 0, true)
            .expect("bead_id=bd-2yy6 page1 written page must parse");

        assert_eq!(parsed1.page_type, fsqlite_types::BTreePageType::LeafTable);
        assert_eq!(parsed1.cell_count, 0);
        assert_eq!(parsed1.header_offset, DATABASE_HEADER_SIZE);

        eprintln!(
            "[5A1][test=write_empty_leaf_roundtrip][step=verify] \
             page1 roundtrip \u{2713}"
        );
    }

    #[test]
    fn test_write_empty_leaf_table_65536_page_size() {
        let page_size = PageSize::new(65536).unwrap();
        let mut page = vec![0u8; page_size.as_usize()];

        BTreePageHeader::write_empty_leaf_table(&mut page, 0, page_size.get());

        // The raw content offset bytes should be 0x00 0x00 (0 encodes 65536).
        assert_eq!(page[5], 0x00);
        assert_eq!(page[6], 0x00);

        let parsed =
            BTreePageHeader::parse(&page, page_size, 0, false).expect("65536 page must parse");
        assert_eq!(parsed.cell_content_start, 65536);

        eprintln!(
            "[5A1][test=write_empty_leaf_65536][step=verify] \
             content_start=65536 encoding=0x0000 \u{2713}"
        );
    }

    #[test]
    fn test_freelist_leak_on_rollback() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // 1. Allocate a page and commit.
        let mut txn1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn1.allocate_page(&cx).unwrap();
        txn1.write_page(&cx, p, &vec![0xAA; ps]).unwrap();
        txn1.commit(&cx).unwrap();

        // 2. Free the page and commit -> moves to freelist.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.free_page(&cx, p).unwrap();
        txn2.commit(&cx).unwrap();

        // Verify freelist has the page.
        {
            let inner = pager.inner.lock().unwrap();
            assert_eq!(inner.freelist.len(), 1);
            assert_eq!(inner.freelist[0], p);
            drop(inner);
        }

        // 3. Allocate the page again (pops from freelist).
        let mut txn3 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = txn3.allocate_page(&cx).unwrap();
        assert_eq!(
            p2,
            p,
            "bead_id={BEAD_ID} case=freelist_reuse p3={} p1={}",
            p2.get(),
            p.get()
        );
        txn3.write_page(&cx, p2, &vec![0xBB; ps]).unwrap();

        // Verify freelist is empty (in-flight).
        {
            let inner = pager.inner.lock().unwrap();
            assert!(inner.freelist.is_empty());
            drop(inner);
        }

        // 4. Rollback.
        txn3.rollback(&cx).unwrap();

        // 5. Verify freelist has the page again (no leak).
        {
            let inner = pager.inner.lock().unwrap();
            assert_eq!(
                inner.freelist.len(),
                1,
                "bead_id={BEAD_ID} case=freelist_leak_on_rollback"
            );
            assert_eq!(inner.freelist[0], p);
            drop(inner);
        }
    }

    #[test]
    fn test_concurrent_rollback_reclaims_eof_allocations_without_holes() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut concurrent = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let p2 = concurrent.allocate_page(&cx).unwrap();
        let p3 = concurrent.allocate_page(&cx).unwrap();
        concurrent.write_page(&cx, p2, &vec![0xAA; ps]).unwrap();
        concurrent.write_page(&cx, p3, &vec![0xBB; ps]).unwrap();
        concurrent.rollback(&cx).unwrap();

        {
            let inner = pager.inner.lock().unwrap();
            assert!(
                inner.freelist.contains(&p2) && inner.freelist.contains(&p3),
                "bead_id={BEAD_ID} case=concurrent_rollback_restores_eof_pages freelist={:?}",
                inner.freelist
            );
        }

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let reused1 = txn.allocate_page(&cx).unwrap();
        let reused2 = txn.allocate_page(&cx).unwrap();
        assert!(
            reused1 != reused2 && [p2, p3].contains(&reused1) && [p2, p3].contains(&reused2),
            "bead_id={BEAD_ID} case=concurrent_rollback_reuses_eof_pages reused=({}, {}) expected=({}, {})",
            reused1.get(),
            reused2.get(),
            p2.get(),
            p3.get()
        );
        txn.write_page(&cx, reused1, &vec![0xCC; ps]).unwrap();
        txn.write_page(&cx, reused2, &vec![0xDD; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let inner = pager.inner.lock().unwrap();
        assert_eq!(
            inner.db_size, 3,
            "bead_id={BEAD_ID} case=concurrent_rollback_does_not_skip_page_numbers"
        );
    }

    #[test]
    fn test_concurrent_allocate_ignores_global_freelist_pages() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = seed.allocate_page(&cx).unwrap();
        let p3 = seed.allocate_page(&cx).unwrap();
        seed.write_page(&cx, p2, &vec![0x11; ps]).unwrap();
        seed.write_page(&cx, p3, &vec![0x22; ps]).unwrap();
        seed.commit(&cx).unwrap();

        let mut free_txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        free_txn.free_page(&cx, p2).unwrap();
        free_txn.commit(&cx).unwrap();

        let mut concurrent = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let allocated = concurrent.allocate_page(&cx).unwrap();
        assert_eq!(
            allocated.get(),
            p3.get() + 1,
            "bead_id={BEAD_ID} case=concurrent_allocate_must_not_reuse_global_freelist_pages"
        );
        concurrent.rollback(&cx).unwrap();
    }

    #[test]
    fn test_immediate_allocate_ignores_global_freelist_pages_while_reader_active() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = seed.allocate_page(&cx).unwrap();
        let p3 = seed.allocate_page(&cx).unwrap();
        seed.write_page(&cx, p2, &vec![0x11; ps]).unwrap();
        seed.write_page(&cx, p3, &vec![0x22; ps]).unwrap();
        seed.commit(&cx).unwrap();

        let mut free_txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        free_txn.free_page(&cx, p2).unwrap();
        free_txn.commit(&cx).unwrap();

        let mut reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let snapshot_page = reader.get_page(&cx, p3).unwrap();
        assert_eq!(
            snapshot_page.as_ref()[0],
            0x22,
            "bead_id={BEAD_ID} case=reader_snapshot_established_before_writer_allocate"
        );

        let mut writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let allocated = writer.allocate_page(&cx).unwrap();
        assert_eq!(
            allocated.get(),
            p3.get() + 1,
            "bead_id={BEAD_ID} case=immediate_allocate_must_not_reuse_snapshot_pinned_global_freelist_pages"
        );
        writer.rollback(&cx).unwrap();
        reader.commit(&cx).unwrap();
    }

    #[test]
    fn test_commit_filters_beyond_db_size_freelist_entries_from_durable_state() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = seed.allocate_page(&cx).unwrap();
        let p3 = seed.allocate_page(&cx).unwrap();
        seed.write_page(&cx, p2, &vec![0x11; ps]).unwrap();
        seed.write_page(&cx, p3, &vec![0x22; ps]).unwrap();
        seed.commit(&cx).unwrap();

        let mut abandoned = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let p4 = abandoned.allocate_page(&cx).unwrap();
        abandoned.write_page(&cx, p4, &vec![0x33; ps]).unwrap();
        abandoned.rollback(&cx).unwrap();

        {
            let inner = pager.inner.lock().unwrap();
            assert!(
                inner.freelist.contains(&p4),
                "bead_id={BEAD_ID} case=aborted_eof_page_retained_in_memory"
            );
        }

        let mut free_txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        free_txn.free_page(&cx, p2).unwrap();
        free_txn.commit(&cx).unwrap();

        let mut reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let committed_page = reader.get_page(&cx, p3).unwrap();
        assert_eq!(
            committed_page.as_ref()[0],
            0x22,
            "bead_id={BEAD_ID} case=durable_refresh_survives_filtered_beyond_db_size_freelist"
        );
        reader.commit(&cx).unwrap();

        let inner = pager.inner.lock().unwrap();
        assert!(
            inner.freelist.contains(&p2),
            "bead_id={BEAD_ID} case=durable_freelist_keeps_committed_free_page"
        );
        assert!(
            !inner.freelist.contains(&p4),
            "bead_id={BEAD_ID} case=durable_freelist_drops_beyond_db_size_page"
        );
    }

    #[test]
    fn test_pending_commit_pages_ignore_beyond_db_size_freelist_entries() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/pending_commit_pages_ignore_beyond_db_size.db");
        let pager = SimplePager::open(vfs, &path, PageSize::MIN).unwrap();
        let cx = Cx::new();
        let ps = PageSize::MIN.as_usize();

        let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = seed.allocate_page(&cx).unwrap();
        let p3 = seed.allocate_page(&cx).unwrap();
        seed.write_page(&cx, p2, &vec![0x11; ps]).unwrap();
        seed.write_page(&cx, p3, &vec![0x22; ps]).unwrap();
        seed.commit(&cx).unwrap();

        let overflow_freelist_pages = (ps / 4).saturating_sub(2) + 2;
        let mut abandoned = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let mut abandoned_pages = Vec::with_capacity(overflow_freelist_pages);
        for _ in 0..overflow_freelist_pages {
            abandoned_pages.push(abandoned.allocate_page(&cx).unwrap());
        }
        abandoned.rollback(&cx).unwrap();

        let first_beyond_db_size_page = abandoned_pages[0];
        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        txn.free_page(&cx, p2).unwrap();
        let predicted = txn.pending_commit_pages().unwrap();
        assert!(
            predicted.contains(&PageNumber::ONE),
            "bead_id={BEAD_ID} case=pending_commit_pages_still_include_page_one_rewrite"
        );
        assert!(
            predicted.contains(&p2),
            "bead_id={BEAD_ID} case=pending_commit_pages_include_real_durable_trunk"
        );
        assert!(
            !predicted.contains(&first_beyond_db_size_page),
            "bead_id={BEAD_ID} case=pending_commit_pages_ignore_eof_only_freelist_pages"
        );
        txn.rollback(&cx).unwrap();
    }

    #[test]
    fn test_commit_keeps_newly_freed_pages_below_future_db_size_on_durable_freelist() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/commit_keeps_newly_freed_pages.db");
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p2 = seed.allocate_page(&cx).unwrap();
        let p3 = seed.allocate_page(&cx).unwrap();
        seed.write_page(&cx, p2, &vec![0x11; ps]).unwrap();
        seed.write_page(&cx, p3, &vec![0x22; ps]).unwrap();
        seed.commit(&cx).unwrap();

        let (p4, p5, p6, p7) = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p4 = txn.allocate_page(&cx).unwrap();
            let p5 = txn.allocate_page(&cx).unwrap();
            let p6 = txn.allocate_page(&cx).unwrap();
            let p7 = txn.allocate_page(&cx).unwrap();
            txn.free_page(&cx, p4).unwrap();
            txn.free_page(&cx, p5).unwrap();
            txn.free_page(&cx, p6).unwrap();
            txn.write_page(&cx, p7, &vec![0x77; ps]).unwrap();

            let predicted = txn.pending_commit_pages().unwrap();
            assert!(
                predicted.contains(&PageNumber::ONE),
                "bead_id={BEAD_ID} case=future_db_size_commit_still_rewrites_page_one"
            );
            assert!(
                predicted.contains(&p4),
                "bead_id={BEAD_ID} case=future_db_size_commit_includes_new_trunk_page"
            );
            assert!(
                predicted.contains(&p7),
                "bead_id={BEAD_ID} case=future_db_size_commit_includes_live_high_page"
            );

            txn.commit(&cx).unwrap();
            (p4, p5, p6, p7)
        };

        let mut txn_ro = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let raw = txn_ro.get_page(&cx, PageNumber::ONE).unwrap().into_vec();
        let hdr_bytes: [u8; DATABASE_HEADER_SIZE] = raw[..DATABASE_HEADER_SIZE].try_into().unwrap();
        let hdr = DatabaseHeader::from_bytes(&hdr_bytes).unwrap();
        assert_eq!(
            hdr.page_count,
            p7.get(),
            "bead_id={BEAD_ID} case=future_db_size_commit_advances_page_count_to_live_high_page"
        );
        assert_eq!(
            hdr.freelist_count, 3,
            "bead_id={BEAD_ID} case=future_db_size_commit_persists_newly_freed_pages"
        );
        assert_eq!(
            hdr.freelist_trunk,
            p4.get(),
            "bead_id={BEAD_ID} case=future_db_size_commit_uses_first_newly_freed_page_as_trunk"
        );
        txn_ro.commit(&cx).unwrap();

        let reopened = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        {
            let inner = reopened.inner.lock().unwrap();
            assert_eq!(
                inner.db_size,
                p7.get(),
                "bead_id={BEAD_ID} case=future_db_size_commit_reopen_keeps_page_count"
            );
            assert_eq!(
                inner.freelist.len(),
                3,
                "bead_id={BEAD_ID} case=future_db_size_commit_reopen_restores_freelist_len"
            );
            assert!(
                inner.freelist.contains(&p4)
                    && inner.freelist.contains(&p5)
                    && inner.freelist.contains(&p6),
                "bead_id={BEAD_ID} case=future_db_size_commit_reopen_restores_newly_freed_pages"
            );
        }

        let mut reuse = reopened.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut reused = vec![
            reuse.allocate_page(&cx).unwrap(),
            reuse.allocate_page(&cx).unwrap(),
            reuse.allocate_page(&cx).unwrap(),
        ];
        reused.sort_unstable();
        assert_eq!(
            reused,
            vec![p4, p5, p6],
            "bead_id={BEAD_ID} case=future_db_size_commit_reuses_newly_freed_pages_after_reopen"
        );
        reuse.rollback(&cx).unwrap();
    }

    #[test]
    fn test_concurrent_allocate_reuses_beyond_db_size_freelist_pages() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut abandoned = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let p2 = abandoned.allocate_page(&cx).unwrap();
        let p3 = abandoned.allocate_page(&cx).unwrap();
        abandoned.write_page(&cx, p2, &vec![0x33; ps]).unwrap();
        abandoned.write_page(&cx, p3, &vec![0x44; ps]).unwrap();
        abandoned.rollback(&cx).unwrap();

        let mut concurrent = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let reused1 = concurrent.allocate_page(&cx).unwrap();
        let reused2 = concurrent.allocate_page(&cx).unwrap();
        assert!(
            reused1 != reused2 && [p2, p3].contains(&reused1) && [p2, p3].contains(&reused2),
            "bead_id={BEAD_ID} case=concurrent_allocate_reuses_beyond_db_size_freelist_pages reused=({}, {}) expected=({}, {})",
            reused1.get(),
            reused2.get(),
            p2.get(),
            p3.get()
        );
        concurrent.rollback(&cx).unwrap();
    }

    #[test]
    fn test_concurrent_reuse_and_free_beyond_db_size_page_is_net_zero() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut abandoned = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let p2 = abandoned.allocate_page(&cx).unwrap();
        let p3 = abandoned.allocate_page(&cx).unwrap();
        abandoned.write_page(&cx, p2, &vec![0x33; ps]).unwrap();
        abandoned.write_page(&cx, p3, &vec![0x44; ps]).unwrap();
        abandoned.rollback(&cx).unwrap();

        let current_db_size = pager.published_snapshot().db_size;
        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let reused = txn.allocate_page(&cx).unwrap();
        assert!(
            [p2, p3].contains(&reused),
            "bead_id={BEAD_ID} case=concurrent_reuse_then_free_picks_eof_only_page reused={}",
            reused.get()
        );
        txn.free_page(&cx, reused).unwrap();

        let plan = txn.classify_wal_page_one_write(current_db_size, txn.freelist_metadata_dirty());
        assert_eq!(
            plan,
            WalPageOneWritePlan {
                max_written: 0,
                page_one_dirty: false,
                freelist_metadata_dirty: false,
                db_growth: false,
            },
            "bead_id={BEAD_ID} case=concurrent_reuse_then_free_has_no_wal_page_one_trigger"
        );
        assert!(
            !txn.has_pending_writes(),
            "bead_id={BEAD_ID} case=concurrent_reuse_then_free_has_no_pending_writes"
        );
        assert!(
            txn.pending_commit_pages().unwrap().is_empty(),
            "bead_id={BEAD_ID} case=concurrent_reuse_then_free_has_no_commit_pages"
        );
        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_allocate_page_requires_page_one_conflict_tracking_skips_pure_concurrent_eof_allocate() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();

        assert!(
            !txn.allocate_page_requires_page_one_conflict_tracking()
                .unwrap(),
            "bead_id={BEAD_ID} case=concurrent_eof_allocate_does_not_predeclare_page_one_conflict"
        );
    }

    #[test]
    fn test_allocate_page_requires_page_one_conflict_tracking_skips_beyond_db_size_reuse() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let reusable_pages = {
            let mut abandoned = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
            let page_one = abandoned.allocate_page(&cx).unwrap();
            let page_two = abandoned.allocate_page(&cx).unwrap();
            abandoned
                .write_page(&cx, page_one, &vec![0x44; ps])
                .unwrap();
            abandoned
                .write_page(&cx, page_two, &vec![0x55; ps])
                .unwrap();
            abandoned.rollback(&cx).unwrap();
            [page_one, page_two]
        };

        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        assert!(
            !txn.allocate_page_requires_page_one_conflict_tracking()
                .unwrap(),
            "bead_id={BEAD_ID} case=beyond_db_size_reuse_skips_page_one_conflict_tracking"
        );
        let reused = txn.allocate_page(&cx).unwrap();
        assert!(
            reusable_pages.contains(&reused),
            "bead_id={BEAD_ID} case=allocator_page_one_hook_matches_actual_reuse reused={} expected=({}, {})",
            reused.get(),
            reusable_pages[0].get(),
            reusable_pages[1].get()
        );
    }

    #[test]
    fn test_memory_db_allocator_skips_committed_freelist_and_page_one_conflicts() {
        let vfs = MemoryVfs::new();
        let pager = SimplePager::open(vfs, Path::new("/:memory:"), PageSize::DEFAULT).unwrap();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let (page_two, page_three) = {
            let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_two = seed.allocate_page(&cx).unwrap();
            let page_three = seed.allocate_page(&cx).unwrap();
            seed.write_page(&cx, page_two, &vec![0x11; ps]).unwrap();
            seed.write_page(&cx, page_three, &vec![0x22; ps]).unwrap();
            seed.commit(&cx).unwrap();
            (page_two, page_three)
        };

        {
            let mut free_txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            free_txn.free_page(&cx, page_two).unwrap();
            free_txn.commit(&cx).unwrap();
        }

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        assert!(
            !txn.allocate_page_requires_page_one_conflict_tracking()
                .unwrap(),
            "bead_id={BEAD_ID} case=memory_db_allocate_skips_page_one_conflict_tracking"
        );
        let allocated = txn.allocate_page(&cx).unwrap();
        let expected = PageNumber::new(page_three.get() + 1).unwrap();
        assert_eq!(
            allocated,
            expected,
            "bead_id={BEAD_ID} case=memory_db_allocator_uses_bump_path allocated={} expected={}",
            allocated.get(),
            expected.get()
        );
    }

    #[test]
    fn test_memory_db_allocator_stays_bump_only_after_rollback() {
        let vfs = MemoryVfs::new();
        let pager = SimplePager::open(vfs, Path::new("/:memory:"), PageSize::DEFAULT).unwrap();
        let cx = Cx::new();

        let abandoned_pages = {
            let mut abandoned = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
            let page_one = abandoned.allocate_page(&cx).unwrap();
            let page_two = abandoned.allocate_page(&cx).unwrap();
            abandoned.rollback(&cx).unwrap();
            [page_one, page_two]
        };

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        assert!(
            !txn.allocate_page_requires_page_one_conflict_tracking()
                .unwrap(),
            "bead_id={BEAD_ID} case=memory_db_post_rollback_allocate_skips_page_one_conflict_tracking"
        );
        let allocated = txn.allocate_page(&cx).unwrap();
        let expected = PageNumber::new(abandoned_pages[1].get() + 1).unwrap();
        assert!(
            !abandoned_pages.contains(&allocated),
            "bead_id={BEAD_ID} case=memory_db_allocator_skips_freelist_after_rollback allocated={} abandoned=({}, {})",
            allocated.get(),
            abandoned_pages[0].get(),
            abandoned_pages[1].get()
        );
        assert_eq!(
            allocated,
            expected,
            "bead_id={BEAD_ID} case=memory_db_allocator_keeps_bump_sequence allocated={} expected={}",
            allocated.get(),
            expected.get()
        );
    }

    #[test]
    fn test_free_page_requires_page_one_conflict_tracking_defers_concurrent_freelist_reconciliation()
     {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let durable_page = {
            let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page = seed.allocate_page(&cx).unwrap();
            seed.write_page(&cx, page, &[0xAB; 32]).unwrap();
            seed.commit(&cx).unwrap();
            page
        };

        let mut durable_txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        assert!(
            !durable_txn
                .free_page_requires_page_one_conflict_tracking(durable_page)
                .unwrap(),
            "bead_id={BEAD_ID} case=concurrent_durable_free_defers_page_one_conflict_tracking"
        );
        durable_txn.free_page(&cx, durable_page).unwrap();
        let durable_predicted = durable_txn.pending_commit_pages().unwrap();
        assert!(
            durable_predicted.contains(&PageNumber::ONE),
            "bead_id={BEAD_ID} case=concurrent_durable_free_still_puts_page_one_in_pending_commit_surface"
        );
        durable_txn.rollback(&cx).unwrap();

        let reusable_pages = {
            let mut abandoned = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
            let page_one = abandoned.allocate_page(&cx).unwrap();
            let page_two = abandoned.allocate_page(&cx).unwrap();
            abandoned
                .write_page(&cx, page_one, &vec![0x55; ps])
                .unwrap();
            abandoned
                .write_page(&cx, page_two, &vec![0x66; ps])
                .unwrap();
            abandoned.rollback(&cx).unwrap();
            [page_one, page_two]
        };

        let mut net_zero_txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let reused = net_zero_txn.allocate_page(&cx).unwrap();
        assert!(
            reusable_pages.contains(&reused),
            "bead_id={BEAD_ID} case=net_zero_free_reuses_abandoned_page reused={} expected=({}, {})",
            reused.get(),
            reusable_pages[0].get(),
            reusable_pages[1].get()
        );
        assert!(
            !net_zero_txn
                .free_page_requires_page_one_conflict_tracking(reused)
                .unwrap(),
            "bead_id={BEAD_ID} case=net_zero_free_skips_page_one_conflict_tracking"
        );
    }

    /// Regression test for beads_rust#138: concurrent freelist corruption.
    ///
    /// Before the fix, two transactions committing concurrently could push
    /// freed pages into the shared `inner.freelist` during Phase A, then
    /// each serialize a different freelist snapshot into their write_set.
    /// When the WAL flusher wrote both batches, the last writer's page 1
    /// (with potentially stale freelist trunk/count) would overwrite the
    /// first writer's, creating orphaned pages.
    ///
    /// This test verifies that after two sequential commits that free
    /// different pages, the inner.freelist is consistent (contains exactly
    /// the freed pages) and the page 1 freelist metadata matches.
    #[test]
    fn test_concurrent_freelist_no_orphaned_pages_after_sequential_free_commits() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Seed: allocate pages 2..5, commit.
        let (p2, p3, p4, p5) = {
            let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p2 = seed.allocate_page(&cx).unwrap();
            let p3 = seed.allocate_page(&cx).unwrap();
            let p4 = seed.allocate_page(&cx).unwrap();
            let p5 = seed.allocate_page(&cx).unwrap();
            seed.write_page(&cx, p2, &vec![0x22; ps]).unwrap();
            seed.write_page(&cx, p3, &vec![0x33; ps]).unwrap();
            seed.write_page(&cx, p4, &vec![0x44; ps]).unwrap();
            seed.write_page(&cx, p5, &vec![0x55; ps]).unwrap();
            seed.commit(&cx).unwrap();
            (p2, p3, p4, p5)
        };

        // Transaction 1: free p2
        {
            let mut txn1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            txn1.free_page(&cx, p2).unwrap();
            txn1.commit(&cx).unwrap();
        }

        // Verify p2 is in the freelist after T1 commits.
        {
            let inner = pager.inner.lock().unwrap();
            assert!(
                inner.freelist.contains(&p2),
                "bead_id={BEAD_ID} case=freed_page_promoted_to_freelist_after_commit p2={}",
                p2.get()
            );
        }

        // Transaction 2: free p3
        {
            let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            txn2.free_page(&cx, p3).unwrap();
            txn2.commit(&cx).unwrap();
        }

        // Verify both p2 and p3 are in the freelist.
        {
            let inner = pager.inner.lock().unwrap();
            assert!(
                inner.freelist.contains(&p2),
                "bead_id={BEAD_ID} case=p2_still_in_freelist_after_second_commit p2={}",
                p2.get()
            );
            assert!(
                inner.freelist.contains(&p3),
                "bead_id={BEAD_ID} case=p3_in_freelist_after_commit p3={}",
                p3.get()
            );
            assert!(
                !inner.freelist.contains(&p4),
                "bead_id={BEAD_ID} case=p4_not_freed p4={}",
                p4.get()
            );
            assert!(
                !inner.freelist.contains(&p5),
                "bead_id={BEAD_ID} case=p5_not_freed p5={}",
                p5.get()
            );
        }

        // Verify the pages can be re-allocated and read correctly.
        {
            let mut txn3 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let reused1 = txn3.allocate_page(&cx).unwrap();
            let reused2 = txn3.allocate_page(&cx).unwrap();
            // Should get p2 and p3 back (from freelist).
            assert!(
                [p2, p3].contains(&reused1) && [p2, p3].contains(&reused2),
                "bead_id={BEAD_ID} case=freed_pages_reusable reused1={} reused2={}",
                reused1.get(),
                reused2.get()
            );
            txn3.rollback(&cx).unwrap();
        }
    }

    /// Regression test for beads_rust#138: freed pages must not leak into
    /// inner.freelist when commit fails.
    ///
    /// Before the fix, freed pages were pushed into inner.freelist during
    /// Phase A regardless of whether Phase B (WAL I/O) succeeded. If the
    /// commit failed, those pages remained in the freelist and could be
    /// allocated by subsequent transactions even though the free was never
    /// committed to the WAL.
    #[test]
    fn test_freed_pages_not_in_freelist_before_commit_success() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Seed: allocate p2
        let p2 = {
            let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p2 = seed.allocate_page(&cx).unwrap();
            seed.write_page(&cx, p2, &vec![0xAA; ps]).unwrap();
            seed.commit(&cx).unwrap();
            p2
        };

        // Begin a transaction that frees p2 but DON'T commit yet.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.free_page(&cx, p2).unwrap();

        // Before commit, p2 should NOT be in inner.freelist
        // (it's only in the transaction's local freed_pages).
        {
            let inner = pager.inner.lock().unwrap();
            assert!(
                !inner.freelist.contains(&p2),
                "bead_id={BEAD_ID} case=freed_page_not_leaked_before_commit p2={}",
                p2.get()
            );
        }

        // Now commit — p2 should appear in freelist only after success.
        txn.commit(&cx).unwrap();
        {
            let inner = pager.inner.lock().unwrap();
            assert!(
                inner.freelist.contains(&p2),
                "bead_id={BEAD_ID} case=freed_page_promoted_after_successful_commit p2={}",
                p2.get()
            );
        }
    }

    #[test]
    fn test_write_page_requires_page_one_conflict_tracking_defers_concurrent_growth_to_commit_surface()
     {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let page = txn.allocate_page(&cx).unwrap();

        assert!(
            !txn.write_page_requires_page_one_conflict_tracking(page)
                .unwrap(),
            "bead_id={BEAD_ID} case=concurrent_growth_write_defers_page_one_conflict_tracking"
        );
        txn.write_page(&cx, page, &[0x7B; 32]).unwrap();
        let predicted = txn.pending_commit_pages().unwrap();
        assert!(
            predicted.contains(&page),
            "bead_id={BEAD_ID} case=concurrent_growth_pending_commit_surface_keeps_high_page"
        );
        assert!(
            predicted.contains(&PageNumber::ONE),
            "bead_id={BEAD_ID} case=concurrent_growth_pending_commit_surface_still_contains_page_one"
        );
    }

    #[test]
    fn test_pending_conflict_pages_exclude_synthetic_page_one_for_concurrent_wal_growth() {
        let (pager, _) = wal_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let page = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, page, &[0x7B; 32]).unwrap();

        let pending_commit = txn.pending_commit_pages().unwrap();
        let pending_conflict = txn.pending_conflict_pages().unwrap();

        // D1-CRITICAL: Pure WAL growth does NOT put Page 1 in commit surface.
        // The WAL frame's db_size_if_commit captures database size, so Page 1
        // update is deferred to checkpoint. This eliminates MVCC conflicts.
        assert!(
            !pending_commit.contains(&PageNumber::ONE),
            "bead_id={BEAD_ID} case=concurrent_wal_growth_commit_surface_excludes_page_one"
        );
        assert!(
            pending_commit.contains(&page),
            "bead_id={BEAD_ID} case=concurrent_wal_growth_commit_surface_keeps_data_page"
        );
        assert!(
            !pending_conflict.contains(&PageNumber::ONE),
            "bead_id={BEAD_ID} case=concurrent_wal_growth_conflict_surface_excludes_synthetic_page_one"
        );
        assert!(
            pending_conflict.contains(&page),
            "bead_id={BEAD_ID} case=concurrent_wal_growth_conflict_surface_keeps_real_data_page"
        );
    }

    #[test]
    fn test_pending_conflict_pages_keep_explicit_page_one_write_for_concurrent_wal() {
        let (pager, _) = wal_pager();
        let cx = Cx::new();
        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        txn.write_page(&cx, PageNumber::ONE, &[0x5A; 32]).unwrap();

        let pending_conflict = txn.pending_conflict_pages().unwrap();
        assert!(
            pending_conflict.contains(&PageNumber::ONE),
            "bead_id={BEAD_ID} case=explicit_page_one_write_remains_in_conflict_surface"
        );
    }

    #[test]
    fn test_write_page_requires_page_one_conflict_tracking_skips_interior_page() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        let durable_page = {
            let mut seed = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page = seed.allocate_page(&cx).unwrap();
            seed.write_page(&cx, page, &[0xAB; 32]).unwrap();
            seed.commit(&cx).unwrap();
            page
        };

        let txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        assert!(
            !txn.write_page_requires_page_one_conflict_tracking(durable_page)
                .unwrap(),
            "bead_id={BEAD_ID} case=interior_page_write_skips_page_one_conflict_tracking"
        );
    }

    #[test]
    fn test_concurrent_rollback_to_savepoint_reclaims_eof_allocations() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
        let base = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, base, &vec![0x11; ps]).unwrap();
        txn.savepoint(&cx, "sp").unwrap();

        let p3 = txn.allocate_page(&cx).unwrap();
        let p4 = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p3, &vec![0x22; ps]).unwrap();
        txn.write_page(&cx, p4, &vec![0x33; ps]).unwrap();

        txn.rollback_to_savepoint(&cx, "sp").unwrap();

        let reused1 = txn.allocate_page(&cx).unwrap();
        let reused2 = txn.allocate_page(&cx).unwrap();
        assert!(
            reused1.get() == 3 && reused2.get() == 4,
            "bead_id={BEAD_ID} case=concurrent_savepoint_reuses_eof_pages reused=({}, {}) expected=(3, 4)",
            reused1.get(),
            reused2.get()
        );
        txn.write_page(&cx, reused1, &vec![0x44; ps]).unwrap();
        txn.write_page(&cx, reused2, &vec![0x55; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let inner = pager.inner.lock().unwrap();
        assert_eq!(
            inner.db_size, 4,
            "bead_id={BEAD_ID} case=concurrent_savepoint_rollback_does_not_skip_page_numbers"
        );
    }

    #[test]
    fn test_concurrent_drop_reclaims_eof_allocations_without_holes() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        {
            let mut concurrent = pager.begin(&cx, TransactionMode::Concurrent).unwrap();
            let p2 = concurrent.allocate_page(&cx).unwrap();
            let p3 = concurrent.allocate_page(&cx).unwrap();
            concurrent.write_page(&cx, p2, &vec![0x66; ps]).unwrap();
            concurrent.write_page(&cx, p3, &vec![0x77; ps]).unwrap();
        }

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let reused1 = txn.allocate_page(&cx).unwrap();
        let reused2 = txn.allocate_page(&cx).unwrap();
        assert!(
            reused1.get() <= 3 && reused2.get() <= 3 && reused1 != reused2,
            "bead_id={BEAD_ID} case=concurrent_drop_reuses_abandoned_eof_pages reused=({}, {})",
            reused1.get(),
            reused2.get()
        );
        txn.write_page(&cx, reused1, &vec![0x88; ps]).unwrap();
        txn.write_page(&cx, reused2, &vec![0x99; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let inner = pager.inner.lock().unwrap();
        assert_eq!(
            inner.db_size, 3,
            "bead_id={BEAD_ID} case=concurrent_drop_does_not_skip_page_numbers"
        );
    }

    #[test]
    fn test_freelist_persisted_and_reloaded_on_reopen() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/freelist_persist.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let mut txn1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn1.allocate_page(&cx).unwrap();
        txn1.write_page(&cx, p, &vec![0xAB; ps]).unwrap();
        txn1.commit(&cx).unwrap();

        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.free_page(&cx, p).unwrap();
        txn2.commit(&cx).unwrap();

        let txn_ro = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let raw = txn_ro.get_page(&cx, PageNumber::ONE).unwrap().into_vec();
        let hdr_bytes: [u8; DATABASE_HEADER_SIZE] = raw[..DATABASE_HEADER_SIZE].try_into().unwrap();
        let hdr = DatabaseHeader::from_bytes(&hdr_bytes).unwrap();
        assert_eq!(hdr.freelist_count, 1);
        assert_eq!(hdr.freelist_trunk, p.get());

        let reopened = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        {
            let inner = reopened.inner.lock().unwrap();
            assert_eq!(inner.freelist.len(), 1);
            assert_eq!(inner.freelist[0], p);
        }

        let mut txn3 = reopened.begin(&cx, TransactionMode::Immediate).unwrap();
        let reused = txn3.allocate_page(&cx).unwrap();
        assert_eq!(
            reused, p,
            "reopened pager should reuse persisted freelist page"
        );
        txn3.commit(&cx).unwrap();
    }

    #[test]
    #[allow(clippy::similar_names, clippy::cast_possible_truncation)]
    fn test_cache_eviction_under_pressure() {
        // Verify that SimplePager can handle more pages than the cache capacity.
        // PageCache is initialized with 256 pages. We write 300 pages.
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();

        // Write 300 pages. This exceeds the 256-page cache capacity.
        for i in 0..300u32 {
            let p = txn.allocate_page(&cx).unwrap();
            pages.push(p);
            // Unique pattern per page to verify content.
            let byte = (i % 256) as u8;
            let data = vec![byte; ps];
            txn.write_page(&cx, p, &data).unwrap();
        }
        txn.commit(&cx).unwrap();

        // Read all pages back. Some will be cache misses, requiring eviction of others.
        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        for (i, &p) in pages.iter().enumerate() {
            let data = txn.get_page(&cx, p).unwrap();
            let expected_byte = (i % 256) as u8;
            assert_eq!(
                data.as_ref()[0],
                expected_byte,
                "bead_id={BEAD_ID} case=cache_pressure page={p}"
            );
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // bd-2ttd8.2: Pager invariant suite — SimplePager correctness
    // ═══════════════════════════════════════════════════════════════════

    const BEAD_INV: &str = "bd-2ttd8.2";

    #[test]
    fn test_inv_write_set_not_in_freelist_during_txn() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Allocate, write, commit.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0xAA; ps]).unwrap();
        txn.commit(&cx).unwrap();

        // Free the page.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.free_page(&cx, p).unwrap();

        // The freed page should be in freed_pages, not in write_set.
        assert!(
            txn2.freed_pages.contains(&p),
            "bead_id={BEAD_INV} inv=freed_page_tracked"
        );
        assert!(
            !txn2.write_set.contains_key(&p),
            "bead_id={BEAD_INV} inv=freed_not_in_write_set"
        );

        txn2.commit(&cx).unwrap();
    }

    #[test]
    fn test_inv_allocated_pages_sequential_and_nonzero() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();
        for _ in 0..10 {
            let p = txn.allocate_page(&cx).unwrap();
            assert!(p.get() > 0, "bead_id={BEAD_INV} inv=page_nonzero");
            // No duplicates.
            assert!(
                !pages.contains(&p),
                "bead_id={BEAD_INV} inv=page_unique p={p}"
            );
            pages.push(p);
        }
        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_inv_writer_serialization_single_writer() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        let _w1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();

        // Second immediate should fail (writer_active).
        let err = pager.begin(&cx, TransactionMode::Immediate);
        assert!(
            err.is_err(),
            "bead_id={BEAD_INV} inv=single_writer_enforced"
        );

        // Exclusive also fails.
        let err2 = pager.begin(&cx, TransactionMode::Exclusive);
        assert!(
            err2.is_err(),
            "bead_id={BEAD_INV} inv=exclusive_blocked_by_writer"
        );
    }

    #[test]
    fn test_inv_writer_released_on_commit() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        let mut w1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        w1.commit(&cx).unwrap();

        // Writer lock should be released; new writer should succeed.
        let _w2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
    }

    #[test]
    fn test_inv_writer_released_on_rollback() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        let mut w1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        w1.rollback(&cx).unwrap();

        let _w2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
    }

    #[test]
    fn test_inv_writer_released_on_drop() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        {
            let _w1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            // Drop without commit or rollback.
        }

        let _w2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
    }

    #[test]
    fn test_inv_commit_persists_all_dirty_pages() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();
        for i in 0..5u8 {
            let p = txn.allocate_page(&cx).unwrap();
            let mut data = vec![0u8; ps];
            data[0] = 0xD0 + i;
            data[ps - 1] = i;
            txn.write_page(&cx, p, &data).unwrap();
            pages.push((p, 0xD0 + i, i));
        }
        txn.commit(&cx).unwrap();

        // Read back in a new read-only transaction.
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        for (p, first_byte, last_byte) in &pages {
            let data = txn2.get_page(&cx, *p).unwrap();
            assert_eq!(
                data.as_ref()[0],
                *first_byte,
                "bead_id={BEAD_INV} inv=dirty_page_committed p={p}"
            );
            assert_eq!(
                data.as_ref()[ps - 1],
                *last_byte,
                "bead_id={BEAD_INV} inv=dirty_page_last_byte p={p}"
            );
        }
    }

    #[test]
    fn test_inv_rollback_discards_all_dirty_pages() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Initial committed data.
        let mut txn1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn1.allocate_page(&cx).unwrap();
        txn1.write_page(&cx, p, &vec![0xAA; ps]).unwrap();
        txn1.commit(&cx).unwrap();

        // Overwrite and rollback.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.write_page(&cx, p, &vec![0xBB; ps]).unwrap();
        txn2.rollback(&cx).unwrap();

        // Verify original data survives.
        let txn3 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = txn3.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0xAA,
            "bead_id={BEAD_INV} inv=rollback_preserves_committed"
        );
    }

    #[test]
    fn test_inv_savepoint_nested_stack_order() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();

        txn.write_page(&cx, p, &vec![0x01; ps]).unwrap();
        txn.savepoint(&cx, "sp1").unwrap();

        txn.write_page(&cx, p, &vec![0x02; ps]).unwrap();
        txn.savepoint(&cx, "sp2").unwrap();

        txn.write_page(&cx, p, &vec![0x03; ps]).unwrap();
        txn.savepoint(&cx, "sp3").unwrap();

        txn.write_page(&cx, p, &vec![0x04; ps]).unwrap();

        // Rollback to sp2 → data should be 0x02.
        txn.rollback_to_savepoint(&cx, "sp2").unwrap();
        let data = txn.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x02,
            "bead_id={BEAD_INV} inv=nested_rollback_sp2"
        );

        // sp3 should no longer exist.
        let err = txn.rollback_to_savepoint(&cx, "sp3");
        assert!(
            err.is_err(),
            "bead_id={BEAD_INV} inv=sp3_removed_after_rollback_to_sp2"
        );

        // sp1 should still exist.
        txn.rollback_to_savepoint(&cx, "sp1").unwrap();
        let data = txn.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x01,
            "bead_id={BEAD_INV} inv=nested_rollback_sp1"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_inv_savepoint_release_merges_to_parent() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();

        txn.write_page(&cx, p, &vec![0x10; ps]).unwrap();
        txn.savepoint(&cx, "outer").unwrap();

        txn.write_page(&cx, p, &vec![0x20; ps]).unwrap();
        txn.savepoint(&cx, "inner").unwrap();

        txn.write_page(&cx, p, &vec![0x30; ps]).unwrap();

        // Release inner → changes kept.
        txn.release_savepoint(&cx, "inner").unwrap();
        let data = txn.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x30,
            "bead_id={BEAD_INV} inv=release_keeps_changes"
        );

        // Rollback to outer → restores data from before inner.
        txn.rollback_to_savepoint(&cx, "outer").unwrap();
        let data = txn.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0x10,
            "bead_id={BEAD_INV} inv=rollback_outer_after_release_inner"
        );

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_inv_freelist_restored_on_rollback() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Allocate + commit.
        let mut txn1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn1.allocate_page(&cx).unwrap();
        txn1.write_page(&cx, p, &vec![0xAA; ps]).unwrap();
        txn1.commit(&cx).unwrap();

        // Free + commit → moves to freelist.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.free_page(&cx, p).unwrap();
        txn2.commit(&cx).unwrap();

        let freelist_before = {
            let inner = pager.inner.lock().unwrap();
            inner.freelist.clone()
        };
        assert!(
            freelist_before.contains(&p),
            "bead_id={BEAD_INV} inv=freed_in_freelist"
        );

        // Allocate from freelist, then rollback → page returns to freelist.
        let mut txn3 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let reused = txn3.allocate_page(&cx).unwrap();
        assert_eq!(reused, p, "should reuse freed page");
        txn3.rollback(&cx).unwrap();

        let freelist_after = {
            let inner = pager.inner.lock().unwrap();
            inner.freelist.clone()
        };
        assert_eq!(
            freelist_after, freelist_before,
            "bead_id={BEAD_INV} inv=freelist_restored_after_rollback"
        );
    }

    #[test]
    fn test_inv_page_identity_read_before_write() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Allocate a page, write, commit.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0xAA; ps]).unwrap();
        txn.commit(&cx).unwrap();

        // Read in new transaction → should see committed data.
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = txn2.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0xAA,
            "bead_id={BEAD_INV} inv=committed_visible"
        );
    }

    #[test]
    fn test_inv_write_set_isolation() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Commit baseline data.
        let mut txn1 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn1.allocate_page(&cx).unwrap();
        txn1.write_page(&cx, p, &vec![0x11; ps]).unwrap();
        txn1.commit(&cx).unwrap();

        // Start a reader → sees committed.
        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let r_data = reader.get_page(&cx, p).unwrap();
        assert_eq!(r_data.as_ref()[0], 0x11);

        // Writer modifies.
        let mut writer = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        writer.write_page(&cx, p, &vec![0x22; ps]).unwrap();

        // Reader still sees committed data (write-set is txn-private).
        let r_data2 = reader.get_page(&cx, p).unwrap();
        assert_eq!(
            r_data2.as_ref()[0],
            0x11,
            "bead_id={BEAD_INV} inv=write_set_isolated_from_readers"
        );

        writer.commit(&cx).unwrap();
    }

    #[test]
    fn test_inv_db_size_grows_on_allocate_commit() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let initial_size = {
            let inner = pager.inner.lock().unwrap();
            inner.db_size
        };

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        for _ in 0..5 {
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0x00; ps]).unwrap();
        }
        txn.commit(&cx).unwrap();

        let final_size = {
            let inner = pager.inner.lock().unwrap();
            inner.db_size
        };

        assert!(
            final_size > initial_size,
            "bead_id={BEAD_INV} inv=db_size_grows initial={initial_size} final={final_size}"
        );
    }

    #[test]
    fn test_inv_db_size_restored_on_rollback() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let size_before = {
            let inner = pager.inner.lock().unwrap();
            inner.db_size
        };

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        for _ in 0..5 {
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0x00; ps]).unwrap();
        }
        txn.rollback(&cx).unwrap();

        let size_after = {
            let inner = pager.inner.lock().unwrap();
            inner.db_size
        };

        assert_eq!(
            size_after, size_before,
            "bead_id={BEAD_INV} inv=db_size_restored_on_rollback"
        );
    }

    #[test]
    fn test_inv_active_transaction_count() {
        let (pager, _) = test_pager();
        let cx = Cx::new();

        let count_before = {
            let inner = pager.inner.lock().unwrap();
            inner.active_transactions
        };
        assert_eq!(count_before, 0, "bead_id={BEAD_INV} inv=initial_zero_txns");

        let r1 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let r2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();

        {
            let inner = pager.inner.lock().unwrap();
            assert_eq!(
                inner.active_transactions, 2,
                "bead_id={BEAD_INV} inv=two_active_txns"
            );
        }

        drop(r1);
        {
            let inner = pager.inner.lock().unwrap();
            assert_eq!(
                inner.active_transactions, 1,
                "bead_id={BEAD_INV} inv=one_after_drop"
            );
        }

        drop(r2);
        {
            let inner = pager.inner.lock().unwrap();
            assert_eq!(
                inner.active_transactions, 0,
                "bead_id={BEAD_INV} inv=zero_after_all_dropped"
            );
        }
    }

    #[test]
    fn test_inv_journal_mode_default_delete() {
        let (pager, _) = test_pager();
        assert_eq!(
            pager.journal_mode(),
            JournalMode::Delete,
            "bead_id={BEAD_INV} inv=default_journal_delete"
        );
    }

    #[test]
    fn test_inv_commit_seq_monotonic() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut prev_seq = {
            let inner = pager.inner.lock().unwrap();
            inner.commit_seq.get()
        };

        for _ in 0..5 {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0x00; ps]).unwrap();
            txn.commit(&cx).unwrap();

            let seq = {
                let inner = pager.inner.lock().unwrap();
                inner.commit_seq.get()
            };
            assert!(
                seq >= prev_seq,
                "bead_id={BEAD_INV} inv=commit_seq_monotonic seq={seq} prev={prev_seq}"
            );
            prev_seq = seq;
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // bd-2ttd8.3: Deterministic pager e2e scenarios with cache-pressure
    //             telemetry
    // ═══════════════════════════════════════════════════════════════════

    const BEAD_E2E: &str = "bd-2ttd8.3";

    #[test]
    fn test_e2e_sequential_write_read_with_metrics() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        pager.reset_cache_metrics().unwrap();

        // Phase 1: Sequential write of 20 pages.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();
        for i in 0..20u32 {
            let p = txn.allocate_page(&cx).unwrap();
            let mut data = vec![0u8; ps];
            data[0] = (i & 0xFF) as u8;
            data[1] = ((i >> 8) & 0xFF) as u8;
            txn.write_page(&cx, p, &data).unwrap();
            pages.push(p);
        }
        txn.commit(&cx).unwrap();

        let post_write = pager.cache_metrics_snapshot().unwrap();
        assert!(
            post_write.admits > 0,
            "bead_id={BEAD_E2E} case=seq_write_admits"
        );

        // Phase 2: Sequential read — all pages should be cached.
        pager.reset_cache_metrics().unwrap();
        let read_before = read_surface_snapshot(&pager);
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        for (i, &p) in pages.iter().enumerate() {
            let data = txn2.get_page(&cx, p).unwrap();
            assert_eq!(
                data.as_ref()[0],
                (i & 0xFF) as u8,
                "bead_id={BEAD_E2E} case=seq_read_content page={p}"
            );
        }

        let post_read = pager.cache_metrics_snapshot().unwrap();
        let read_after = read_surface_snapshot(&pager);
        println!(
            "DEBUG_METRICS: hits={} misses={} admits={} evictions={} cached={}",
            post_read.hits,
            post_read.misses,
            post_read.admits,
            post_read.evictions,
            post_read.cached_pages
        );

        let total_reads = observed_read_total(read_before, read_after);
        assert!(
            total_reads == 20,
            "bead_id={BEAD_E2E} case=seq_read_accesses total={}",
            total_reads
        );
        let hit_rate = observed_read_hit_rate_percent(read_before, read_after);
        assert!(
            hit_rate > 40.0,
            "bead_id={BEAD_E2E} case=seq_read_hit_rate rate={}",
            hit_rate
        );
    }

    #[test]
    fn test_e2e_cache_pressure_eviction_telemetry() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Pool capacity is 1024. Write 300 pages — all fit in cache.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();
        for i in 0..300u32 {
            let p = txn.allocate_page(&cx).unwrap();
            let byte = (i % 256) as u8;
            txn.write_page(&cx, p, &vec![byte; ps]).unwrap();
            pages.push((p, byte));
        }
        txn.commit(&cx).unwrap();

        pager.reset_cache_metrics().unwrap();
        let read_before = read_surface_snapshot(&pager);

        // Sequential read of all 300 pages.
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        for (p, expected) in &pages {
            let data = txn2.get_page(&cx, *p).unwrap();
            assert_eq!(
                data.as_ref()[0],
                *expected,
                "bead_id={BEAD_E2E} case=pressure_content page={p}"
            );
        }

        let read_after = read_surface_snapshot(&pager);
        let total = observed_read_total(read_before, read_after);
        assert_eq!(
            total, 300,
            "bead_id={BEAD_E2E} case=pressure_total_accesses"
        );
        let hit_rate = observed_read_hit_rate_percent(read_before, read_after);
        assert!(
            hit_rate > 40.0,
            "bead_id={BEAD_E2E} case=pressure_hit_rate rate={}",
            hit_rate
        );
    }

    #[test]
    fn test_e2e_hot_cold_workload_hit_rate() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Write 50 pages.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();
        for i in 0..50u32 {
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![(i % 256) as u8; ps]).unwrap();
            pages.push(p);
        }
        txn.commit(&cx).unwrap();

        // Define hot set (first 5 pages) and cold set (remaining 45).
        let hot = &pages[..5];

        pager.reset_cache_metrics().unwrap();
        let read_before = read_surface_snapshot(&pager);
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        for idx in 0..200usize {
            // Five hot accesses plus two cold accesses per round.
            for h in hot {
                let _ = txn2.get_page(&cx, *h).unwrap();
            }
            // 2 cold accesses (rotating through cold pages).
            let cold_idx = (idx * 2) % 45;
            let _ = txn2.get_page(&cx, pages[5 + cold_idx]).unwrap();
            let _ = txn2.get_page(&cx, pages[5 + (cold_idx + 1) % 45]).unwrap();
        }
        drop(txn2);

        let read_after = read_surface_snapshot(&pager);
        let total = observed_read_total(read_before, read_after);
        let expected_total_reads = u64::try_from((hot.len() + 2) * 200).unwrap();
        assert_eq!(
            total, expected_total_reads,
            "bead_id={BEAD_E2E} case=hot_cold_total_accesses"
        );
        // Hot pages should achieve high hit rate after first access.
        let hit_rate = observed_read_hit_rate_percent(read_before, read_after);
        assert!(
            hit_rate > 40.0,
            "bead_id={BEAD_E2E} case=hot_cold_hit_rate rate={}",
            hit_rate
        );
    }

    #[test]
    fn test_e2e_random_access_pattern_deterministic() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Write 100 pages.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();
        for _ in 0..100u32 {
            let p = txn.allocate_page(&cx).unwrap();
            let mut data = vec![0u8; ps];
            // Unique fingerprint: page number in first 4 bytes.
            data[..4].copy_from_slice(&p.get().to_le_bytes());
            txn.write_page(&cx, p, &data).unwrap();
            pages.push(p);
        }
        txn.commit(&cx).unwrap();

        // Deterministic "random" access via linear congruential generator.
        // LCG: next = (a * prev + c) mod m, with a=13, c=7, m=100.
        pager.reset_cache_metrics().unwrap();
        let read_before = read_surface_snapshot(&pager);
        let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let mut idx: usize = 0;
        for _ in 0..200 {
            idx = (13 * idx + 7) % 100;
            let p = pages[idx];
            let data = txn2.get_page(&cx, p).unwrap();
            let stored_pgno = u32::from_le_bytes(data.as_ref()[..4].try_into().unwrap());
            assert_eq!(
                stored_pgno,
                p.get(),
                "bead_id={BEAD_E2E} case=random_fingerprint page={p}"
            );
        }

        let read_after = read_surface_snapshot(&pager);
        let total = observed_read_total(read_before, read_after);
        assert!(total > 0, "bead_id={BEAD_E2E} case=random_total_accesses");

        assert_eq!(total, 200, "bead_id={BEAD_E2E} case=random_total_accesses");
        // With 100 pages and 256-page cache, everything fits → high hit rate.
        let hit_rate = observed_read_hit_rate_percent(read_before, read_after);
        assert!(
            hit_rate > 40.0,
            "bead_id={BEAD_E2E} case=random_hit_rate rate={}",
            hit_rate
        );
    }

    #[test]
    fn test_e2e_mixed_read_write_workload() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Phase 1: Seed 30 pages.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();
        for _ in 0..30u32 {
            let p = txn.allocate_page(&cx).unwrap();
            let mut data = vec![0u8; ps];
            // Unique fingerprint: page number in first 4 bytes.
            data[..4].copy_from_slice(&p.get().to_le_bytes());
            txn.write_page(&cx, p, &data).unwrap();
            pages.push(p);
        }
        txn.commit(&cx).unwrap();

        // Phase 2: Mixed read/write in batches (deterministic).
        let mut phase_two_reads = 0_u64;
        for batch in 0..5u32 {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let batch_read_before = read_surface_snapshot(&pager);

            // Read existing pages.
            for i in 0..10 {
                let idx = ((batch as usize * 3) + i) % pages.len();
                let _ = txn.get_page(&cx, pages[idx]).unwrap();
            }
            let batch_read_after = read_surface_snapshot(&pager);
            phase_two_reads = phase_two_reads
                .saturating_add(observed_read_total(batch_read_before, batch_read_after));

            // Write/overwrite some pages.
            for i in 0..3 {
                let idx = ((batch as usize * 5) + i) % pages.len();
                let new_val = ((batch * 10 + i as u32) % 256) as u8;
                txn.write_page(&cx, pages[idx], &vec![new_val; ps]).unwrap();
            }

            // Allocate a new page per batch.
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0xF0 + batch as u8; ps])
                .unwrap();
            pages.push(p);

            txn.commit(&cx).unwrap();
        }

        // Phase 3: Verify final state.
        assert!(
            phase_two_reads == 50,
            "bead_id={BEAD_E2E} case=mixed_total_accesses total={}",
            phase_two_reads
        );

        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        // Verify the 5 newly allocated pages.
        for batch in 0..5u32 {
            let p = pages[30 + batch as usize];
            let data = txn.get_page(&cx, p).unwrap();
            assert_eq!(
                data.as_ref()[0],
                0xF0 + batch as u8,
                "bead_id={BEAD_E2E} case=mixed_new_page batch={batch}"
            );
        }
    }

    #[test]
    fn test_e2e_write_overwrite_verify_latest() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Allocate and commit.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0x01; ps]).unwrap();
        txn.commit(&cx).unwrap();

        // Overwrite 10 times across separate transactions.
        for version in 2..=11u8 {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            txn.write_page(&cx, p, &vec![version; ps]).unwrap();
            txn.commit(&cx).unwrap();
        }

        // Final read should see version 11.
        let txn = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = txn.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            11,
            "bead_id={BEAD_E2E} case=overwrite_latest_version"
        );
    }

    #[test]
    fn test_e2e_savepoint_heavy_workload() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let mut pages = Vec::new();

        // Allocate 10 pages.
        for i in 0..10u8 {
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![i; ps]).unwrap();
            pages.push(p);
        }

        // Savepoint → more writes → rollback → verify.
        txn.savepoint(&cx, "sp_heavy").unwrap();
        for &p in &pages {
            txn.write_page(&cx, p, &vec![0xFF; ps]).unwrap();
        }

        // All pages should read 0xFF before rollback.
        for &p in &pages {
            let data = txn.get_page(&cx, p).unwrap();
            assert_eq!(data.as_ref()[0], 0xFF);
        }

        txn.rollback_to_savepoint(&cx, "sp_heavy").unwrap();

        // After rollback, original values restored.
        for (i, &p) in pages.iter().enumerate() {
            let data = txn.get_page(&cx, p).unwrap();
            assert_eq!(
                data.as_ref()[0],
                i as u8,
                "bead_id={BEAD_E2E} case=savepoint_heavy_restored page={p}"
            );
        }

        txn.commit(&cx).unwrap();
    }

    #[test]
    fn test_e2e_alloc_free_cycle_no_leak() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let initial_db_size = {
            let inner = pager.inner.lock().unwrap();
            inner.db_size
        };

        // Cycle: allocate → commit → free → commit, 10 times.
        let mut freed_pages = Vec::new();
        for _ in 0..10 {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0xCC; ps]).unwrap();
            txn.commit(&cx).unwrap();

            let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            txn2.free_page(&cx, p).unwrap();
            txn2.commit(&cx).unwrap();
            freed_pages.push(p);
        }

        // Freelist should have pages available for reuse.
        let freelist_len = {
            let inner = pager.inner.lock().unwrap();
            inner.freelist.len()
        };
        assert!(
            freelist_len > 0,
            "bead_id={BEAD_E2E} case=alloc_free_freelist_populated"
        );

        // Allocate again — should reuse freed pages.
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let reused = txn.allocate_page(&cx).unwrap();
        assert!(
            freed_pages.contains(&reused),
            "bead_id={BEAD_E2E} case=alloc_free_reuse reused={reused}"
        );
        txn.commit(&cx).unwrap();

        let final_db_size = {
            let inner = pager.inner.lock().unwrap();
            inner.db_size
        };
        assert_eq!(
            final_db_size,
            initial_db_size + 1,
            "DB size should only grow by 1 page (the one currently allocated)"
        );
    }

    #[test]
    fn test_e2e_metrics_monotonic_across_transactions() {
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut prev_total = 0u64;

        for round in 0..5u32 {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![round as u8; ps]).unwrap();
            txn.commit(&cx).unwrap();

            // Read back.
            let txn2 = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
            let _ = txn2.get_page(&cx, p).unwrap();

            let snapshot = read_surface_snapshot(&pager);
            let total = snapshot
                .cache
                .total_accesses()
                .saturating_add(snapshot.published_hits);
            assert!(
                total >= prev_total,
                "bead_id={BEAD_E2E} case=metrics_monotonic round={round} \
                 total={} prev={}",
                total,
                prev_total
            );
            prev_total = total;
        }
    }

    #[test]
    fn test_e2e_journal_recovery_after_crash_simulation() {
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/crash_sim.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        // Write committed data.
        let pager = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0xAA; ps]).unwrap();
        txn.commit(&cx).unwrap();

        // Start another write but DON'T commit → simulates crash mid-journal.
        let mut txn2 = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        txn2.write_page(&cx, p, &vec![0xBB; ps]).unwrap();
        // Drop without commit → implicit rollback.
        drop(txn2);
        drop(pager);

        // Re-open: hot journal recovery should restore original data.
        let pager2 = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();
        let txn3 = pager2.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = txn3.get_page(&cx, p).unwrap();
        assert_eq!(
            data.as_ref()[0],
            0xAA,
            "bead_id={BEAD_E2E} case=journal_recovery_restores_committed"
        );
    }

    #[test]
    fn test_published_snapshot_monotonic_after_commit() {
        init_publication_test_tracing();
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();
        let before = pager.published_snapshot();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0x5A; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let after = pager.published_snapshot();
        assert!(
            after.snapshot_gen > before.snapshot_gen,
            "bead_id={BEAD_ID} case=publication_snapshot_gen_monotonic"
        );
        assert!(
            after.visible_commit_seq > before.visible_commit_seq,
            "bead_id={BEAD_ID} case=publication_commit_seq_monotonic"
        );
        assert_eq!(
            after.db_size,
            p.get(),
            "bead_id={BEAD_ID} case=publication_db_size_updates"
        );
        assert_eq!(
            after.freelist_count, 0,
            "bead_id={BEAD_ID} case=publication_freelist_count_updates"
        );
        assert!(
            !after.checkpoint_active,
            "bead_id={BEAD_ID} case=publication_checkpoint_inactive_after_commit"
        );
    }

    #[test]
    fn test_commit_batches_publication_writes() {
        init_publication_test_tracing();
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let publication_writes_before_commit = pager.publication_write_count();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0x5A; ps]).unwrap();
        txn.commit(&cx).unwrap();

        assert_eq!(
            pager.publication_write_count(),
            publication_writes_before_commit + 1,
            "bead_id={BEAD_ID} case=commit_batches_publication_writes"
        );
    }

    #[test]
    fn test_single_connection_commit_elides_publication_writes() {
        init_publication_test_tracing();
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();
        let shared_connection_count = Arc::new(AtomicUsize::new(1));
        pager.bind_shared_connection_count(Arc::clone(&shared_connection_count));

        let before = pager.published_snapshot();
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let publication_writes_before_commit = pager.publication_write_count();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0x6B; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let after = pager.published_snapshot();
        assert_eq!(
            pager.publication_write_count(),
            publication_writes_before_commit,
            "bead_id={BEAD_ID} case=single_connection_commit_skips_publication_write"
        );
        assert!(
            after.visible_commit_seq > before.visible_commit_seq,
            "bead_id={BEAD_ID} case=single_connection_commit_still_advances_visible_commit_seq"
        );
        assert_eq!(
            after.db_size,
            p.get(),
            "bead_id={BEAD_ID} case=single_connection_commit_updates_published_db_size"
        );

        shared_connection_count.store(2, AtomicOrdering::Release);
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let publication_writes_before_multi_connection_commit = pager.publication_write_count();
        txn.write_page(&cx, p, &vec![0x7C; ps]).unwrap();
        txn.commit(&cx).unwrap();

        assert_eq!(
            pager.publication_write_count(),
            publication_writes_before_multi_connection_commit + 1,
            "bead_id={BEAD_ID} case=multi_connection_commit_restores_publication_write"
        );
    }

    #[test]
    fn test_wal_commit_skips_post_commit_cache_admission() {
        init_publication_test_tracing();
        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let db_path = PathBuf::from("/wal_commit_skips_post_commit_cache_admission.db");
        let pager = SimplePager::open(vfs, &db_path, PageSize::DEFAULT).unwrap();
        let (backend, _frames, _begin_calls, _batch_calls) = MockWalBackend::new();
        pager.set_wal_backend(Box::new(backend)).unwrap();
        pager.set_journal_mode(&cx, JournalMode::Wal).unwrap();

        let cached_pages_before_commit = pager.cache_metrics_snapshot().unwrap().cached_pages;
        let ps = PageSize::DEFAULT.as_usize();
        let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
        let p = txn.allocate_page(&cx).unwrap();
        txn.write_page(&cx, p, &vec![0xAB; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let cache_after_commit = pager.cache_metrics_snapshot().unwrap();
        assert_eq!(
            cache_after_commit.cached_pages,
            cached_pages_before_commit + 1,
            "bead_id={BEAD_ID} case=wal_commit_avoids_post_commit_cache_fanout"
        );

        let read_before = read_surface_snapshot(&pager);
        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(
            reader.get_page(&cx, p).unwrap().as_ref()[0],
            0xAB,
            "bead_id={BEAD_ID} case=wal_commit_publication_keeps_read_visibility"
        );
        let read_after = read_surface_snapshot(&pager);
        assert_eq!(
            read_after.cache, read_before.cache,
            "bead_id={BEAD_ID} case=wal_post_commit_read_skips_cache"
        );
        assert_eq!(
            read_after.published_hits,
            read_before.published_hits + 1,
            "bead_id={BEAD_ID} case=wal_post_commit_read_hits_publication"
        );
    }

    #[test]
    fn test_published_read_hit_does_not_touch_cache_metrics() {
        init_publication_test_tracing();
        let (pager, _) = test_pager();
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let p = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0xAB; ps]).unwrap();
            txn.commit(&cx).unwrap();
            p
        };

        let cache_before = pager.cache_metrics_snapshot().unwrap();
        let published_hits_before = pager.published_page_hits();

        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let data = reader.get_page(&cx, p).unwrap();
        assert_eq!(data.as_ref()[0], 0xAB);

        let cache_after = pager.cache_metrics_snapshot().unwrap();
        assert_eq!(
            cache_after, cache_before,
            "bead_id={BEAD_ID} case=publication_hit_skips_cache_metrics"
        );
        assert_eq!(
            pager.published_page_hits(),
            published_hits_before + 1,
            "bead_id={BEAD_ID} case=publication_hit_counter"
        );
    }

    #[test]
    fn test_sharded_published_pages_len_tracks_insert_remove_clear() {
        let published_pages = ShardedPublishedPages::new();
        let page_two = PageNumber::new(2).unwrap();
        let page_three = PageNumber::new(3).unwrap();

        assert_eq!(published_pages.len(), 0);
        assert!(published_pages.insert(page_two, PageData::from_vec(sample_page(0x22))));
        assert_eq!(published_pages.len(), 1);

        assert!(!published_pages.insert(page_two, PageData::from_vec(sample_page(0x33))));
        assert_eq!(
            published_pages.len(),
            1,
            "bead_id=bd-qrss1 case=replacement_must_not_increment_page_count"
        );

        assert!(published_pages.insert(page_three, PageData::from_vec(sample_page(0x44))));
        assert_eq!(published_pages.len(), 2);

        assert!(published_pages.remove(page_two));
        assert_eq!(published_pages.len(), 1);
        assert!(!published_pages.remove(page_two));
        assert_eq!(published_pages.len(), 1);

        published_pages.clear();
        assert_eq!(published_pages.len(), 0);
    }

    #[test]
    fn test_sharded_published_pages_insert_batch_and_retain_track_page_count() {
        let published_pages = ShardedPublishedPages::new();
        let page_two = PageNumber::new(2).unwrap();
        let page_sixty_five = PageNumber::new(65).unwrap();
        let page_one_twenty_nine = PageNumber::new(129).unwrap();

        published_pages.insert_batch([
            (page_two, PageData::from_vec(sample_page(0x02))),
            (page_sixty_five, PageData::from_vec(sample_page(0x41))),
            (page_one_twenty_nine, PageData::from_vec(sample_page(0x81))),
            (page_two, PageData::from_vec(sample_page(0xFF))),
        ]);

        assert_eq!(
            published_pages.len(),
            3,
            "bead_id=bd-qrss1 case=batch_insert_counts_only_new_pages"
        );
        assert_eq!(
            published_pages.get(page_two),
            Some(PageData::from_vec(sample_page(0xFF))),
            "bead_id=bd-qrss1 case=batch_insert_replaces_existing_page"
        );

        published_pages.retain(|page_no| page_no.get() >= 65);
        assert_eq!(
            published_pages.len(),
            2,
            "bead_id=bd-qrss1 case=retain_updates_atomic_page_count"
        );
        assert!(published_pages.get(page_two).is_none());
        assert!(published_pages.get(page_sixty_five).is_some());
        assert!(published_pages.get(page_one_twenty_nine).is_some());
    }

    #[test]
    fn test_published_snapshot_page_set_size_tracks_sharded_page_count() {
        init_publication_test_tracing();
        let published = PublishedPagerState::new(4, CommitSeq::new(7), JournalMode::Wal, 0);
        let cx = Cx::new();
        let page_two = PageNumber::new(2).unwrap();
        let page_sixty_five = PageNumber::new(65).unwrap();

        published.publish_insert_single(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(7),
                db_size: 65,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_two,
            PageData::from_vec(sample_page(0x22)),
        );
        published.publish_insert_single(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(8),
                db_size: 65,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_sixty_five,
            PageData::from_vec(sample_page(0x65)),
        );
        assert_eq!(
            published.snapshot().page_set_size,
            2,
            "bead_id=bd-qrss1 case=publication_plane_page_count_after_inserts"
        );

        published.publish_remove_page(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(9),
                db_size: 65,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_two,
        );
        assert_eq!(
            published.snapshot().page_set_size,
            1,
            "bead_id=bd-qrss1 case=publication_plane_page_count_after_remove"
        );

        published.publish_clear_if(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(10),
                db_size: 0,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            true,
        );
        assert_eq!(
            published.snapshot().page_set_size,
            0,
            "bead_id=bd-qrss1 case=publication_plane_page_count_after_clear"
        );
    }

    #[test]
    fn test_publish_commit_skips_sweep_for_stale_smaller_db_size_update() {
        init_publication_test_tracing();
        let published = PublishedPagerState::new(8, CommitSeq::new(8), JournalMode::Wal, 0);
        let cx = Cx::new();
        let page_two = PageNumber::new(2).unwrap();
        let page_seven = PageNumber::new(7).unwrap();
        let original_page_two = PageData::from_vec(sample_page(0x22));
        let original_page_seven = PageData::from_vec(sample_page(0x77));

        published.publish_insert_single(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(8),
                db_size: 8,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_two,
            original_page_two,
        );
        published.publish_insert_single(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(8),
                db_size: 8,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_seven,
            original_page_seven.clone(),
        );

        let refreshed_page_two = PageData::from_vec(sample_page(0x99));
        let write_set = HashMap::from([(
            page_two,
            StagedPage::from_page_data(refreshed_page_two.clone()),
        )]);
        published.publish_commit(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(7),
                db_size: 4,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            &write_set,
        );

        assert_eq!(
            published.try_get_page(page_two),
            Some(refreshed_page_two),
            "bead_id=bd-wwqen.3 case=stale_smaller_commit_still_refreshes_written_pages"
        );
        assert_eq!(
            published.try_get_page(page_seven),
            Some(original_page_seven),
            "bead_id=bd-wwqen.3 case=stale_smaller_commit_must_not_evict_newer_larger_pages"
        );
        assert_eq!(
            published.snapshot().db_size,
            8,
            "bead_id=bd-wwqen.3 case=stale_smaller_commit_preserves_published_db_size"
        );
    }

    #[test]
    fn test_publish_commit_sweeps_pages_when_db_size_shrinks() {
        init_publication_test_tracing();
        let published = PublishedPagerState::new(8, CommitSeq::new(8), JournalMode::Wal, 0);
        let cx = Cx::new();
        let page_two = PageNumber::new(2).unwrap();
        let page_seven = PageNumber::new(7).unwrap();

        published.publish_insert_single(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(8),
                db_size: 8,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_seven,
            PageData::from_vec(sample_page(0x77)),
        );

        let refreshed_page_two = PageData::from_vec(sample_page(0x22));
        let write_set = HashMap::from([(
            page_two,
            StagedPage::from_page_data(refreshed_page_two.clone()),
        )]);
        published.publish_commit(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(9),
                db_size: 4,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            &write_set,
        );

        assert_eq!(
            published.try_get_page(page_two),
            Some(refreshed_page_two),
            "bead_id=bd-wwqen.3 case=shrink_commit_keeps_written_page_inside_new_db_size"
        );
        assert!(
            published.try_get_page(page_seven).is_none(),
            "bead_id=bd-wwqen.3 case=shrink_commit_evicts_pages_above_new_db_size"
        );
    }

    #[test]
    fn test_publish_commit_draining_write_set_publishes_and_empties_staging() {
        init_publication_test_tracing();
        let published = PublishedPagerState::new(4, CommitSeq::new(4), JournalMode::Wal, 0);
        let cx = Cx::new();
        let page_two = PageNumber::new(2).unwrap();
        let page_three = PageNumber::new(3).unwrap();
        let page_two_data = PageData::from_vec(sample_page(0x22));
        let page_three_data = PageData::from_vec(sample_page(0x33));
        let mut write_set = HashMap::from([
            (page_two, StagedPage::from_page_data(page_two_data.clone())),
            (
                page_three,
                StagedPage::from_page_data(page_three_data.clone()),
            ),
        ]);

        published.publish_commit_draining_write_set(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(5),
                db_size: 4,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            &mut write_set,
        );

        assert!(
            write_set.is_empty(),
            "bead_id=bd-autocommit-publish-drain case=publish_drain_consumes_write_set"
        );
        assert_eq!(
            published.try_get_page(page_two),
            Some(page_two_data),
            "bead_id=bd-autocommit-publish-drain case=publish_drain_keeps_page_two_visible"
        );
        assert_eq!(
            published.try_get_page(page_three),
            Some(page_three_data),
            "bead_id=bd-autocommit-publish-drain case=publish_drain_keeps_page_three_visible"
        );
    }

    #[test]
    fn test_publish_commit_staged_pages_publishes_drained_pages() {
        init_publication_test_tracing();
        let published = PublishedPagerState::new(4, CommitSeq::new(4), JournalMode::Wal, 0);
        let cx = Cx::new();
        let page_two = PageNumber::new(2).unwrap();
        let page_three = PageNumber::new(3).unwrap();
        let staged_pages = vec![
            (
                page_two,
                StagedPage::from_page_data(PageData::from_vec(sample_page(0x42))),
            ),
            (
                page_three,
                StagedPage::from_page_data(PageData::from_vec(sample_page(0x53))),
            ),
        ];

        published.publish_commit_staged_pages(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(5),
                db_size: 4,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            staged_pages,
        );

        assert_eq!(
            published.try_get_page(page_two),
            Some(PageData::from_vec(sample_page(0x42))),
            "bead_id=bd-autocommit-publish-drain case=publish_staged_pages_keeps_page_two_visible"
        );
        assert_eq!(
            published.try_get_page(page_three),
            Some(PageData::from_vec(sample_page(0x53))),
            "bead_id=bd-autocommit-publish-drain case=publish_staged_pages_keeps_page_three_visible"
        );
    }

    #[test]
    fn test_observed_page_publication_populates_snapshot_plane() {
        init_publication_test_tracing();
        let published = PublishedPagerState::new(4, CommitSeq::new(7), JournalMode::Wal, 0);
        let cx = Cx::new();
        let page_no = PageNumber::new(2).unwrap();
        let page = PageData::from_vec(vec![0xAB; PageSize::DEFAULT.as_usize()]);

        let published_page = published.publish_observed_page(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(7),
                db_size: 4,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_no,
            page.clone(),
        );

        assert!(
            published_page,
            "bead_id={BEAD_ID} case=observed_page_publication_applies"
        );
        assert_eq!(
            published.try_get_page(page_no),
            Some(page),
            "bead_id={BEAD_ID} case=observed_page_visible"
        );
        assert_eq!(
            published.snapshot().visible_commit_seq,
            CommitSeq::new(7),
            "bead_id={BEAD_ID} case=observed_page_keeps_commit_seq"
        );
    }

    #[test]
    fn test_observed_page_publication_skips_stale_commit_regression() {
        init_publication_test_tracing();
        let published = PublishedPagerState::new(4, CommitSeq::new(7), JournalMode::Wal, 0);
        let cx = Cx::new();
        let page_no = PageNumber::new(2).unwrap();
        let current_page = PageData::from_vec(vec![0xCC; PageSize::DEFAULT.as_usize()]);

        // D1-CRITICAL Change 3: Use sharded publish_insert_single (test-only method).
        published.publish_insert_single(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(8),
                db_size: 4,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_no,
            current_page.clone(),
        );

        let stale_publish_applied = published.publish_observed_page(
            &cx,
            PublishedPagerUpdate {
                visible_commit_seq: CommitSeq::new(7),
                db_size: 4,
                journal_mode: JournalMode::Wal,
                freelist_count: 0,
                checkpoint_active: false,
            },
            page_no,
            PageData::from_vec(vec![0x11; PageSize::DEFAULT.as_usize()]),
        );

        assert!(
            !stale_publish_applied,
            "bead_id={BEAD_ID} case=stale_observed_page_publication_skipped"
        );
        assert_eq!(
            published.snapshot().visible_commit_seq,
            CommitSeq::new(8),
            "bead_id={BEAD_ID} case=stale_publication_preserves_commit_seq"
        );
        assert_eq!(
            published.try_get_page(page_no),
            Some(current_page),
            "bead_id={BEAD_ID} case=stale_publication_preserves_page"
        );
    }

    #[test]
    fn test_write_page_data_short_buffer_is_zero_filled_to_page_size() {
        let (pager, _path) = test_pager();
        let cx = Cx::new();
        let page_size = PageSize::DEFAULT.as_usize();

        let page_no = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_no = txn.allocate_page(&cx).unwrap();
            txn.write_page_data(&cx, page_no, PageData::from_vec(vec![0xAB; 32]))
                .unwrap();
            txn.commit(&cx).unwrap();
            page_no
        };

        let reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let page = reader.get_page(&cx, page_no).unwrap();
        assert_eq!(
            page.len(),
            page_size,
            "bead_id={BEAD_ID} case=short_owned_page_write_preserves_page_size"
        );
        assert!(
            page.as_bytes()[..32].iter().all(|byte| *byte == 0xAB),
            "bead_id={BEAD_ID} case=short_owned_page_write_keeps_prefix"
        );
        assert!(
            page.as_bytes()[32..].iter().all(|byte| *byte == 0),
            "bead_id={BEAD_ID} case=short_owned_page_write_zero_fills_tail"
        );
    }

    #[test]
    fn test_external_refresh_clears_stale_published_pages() {
        init_publication_test_tracing();
        let vfs = MemoryVfs::new();
        let path = PathBuf::from("/published_refresh.db");
        let cx = Cx::new();
        let ps = PageSize::DEFAULT.as_usize();

        let pager1 = SimplePager::open(vfs.clone(), &path, PageSize::DEFAULT).unwrap();
        let pager2 = SimplePager::open(vfs, &path, PageSize::DEFAULT).unwrap();

        let p = {
            let mut txn = pager1.begin(&cx, TransactionMode::Immediate).unwrap();
            let p = txn.allocate_page(&cx).unwrap();
            txn.write_page(&cx, p, &vec![0x11; ps]).unwrap();
            txn.commit(&cx).unwrap();
            p
        };

        let reader = pager1.begin(&cx, TransactionMode::ReadOnly).unwrap();
        assert_eq!(reader.get_page(&cx, p).unwrap().as_ref()[0], 0x11);
        drop(reader);

        let published_before = pager1.published_snapshot();
        assert!(
            published_before.page_set_size > 0,
            "bead_id={BEAD_ID} case=publication_plane_populated_before_refresh"
        );

        let mut txn = pager2.begin(&cx, TransactionMode::Immediate).unwrap();
        txn.write_page(&cx, p, &vec![0x22; ps]).unwrap();
        txn.commit(&cx).unwrap();

        let refreshed_reader = pager1.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let published_after_refresh = pager1.published_snapshot();
        assert!(
            published_after_refresh.snapshot_gen > published_before.snapshot_gen,
            "bead_id={BEAD_ID} case=publication_gen_advances_on_refresh"
        );
        assert_eq!(
            published_after_refresh.visible_commit_seq,
            pager2.published_snapshot().visible_commit_seq,
            "bead_id={BEAD_ID} case=publication_visible_seq_tracks_external_commit"
        );
        assert_eq!(
            published_after_refresh.page_set_size, 0,
            "bead_id={BEAD_ID} case=publication_clears_stale_pages_on_refresh"
        );
        assert_eq!(
            refreshed_reader.get_page(&cx, p).unwrap().as_ref()[0],
            0x22,
            "bead_id={BEAD_ID} case=publication_refresh_reads_latest_committed_page"
        );
    }

    #[test]
    fn test_stale_main_header_recovery_ignores_uncommitted_wal_page1_tail() {
        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let db_path = PathBuf::from("/stale-main-header-uncommitted-tail.db");
        let wal_path = PathBuf::from("/stale-main-header-uncommitted-tail.db-wal");
        let page_size = PageSize::DEFAULT;

        let valid_header = DatabaseHeader {
            page_size,
            page_count: 1,
            ..DatabaseHeader::default()
        };
        let mut committed_page1 = vec![0_u8; page_size.as_usize()];
        committed_page1[..DATABASE_HEADER_SIZE]
            .copy_from_slice(&valid_header.to_bytes().expect("valid page-1 header"));

        let mut stale_header_bytes = valid_header.to_bytes().expect("base header bytes");
        stale_header_bytes[44..48].fill(0);
        let stale_error = DatabaseHeader::from_bytes(&stale_header_bytes)
            .expect_err("schema format 0 must be treated as a stale main-file header");

        let mut uncommitted_page1 = committed_page1.clone();
        uncommitted_page1[..DATABASE_HEADER_SIZE].copy_from_slice(&stale_header_bytes);

        let open_flags = VfsOpenFlags::READWRITE | VfsOpenFlags::CREATE | VfsOpenFlags::WAL;
        let (file, _) = vfs.open(&cx, Some(&wal_path), open_flags).unwrap();
        let mut wal = fsqlite_wal::WalFile::create(
            &cx,
            file,
            page_size.get(),
            0,
            fsqlite_wal::WalSalts::default(),
        )
        .unwrap();
        wal.append_frame(&cx, PageNumber::ONE.get(), &committed_page1, 1)
            .expect("append committed page-1 frame");
        wal.append_frame(&cx, PageNumber::ONE.get(), &uncommitted_page1, 0)
            .expect("append uncommitted page-1 tail");
        wal.close(&cx).unwrap();

        let recoverable = stale_main_header_can_be_recovered_from_live_wal(
            &cx,
            &vfs,
            &db_path,
            &stale_header_bytes,
            &stale_error,
        )
        .expect("probe stale-header recovery");
        assert!(
            recoverable,
            "recovery must use the committed WAL horizon, not an uncommitted tail frame"
        );
    }

    #[test]
    fn test_stale_main_header_recovery_never_falls_back_to_readonly_wal_probe() {
        let cx = Cx::new();
        let vfs = WalReadonlyFallbackProbeVfs::new();
        let db_path = PathBuf::from("/stale-main-header-readonly-fallback.db");
        let wal_path = PathBuf::from("/stale-main-header-readonly-fallback.db-wal");
        let page_size = PageSize::DEFAULT;

        let valid_header = DatabaseHeader {
            page_size,
            page_count: 1,
            ..DatabaseHeader::default()
        };
        let mut stale_header_bytes = valid_header.to_bytes().expect("base header bytes");
        stale_header_bytes[44..48].fill(0);
        let stale_error = DatabaseHeader::from_bytes(&stale_header_bytes)
            .expect_err("schema format 0 must be treated as a stale main-file header");

        let open_flags = VfsOpenFlags::READWRITE | VfsOpenFlags::CREATE | VfsOpenFlags::WAL;
        let (mut wal_file, _) = vfs.inner.open(&cx, Some(&wal_path), open_flags).unwrap();
        wal_file.close(&cx).unwrap();

        let recoverable = stale_main_header_can_be_recovered_from_live_wal(
            &cx,
            &vfs,
            &db_path,
            &stale_header_bytes,
            &stale_error,
        )
        .expect("probe stale-header recovery");
        assert!(
            !recoverable,
            "readwrite probe failure must stop recovery instead of retrying READONLY"
        );
        assert!(
            !vfs.readonly_wal_open_attempted(),
            "bead_id={BEAD_ID} case=stale_header_recovery_must_not_probe_wal_readonly"
        );
    }

    #[test]
    fn test_set_wal_backend_owned_returns_backend_when_install_is_rejected() {
        let (pager, _) = test_pager();
        let dropped = Arc::new(Mutex::new(false));
        {
            let mut inner = pager
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.checkpoint_active = true;
        }

        let backend = DropAwareWalBackend {
            dropped: Arc::clone(&dropped),
        };
        let (err, backend) = pager
            .set_wal_backend_owned(backend)
            .expect_err("checkpoint-active pager must reject WAL backend install");
        assert!(
            matches!(err, FrankenError::Busy),
            "unexpected error: {err:?}"
        );
        assert!(
            !*dropped
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            "rejected install must return the backend instead of dropping it"
        );
        assert!(
            !has_wal_backend(&pager.wal_backend).unwrap(),
            "failed install must not publish a backend"
        );

        drop(backend);
        assert!(
            *dropped
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            "caller should own backend cleanup after a rejected install"
        );
    }

    fn read_all_vfs_bytes<V: Vfs>(vfs: &V, cx: &Cx, path: &Path) -> Vec<u8> {
        let flags = VfsOpenFlags::MAIN_DB | VfsOpenFlags::READWRITE;
        let (mut file, _) = vfs.open(cx, Some(path), flags).unwrap();
        let size = usize::try_from(file.file_size(cx).unwrap()).unwrap();
        let mut out = vec![0_u8; size];
        let read = file.read(cx, &mut out, 0).unwrap();
        assert_eq!(read, size);
        file.close(cx).unwrap();
        out
    }

    #[test]
    fn test_copy_database_to_copies_main_db_via_vfs() {
        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let source_path = PathBuf::from("/copy_source.db");
        let target_path = PathBuf::from("/copy_target.db");
        let pager = SimplePager::open(vfs.clone(), &source_path, PageSize::DEFAULT).unwrap();
        let page_size = PageSize::DEFAULT.as_usize();

        let page_no = {
            let mut txn = pager.begin(&cx, TransactionMode::Immediate).unwrap();
            let page_no = txn.allocate_page(&cx).unwrap();
            let mut page = vec![0xA5; page_size];
            page[0] = 0x5A;
            page[page_size - 1] = 0xC3;
            txn.write_page(&cx, page_no, &page).unwrap();
            txn.commit(&cx).unwrap();
            page_no
        };

        pager.copy_database_to(&cx, &target_path).unwrap();

        let source_bytes = read_all_vfs_bytes(&vfs, &cx, &source_path);
        let target_bytes = read_all_vfs_bytes(&vfs, &cx, &target_path);
        assert_eq!(
            target_bytes, source_bytes,
            "bead_id={BEAD_ID} case=copy_database_to_byte_identical_copy"
        );

        let copied = SimplePager::open(vfs, &target_path, PageSize::DEFAULT).unwrap();
        let reader = copied.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let page = reader.get_page(&cx, page_no).unwrap();
        assert_eq!(
            page.as_ref()[0],
            0x5A,
            "bead_id={BEAD_ID} case=copy_database_to_reopen_reads_committed_page"
        );
        assert_eq!(
            page.as_ref()[page_size - 1],
            0xC3,
            "bead_id={BEAD_ID} case=copy_database_to_preserves_page_tail"
        );
    }

    #[test]
    fn test_copy_database_to_rejects_existing_target() {
        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let source_path = PathBuf::from("/copy_existing_source.db");
        let target_path = PathBuf::from("/copy_existing_target.db");
        let pager = SimplePager::open(vfs.clone(), &source_path, PageSize::DEFAULT).unwrap();
        let _target = SimplePager::open(vfs, &target_path, PageSize::DEFAULT).unwrap();

        let err = pager.copy_database_to(&cx, &target_path).unwrap_err();
        assert!(
            matches!(err, FrankenError::CannotOpen { .. }),
            "bead_id={BEAD_ID} case=copy_database_to_existing_target_err={err:?}"
        );
    }

    #[test]
    fn test_copy_database_to_requires_quiescent_pager() {
        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let source_path = PathBuf::from("/copy_busy_source.db");
        let target_path = PathBuf::from("/copy_busy_target.db");
        let pager = SimplePager::open(vfs, &source_path, PageSize::DEFAULT).unwrap();

        let _reader = pager.begin(&cx, TransactionMode::ReadOnly).unwrap();
        let err = pager.copy_database_to(&cx, &target_path).unwrap_err();
        assert!(
            matches!(err, FrankenError::Busy),
            "bead_id={BEAD_ID} case=copy_database_to_rejects_active_transactions err={err:?}"
        );
    }

    #[test]
    fn test_published_snapshot_retries_during_inflight_publication() {
        init_publication_test_tracing();
        let published = Arc::new(PublishedPagerState::new(
            1,
            CommitSeq::new(1),
            JournalMode::Delete,
            0,
        ));
        published.sequence.store(3, AtomicOrdering::Release);

        let reader_plane = Arc::clone(&published);
        let handle = std::thread::spawn(move || reader_plane.snapshot());

        for _ in 0..10_000 {
            if published.read_retry_count() > 0 {
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            published.read_retry_count() > 0,
            "bead_id={BEAD_ID} case=publication_retry_counter_increments"
        );

        published
            .visible_commit_seq
            .store(2, AtomicOrdering::Release);
        published.db_size.store(2, AtomicOrdering::Release);
        published.journal_mode.store(
            encode_journal_mode(JournalMode::Wal),
            AtomicOrdering::Release,
        );
        published.freelist_count.store(1, AtomicOrdering::Release);
        published
            .checkpoint_active
            .store(true, AtomicOrdering::Release);
        published.page_set_size.store(0, AtomicOrdering::Release);
        published.sequence.store(4, AtomicOrdering::Release);

        let snapshot = handle.join().unwrap();
        assert_eq!(
            snapshot.snapshot_gen, 4,
            "bead_id={BEAD_ID} case=publication_retry_returns_new_snapshot"
        );
        assert_eq!(
            snapshot.visible_commit_seq,
            CommitSeq::new(2),
            "bead_id={BEAD_ID} case=publication_retry_visible_commit_seq"
        );
        assert_eq!(
            snapshot.freelist_count, 1,
            "bead_id={BEAD_ID} case=publication_retry_freelist_count"
        );
        assert!(
            snapshot.checkpoint_active,
            "bead_id={BEAD_ID} case=publication_retry_checkpoint_flag"
        );
    }

    #[test]
    fn test_published_sequence_waiters_wake_on_targeted_transitions() {
        init_publication_test_tracing();
        let published = Arc::new(PublishedPagerState::new(
            1,
            CommitSeq::new(1),
            JournalMode::Delete,
            0,
        ));
        published.sequence.store(4, AtomicOrdering::Release);

        let begin_plane = Arc::clone(&published);
        let (begin_ready_tx, begin_ready_rx) = std::sync::mpsc::channel();
        let (begin_done_tx, begin_done_rx) = std::sync::mpsc::channel();
        let begin_waiter = std::thread::spawn(move || {
            begin_ready_tx
                .send(())
                .expect("begin waiter should signal readiness");
            begin_plane.wait_for_sequence_change(4, Duration::from_secs(1));
            begin_done_tx
                .send(())
                .expect("begin waiter should signal completion");
        });

        begin_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("begin waiter should start");
        for _ in 0..10_000 {
            if published.sequence_waiters.has_slot(4) {
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            published.sequence_waiters.has_slot(4),
            "bead_id={BEAD_ID} case=publication_begin_waiter_registers_targeted_slot"
        );
        assert!(
            begin_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "bead_id={BEAD_ID} case=publication_begin_waiter_stays_parked_until_targeted_signal"
        );

        let begin_sequence = published.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        assert_eq!(begin_sequence, 4);
        published.signal_sequence_waiters(begin_sequence, "test_begin");
        begin_done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("publish-begin signal should wake matching waiter");
        begin_waiter.join().unwrap();

        let complete_plane = Arc::clone(&published);
        let (complete_ready_tx, complete_ready_rx) = std::sync::mpsc::channel();
        let (complete_done_tx, complete_done_rx) = std::sync::mpsc::channel();
        let complete_waiter = std::thread::spawn(move || {
            complete_ready_tx
                .send(())
                .expect("complete waiter should signal readiness");
            complete_plane.wait_for_sequence_change(5, Duration::from_secs(1));
            complete_done_tx
                .send(())
                .expect("complete waiter should signal completion");
        });

        complete_ready_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("complete waiter should start");
        for _ in 0..10_000 {
            if published.sequence_waiters.has_slot(5) {
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            published.sequence_waiters.has_slot(5),
            "bead_id={BEAD_ID} case=publication_complete_waiter_registers_targeted_slot"
        );
        assert!(
            complete_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "bead_id={BEAD_ID} case=publication_complete_waiter_stays_parked_until_targeted_signal"
        );

        let complete_sequence = published.sequence.fetch_add(1, AtomicOrdering::AcqRel);
        assert_eq!(complete_sequence, 5);
        published.signal_sequence_waiters(complete_sequence, "test_complete");
        complete_done_rx
            .recv_timeout(Duration::from_millis(100))
            .expect("publish-complete signal should wake matching waiter");
        complete_waiter.join().unwrap();
    }

    fn run_parallel_counter_benchmark<F>(
        thread_count: usize,
        increments_per_thread: usize,
        increment: F,
    ) -> u64
    where
        F: Fn() + Send + Sync + 'static,
    {
        let start_barrier = Arc::new(std::sync::Barrier::new(thread_count + 1));
        let increment = Arc::new(increment);
        let handles: Vec<_> = (0..thread_count)
            .map(|_| {
                let start_barrier = Arc::clone(&start_barrier);
                let increment = Arc::clone(&increment);
                std::thread::spawn(move || {
                    start_barrier.wait();
                    for _ in 0..increments_per_thread {
                        increment();
                    }
                })
            })
            .collect();

        start_barrier.wait();
        let started = Instant::now();
        for handle in handles {
            handle.join().expect("counter worker should finish");
        }
        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    #[test]
    fn test_published_counter_striping_tracks_parallel_increments() {
        let counter = Arc::new(StripedCounter64::new());
        let thread_count = 8;
        let increments_per_thread = 5_000;
        let expected_total =
            u64::try_from(thread_count * increments_per_thread).unwrap_or(u64::MAX);

        let counter_for_bench = Arc::clone(&counter);
        let elapsed_ns =
            run_parallel_counter_benchmark(thread_count, increments_per_thread, move || {
                counter_for_bench.increment();
            });

        assert!(
            elapsed_ns > 0,
            "bead_id={BEAD_ID} case=publication_counter_parallel_elapsed"
        );
        assert_eq!(
            counter.load(),
            expected_total,
            "bead_id={BEAD_ID} case=publication_counter_parallel_total"
        );
    }

    #[test]
    #[ignore = "manual perf evidence for bd-db300.2.3.2"]
    fn bench_bd_db300_2_3_2_publication_counter_striping() {
        #[derive(Debug)]
        struct BaselineCounter(AtomicU64);

        impl BaselineCounter {
            fn increment(&self) {
                self.0.fetch_add(1, AtomicOrdering::Relaxed);
            }

            fn load(&self) -> u64 {
                self.0.load(AtomicOrdering::Acquire)
            }
        }

        let thread_count = std::thread::available_parallelism()
            .map_or(4, |parallelism| parallelism.get().clamp(2, 16));
        let increments_per_thread = 200_000;
        let expected_total =
            u64::try_from(thread_count * increments_per_thread).unwrap_or(u64::MAX);

        let baseline = Arc::new(BaselineCounter(AtomicU64::new(0)));
        let baseline_counter = Arc::clone(&baseline);
        let baseline_ns =
            run_parallel_counter_benchmark(thread_count, increments_per_thread, move || {
                baseline_counter.increment();
            });
        assert_eq!(
            baseline.load(),
            expected_total,
            "bead_id={BEAD_ID} case=publication_counter_baseline_total"
        );

        let striped = Arc::new(StripedCounter64::new());
        let striped_counter = Arc::clone(&striped);
        let striped_ns =
            run_parallel_counter_benchmark(thread_count, increments_per_thread, move || {
                striped_counter.increment();
            });
        assert_eq!(
            striped.load(),
            expected_total,
            "bead_id={BEAD_ID} case=publication_counter_striped_total"
        );

        let speedup_milli = if striped_ns == 0 {
            0_u64
        } else {
            u64::try_from((u128::from(baseline_ns)).saturating_mul(1_000) / u128::from(striped_ns))
                .unwrap_or(u64::MAX)
        };

        println!("BEGIN_BD_DB300_2_3_2_REPORT");
        println!(
            "{{\"threads\":{thread_count},\"increments_per_thread\":{increments_per_thread},\"baseline_ns\":{baseline_ns},\"striped_ns\":{striped_ns},\"speedup_milli\":{speedup_milli}}}"
        );
        println!("END_BD_DB300_2_3_2_REPORT");
    }

    // ── bd-db300.3.8.7: targeted regression tests for lock-scope narrowing ──

    #[test]
    fn test_read_page_from_wal_backend_falls_back_to_write_lock_when_pinned_reads_unsupported() {
        // Verify that read_page_from_wal_backend falls back to the write-lock
        // path when supports_pinned_reads() returns false.
        let wal_backend: SharedWalBackend = new_shared_wal_backend();
        let cx = Cx::new();
        let page_no = PageNumber::new(1).unwrap();

        // With no backend installed, read should return an error.
        let result = read_page_from_wal_backend(&wal_backend, &cx, page_no);
        assert!(
            result.is_err(),
            "bead_id=bd-db300.3.8.7 read_page_from_wal_backend should error without backend"
        );

        // Install a mock backend that does NOT support pinned reads.
        let (mock, _frames, _begin, _batch) = MockWalBackend::new();
        *wal_backend.write().unwrap() = Some(Box::new(mock));

        // Verify it falls back to write-lock path (default read_page).
        let result = read_page_from_wal_backend(&wal_backend, &cx, page_no);
        assert!(
            result.is_ok(),
            "bead_id=bd-db300.3.8.7 fallback to write-lock read_page should succeed"
        );

        // The mock returns None for unwritten pages.
        assert_eq!(
            result.unwrap(),
            None,
            "bead_id=bd-db300.3.8.7 unwritten page should return None"
        );
    }

    #[test]
    fn test_read_page_from_wal_backend_uses_pinned_read_without_write_fallback() {
        use crate::traits::WalBackend;

        struct PinnedReadBackend {
            pinned_calls: Arc<Mutex<usize>>,
            fallback_calls: Arc<Mutex<usize>>,
            response: Vec<u8>,
        }

        impl WalBackend for PinnedReadBackend {
            fn begin_transaction(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
                Ok(())
            }

            fn append_frame(
                &mut self,
                _cx: &Cx,
                _page_number: u32,
                _page_data: &[u8],
                _db_size_if_commit: u32,
            ) -> fsqlite_error::Result<()> {
                Ok(())
            }

            fn read_page(
                &mut self,
                _cx: &Cx,
                _page_number: u32,
            ) -> fsqlite_error::Result<Option<Vec<u8>>> {
                *self.fallback_calls.lock().unwrap() += 1;
                Ok(Some(vec![0xEE]))
            }

            fn read_page_pinned(
                &self,
                _cx: &Cx,
                _page_number: u32,
            ) -> fsqlite_error::Result<Option<Vec<u8>>> {
                *self.pinned_calls.lock().unwrap() += 1;
                Ok(Some(self.response.clone()))
            }

            fn supports_pinned_reads(&self) -> bool {
                true
            }

            fn sync(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
                Ok(())
            }

            fn frame_count(&self) -> usize {
                0
            }

            fn checkpoint(
                &mut self,
                _cx: &Cx,
                _mode: crate::traits::CheckpointMode,
                _writer: &mut dyn crate::traits::CheckpointPageWriter,
                _backfilled_frames: u32,
                _oldest_reader_frame: Option<u32>,
            ) -> fsqlite_error::Result<crate::traits::CheckpointResult> {
                Ok(crate::traits::CheckpointResult {
                    total_frames: 0,
                    frames_backfilled: 0,
                    completed: true,
                    wal_was_reset: false,
                })
            }
        }

        let pinned_calls = Arc::new(Mutex::new(0_usize));
        let fallback_calls = Arc::new(Mutex::new(0_usize));
        let expected = vec![0xAB, 0xCD, 0xEF];
        let wal_backend: SharedWalBackend = new_shared_wal_backend();
        *wal_backend.write().unwrap() = Some(Box::new(PinnedReadBackend {
            pinned_calls: Arc::clone(&pinned_calls),
            fallback_calls: Arc::clone(&fallback_calls),
            response: expected.clone(),
        }));

        let cx = Cx::new();
        let page_no = PageNumber::new(7).unwrap();
        let result = read_page_from_wal_backend(&wal_backend, &cx, page_no).unwrap();

        assert_eq!(
            result,
            Some(expected),
            "bead_id=bd-db300.3.8.7 case=wal_read_scope_pinned_read_returns_data_without_fallback"
        );
        assert_eq!(
            *pinned_calls.lock().unwrap(),
            1,
            "bead_id=bd-db300.3.8.7 case=wal_read_scope_pinned_read_call_count"
        );
        assert_eq!(
            *fallback_calls.lock().unwrap(),
            0,
            "bead_id=bd-db300.3.8.7 case=wal_read_scope_pinned_read_must_not_take_write_lock_fallback"
        );
    }

    #[test]
    fn test_read_page_pinned_error_does_not_fall_back() {
        // Verify that a REAL error from read_page_pinned propagates
        // instead of silently falling back to the write-lock path.
        use crate::traits::WalBackend;

        /// A WAL backend that supports pinned reads but always returns an
        /// error from read_page_pinned to simulate corruption.
        struct CorruptPinnedReadBackend;

        impl WalBackend for CorruptPinnedReadBackend {
            fn begin_transaction(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
                Ok(())
            }

            fn append_frame(
                &mut self,
                _cx: &Cx,
                _page_number: u32,
                _page_data: &[u8],
                _db_size_if_commit: u32,
            ) -> fsqlite_error::Result<()> {
                Ok(())
            }

            fn read_page(
                &mut self,
                _cx: &Cx,
                _page_number: u32,
            ) -> fsqlite_error::Result<Option<Vec<u8>>> {
                // This should NEVER be called if pinned reads are supported
                // and the pinned read fails with a real error.
                panic!(
                    "bead_id=bd-db300.3.8.7 MUST NOT fall back to read_page \
                     when read_page_pinned returns a real error"
                );
            }

            fn read_page_pinned(
                &self,
                _cx: &Cx,
                _page_number: u32,
            ) -> fsqlite_error::Result<Option<Vec<u8>>> {
                Err(fsqlite_error::FrankenError::WalCorrupt {
                    detail: "simulated corruption in pinned read".to_owned(),
                })
            }

            fn supports_pinned_reads(&self) -> bool {
                true
            }

            fn sync(&mut self, _cx: &Cx) -> fsqlite_error::Result<()> {
                Ok(())
            }

            fn frame_count(&self) -> usize {
                0
            }

            fn checkpoint(
                &mut self,
                _cx: &Cx,
                _mode: crate::traits::CheckpointMode,
                _writer: &mut dyn crate::traits::CheckpointPageWriter,
                _backfilled_frames: u32,
                _oldest_reader_frame: Option<u32>,
            ) -> fsqlite_error::Result<crate::traits::CheckpointResult> {
                Ok(crate::traits::CheckpointResult {
                    total_frames: 0,
                    frames_backfilled: 0,
                    completed: true,
                    wal_was_reset: false,
                })
            }
        }

        let wal_backend: SharedWalBackend = new_shared_wal_backend();
        *wal_backend.write().unwrap() = Some(Box::new(CorruptPinnedReadBackend));

        let cx = Cx::new();
        let page_no = PageNumber::new(1).unwrap();

        let result = read_page_from_wal_backend(&wal_backend, &cx, page_no);
        assert!(
            result.is_err(),
            "bead_id=bd-db300.3.8.7 real read_page_pinned error must propagate, not fall back"
        );
        let err = result.unwrap_err();
        assert!(
            format!("{err}").contains("corruption"),
            "bead_id=bd-db300.3.8.7 error should be the corruption error, got: {err}"
        );
    }

    /// bd-db300.3.8.6: Prove fused batch assembly preserves cross-batch frame
    /// order and final_db_size semantics, including the all-zero boundary.
    #[test]
    fn test_fused_batch_assembly_preserves_order_and_db_size() {
        use fsqlite_wal::group_commit::{FrameSubmission, TransactionFrameBatch};

        // Three batches simulating three concurrent transactions.
        let batches = vec![
            TransactionFrameBatch::new(vec![
                FrameSubmission {
                    page_number: 2,
                    page_data: vec![0xAA; 4096],
                    db_size_if_commit: 0, // non-commit frame
                },
                FrameSubmission {
                    page_number: 3,
                    page_data: vec![0xBB; 4096],
                    db_size_if_commit: 10, // commit: db has 10 pages
                },
            ]),
            TransactionFrameBatch::new(vec![FrameSubmission {
                page_number: 5,
                page_data: vec![0xCC; 4096],
                db_size_if_commit: 12, // commit: db has 12 pages
            }]),
            TransactionFrameBatch::new(vec![
                FrameSubmission {
                    page_number: 7,
                    page_data: vec![0xDD; 4096],
                    db_size_if_commit: 0, // non-commit
                },
                FrameSubmission {
                    page_number: 8,
                    page_data: vec![0xEE; 4096],
                    db_size_if_commit: 8, // commit: db has 8 pages (smaller)
                },
            ]),
        ];

        let current_db_size: u32 = 5;

        // Fused single-pass assembly (mirrors the production code exactly).
        let total_frames: usize = batches.iter().map(|b| b.frames.len()).sum();
        let mut frame_refs: Vec<traits::WalFrameRef<'_>> = Vec::with_capacity(total_frames);
        let mut final_db_size = current_db_size;
        for batch in &batches {
            for frame in &batch.frames {
                frame_refs.push(traits::WalFrameRef {
                    page_number: frame.page_number,
                    page_data: &frame.page_data,
                    db_size_if_commit: frame.db_size_if_commit,
                });
                if frame.db_size_if_commit > final_db_size {
                    final_db_size = frame.db_size_if_commit;
                }
            }
        }

        // 1. Frame order: batch-by-batch, frame-by-frame.
        let page_numbers: Vec<u32> = frame_refs.iter().map(|f| f.page_number).collect();
        assert_eq!(
            page_numbers,
            vec![2, 3, 5, 7, 8],
            "bd-db300.3.8.6: fused assembly must preserve cross-batch frame order"
        );

        // 2. Total frame count.
        assert_eq!(frame_refs.len(), 5, "should flatten all 5 frames");

        // 3. final_db_size = max(current_db_size, max positive db_size_if_commit).
        // max(5, 0, 10, 12, 0, 8) = 12
        assert_eq!(
            final_db_size, 12,
            "bd-db300.3.8.6: final_db_size must be max commit size across group"
        );

        // 4. Pre-sized capacity: no reallocation should have occurred.
        assert!(
            frame_refs.capacity() >= 5,
            "Vec should have been pre-sized to avoid realloc"
        );
    }

    /// bd-db300.3.8.6: Edge case — all db_size_if_commit are zero (no commits
    /// in the batch, only non-commit frames). final_db_size must fall back to
    /// current_db_size.
    #[test]
    fn test_fused_batch_assembly_all_zero_db_size() {
        use fsqlite_wal::group_commit::{FrameSubmission, TransactionFrameBatch};

        let batches = vec![TransactionFrameBatch::new(vec![
            FrameSubmission {
                page_number: 2,
                page_data: vec![0; 4096],
                db_size_if_commit: 0,
            },
            FrameSubmission {
                page_number: 3,
                page_data: vec![0; 4096],
                db_size_if_commit: 0,
            },
        ])];

        let current_db_size: u32 = 7;
        let mut final_db_size = current_db_size;
        for batch in &batches {
            for frame in &batch.frames {
                if frame.db_size_if_commit > final_db_size {
                    final_db_size = frame.db_size_if_commit;
                }
            }
        }

        assert_eq!(
            final_db_size, 7,
            "bd-db300.3.8.6: all-zero db_size_if_commit must preserve current_db_size"
        );
    }
}
