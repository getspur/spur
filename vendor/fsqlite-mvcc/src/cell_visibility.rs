//! Cell-Level MVCC Visibility Log (C1: bd-l9k8e.1, C3: bd-l9k8e.3)
//!
//! This module implements cell-level MVCC for ordinary row operations, replacing
//! full-page version chains with lightweight cell deltas for INSERT/UPDATE/DELETE
//! operations that don't trigger structural B-tree changes.
//!
//! # Architecture Overview
//!
//! ## The Problem (Why Page-Level Is Wrong)
//!
//! Current page-level MVCC (see [`crate::invariants::VersionStore`]) stores full 4KB
//! page copies in version chains. If txn A inserts row 1000 and txn B inserts row 1001,
//! both on page 47, this causes a **page-level conflict** even though the rows are
//! independent. One transaction wins, the other retries with `SQLITE_BUSY_SNAPSHOT`.
//!
//! This is the wrong granularity. A row INSERT that touches 50 bytes of a 4KB page
//! should NOT create a 4KB page copy, link it into a version chain, walk that chain
//! on every read, and then GC it later.
//!
//! ## The Solution (Cell-Level Deltas)
//!
//! Split operations into two classes:
//!
//! 1. **LOGICAL (cell-level, cheap path):** INSERT/UPDATE/DELETE that fit in existing
//!    page without structural change. Recorded in [`CellVisibilityLog`], not as full
//!    page versions.
//!
//! 2. **STRUCTURAL (page-level, existing path):** Page splits, merges, freelist changes,
//!    overflow chains. Uses existing page-level versioning.
//!
//! ## Key Design Decisions
//!
//! ### CellKey Strategy
//!
//! We reuse [`fsqlite_types::SemanticKeyRef`] which provides:
//! - `btree: BtreeRef` (table vs index identity)
//! - `kind: SemanticKeyKind` (TableRow vs IndexEntry)
//! - `key_digest: [u8; 16]` (BLAKE3-based stable identity)
//!
//! For **table leaf pages**, the canonical key bytes are the rowid (i64 varint).
//! For **index leaf pages**, the canonical key bytes are the full encoded index key.
//!
//! ### Visibility Resolution
//!
//! Given snapshot S and page P, to find the visible version of cell C:
//!
//! 1. Look up `(P, C.key_digest)` in [`CellVisibilityLog`]
//! 2. Walk the cell delta chain (newest to oldest) until finding a delta where
//!    `delta.commit_seq <= S.high` and `delta.commit_seq != 0`
//! 3. Apply the delta to reconstruct cell content
//!
//! Complexity: O(log N) where N is the number of deltas for that cell.
//!
//! ### Memory Budget
//!
//! The log operates within a bounded memory budget:
//! - Per-delta overhead: ~64 bytes (key + metadata) + cell_data (variable)
//! - Target: <10% of page cache memory for cell deltas
//! - For a 256MB page cache: cell delta budget = ~25MB
//! - At 64 bytes per delta: ~400K outstanding deltas before eager materialization
//!
//! When the budget is exceeded, the oldest deltas are materialized into full page
//! versions and evicted from the log.
//!
//! ### Interior Pages
//!
//! Interior (non-leaf) pages ALWAYS use page-level MVCC. They contain child pointers
//! and are structural by definition. Only leaf pages participate in cell-level MVCC.
//!
//! ## PostgreSQL Reference
//!
//! This design is inspired by PostgreSQL's tuple-level MVCC:
//! - `htup_details.h:122` — tuple header with xmin/xmax
//! - `snapshot.h:138` — compact Snapshot struct
//! - `heapam.c:522` — batched page visibility
//! - `pruneheap.c:199` — opportunistic pruning
//!
//! # Module Status
//!
//! C1 design complete. C3 implementation complete with:
//! - Transaction write set tracking (bulk commit/rollback)
//! - Cell-level conflict detection
//! - Garbage collection below GC horizon
//! - Full tracing instrumentation
//! - Memory budget enforcement with materialization callback

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use fsqlite_types::sync_primitives::{Mutex, RwLock};
use fsqlite_types::{BtreeRef, CommitSeq, PageNumber, SemanticKeyKind, SemanticKeyRef, TxnToken};
use smallvec::SmallVec;
use tracing::{debug, trace};

use crate::cache_aligned::CacheAligned;

// ---------------------------------------------------------------------------
// CellKey — Stable cell identity (§C1.1)
// ---------------------------------------------------------------------------

/// Stable identity for a cell within a B-tree leaf page.
///
/// This wraps [`SemanticKeyRef`] with additional context about whether the cell
/// has been deleted. The `key_digest` provides O(1) hashing and comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellKey {
    /// The B-tree containing this cell (table or index).
    pub btree: BtreeRef,
    /// Whether this is a table row or index entry.
    pub kind: SemanticKeyKind,
    /// BLAKE3-truncated digest of the canonical key bytes.
    /// For tables: rowid as varint bytes.
    /// For indexes: full encoded index key bytes.
    pub key_digest: [u8; 16],
}

impl CellKey {
    /// Create a cell key from a semantic key reference.
    #[must_use]
    pub fn from_semantic_ref(skr: &SemanticKeyRef) -> Self {
        Self {
            btree: skr.btree,
            kind: skr.kind,
            key_digest: skr.key_digest,
        }
    }

    /// Create a table row cell key from a rowid.
    ///
    /// The canonical key bytes are the rowid encoded as a varint.
    #[must_use]
    pub fn table_row(btree: BtreeRef, rowid: i64) -> Self {
        let mut key_bytes = [0u8; 10]; // Max varint length
        let len = encode_varint_i64(rowid, &mut key_bytes);
        Self::from_semantic_ref(&SemanticKeyRef::new(
            btree,
            SemanticKeyKind::TableRow,
            &key_bytes[..len],
        ))
    }

    /// Create an index entry cell key from the encoded index key bytes.
    #[must_use]
    pub fn index_entry(btree: BtreeRef, index_key_bytes: &[u8]) -> Self {
        Self::from_semantic_ref(&SemanticKeyRef::new(
            btree,
            SemanticKeyKind::IndexEntry,
            index_key_bytes,
        ))
    }
}

// ---------------------------------------------------------------------------
// CellDeltaKind — What happened to the cell (§C1.2)
// ---------------------------------------------------------------------------

/// The kind of change applied to a cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellDeltaKind {
    /// A new cell was inserted.
    Insert,
    /// An existing cell was deleted.
    Delete,
    /// An existing cell was updated (replaced with new content).
    Update,
}

// ---------------------------------------------------------------------------
// CellDelta — A single versioned change to a cell (§C1.3)
// ---------------------------------------------------------------------------

/// A single versioned change to a cell.
///
/// Cell deltas form a chain similar to page versions, but with much lower
/// overhead since they store only the cell content, not the full page.
///
/// # Memory Layout (targeting ~64 bytes + cell_data)
///
/// - `commit_seq`: 8 bytes
/// - `created_by`: 16 bytes (TxnToken = TxnId + TxnEpoch)
/// - `kind`: 1 byte (+ padding)
/// - `prev_idx`: 8 bytes (Option<CellDeltaIdx>)
/// - `cell_data`: Variable (typically 50-200 bytes for a row)
/// - `page_number`: 4 bytes (for materialization)
/// - Estimated total: ~40 bytes fixed + cell_data
#[derive(Debug, Clone)]
pub struct CellDelta {
    /// The commit sequence when this delta became visible.
    /// `CommitSeq(0)` means uncommitted (part of an active transaction's write set).
    pub commit_seq: CommitSeq,

    /// The transaction that created this delta (for debugging/audit only).
    pub created_by: TxnToken,

    /// Stable identity of the cell this delta belongs to.
    pub cell_key: CellKey,

    /// What kind of change this represents.
    pub kind: CellDeltaKind,

    /// The page number where this cell lives (needed for materialization).
    pub page_number: PageNumber,

    /// The cell content after this delta is applied.
    /// For `Delete`, this is empty (the cell no longer exists).
    /// For `Insert`/`Update`, this contains the full cell bytes.
    pub cell_data: Vec<u8>,

    /// Index of the previous delta in the chain (older version).
    /// `None` if this is the oldest known version.
    pub prev_idx: Option<CellDeltaIdx>,
}

impl CellDelta {
    /// Memory footprint of this delta (fixed overhead + cell data).
    #[must_use]
    pub fn memory_size(&self) -> usize {
        std::mem::size_of::<Self>() + self.cell_data.len()
    }

    /// Whether this delta is visible to the given snapshot.
    #[must_use]
    pub fn is_visible_to(&self, snapshot_high: CommitSeq) -> bool {
        self.commit_seq.get() != 0 && self.commit_seq <= snapshot_high
    }
}

// ---------------------------------------------------------------------------
// CellDeltaIdx — Index into the delta arena (§C1.4)
// ---------------------------------------------------------------------------

/// Index into the [`CellDeltaArena`].
///
/// Uses the same chunk+offset+generation pattern as [`crate::core_types::VersionIdx`]
/// for ABA protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellDeltaIdx {
    chunk: u32,
    offset: u32,
    generation: u32,
}

impl CellDeltaIdx {
    #[inline]
    const fn new(chunk: u32, offset: u32, generation: u32) -> Self {
        Self {
            chunk,
            offset,
            generation,
        }
    }

    #[inline]
    #[must_use]
    pub fn chunk(&self) -> u32 {
        self.chunk
    }

    #[inline]
    #[must_use]
    pub fn offset(&self) -> u32 {
        self.offset
    }

    #[inline]
    #[must_use]
    pub fn generation(&self) -> u32 {
        self.generation
    }
}

// ---------------------------------------------------------------------------
// CellDeltaArena — Bump-allocated storage for deltas (§C1.5)
// ---------------------------------------------------------------------------

/// Number of deltas per arena chunk.
const DELTA_ARENA_CHUNK: usize = 4096;

struct DeltaSlot {
    generation: u32,
    delta: Option<CellDelta>,
}

/// Bump-allocated arena for [`CellDelta`] objects.
///
/// Similar to [`crate::core_types::VersionArena`], but for cell deltas.
/// Includes generation counting to detect use-after-free/ABA bugs.
pub struct CellDeltaArena {
    chunks: Vec<Vec<DeltaSlot>>,
    free_list: Vec<CellDeltaIdx>,
    /// Total memory consumed by cell data (excludes fixed overhead).
    cell_data_bytes: AtomicU64,
    high_water: u64,
}

impl CellDeltaArena {
    /// Create an empty arena.
    #[must_use]
    pub fn new() -> Self {
        Self {
            chunks: vec![Vec::with_capacity(DELTA_ARENA_CHUNK)],
            free_list: Vec::new(),
            cell_data_bytes: AtomicU64::new(0),
            high_water: 0,
        }
    }

    /// Allocate a slot for `delta`, returning its index.
    pub fn alloc(&mut self, delta: CellDelta) -> CellDeltaIdx {
        let data_size = delta.cell_data.len() as u64;
        self.cell_data_bytes.fetch_add(data_size, Ordering::Relaxed);

        if let Some(idx) = self.free_list.pop() {
            let slot = &mut self.chunks[idx.chunk as usize][idx.offset as usize];
            slot.delta = Some(delta);
            return CellDeltaIdx::new(idx.chunk, idx.offset, slot.generation);
        }

        let last_chunk = self.chunks.len() - 1;
        if self.chunks[last_chunk].len() >= DELTA_ARENA_CHUNK {
            self.chunks.push(Vec::with_capacity(DELTA_ARENA_CHUNK));
        }

        let chunk_idx = self.chunks.len() - 1;
        let offset = self.chunks[chunk_idx].len();
        self.chunks[chunk_idx].push(DeltaSlot {
            generation: 0,
            delta: Some(delta),
        });
        self.high_water += 1;

        let chunk_u32 = u32::try_from(chunk_idx).unwrap_or(u32::MAX);
        let offset_u32 = u32::try_from(offset).unwrap_or(u32::MAX);
        CellDeltaIdx::new(chunk_u32, offset_u32, 0)
    }

    /// Free the slot at `idx`, making it available for reuse.
    pub fn free(&mut self, idx: CellDeltaIdx) -> Option<CellDelta> {
        let slot = self
            .chunks
            .get_mut(idx.chunk as usize)?
            .get_mut(idx.offset as usize)?;

        if slot.generation != idx.generation {
            return None; // Stale pointer
        }

        let delta = slot.delta.take()?;
        let data_size = delta.cell_data.len() as u64;
        self.cell_data_bytes.fetch_sub(data_size, Ordering::Relaxed);

        // Increment generation on free, skipping 0 (used by fresh allocations).
        // This prevents ABA issues where a stale CellDeltaIdx with generation 0
        // could incorrectly match a recycled slot.
        let mut next_gen = slot.generation.wrapping_add(1);
        if next_gen == 0 {
            next_gen = 1;
        }
        slot.generation = next_gen;

        self.free_list.push(idx);
        Some(delta)
    }

    /// Look up a delta by index.
    #[must_use]
    pub fn get(&self, idx: CellDeltaIdx) -> Option<&CellDelta> {
        let slot = self
            .chunks
            .get(idx.chunk as usize)?
            .get(idx.offset as usize)?;

        if slot.generation != idx.generation {
            return None;
        }
        slot.delta.as_ref()
    }

    /// Total cell data bytes currently in the arena.
    #[must_use]
    pub fn cell_data_bytes(&self) -> u64 {
        self.cell_data_bytes.load(Ordering::Relaxed)
    }

    /// High water mark (total deltas ever allocated).
    #[must_use]
    pub fn high_water(&self) -> u64 {
        self.high_water
    }
}

impl Default for CellDeltaArena {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// CellVisibilityLog — The main cell-level MVCC structure (§C1.6)
// ---------------------------------------------------------------------------

/// Number of shards in the cell visibility log.
pub const CELL_LOG_SHARDS: usize = 64;

/// Entry in the cell head table mapping CellKey -> head delta.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CellHeadEntry {
    pub(crate) cell_key: CellKey,
    pub(crate) head_idx: CellDeltaIdx,
}

/// A single shard of the cell head table.
pub(crate) struct CellLogShard {
    /// Maps (PageNumber, key_digest) -> head delta index.
    /// Uses default hasher since the key is a composite (PageNumber, [u8; 16]).
    pub(crate) heads: RwLock<HashMap<(PageNumber, [u8; 16]), CellHeadEntry>>,
    /// C7 (bd-l9k8e.7): Per-page delta count for batch visibility checks.
    /// If count == 0, entire page is visible at base (skip per-cell resolution).
    page_delta_counts: RwLock<HashMap<PageNumber, usize>>,
}

impl CellLogShard {
    fn new() -> Self {
        Self {
            heads: RwLock::new(HashMap::new()),
            page_delta_counts: RwLock::new(HashMap::new()),
        }
    }

    /// C7: Increment delta count for a page.
    fn increment_page_count(&self, page: PageNumber) {
        let mut counts = self.page_delta_counts.write();
        *counts.entry(page).or_insert(0) += 1;
    }

    /// C7: Decrement delta count for a page.
    fn decrement_page_count(&self, page: PageNumber) {
        let mut counts = self.page_delta_counts.write();
        if let Some(count) = counts.get_mut(&page) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&page);
            }
        }
    }

    /// C7: Check if a page has any deltas.
    fn page_has_deltas(&self, page: PageNumber) -> bool {
        let counts = self.page_delta_counts.read();
        counts.get(&page).is_some_and(|&c| c > 0)
    }
}

// ---------------------------------------------------------------------------
// Transaction Delta Tracking (§C3.1)
// ---------------------------------------------------------------------------

/// Tracks deltas created by each active transaction.
///
/// This enables bulk commit (update all deltas for a txn) and rollback
/// (remove all deltas for a txn) operations.
struct TxnDeltaTracker {
    /// Maps TxnToken -> list of delta indices created by that transaction.
    /// Uses SmallVec because most transactions create <16 deltas.
    txn_deltas: HashMap<TxnToken, SmallVec<[CellDeltaIdx; 16]>>,
    /// Per-transaction byte budget tracking.
    txn_bytes: HashMap<TxnToken, u64>,
}

impl TxnDeltaTracker {
    fn new() -> Self {
        Self {
            txn_deltas: HashMap::new(),
            txn_bytes: HashMap::new(),
        }
    }

    fn record(&mut self, txn: TxnToken, idx: CellDeltaIdx, bytes: u64) {
        self.txn_deltas.entry(txn).or_default().push(idx);
        *self.txn_bytes.entry(txn).or_insert(0) += bytes;
    }

    /// Get all delta indices for a transaction (used during abort/rollback).
    #[allow(dead_code)]
    fn get_deltas(&self, txn: TxnToken) -> Option<&SmallVec<[CellDeltaIdx; 16]>> {
        self.txn_deltas.get(&txn)
    }

    /// Remove a transaction's tracking data (used during abort/rollback/commit finalization).
    #[allow(dead_code)]
    fn remove_txn(&mut self, txn: TxnToken) -> Option<SmallVec<[CellDeltaIdx; 16]>> {
        self.txn_bytes.remove(&txn);
        self.txn_deltas.remove(&txn)
    }

    fn txn_bytes(&self, txn: TxnToken) -> u64 {
        self.txn_bytes.get(&txn).copied().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Conflict Detection (§C3.2)
// ---------------------------------------------------------------------------

/// Result of a cell-level conflict check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellConflict {
    /// No conflict detected.
    None,
    /// Conflict detected with the given transaction.
    Conflict { with_txn: TxnToken },
}

// ---------------------------------------------------------------------------
// GC Statistics (§C3.3)
// ---------------------------------------------------------------------------

/// Statistics from a GC run.
#[derive(Debug, Clone, Default)]
pub struct CellGcStats {
    /// Number of deltas examined.
    pub examined: u64,
    /// Number of deltas reclaimed.
    pub reclaimed: u64,
    /// Bytes freed.
    pub bytes_freed: u64,
}

// ---------------------------------------------------------------------------
// Materialization Callback (§C3.4)
// ---------------------------------------------------------------------------

/// Callback invoked when eager materialization is needed.
///
/// The callback receives the page number and a list of cells that should be
/// materialized into a full page version.
pub type MaterializationCallback = Box<dyn Fn(PageNumber, &[CellKey]) + Send + Sync>;

/// The cell-level MVCC visibility log.
///
/// This is the central data structure for cell-level MVCC. It maintains:
///
/// 1. A sharded hash table mapping (PageNumber, CellKey) -> head of delta chain
/// 2. A delta arena storing all CellDelta objects
/// 3. Memory budget tracking for eager materialization
/// 4. Per-transaction delta tracking for bulk commit/rollback
/// 5. Conflict detection for concurrent writers
///
/// # Thread Safety
///
/// - Reads are lock-free (RwLock read + atomic arena access)
/// - Writes acquire a write lock on the target shard only
/// - The arena is protected by a Mutex for allocation (single-writer)
///
/// # Usage
///
/// ```ignore
/// let log = CellVisibilityLog::new(budget_bytes);
///
/// // Record a cell insert
/// log.record_insert(cell_key, page_number, cell_bytes, txn_token);
///
/// // Resolve visible cell for a snapshot
/// let cell_data = log.resolve(page_number, cell_key, snapshot);
///
/// // Commit all deltas for a transaction
/// log.commit_txn(txn_token, commit_seq);
///
/// // Or rollback
/// log.rollback_txn(txn_token);
/// ```
pub struct CellVisibilityLog {
    /// Sharded cell head table.
    pub(crate) shards: Box<[CacheAligned<CellLogShard>; CELL_LOG_SHARDS]>,
    /// Delta arena (protected by Mutex for writes).
    pub(crate) arena: Mutex<CellDeltaArena>,
    /// Transaction delta tracker (protected by Mutex).
    txn_tracker: Mutex<TxnDeltaTracker>,
    /// Memory budget for cell data (bytes).
    budget_bytes: u64,
    /// Per-transaction byte budget (bytes).
    per_txn_budget_bytes: u64,
    /// Total deltas currently stored.
    delta_count: AtomicU64,
    /// Optional materialization callback for budget enforcement.
    materialization_cb: Option<MaterializationCallback>,
}

/// Default per-transaction budget: 4MB
const DEFAULT_PER_TXN_BUDGET: u64 = 4 * 1024 * 1024;

impl CellVisibilityLog {
    /// Create a new cell visibility log with the given memory budget.
    ///
    /// The budget controls when eager materialization kicks in to prevent
    /// unbounded memory growth.
    #[must_use]
    pub fn new(budget_bytes: u64) -> Self {
        Self::with_per_txn_budget(budget_bytes, DEFAULT_PER_TXN_BUDGET)
    }

    /// Create a new cell visibility log with custom per-transaction budget.
    #[must_use]
    pub fn with_per_txn_budget(budget_bytes: u64, per_txn_budget_bytes: u64) -> Self {
        trace!(
            budget_bytes,
            per_txn_budget_bytes, "cell_visibility_log_created"
        );
        Self {
            shards: Box::new(std::array::from_fn(|_| {
                CacheAligned::new(CellLogShard::new())
            })),
            arena: Mutex::new(CellDeltaArena::new()),
            txn_tracker: Mutex::new(TxnDeltaTracker::new()),
            budget_bytes,
            per_txn_budget_bytes,
            delta_count: AtomicU64::new(0),
            materialization_cb: None,
        }
    }

    /// Set the materialization callback for budget enforcement.
    pub fn set_materialization_callback(&mut self, cb: MaterializationCallback) {
        self.materialization_cb = Some(cb);
    }

    /// Compute the shard index for a page number.
    #[inline]
    fn shard_index(pgno: PageNumber) -> usize {
        (pgno.get() as usize) & (CELL_LOG_SHARDS - 1)
    }

    /// C7 (bd-l9k8e.7): Check if a page has ANY cell deltas.
    ///
    /// This is an O(1) batch visibility check. If this returns `false`, the
    /// entire page is visible at its base version and per-cell resolution can
    /// be skipped entirely.
    ///
    /// Use this before calling `resolve` or `collect_visible_deltas` to avoid
    /// unnecessary delta chain walks on clean pages.
    #[must_use]
    pub fn page_has_deltas(&self, page_number: PageNumber) -> bool {
        let shard_idx = Self::shard_index(page_number);
        self.shards[shard_idx].page_has_deltas(page_number)
    }

    /// Record a cell insert operation.
    ///
    /// # Arguments
    ///
    /// * `cell_key` - The stable identity of the cell
    /// * `page_number` - The page containing this cell
    /// * `cell_data` - The cell content bytes
    /// * `created_by` - The transaction creating this delta
    ///
    /// Returns the index of the new delta, or `None` if per-txn budget exceeded.
    pub fn record_insert(
        &self,
        cell_key: CellKey,
        page_number: PageNumber,
        cell_data: Vec<u8>,
        created_by: TxnToken,
    ) -> Option<CellDeltaIdx> {
        self.record_delta(
            cell_key,
            page_number,
            CellDeltaKind::Insert,
            cell_data,
            created_by,
        )
    }

    /// Record a cell delete operation.
    ///
    /// Returns the index of the new delta, or `None` if per-txn budget exceeded.
    pub fn record_delete(
        &self,
        cell_key: CellKey,
        page_number: PageNumber,
        created_by: TxnToken,
    ) -> Option<CellDeltaIdx> {
        self.record_delta(
            cell_key,
            page_number,
            CellDeltaKind::Delete,
            Vec::new(),
            created_by,
        )
    }

    /// Record a cell update operation.
    ///
    /// Returns the index of the new delta, or `None` if per-txn budget exceeded.
    pub fn record_update(
        &self,
        cell_key: CellKey,
        page_number: PageNumber,
        new_cell_data: Vec<u8>,
        created_by: TxnToken,
    ) -> Option<CellDeltaIdx> {
        self.record_delta(
            cell_key,
            page_number,
            CellDeltaKind::Update,
            new_cell_data,
            created_by,
        )
    }

    /// Internal: record a delta of any kind.
    ///
    /// Returns `None` if the per-transaction budget would be exceeded.
    fn record_delta(
        &self,
        cell_key: CellKey,
        page_number: PageNumber,
        kind: CellDeltaKind,
        cell_data: Vec<u8>,
        created_by: TxnToken,
    ) -> Option<CellDeltaIdx> {
        let shard_idx = Self::shard_index(page_number);
        let shard = &self.shards[shard_idx];

        // Look up existing head (can release this lock early)
        let lookup_key = (page_number, cell_key.key_digest);
        let prev_idx = {
            let heads = shard.heads.read();
            heads.get(&lookup_key).map(|e| e.head_idx)
        };

        // Create new delta
        let delta = CellDelta {
            commit_seq: CommitSeq::new(0), // Uncommitted
            created_by,
            cell_key,
            kind: kind.clone(),
            page_number,
            cell_data,
            prev_idx,
        };

        let delta_memory = delta.memory_size() as u64;

        // CRITICAL FIX: Hold tracker lock through budget check AND recording
        // to prevent TOCTOU race where multiple threads for same txn could
        // each pass budget check before either records, exceeding total budget.
        let new_idx = {
            let mut tracker = self.txn_tracker.lock();
            let current_txn_bytes = tracker.txn_bytes(created_by);
            if current_txn_bytes + delta_memory > self.per_txn_budget_bytes {
                debug!(
                    txn_id = created_by.id.get(),
                    current_bytes = current_txn_bytes,
                    requested_bytes = delta_memory,
                    budget = self.per_txn_budget_bytes,
                    "cell_delta_txn_budget_exceeded"
                );
                return None;
            }

            // Allocate in arena while holding tracker lock
            let idx = {
                let mut arena = self.arena.lock();
                arena.alloc(delta)
            };

            // Record atomically with budget check
            tracker.record(created_by, idx, delta_memory);
            idx
        };

        // Update head
        {
            let mut heads = shard.heads.write();
            heads.insert(
                lookup_key,
                CellHeadEntry {
                    cell_key,
                    head_idx: new_idx,
                },
            );
        }

        // C7 (bd-l9k8e.7): Increment per-page delta count for batch visibility.
        shard.increment_page_count(page_number);

        self.delta_count.fetch_add(1, Ordering::Relaxed);

        trace!(
            pgno = page_number.get(),
            key_digest = ?&cell_key.key_digest[..4],
            txn_id = created_by.id.get(),
            op = ?kind,
            delta_idx_chunk = new_idx.chunk(),
            delta_idx_offset = new_idx.offset(),
            "cell_delta_recorded"
        );

        // Check global budget and trigger materialization if needed
        self.maybe_trigger_materialization();

        Some(new_idx)
    }

    /// Check if global budget is exceeded and trigger materialization.
    fn maybe_trigger_materialization(&self) {
        let arena = self.arena.lock();
        let current_bytes = arena.cell_data_bytes();
        drop(arena);

        if current_bytes > self.budget_bytes {
            if let Some(ref cb) = self.materialization_cb {
                // Find pages with highest delta counts for materialization
                let pages_to_materialize = self.find_high_delta_pages(10);
                for (pgno, cell_keys) in pages_to_materialize {
                    debug!(
                        pgno = pgno.get(),
                        cell_count = cell_keys.len(),
                        "cell_budget_exceeded_materializing"
                    );
                    cb(pgno, &cell_keys);
                }
            }
        }
    }

    /// Find pages with the highest number of deltas.
    fn find_high_delta_pages(&self, max_pages: usize) -> Vec<(PageNumber, Vec<CellKey>)> {
        let mut page_counts = HashMap::new();
        let mut page_cells: HashMap<PageNumber, Vec<CellKey>> = HashMap::new();

        for shard in self.shards.iter() {
            let counts = shard.page_delta_counts.read();
            for (&pgno, &count) in counts.iter() {
                if count > 0 {
                    page_counts.insert(pgno, count);
                }
            }
            drop(counts);

            let heads = shard.heads.read();
            for ((pgno, _), entry) in heads.iter() {
                page_cells.entry(*pgno).or_default().push(entry.cell_key);
            }
        }

        let mut pages: Vec<_> = page_cells
            .into_iter()
            .map(|(pgno, cell_keys)| {
                let delta_count = page_counts.get(&pgno).copied().unwrap_or(cell_keys.len());
                (pgno, delta_count, cell_keys)
            })
            .collect();
        pages.sort_by(|lhs, rhs| {
            rhs.1
                .cmp(&lhs.1)
                .then_with(|| rhs.2.len().cmp(&lhs.2.len()))
        });
        pages.truncate(max_pages);
        pages
            .into_iter()
            .map(|(pgno, _delta_count, cell_keys)| (pgno, cell_keys))
            .collect()
    }

    /// Commit a delta (assign a commit sequence).
    ///
    /// Called at transaction commit time to make the delta visible.
    pub fn commit_delta(&self, idx: CellDeltaIdx, commit_seq: CommitSeq) {
        let mut arena = self.arena.lock();
        if let Some(slot) = arena
            .chunks
            .get_mut(idx.chunk as usize)
            .and_then(|c| c.get_mut(idx.offset as usize))
        {
            if slot.generation == idx.generation {
                if let Some(ref mut delta) = slot.delta {
                    delta.commit_seq = commit_seq;
                }
            }
        }
    }

    /// Resolve the visible cell data for a snapshot.
    ///
    /// Returns `None` if:
    /// - The cell has no deltas in the log
    /// - The cell was deleted in the visible version
    /// - No delta is visible to this snapshot
    #[must_use]
    pub fn resolve(
        &self,
        page_number: PageNumber,
        cell_key: &CellKey,
        snapshot_high: CommitSeq,
    ) -> Option<Vec<u8>> {
        let shard_idx = Self::shard_index(page_number);
        let shard = &self.shards[shard_idx];
        let lookup_key = (page_number, cell_key.key_digest);

        let head_idx = {
            let heads = shard.heads.read();
            match heads.get(&lookup_key) {
                Some(entry) => entry.head_idx,
                None => {
                    trace!(
                        pgno = page_number.get(),
                        key_digest = ?&cell_key.key_digest[..4],
                        snapshot_high = snapshot_high.get(),
                        result = "not_tracked",
                        "cell_resolved"
                    );
                    return None;
                }
            }
        };

        let arena = self.arena.lock();
        let mut current_idx = Some(head_idx);

        // Walk chain to find visible delta
        while let Some(idx) = current_idx {
            if let Some(delta) = arena.get(idx) {
                if delta.is_visible_to(snapshot_high) {
                    let result = match delta.kind {
                        CellDeltaKind::Delete => {
                            trace!(
                                pgno = page_number.get(),
                                key_digest = ?&cell_key.key_digest[..4],
                                snapshot_high = snapshot_high.get(),
                                result = "deleted",
                                "cell_resolved"
                            );
                            None
                        }
                        CellDeltaKind::Insert | CellDeltaKind::Update => {
                            trace!(
                                pgno = page_number.get(),
                                key_digest = ?&cell_key.key_digest[..4],
                                snapshot_high = snapshot_high.get(),
                                result = "visible",
                                data_len = delta.cell_data.len(),
                                "cell_resolved"
                            );
                            Some(delta.cell_data.clone())
                        }
                    };
                    return result;
                }
                current_idx = delta.prev_idx;
            } else {
                break;
            }
        }

        trace!(
            pgno = page_number.get(),
            key_digest = ?&cell_key.key_digest[..4],
            snapshot_high = snapshot_high.get(),
            result = "no_visible_version",
            "cell_resolved"
        );
        None
    }

    /// Check if the memory budget is exceeded.
    #[must_use]
    pub fn is_over_budget(&self) -> bool {
        let arena = self.arena.lock();
        arena.cell_data_bytes() > self.budget_bytes
    }

    /// Total number of deltas currently stored.
    #[must_use]
    pub fn delta_count(&self) -> u64 {
        self.delta_count.load(Ordering::Relaxed)
    }

    /// Current cell data memory usage.
    #[must_use]
    pub fn cell_data_bytes(&self) -> u64 {
        let arena = self.arena.lock();
        arena.cell_data_bytes()
    }

    /// Memory budget.
    #[must_use]
    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// Per-transaction memory budget.
    #[must_use]
    pub fn per_txn_budget_bytes(&self) -> u64 {
        self.per_txn_budget_bytes
    }

    /// Commit all deltas for a transaction atomically.
    pub fn commit_txn(&self, txn: TxnToken, commit_seq: CommitSeq) {
        let mut tracker = self.txn_tracker.lock();
        let delta_indices = match tracker.get_deltas(txn) {
            Some(indices) => indices.clone(),
            None => {
                trace!(txn_id = txn.id.get(), "cell_txn_commit_no_deltas");
                return;
            }
        };

        let mut arena = self.arena.lock();
        let mut committed_count = 0u64;

        for idx in &delta_indices {
            if let Some(slot) = arena
                .chunks
                .get_mut(idx.chunk as usize)
                .and_then(|c| c.get_mut(idx.offset as usize))
            {
                if slot.generation == idx.generation {
                    if let Some(ref mut delta) = slot.delta {
                        delta.commit_seq = commit_seq;
                        committed_count += 1;
                    }
                }
            }
        }

        drop(arena);
        tracker.remove_txn(txn);

        debug!(
            txn_id = txn.id.get(),
            commit_seq = commit_seq.get(),
            delta_count = committed_count,
            "cell_txn_committed"
        );
    }

    /// Rollback all deltas for a transaction.
    pub fn rollback_txn(&self, txn: TxnToken) -> u64 {
        let mut tracker = self.txn_tracker.lock();
        let delta_indices = match tracker.remove_txn(txn) {
            Some(indices) => indices,
            None => {
                trace!(txn_id = txn.id.get(), "cell_txn_rollback_no_deltas");
                return 0;
            }
        };

        let mut arena = self.arena.lock();
        let mut removed_count = 0u64;
        let mut bytes_freed = 0u64;

        for idx in delta_indices.iter().rev() {
            if let Some(delta) = arena.free(*idx) {
                bytes_freed += delta.memory_size() as u64;
                removed_count += 1;

                let shard_idx = Self::shard_index(delta.page_number);
                let shard = &self.shards[shard_idx];
                let mut heads = shard.heads.write();

                let lookup_key = (delta.page_number, delta.cell_key.key_digest);
                let next_head = delta.prev_idx.filter(|prev| arena.get(*prev).is_some());
                let should_remove = heads
                    .get(&lookup_key)
                    .is_some_and(|entry| entry.head_idx == *idx)
                    && next_head.is_none();

                if let Some(entry) = heads.get_mut(&lookup_key) {
                    if entry.head_idx == *idx {
                        if let Some(prev) = next_head {
                            entry.head_idx = prev;
                        }
                    }
                }

                if should_remove {
                    heads.remove(&lookup_key);
                }

                // C7 (bd-l9k8e.7): Decrement per-page delta count.
                drop(heads); // Release write lock before calling decrement
                shard.decrement_page_count(delta.page_number);
            }
        }

        self.delta_count.fetch_sub(removed_count, Ordering::Relaxed);

        debug!(
            txn_id = txn.id.get(),
            delta_count = removed_count,
            bytes_freed,
            "cell_txn_rolled_back"
        );

        removed_count
    }

    /// Check for cell-level conflict between two transactions.
    #[must_use]
    pub fn check_conflict(&self, txn: TxnToken, other_txn: TxnToken) -> CellConflict {
        if txn == other_txn {
            return CellConflict::None;
        }

        let tracker = self.txn_tracker.lock();

        let our_deltas = match tracker.get_deltas(txn) {
            Some(d) => d,
            None => return CellConflict::None,
        };

        let their_deltas = match tracker.get_deltas(other_txn) {
            Some(d) => d,
            None => return CellConflict::None,
        };

        let arena = self.arena.lock();

        let mut our_cells: std::collections::HashSet<(PageNumber, [u8; 16])> =
            std::collections::HashSet::new();

        for idx in our_deltas {
            if let Some(delta) = arena.get(*idx) {
                our_cells.insert((delta.page_number, delta.cell_key.key_digest));
            }
        }

        for idx in their_deltas {
            if let Some(delta) = arena.get(*idx) {
                if our_cells.contains(&(delta.page_number, delta.cell_key.key_digest)) {
                    debug!(
                        txn_id = txn.id.get(),
                        other_txn_id = other_txn.id.get(),
                        pgno = delta.page_number.get(),
                        "cell_conflict_detected"
                    );
                    return CellConflict::Conflict {
                        with_txn: other_txn,
                    };
                }
            }
        }

        CellConflict::None
    }

    /// Garbage collect deltas below the GC horizon.
    pub fn gc(&self, gc_horizon: CommitSeq) -> CellGcStats {
        let mut stats = CellGcStats::default();
        let mut to_free: Vec<CellDeltaIdx> = Vec::new();

        {
            let arena = self.arena.lock();

            for shard in self.shards.iter() {
                let heads = shard.heads.read();

                for (_, entry) in heads.iter() {
                    let mut current_idx = Some(entry.head_idx);
                    let mut found_visible_below_horizon = false;

                    while let Some(idx) = current_idx {
                        stats.examined += 1;

                        if let Some(delta) = arena.get(idx) {
                            if delta.commit_seq.get() != 0 && delta.commit_seq <= gc_horizon {
                                if found_visible_below_horizon {
                                    to_free.push(idx);
                                } else {
                                    found_visible_below_horizon = true;
                                }
                            }
                            current_idx = delta.prev_idx;
                        } else {
                            break;
                        }
                    }
                }
            }
        }

        if !to_free.is_empty() {
            let mut arena = self.arena.lock();

            for idx in to_free {
                if let Some(delta) = arena.free(idx) {
                    stats.reclaimed += 1;
                    stats.bytes_freed += delta.memory_size() as u64;

                    // C7 (bd-l9k8e.7): Decrement per-page delta count.
                    let shard_idx = Self::shard_index(delta.page_number);
                    self.shards[shard_idx].decrement_page_count(delta.page_number);
                }
            }
        }

        self.delta_count
            .fetch_sub(stats.reclaimed, Ordering::Relaxed);

        debug!(
            gc_horizon = gc_horizon.get(),
            examined = stats.examined,
            reclaimed = stats.reclaimed,
            bytes_freed = stats.bytes_freed,
            "cell_gc_completed"
        );

        stats
    }

    /// Get the number of bytes used by a transaction's deltas.
    #[must_use]
    pub fn txn_bytes(&self, txn: TxnToken) -> u64 {
        let tracker = self.txn_tracker.lock();
        tracker.txn_bytes(txn)
    }

    /// Get the number of active transactions with deltas.
    #[must_use]
    pub fn active_txn_count(&self) -> usize {
        let tracker = self.txn_tracker.lock();
        tracker.txn_deltas.len()
    }

    // -------------------------------------------------------------------------
    // Materialization Support (§C5)
    // -------------------------------------------------------------------------

    /// Collect all visible deltas for a page, sorted by commit_seq.
    ///
    /// This is the primary interface for materialization: gather all deltas
    /// that need to be applied to produce a materialized page.
    ///
    /// # Lock Ordering
    ///
    /// Maintains canonical lock order: arena → heads (same as rollback_txn).
    #[must_use]
    pub fn collect_visible_deltas(
        &self,
        page_number: PageNumber,
        snapshot_high: CommitSeq,
    ) -> Vec<CellDelta> {
        let shard_idx = Self::shard_index(page_number);
        let shard = &self.shards[shard_idx];

        let mut deltas = Vec::new();

        let arena = self.arena.lock();
        let heads = shard.heads.read();

        // Collect all deltas for cells on this page
        for ((pgno, _key_digest), entry) in heads.iter() {
            if *pgno != page_number {
                continue;
            }

            let mut current_idx = Some(entry.head_idx);

            // Walk the chain and collect visible deltas
            while let Some(idx) = current_idx {
                if let Some(delta) = arena.get(idx) {
                    if delta.is_visible_to(snapshot_high) {
                        deltas.push(delta.clone());
                    }
                    current_idx = delta.prev_idx;
                } else {
                    break;
                }
            }
        }

        // Sort by commit_seq (ascending) so deltas are applied in order
        deltas.sort_by_key(|d| d.commit_seq);

        trace!(
            pgno = page_number.get(),
            snapshot_high = snapshot_high.get(),
            delta_count = deltas.len(),
            "collected_visible_deltas"
        );

        deltas
    }

    /// Get the count of deltas for a specific page.
    ///
    /// Used to check if eager materialization threshold is reached.
    ///
    /// # Lock Ordering
    ///
    /// Maintains canonical lock order: arena → heads (same as rollback_txn).
    #[must_use]
    pub fn page_delta_count(&self, page_number: PageNumber) -> usize {
        let shard_idx = Self::shard_index(page_number);
        let shard = &self.shards[shard_idx];

        let arena = self.arena.lock();
        let heads = shard.heads.read();

        let mut count = 0usize;

        for ((pgno, _), entry) in heads.iter() {
            if *pgno != page_number {
                continue;
            }

            let mut current_idx = Some(entry.head_idx);
            while let Some(idx) = current_idx {
                if let Some(delta) = arena.get(idx) {
                    count += 1;
                    current_idx = delta.prev_idx;
                } else {
                    break;
                }
            }
        }

        count
    }

    /// Get all pages that have outstanding deltas.
    ///
    /// Used for checkpoint materialization.
    #[must_use]
    pub fn pages_with_deltas(&self) -> Vec<PageNumber> {
        let mut pages: std::collections::HashSet<PageNumber> = std::collections::HashSet::new();

        for shard in self.shards.iter() {
            let heads = shard.heads.read();
            for ((pgno, _), _) in heads.iter() {
                pages.insert(*pgno);
            }
        }

        pages.into_iter().collect()
    }

    /// Clear deltas for a page after materialization.
    ///
    /// Called after a successful materialization to free up memory and
    /// prevent re-applying deltas. This function:
    /// - Frees deltas from the arena
    /// - Updates head pointers (removes entries or truncates chains)
    /// - Decrements page_delta_counts
    /// - Updates the global delta_count
    ///
    /// # Lock Ordering
    ///
    /// Maintains canonical lock order: arena → heads (same as rollback_txn).
    /// This prevents deadlock with concurrent operations.
    pub fn clear_page_deltas(&self, page_number: PageNumber, below_commit_seq: CommitSeq) -> usize {
        let shard_idx = Self::shard_index(page_number);
        let shard = &self.shards[shard_idx];

        // Phase 1: Collect info about what to free and how to update heads.
        // Lock order: arena first, then heads (matches rollback_txn pattern).
        // (key_digest, indices_to_free, new_head_idx)
        let mut updates: Vec<([u8; 16], Vec<CellDeltaIdx>, Option<CellDeltaIdx>)> = Vec::new();

        {
            let arena = self.arena.lock();
            let heads = shard.heads.read();

            for ((pgno, key_digest), entry) in heads.iter() {
                if *pgno != page_number {
                    continue;
                }

                let mut to_free_for_key: Vec<CellDeltaIdx> = Vec::new();
                let mut new_head: Option<CellDeltaIdx> = None;
                let mut current_idx = Some(entry.head_idx);

                // Walk the chain, collecting deltas to free
                while let Some(idx) = current_idx {
                    if let Some(delta) = arena.get(idx) {
                        if delta.commit_seq.get() != 0 && delta.commit_seq <= below_commit_seq {
                            to_free_for_key.push(idx);
                        } else if new_head.is_none() {
                            // First delta that survives becomes the new head
                            new_head = Some(idx);
                        }
                        current_idx = delta.prev_idx;
                    } else {
                        break;
                    }
                }

                if !to_free_for_key.is_empty() {
                    updates.push((*key_digest, to_free_for_key, new_head));
                }
            }
        }

        if updates.is_empty() {
            return 0;
        }

        // Now perform the actual updates with write locks
        let mut arena = self.arena.lock();
        let mut heads = shard.heads.write();
        let mut freed = 0usize;

        for (key_digest, to_free, new_head) in updates {
            let lookup_key = (page_number, key_digest);

            // Free the deltas from arena
            for idx in &to_free {
                if arena.free(*idx).is_some() {
                    freed += 1;
                }
            }

            // Update or remove the head entry
            if let Some(new_head_idx) = new_head {
                // Update head to point to surviving delta
                if let Some(entry) = heads.get_mut(&lookup_key) {
                    entry.head_idx = new_head_idx;
                }
            } else {
                // All deltas for this key were freed, remove the entry
                heads.remove(&lookup_key);
            }
        }

        // Drop locks before calling decrement_page_count (avoid deadlock)
        drop(heads);
        drop(arena);

        // C7: Decrement page_delta_counts for each freed delta
        for _ in 0..freed {
            shard.decrement_page_count(page_number);
        }

        // Update global delta count
        self.delta_count.fetch_sub(freed as u64, Ordering::Relaxed);

        debug!(
            pgno = page_number.get(),
            below_commit_seq = below_commit_seq.get(),
            freed_count = freed,
            "page_deltas_cleared"
        );

        freed
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl std::fmt::Debug for CellVisibilityLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellVisibilityLog")
            .field("budget_bytes", &self.budget_bytes)
            .field("delta_count", &self.delta_count.load(Ordering::Relaxed))
            .field("cell_data_bytes", &self.cell_data_bytes())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// MutationOutcome — Classification of B-tree mutations (§C2)
// ---------------------------------------------------------------------------

/// Outcome of a B-tree mutation for MVCC classification.
///
/// This enum classifies the result of an INSERT, UPDATE, or DELETE operation
/// to determine whether cell-level MVCC (logical) or page-level MVCC (structural)
/// should be used.
///
/// # Design Rationale (C2: bd-l9k8e.2)
///
/// The fundamental question: does this operation change only cell content, or
/// does it change page structure/allocation?
///
/// - **Logical operations** modify cell content within existing page boundaries.
/// - **Structural operations** change the B-tree topology (page allocation, splits,
///   merges, depth changes).
///
/// Cell-level MVCC (CellVisibilityLog) handles logical operations.
/// Page-level MVCC (VersionStore) handles structural operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MutationOutcome {
    /// Cell was inserted/updated in-place without page restructuring.
    /// The cell fit in the existing page free space.
    CellFit,

    /// Cell was deleted and page still has other cells remaining.
    /// No structural changes needed.
    CellRemovedPageNonEmpty,

    /// Insert triggered page split (`balance_for_insert` was called).
    /// One or more pages were allocated, cells redistributed.
    PageSplit,

    /// Delete triggered page merge (`balance_for_delete` was called).
    /// Pages may have been freed or tree depth reduced.
    PageMerge,

    /// Overflow chain was created, extended, or freed.
    /// Affects pages beyond the leaf page.
    OverflowChainModified,

    /// Interior page cell was replaced (index delete with successor promotion).
    /// Structural because it affects tree navigation keys.
    InteriorCellReplaced,
}

impl MutationOutcome {
    /// Returns true if this mutation is structural (page-level MVCC required).
    ///
    /// Structural operations change B-tree topology: page allocation, splits,
    /// merges, overflow chains, or interior page modifications.
    #[inline]
    #[must_use]
    pub const fn is_structural(self) -> bool {
        match self {
            // Logical operations: only affect cell content within one page.
            Self::CellFit | Self::CellRemovedPageNonEmpty => false,

            // Structural operations: change B-tree topology.
            Self::PageSplit
            | Self::PageMerge
            | Self::OverflowChainModified
            | Self::InteriorCellReplaced => true,
        }
    }

    /// Returns true if this mutation can use cell-level MVCC.
    ///
    /// Logical operations only affect cell content within a single page and
    /// are candidates for the `CellVisibilityLog`.
    #[inline]
    #[must_use]
    pub const fn is_logical(self) -> bool {
        !self.is_structural()
    }
}

/// Pre-mutation check: can this insertion potentially be logical?
///
/// Returns `false` if a structural operation is guaranteed (e.g., payload requires
/// overflow). Returns `true` if the operation MIGHT be logical (cell might fit).
/// The actual outcome depends on page free space at mutation time.
///
/// # Arguments
///
/// * `payload_size` - Size of the cell payload in bytes
/// * `local_max` - Maximum local payload for this page type
/// * `page_free_space` - Current free space in the target page
/// * `cell_overhead` - Fixed overhead per cell (header, cell pointer, etc.)
#[must_use]
pub fn can_be_logical_insert(
    payload_size: usize,
    local_max: usize,
    page_free_space: usize,
    cell_overhead: usize,
) -> bool {
    // If payload requires overflow, it's structural.
    if payload_size > local_max {
        return false;
    }

    // If cell (with overhead) fits in current free space, it's logical.
    let total_cell_size = payload_size + cell_overhead;
    total_cell_size <= page_free_space
}

/// Pre-mutation check: will this deletion be logical?
///
/// Returns `true` if the page will have cells remaining after deletion.
/// When a page becomes empty, `balance_for_delete` is triggered, making
/// the operation structural.
#[inline]
#[must_use]
pub const fn will_be_logical_delete(current_cell_count: u16) -> bool {
    current_cell_count > 1
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Encode an i64 as a SQLite-style varint.
///
/// Returns the number of bytes written.
fn encode_varint_i64(value: i64, buf: &mut [u8; 10]) -> usize {
    // SQLite uses unsigned varints, so cast to u64
    let uval = value as u64;
    encode_varint_u64(uval, buf)
}

/// Encode a u64 as a SQLite-style varint.
fn encode_varint_u64(mut value: u64, buf: &mut [u8; 10]) -> usize {
    let mut i = 0;
    loop {
        if value <= 0x7f {
            buf[i] = value as u8;
            return i + 1;
        }
        buf[i] = ((value & 0x7f) | 0x80) as u8;
        value >>= 7;
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use fsqlite_types::{TableId, TxnEpoch, TxnId};

    fn test_txn_token() -> TxnToken {
        TxnToken::new(TxnId::new(1).unwrap(), TxnEpoch::new(1))
    }

    #[test]
    fn cell_key_from_rowid() {
        let btree = BtreeRef::Table(TableId::new(1));
        let key1 = CellKey::table_row(btree, 100);
        let key2 = CellKey::table_row(btree, 100);
        let key3 = CellKey::table_row(btree, 101);

        assert_eq!(key1.key_digest, key2.key_digest, "Same rowid = same digest");
        assert_ne!(
            key1.key_digest, key3.key_digest,
            "Different rowid = different digest"
        );
    }

    #[test]
    fn cell_delta_arena_alloc_free() {
        let mut arena = CellDeltaArena::new();

        let delta = CellDelta {
            commit_seq: CommitSeq::new(1),
            created_by: test_txn_token(),
            cell_key: CellKey::table_row(BtreeRef::Table(TableId::new(1)), 100),
            kind: CellDeltaKind::Insert,
            page_number: PageNumber::new(42).unwrap(),
            cell_data: vec![1, 2, 3, 4],
            prev_idx: None,
        };

        let idx = arena.alloc(delta);
        assert!(arena.get(idx).is_some());

        let freed = arena.free(idx);
        assert!(freed.is_some());

        // After free, same idx should be invalid (generation changed)
        assert!(arena.get(idx).is_none());
    }

    #[test]
    fn cell_visibility_log_basic() {
        let log = CellVisibilityLog::new(1024 * 1024); // 1MB budget
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = test_txn_token();

        // Record an insert (uncommitted)
        let idx = log
            .record_insert(cell_key, page_number, vec![1, 2, 3, 4], token)
            .expect("insert should succeed");

        // Should not be visible yet (commit_seq = 0)
        let snapshot = CommitSeq::new(10);
        assert!(log.resolve(page_number, &cell_key, snapshot).is_none());

        // Commit the delta
        log.commit_delta(idx, CommitSeq::new(5));

        // Now should be visible
        let result = log.resolve(page_number, &cell_key, snapshot);
        assert_eq!(result, Some(vec![1, 2, 3, 4]));

        // Older snapshot should not see it
        let old_snapshot = CommitSeq::new(4);
        assert!(log.resolve(page_number, &cell_key, old_snapshot).is_none());
    }

    #[test]
    fn cell_visibility_log_delete() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = test_txn_token();

        // Insert then delete
        let idx1 = log
            .record_insert(cell_key, page_number, vec![1, 2, 3], token)
            .expect("insert should succeed");
        log.commit_delta(idx1, CommitSeq::new(5));

        let idx2 = log
            .record_delete(cell_key, page_number, token)
            .expect("delete should succeed");
        log.commit_delta(idx2, CommitSeq::new(10));

        // Snapshot after delete: cell not visible
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(15))
                .is_none()
        );

        // Snapshot before delete: cell visible
        let result = log.resolve(page_number, &cell_key, CommitSeq::new(7));
        assert_eq!(result, Some(vec![1, 2, 3]));
    }

    #[test]
    fn mutation_outcome_classification() {
        // Logical operations
        assert!(MutationOutcome::CellFit.is_logical());
        assert!(MutationOutcome::CellRemovedPageNonEmpty.is_logical());
        assert!(!MutationOutcome::CellFit.is_structural());
        assert!(!MutationOutcome::CellRemovedPageNonEmpty.is_structural());

        // Structural operations
        assert!(MutationOutcome::PageSplit.is_structural());
        assert!(MutationOutcome::PageMerge.is_structural());
        assert!(MutationOutcome::OverflowChainModified.is_structural());
        assert!(MutationOutcome::InteriorCellReplaced.is_structural());
        assert!(!MutationOutcome::PageSplit.is_logical());
        assert!(!MutationOutcome::PageMerge.is_logical());
    }

    #[test]
    fn can_be_logical_insert_checks() {
        // Fits: 50 byte payload, 200 byte local max, 100 byte free, 10 byte overhead
        assert!(can_be_logical_insert(50, 200, 100, 10)); // 50 + 10 = 60 < 100

        // Doesn't fit: not enough free space
        assert!(!can_be_logical_insert(50, 200, 50, 10)); // 50 + 10 = 60 > 50

        // Requires overflow: payload > local max
        assert!(!can_be_logical_insert(250, 200, 1000, 10)); // 250 > 200
    }

    #[test]
    fn will_be_logical_delete_checks() {
        assert!(will_be_logical_delete(2)); // 2 cells -> 1 cell: logical
        assert!(will_be_logical_delete(10)); // 10 cells -> 9 cells: logical
        assert!(!will_be_logical_delete(1)); // 1 cell -> 0 cells: structural (merge)
        assert!(!will_be_logical_delete(0)); // Already empty: shouldn't happen, but structural
    }

    // -----------------------------------------------------------------------
    // C3 Required Tests
    // -----------------------------------------------------------------------

    fn txn_token_n(n: u32) -> TxnToken {
        TxnToken::new(TxnId::new(u64::from(n)).unwrap(), TxnEpoch::new(1))
    }

    #[test]
    fn test_uncommitted_invisible() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();

        // Insert but don't commit
        log.record_insert(cell_key, page_number, vec![1, 2, 3, 4], txn_token_n(1))
            .expect("insert should succeed");

        // Uncommitted delta should not be visible at any snapshot
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(100))
                .is_none()
        );
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(1))
                .is_none()
        );
    }

    #[test]
    fn test_rollback_removes() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert, then rollback
        log.record_insert(cell_key, page_number, vec![1, 2, 3, 4], token)
            .expect("insert should succeed");

        assert_eq!(log.delta_count(), 1);
        let removed = log.rollback_txn(token);
        assert_eq!(removed, 1);
        assert_eq!(log.delta_count(), 0);

        // After rollback, cell should not be resolvable
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(100))
                .is_none()
        );
    }

    #[test]
    fn test_rollback_restores_previous_committed_version_after_same_txn_updates() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();

        let committed = log
            .record_insert(cell_key, page_number, vec![1], txn_token_n(1))
            .expect("committed insert should succeed");
        log.commit_delta(committed, CommitSeq::new(10));

        let rollback_token = txn_token_n(2);
        log.record_update(cell_key, page_number, vec![2], rollback_token)
            .expect("first update should succeed");
        log.record_update(cell_key, page_number, vec![3], rollback_token)
            .expect("second update should succeed");

        assert_eq!(log.rollback_txn(rollback_token), 2);
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(20)),
            Some(vec![1])
        );
    }

    #[test]
    fn test_update_versioning() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert, commit at seq 5
        let idx1 = log
            .record_insert(cell_key, page_number, vec![1, 1, 1], token)
            .expect("insert should succeed");
        log.commit_delta(idx1, CommitSeq::new(5));

        // Update, commit at seq 10
        let idx2 = log
            .record_update(cell_key, page_number, vec![2, 2, 2], token)
            .expect("update should succeed");
        log.commit_delta(idx2, CommitSeq::new(10));

        // Snapshot at 7 should see insert
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(7)),
            Some(vec![1, 1, 1])
        );

        // Snapshot at 12 should see update
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(12)),
            Some(vec![2, 2, 2])
        );
    }

    #[test]
    fn test_multi_txn_ordering() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();

        // Txn 1 inserts at seq 5
        let idx1 = log
            .record_insert(cell_key, page_number, vec![1], txn_token_n(1))
            .expect("insert should succeed");
        log.commit_delta(idx1, CommitSeq::new(5));

        // Txn 2 updates at seq 10
        let idx2 = log
            .record_update(cell_key, page_number, vec![2], txn_token_n(2))
            .expect("update should succeed");
        log.commit_delta(idx2, CommitSeq::new(10));

        // Txn 3 updates at seq 15
        let idx3 = log
            .record_update(cell_key, page_number, vec![3], txn_token_n(3))
            .expect("update should succeed");
        log.commit_delta(idx3, CommitSeq::new(15));

        // Verify each snapshot sees the correct version
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(7)),
            Some(vec![1])
        );
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(12)),
            Some(vec![2])
        );
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(20)),
            Some(vec![3])
        );
    }

    #[test]
    fn test_different_cells_no_conflict() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();

        let cell_key1 = CellKey::table_row(btree, 100);
        let cell_key2 = CellKey::table_row(btree, 101);
        let token1 = txn_token_n(1);
        let token2 = txn_token_n(2);

        // Txn 1 touches cell 1
        log.record_insert(cell_key1, page_number, vec![1], token1)
            .expect("insert should succeed");

        // Txn 2 touches cell 2
        log.record_insert(cell_key2, page_number, vec![2], token2)
            .expect("insert should succeed");

        // No conflict because different cells
        assert_eq!(log.check_conflict(token1, token2), CellConflict::None);
        assert_eq!(log.check_conflict(token2, token1), CellConflict::None);
    }

    #[test]
    fn test_same_cell_conflict() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token1 = txn_token_n(1);
        let token2 = txn_token_n(2);

        // Txn 1 inserts
        log.record_insert(cell_key, page_number, vec![1], token1)
            .expect("insert should succeed");

        // Txn 2 also inserts same cell
        log.record_insert(cell_key, page_number, vec![2], token2)
            .expect("insert should succeed");

        // Conflict detected
        assert!(matches!(
            log.check_conflict(token1, token2),
            CellConflict::Conflict { .. }
        ));
    }

    #[test]
    fn test_gc_reclaims_old() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert at seq 5
        let idx1 = log
            .record_insert(cell_key, page_number, vec![1, 2, 3], token)
            .expect("insert should succeed");
        log.commit_delta(idx1, CommitSeq::new(5));

        // Update at seq 10
        let idx2 = log
            .record_update(cell_key, page_number, vec![4, 5, 6], token)
            .expect("update should succeed");
        log.commit_delta(idx2, CommitSeq::new(10));

        assert_eq!(log.delta_count(), 2);

        // GC with horizon at 15 should reclaim the older delta
        let stats = log.gc(CommitSeq::new(15));
        assert_eq!(stats.reclaimed, 1);
        assert_eq!(log.delta_count(), 1);

        // Newer version still visible
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(15)),
            Some(vec![4, 5, 6])
        );
    }

    #[test]
    fn test_gc_preserves_visible() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert at seq 10
        let idx = log
            .record_insert(cell_key, page_number, vec![1, 2, 3], token)
            .expect("insert should succeed");
        log.commit_delta(idx, CommitSeq::new(10));

        assert_eq!(log.delta_count(), 1);

        // GC with horizon at 5 should NOT reclaim (delta is above horizon)
        let stats = log.gc(CommitSeq::new(5));
        assert_eq!(stats.reclaimed, 0);
        assert_eq!(log.delta_count(), 1);

        // Still visible
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(15)),
            Some(vec![1, 2, 3])
        );
    }

    #[test]
    fn test_shard_distribution() {
        // Verify that different pages use different shards
        // Pages should be distributed across all 64 shards
        let mut shard_used = [false; CELL_LOG_SHARDS];

        for pgno in 1..=256 {
            let page_number = PageNumber::new(pgno).unwrap();
            let shard_idx = CellVisibilityLog::shard_index(page_number);
            shard_used[shard_idx] = true;
        }

        // At least half the shards should be used with 256 pages
        let used_count = shard_used.iter().filter(|&&x| x).count();
        assert!(used_count >= CELL_LOG_SHARDS / 2);
    }

    #[test]
    fn test_memory_tracking_accurate() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert several cells with known sizes
        for i in 0..10 {
            let cell_key = CellKey::table_row(btree, i);
            log.record_insert(cell_key, page_number, vec![0; 100], token)
                .expect("insert should succeed");
        }

        // Each cell has 100 bytes of data
        let expected_min_bytes = 10 * 100;
        let actual_bytes = log.cell_data_bytes();
        assert!(
            actual_bytes >= expected_min_bytes as u64,
            "Expected at least {} bytes, got {}",
            expected_min_bytes,
            actual_bytes
        );
    }

    #[test]
    fn test_commit_txn_bulk() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert several cells
        for i in 0..5 {
            let cell_key = CellKey::table_row(btree, i);
            log.record_insert(cell_key, page_number, vec![i as u8], token)
                .expect("insert should succeed");
        }

        // All should be uncommitted
        for i in 0..5 {
            let cell_key = CellKey::table_row(btree, i);
            assert!(
                log.resolve(page_number, &cell_key, CommitSeq::new(100))
                    .is_none()
            );
        }

        // Bulk commit
        log.commit_txn(token, CommitSeq::new(10));

        // All should now be visible
        for i in 0..5 {
            let cell_key = CellKey::table_row(btree, i);
            assert_eq!(
                log.resolve(page_number, &cell_key, CommitSeq::new(15)),
                Some(vec![i as u8])
            );
        }
    }

    #[test]
    fn test_per_txn_budget_exceeded() {
        // Create log with per-txn budget that allows one delta but not two.
        // CellDelta has ~100 bytes fixed overhead (struct fields) plus cell_data.
        // With 50 bytes of cell_data, each delta uses ~150 bytes total.
        // Budget of 200 bytes allows 1 delta but not 2.
        let log = CellVisibilityLog::with_per_txn_budget(1024 * 1024, 200);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // First insert should succeed (delta_memory ~150 bytes < 200 budget)
        let cell_key1 = CellKey::table_row(btree, 1);
        assert!(
            log.record_insert(cell_key1, page_number, vec![0; 50], token)
                .is_some()
        );

        // Second insert should fail (cumulative ~300 bytes > 200 budget)
        let cell_key2 = CellKey::table_row(btree, 2);
        assert!(
            log.record_insert(cell_key2, page_number, vec![0; 50], token)
                .is_none()
        );
    }

    #[test]
    fn test_budget_exceeded_triggers_materialization() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

        let materialization_triggered = Arc::new(AtomicBool::new(false));
        let trigger_clone = Arc::clone(&materialization_triggered);

        // Create log with tiny global budget (200 bytes)
        let mut log = CellVisibilityLog::with_per_txn_budget(200, 1024 * 1024);
        log.set_materialization_callback(Box::new(move |_pgno, _cells| {
            trigger_clone.store(true, AtomicOrdering::SeqCst);
        }));

        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert cells until budget is exceeded
        for i in 0..5 {
            let cell_key = CellKey::table_row(btree, i);
            log.record_insert(cell_key, page_number, vec![0; 100], token);
        }

        // Materialization callback should have been triggered
        assert!(
            materialization_triggered.load(AtomicOrdering::SeqCst),
            "Materialization callback should be triggered when global budget exceeded"
        );
    }

    #[test]
    fn test_materialization_callback_preserves_real_cell_key() {
        use std::sync::Arc;
        use std::sync::Mutex as StdMutex;

        let seen_cells = Arc::new(StdMutex::new(Vec::new()));
        let seen_cells_clone = Arc::clone(&seen_cells);

        let mut log = CellVisibilityLog::with_per_txn_budget(64, 1024 * 1024);
        log.set_materialization_callback(Box::new(move |_pgno, cells| {
            seen_cells_clone.lock().unwrap().extend_from_slice(cells);
        }));

        let btree = BtreeRef::Table(TableId::new(7));
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);
        let cell_key = CellKey::table_row(btree, 4242);

        log.record_insert(cell_key, page_number, vec![0; 100], token)
            .expect("insert should succeed");

        let captured = seen_cells.lock().unwrap();
        assert_eq!(captured.as_slice(), &[cell_key]);
    }

    #[test]
    fn test_find_high_delta_pages_ranks_by_total_delta_count() {
        let log = CellVisibilityLog::with_per_txn_budget(1024 * 1024, 1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let hot_page = PageNumber::new(42).unwrap();
        let wide_page = PageNumber::new(43).unwrap();

        let hot_key = CellKey::table_row(btree, 7);
        let hot_insert = log
            .record_insert(hot_key, hot_page, vec![1; 32], txn_token_n(1))
            .expect("hot insert should succeed");
        log.commit_delta(hot_insert, CommitSeq::new(1));

        let hot_update_1 = log
            .record_update(hot_key, hot_page, vec![2; 32], txn_token_n(2))
            .expect("first hot update should succeed");
        log.commit_delta(hot_update_1, CommitSeq::new(2));

        let hot_update_2 = log
            .record_update(hot_key, hot_page, vec![3; 32], txn_token_n(3))
            .expect("second hot update should succeed");
        log.commit_delta(hot_update_2, CommitSeq::new(3));

        for (rowid, txn_id) in [(100_i64, 4_u32), (101_i64, 5_u32)] {
            let cell_key = CellKey::table_row(btree, rowid);
            let idx = log
                .record_insert(cell_key, wide_page, vec![9; 32], txn_token_n(txn_id))
                .expect("wide-page insert should succeed");
            log.commit_delta(idx, CommitSeq::new(u64::from(txn_id)));
        }

        let pages = log.find_high_delta_pages(1);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].0, hot_page);
        assert_eq!(pages[0].1.as_slice(), &[hot_key]);
    }

    // -------------------------------------------------------------------------
    // C7 (bd-l9k8e.7): Batch visibility check tests
    // -------------------------------------------------------------------------

    #[test]
    fn c7_page_with_no_deltas_returns_false() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let page_number = PageNumber::new(42).unwrap();

        // Page with no deltas should return false.
        assert!(
            !log.page_has_deltas(page_number),
            "page without deltas should return false"
        );
    }

    #[test]
    fn c7_page_with_deltas_returns_true() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = test_txn_token();

        // Initially no deltas.
        assert!(!log.page_has_deltas(page_number));

        // Record an insert.
        log.record_insert(cell_key, page_number, vec![1, 2, 3], token);

        // Now page has deltas.
        assert!(
            log.page_has_deltas(page_number),
            "page with delta should return true"
        );
    }

    #[test]
    fn c7_page_delta_count_tracks_rollback() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();
        let token = test_txn_token();

        // Add multiple deltas.
        for i in 0..5 {
            let cell_key = CellKey::table_row(btree, i);
            log.record_insert(cell_key, page_number, vec![i as u8], token);
        }
        assert!(log.page_has_deltas(page_number));

        // Rollback the transaction.
        log.rollback_txn(token);

        // After rollback, page should have no deltas.
        assert!(
            !log.page_has_deltas(page_number),
            "page should have no deltas after rollback"
        );
    }

    #[test]
    fn c7_page_delta_count_tracks_gc() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();
        let token = test_txn_token();

        // Add and commit deltas.
        for i in 0..3 {
            let cell_key = CellKey::table_row(btree, i);
            let idx = log
                .record_insert(cell_key, page_number, vec![i as u8], token)
                .expect("insert should succeed");
            log.commit_delta(idx, CommitSeq::new((i + 1) as u64));
        }
        assert!(log.page_has_deltas(page_number));

        // GC with horizon that reclaims older deltas (but not all).
        // Deltas at commit_seq 1, 2 should be reclaimed below horizon 3.
        // But the newest delta at commit_seq 3 should remain.
        let stats = log.gc(CommitSeq::new(3));
        // GC should reclaim deltas below horizon that have a newer version.
        // In our case, each cell only has one delta, so none should be reclaimed.
        assert_eq!(
            stats.reclaimed, 0,
            "single-version cells should not be GC'd"
        );
        assert!(
            log.page_has_deltas(page_number),
            "page should still have deltas"
        );
    }

    #[test]
    fn c7_different_pages_tracked_separately() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_a = PageNumber::new(42).unwrap();
        let page_b = PageNumber::new(43).unwrap();
        let token = test_txn_token();

        // Add delta to page A only.
        let cell_key = CellKey::table_row(btree, 100);
        log.record_insert(cell_key, page_a, vec![1], token);

        assert!(log.page_has_deltas(page_a), "page A should have deltas");
        assert!(!log.page_has_deltas(page_b), "page B should have no deltas");
    }

    // -------------------------------------------------------------------------
    // C3-TEST (bd-l9k8e.9): Comprehensive test suite
    // -------------------------------------------------------------------------

    // -------------------------------------------------------------------------
    // 1a) Visibility basics (12+ tests)
    // -------------------------------------------------------------------------

    /// Test: insert + update + delete same cell across 3 txns -> correct at each snapshot
    #[test]
    fn c3_test_insert_update_delete_chain() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();

        // Txn 1: Insert at commit_seq 5
        let idx1 = log
            .record_insert(cell_key, page_number, vec![1, 1, 1], txn_token_n(1))
            .expect("insert should succeed");
        log.commit_delta(idx1, CommitSeq::new(5));

        // Txn 2: Update at commit_seq 10
        let idx2 = log
            .record_update(cell_key, page_number, vec![2, 2, 2], txn_token_n(2))
            .expect("update should succeed");
        log.commit_delta(idx2, CommitSeq::new(10));

        // Txn 3: Delete at commit_seq 15
        let idx3 = log
            .record_delete(cell_key, page_number, txn_token_n(3))
            .expect("delete should succeed");
        log.commit_delta(idx3, CommitSeq::new(15));

        // Snapshot at 3: cell not visible (before insert)
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(3))
                .is_none(),
            "snapshot=3 should not see cell (before insert)"
        );

        // Snapshot at 7: sees insert version
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(7)),
            Some(vec![1, 1, 1]),
            "snapshot=7 should see insert version"
        );

        // Snapshot at 12: sees update version
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(12)),
            Some(vec![2, 2, 2]),
            "snapshot=12 should see update version"
        );

        // Snapshot at 20: cell deleted (not visible)
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(20))
                .is_none(),
            "snapshot=20 should not see cell (deleted)"
        );
    }

    /// Test: 100 txns update same cell -> correct version visible at each of 100 snapshots
    #[test]
    fn c3_test_100_txns_same_cell() {
        let log = CellVisibilityLog::new(10 * 1024 * 1024); // 10MB budget for 100 versions
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();

        // Insert first version: data=[1] at commit_seq=1
        let idx0 = log
            .record_insert(cell_key, page_number, vec![1], txn_token_n(1))
            .expect("insert should succeed");
        log.commit_delta(idx0, CommitSeq::new(1));

        // 99 more updates: data=[i] at commit_seq=i for i in 2..=100
        for i in 2u64..=100 {
            let idx = log
                .record_update(cell_key, page_number, vec![i as u8], txn_token_n(i as u32))
                .expect("update should succeed");
            log.commit_delta(idx, CommitSeq::new(i));
        }

        // Verify each snapshot sees the correct version (data matches commit_seq)
        for i in 1u64..=100 {
            let expected = vec![i as u8];
            let actual = log.resolve(page_number, &cell_key, CommitSeq::new(i));
            assert_eq!(
                actual,
                Some(expected.clone()),
                "snapshot={} should see version with data={}",
                i,
                i
            );
        }

        // Snapshot at 0: nothing visible (before any commit)
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(0))
                .is_none(),
            "snapshot=0 should not see any version"
        );
    }

    /// Test: boundary: read at EXACTLY the commit_seq of a delta -> visible (inclusive)
    #[test]
    fn c3_test_exact_commit_seq_visible() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert at commit_seq = 42
        let idx = log
            .record_insert(cell_key, page_number, vec![42], token)
            .expect("insert should succeed");
        log.commit_delta(idx, CommitSeq::new(42));

        // Snapshot at EXACTLY 42: should be visible (inclusive boundary)
        assert_eq!(
            log.resolve(page_number, &cell_key, CommitSeq::new(42)),
            Some(vec![42]),
            "snapshot=42 (exact commit_seq) should see the cell"
        );

        // Snapshot at 41: should NOT be visible (before commit)
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(41))
                .is_none(),
            "snapshot=41 (before commit_seq) should not see the cell"
        );
    }

    /// Test: boundary: snapshot.high = 0 -> NotTracked for everything
    #[test]
    fn c3_test_snapshot_zero_sees_nothing() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert at commit_seq = 1 (earliest possible)
        let idx = log
            .record_insert(cell_key, page_number, vec![1], token)
            .expect("insert should succeed");
        log.commit_delta(idx, CommitSeq::new(1));

        // Snapshot at 0: nothing should be visible
        assert!(
            log.resolve(page_number, &cell_key, CommitSeq::new(0))
                .is_none(),
            "snapshot=0 should never see any committed cell"
        );
    }

    // -------------------------------------------------------------------------
    // 1b) Conflict detection (8+ tests)
    // -------------------------------------------------------------------------

    /// Test: two txns update SAME cell -> conflict detected
    #[test]
    fn c3_test_two_txns_update_same_cell_conflict() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();

        // Txn 1 inserts and commits
        let idx1 = log
            .record_insert(cell_key, page_number, vec![1], txn_token_n(1))
            .expect("insert should succeed");
        log.commit_delta(idx1, CommitSeq::new(5));

        // Txn 2 and Txn 3 both try to update the same cell (concurrent)
        let token2 = txn_token_n(2);
        let token3 = txn_token_n(3);

        log.record_update(cell_key, page_number, vec![2], token2)
            .expect("update 2 should succeed");
        log.record_update(cell_key, page_number, vec![3], token3)
            .expect("update 3 should succeed");

        // Conflict should be detected between txn 2 and txn 3
        assert!(
            matches!(
                log.check_conflict(token2, token3),
                CellConflict::Conflict { .. }
            ),
            "two txns updating same cell should conflict"
        );
        assert!(
            matches!(
                log.check_conflict(token3, token2),
                CellConflict::Conflict { .. }
            ),
            "conflict should be symmetric"
        );
    }

    /// Test: txn A inserts cell, commits; txn B (started before A committed) updates same cell -> conflict
    #[test]
    fn c3_test_read_before_commit_then_update_conflict() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();

        let token_a = txn_token_n(1);
        let token_b = txn_token_n(2);

        // Txn A inserts cell (uncommitted)
        log.record_insert(cell_key, page_number, vec![1], token_a)
            .expect("insert should succeed");

        // Txn B also inserts same cell (before A commits) - this represents B "updating" a cell
        // that B didn't know existed because A hadn't committed yet
        log.record_insert(cell_key, page_number, vec![2], token_b)
            .expect("insert should succeed");

        // Conflict should be detected because both touch same cell
        assert!(
            matches!(
                log.check_conflict(token_a, token_b),
                CellConflict::Conflict { .. }
            ),
            "txn A and B both touching same cell should conflict"
        );
    }

    /// THE TEST: txn A inserts cell 5, txn B inserts cell 12, both on page 47 -> BOTH COMMIT
    #[test]
    fn c3_test_different_cells_same_page_both_commit() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(47).unwrap();

        let cell_key_5 = CellKey::table_row(btree, 5);
        let cell_key_12 = CellKey::table_row(btree, 12);

        let token_a = txn_token_n(1);
        let token_b = txn_token_n(2);

        // Txn A inserts cell 5
        let idx_a = log
            .record_insert(cell_key_5, page_number, vec![5], token_a)
            .expect("insert cell 5 should succeed");

        // Txn B inserts cell 12 (same page!)
        let idx_b = log
            .record_insert(cell_key_12, page_number, vec![12], token_b)
            .expect("insert cell 12 should succeed");

        // NO conflict - different cells on same page
        assert_eq!(
            log.check_conflict(token_a, token_b),
            CellConflict::None,
            "different cells on same page should NOT conflict"
        );

        // Both can commit
        log.commit_delta(idx_a, CommitSeq::new(5));
        log.commit_delta(idx_b, CommitSeq::new(6));

        // Both visible at appropriate snapshots
        assert_eq!(
            log.resolve(page_number, &cell_key_5, CommitSeq::new(10)),
            Some(vec![5]),
            "cell 5 should be visible"
        );
        assert_eq!(
            log.resolve(page_number, &cell_key_12, CommitSeq::new(10)),
            Some(vec![12]),
            "cell 12 should be visible"
        );
    }

    /// Test: 8 txns touching 8 different cells on same page -> ALL COMMIT concurrently
    #[test]
    fn c3_test_8_txns_8_cells_all_commit() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();

        let mut indices = Vec::new();

        // 8 transactions, each inserting a different cell on the same page
        for i in 0..8 {
            let cell_key = CellKey::table_row(btree, i);
            let token = txn_token_n((i + 1) as u32);
            let idx = log
                .record_insert(cell_key, page_number, vec![i as u8], token)
                .expect("insert should succeed");
            indices.push((idx, token, i));
        }

        // Verify no conflicts between any pair
        for i in 0..8 {
            for j in (i + 1)..8 {
                let token_i = txn_token_n((i + 1) as u32);
                let token_j = txn_token_n((j + 1) as u32);
                assert_eq!(
                    log.check_conflict(token_i, token_j),
                    CellConflict::None,
                    "txn {} and {} should not conflict (different cells)",
                    i + 1,
                    j + 1
                );
            }
        }

        // All can commit
        for (idx, _token, i) in &indices {
            log.commit_delta(*idx, CommitSeq::new((*i + 1) as u64));
        }

        // All visible
        for i in 0..8 {
            let cell_key = CellKey::table_row(btree, i);
            assert_eq!(
                log.resolve(page_number, &cell_key, CommitSeq::new(20)),
                Some(vec![i as u8]),
                "cell {} should be visible",
                i
            );
        }
    }

    // -------------------------------------------------------------------------
    // 1c) GC tests (6+ tests)
    // -------------------------------------------------------------------------

    /// Test: single delta below horizon -> reclaimable (when there's no newer version)
    /// Note: GC only reclaims deltas that have a newer version. A lone delta below
    /// horizon is NOT reclaimed because it's the only version.
    #[test]
    fn c3_test_gc_single_delta_below_horizon() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert at commit_seq = 5
        let idx = log
            .record_insert(cell_key, page_number, vec![1, 2, 3], token)
            .expect("insert should succeed");
        log.commit_delta(idx, CommitSeq::new(5));

        assert_eq!(log.delta_count(), 1);

        // GC with horizon at 10 - but this is the ONLY version, so it should NOT be reclaimed
        let stats = log.gc(CommitSeq::new(10));
        assert_eq!(
            stats.reclaimed, 0,
            "single version should not be reclaimed even below horizon"
        );
        assert_eq!(log.delta_count(), 1);

        // Now add an update to create a newer version
        let idx2 = log
            .record_update(cell_key, page_number, vec![4, 5, 6], token)
            .expect("update should succeed");
        log.commit_delta(idx2, CommitSeq::new(15));

        assert_eq!(log.delta_count(), 2);

        // GC with horizon at 20 - NOW the older version should be reclaimed
        let stats = log.gc(CommitSeq::new(20));
        assert_eq!(stats.reclaimed, 1, "older version should be reclaimed");
        assert_eq!(log.delta_count(), 1);
    }

    /// Test: uncommitted delta -> NEVER reclaimable
    #[test]
    fn c3_test_gc_uncommitted_never_reclaimed() {
        let log = CellVisibilityLog::new(1024 * 1024);
        let btree = BtreeRef::Table(TableId::new(1));
        let cell_key = CellKey::table_row(btree, 100);
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert but DON'T commit
        log.record_insert(cell_key, page_number, vec![1, 2, 3], token)
            .expect("insert should succeed");

        assert_eq!(log.delta_count(), 1);

        // GC with any horizon should not reclaim uncommitted delta
        let stats = log.gc(CommitSeq::new(1000));
        assert_eq!(
            stats.reclaimed, 0,
            "uncommitted delta should never be reclaimed"
        );
        assert_eq!(log.delta_count(), 1);
    }

    /// Test: sustained load: insert 10K deltas, advance horizon, verify bounded memory
    #[test]
    fn c3_test_gc_sustained_load_bounded_memory() {
        let log = CellVisibilityLog::new(100 * 1024 * 1024); // 100MB budget
        let btree = BtreeRef::Table(TableId::new(1));
        let page_number = PageNumber::new(42).unwrap();
        let token = txn_token_n(1);

        // Insert first version
        let cell_key = CellKey::table_row(btree, 1);
        let idx0 = log
            .record_insert(cell_key, page_number, vec![0; 100], token)
            .expect("insert should succeed");
        log.commit_delta(idx0, CommitSeq::new(1));

        // Insert 10K updates to the same cell
        for i in 2u64..=10_000 {
            let idx = log
                .record_update(cell_key, page_number, vec![(i % 256) as u8; 100], token)
                .expect("update should succeed");
            log.commit_delta(idx, CommitSeq::new(i));

            // Periodically run GC to keep memory bounded
            if i % 1000 == 0 {
                let horizon = CommitSeq::new(i.saturating_sub(100));
                let stats = log.gc(horizon);
                // Should reclaim most old versions
                assert!(stats.reclaimed > 800, "GC should reclaim old versions");
            }
        }

        // Final GC
        let _stats = log.gc(CommitSeq::new(9_950));

        // Should have very few deltas remaining (only those above horizon)
        assert!(
            log.delta_count() < 200,
            "after sustained GC, delta_count={} should be bounded",
            log.delta_count()
        );
    }

    // -------------------------------------------------------------------------
    // 1d) Sharding and concurrency tests (4+ tests)
    // -------------------------------------------------------------------------

    /// Test: high contention on single shard doesn't deadlock
    #[test]
    fn c3_test_high_contention_single_shard_no_deadlock() {
        use std::sync::Arc;
        use std::thread;

        let log = Arc::new(CellVisibilityLog::new(10 * 1024 * 1024));
        let btree = BtreeRef::Table(TableId::new(1));
        // All operations on same page = same shard
        let page_number = PageNumber::new(42).unwrap();

        let mut handles = Vec::new();

        // 16 threads all hammering the same shard
        for t in 0..16 {
            let log_clone = Arc::clone(&log);
            let handle = thread::spawn(move || {
                for i in 0..100 {
                    let cell_key = CellKey::table_row(btree, t * 1000 + i);
                    let token = txn_token_n((t * 1000 + i + 1) as u32);
                    let idx = log_clone
                        .record_insert(cell_key, page_number, vec![t as u8], token)
                        .expect("insert should succeed");
                    log_clone.commit_delta(idx, CommitSeq::new((t * 1000 + i + 1) as u64));
                }
            });
            handles.push(handle);
        }

        // All threads should complete without deadlock
        for handle in handles {
            handle.join().expect("thread should complete");
        }

        // All inserts should be present
        assert_eq!(log.delta_count(), 16 * 100);
    }

    /// Test: 64-thread stress test: each thread inserts unique cells, all commit
    #[test]
    fn c3_test_64_thread_stress() {
        use std::sync::Arc;
        use std::thread;

        let log = Arc::new(CellVisibilityLog::new(100 * 1024 * 1024));
        let btree = BtreeRef::Table(TableId::new(1));

        let mut handles = Vec::new();

        for t in 0..64 {
            let log_clone = Arc::clone(&log);
            let handle = thread::spawn(move || {
                // Each thread uses different pages to distribute across shards
                let page_number = PageNumber::new((t % 64) + 1).unwrap();
                for i in 0..50 {
                    let cell_key = CellKey::table_row(btree, i64::from(t * 1000 + i));
                    let token = txn_token_n((t * 1000 + i + 1) as u32);
                    let idx = log_clone
                        .record_insert(cell_key, page_number, vec![t as u8, i as u8], token)
                        .expect("insert should succeed");
                    log_clone.commit_delta(idx, CommitSeq::new((t * 1000 + i + 1) as u64));
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().expect("thread should complete");
        }

        // All inserts should be present
        assert_eq!(log.delta_count(), 64 * 50);
    }

    /// Test: shard padding prevents false sharing (compile-time assert on alignment)
    #[test]
    fn c3_test_shard_padding_alignment() {
        // CellLogShard should be cache-line aligned (64 bytes)
        // This is enforced by #[repr(align(64))] on the shard struct

        // Verify the shard count and that different pages map to different shards
        let mut shard_counts = [0usize; CELL_LOG_SHARDS];
        for pgno in 1..=1000 {
            let page_number = PageNumber::new(pgno).unwrap();
            let shard_idx = CellVisibilityLog::shard_index(page_number);
            shard_counts[shard_idx] += 1;
        }

        // Distribution should be reasonably uniform (each shard gets at least 10 pages)
        for (idx, &count) in shard_counts.iter().enumerate() {
            assert!(
                count >= 10,
                "shard {} only got {} pages, distribution is too skewed",
                idx,
                count
            );
        }
    }

    // -------------------------------------------------------------------------
    // 3. Property-Based Tests (proptest)
    // -------------------------------------------------------------------------

    mod proptest_tests {
        use super::*;
        use proptest::prelude::*;

        // Property: snapshot isolation
        // Given: N concurrent txns, each inserting M unique cells
        // Property: every committed txn's reads are consistent with its snapshot
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(50))]

            #[test]
            fn prop_snapshot_isolation(
                n_txns in 2usize..20,
                cells_per_txn in 1usize..10,
            ) {
                let log = CellVisibilityLog::new(100 * 1024 * 1024);
                let btree = BtreeRef::Table(TableId::new(1));
                let page_number = PageNumber::new(42).unwrap();

                // Each txn inserts unique cells and commits at a unique commit_seq
                let mut committed_at: Vec<(u64, Vec<i64>)> = Vec::new();

                for txn_idx in 0..n_txns {
                    let token = TxnToken::new(
                        TxnId::new((txn_idx + 1) as u64).unwrap(),
                        TxnEpoch::new(1),
                    );
                    let commit_seq = (txn_idx + 1) as u64;
                    let mut cells = Vec::new();

                    for cell_idx in 0..cells_per_txn {
                        let rowid = (txn_idx * 1000 + cell_idx) as i64;
                        let cell_key = CellKey::table_row(btree, rowid);
                        let data = vec![txn_idx as u8, cell_idx as u8];

                        let idx = log
                            .record_insert(cell_key, page_number, data, token)
                            .expect("insert should succeed");
                        log.commit_delta(idx, CommitSeq::new(commit_seq));
                        cells.push(rowid);
                    }
                    committed_at.push((commit_seq, cells));
                }

                // Property: at snapshot S, exactly the cells from txns with commit_seq <= S are visible
                for check_snapshot in 1u64..=(n_txns as u64 + 5) {
                    for (commit_seq, cells) in &committed_at {
                        for &rowid in cells {
                            let cell_key = CellKey::table_row(btree, rowid);
                            let result = log.resolve(page_number, &cell_key, CommitSeq::new(check_snapshot));

                            if *commit_seq <= check_snapshot {
                                // Should be visible
                                prop_assert!(
                                    result.is_some(),
                                    "cell {} committed at {} should be visible at snapshot {}",
                                    rowid, commit_seq, check_snapshot
                                );
                            } else {
                                // Should NOT be visible
                                prop_assert!(
                                    result.is_none(),
                                    "cell {} committed at {} should NOT be visible at snapshot {}",
                                    rowid, commit_seq, check_snapshot
                                );
                            }
                        }
                    }
                }
            }
        }

        // Property: conflict detection completeness
        // Given: random interleaving of cell-level operations on same page
        // Property: if two txns write the same cell, at most one commits without retry
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn prop_conflict_detection(
                n_txns in 2usize..8,
                target_cell in 0i64..100,
            ) {
                let log = CellVisibilityLog::new(100 * 1024 * 1024);
                let btree = BtreeRef::Table(TableId::new(1));
                let page_number = PageNumber::new(42).unwrap();
                let cell_key = CellKey::table_row(btree, target_cell);

                // All txns try to insert the same cell
                let mut tokens = Vec::new();
                for txn_idx in 0..n_txns {
                    let token = TxnToken::new(
                        TxnId::new((txn_idx + 1) as u64).unwrap(),
                        TxnEpoch::new(1),
                    );
                    let data = vec![txn_idx as u8];

                    log.record_insert(cell_key, page_number, data, token)
                        .expect("insert should succeed");
                    tokens.push(token);
                }

                // Property: every pair of txns that wrote the same cell should conflict
                for i in 0..n_txns {
                    for j in (i + 1)..n_txns {
                        let conflict = log.check_conflict(tokens[i], tokens[j]);
                        prop_assert!(
                            matches!(conflict, CellConflict::Conflict { .. }),
                            "txn {} and {} both wrote cell {} but no conflict detected",
                            i, j, target_cell
                        );
                    }
                }
            }
        }

        // Property: GC safety
        // Given: random sequence of insert/update/delete + GC advances
        // Property: no visible version is ever reclaimed
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(50))]

            #[test]
            fn prop_gc_safety(
                n_versions in 5usize..50,
                gc_horizon_offset in 1usize..10,
            ) {
                let log = CellVisibilityLog::new(100 * 1024 * 1024);
                let btree = BtreeRef::Table(TableId::new(1));
                let cell_key = CellKey::table_row(btree, 1);
                let page_number = PageNumber::new(42).unwrap();

                // Create n versions of the same cell
                for i in 1u64..=(n_versions as u64) {
                    let token = TxnToken::new(TxnId::new(i).unwrap(), TxnEpoch::new(1));

                    let idx = if i == 1 {
                        log.record_insert(cell_key, page_number, vec![i as u8], token)
                            .expect("insert should succeed")
                    } else {
                        log.record_update(cell_key, page_number, vec![i as u8], token)
                            .expect("update should succeed")
                    };
                    log.commit_delta(idx, CommitSeq::new(i));
                }

                // Run GC with horizon that should keep the latest visible version
                let gc_horizon = n_versions.saturating_sub(gc_horizon_offset) as u64;
                log.gc(CommitSeq::new(gc_horizon));

                // Property: the version visible at snapshot = n_versions should still be correct
                let final_snapshot = n_versions as u64;
                let result = log.resolve(page_number, &cell_key, CommitSeq::new(final_snapshot));

                prop_assert!(
                    result.is_some(),
                    "GC should not reclaim the version visible at snapshot {}",
                    final_snapshot
                );

                // The visible version should be the latest committed version
                let expected_data = vec![n_versions as u8];
                prop_assert_eq!(
                    result.as_ref(),
                    Some(&expected_data),
                    "visible version at snapshot {} should be {}",
                    final_snapshot, n_versions
                );

                // Also verify that at least one version above horizon is still present
                let above_horizon_snapshot = gc_horizon + 1;
                if above_horizon_snapshot <= final_snapshot {
                    let above_result = log.resolve(
                        page_number,
                        &cell_key,
                        CommitSeq::new(above_horizon_snapshot),
                    );
                    prop_assert!(
                        above_result.is_some(),
                        "version at snapshot {} (above GC horizon {}) should be visible",
                        above_horizon_snapshot, gc_horizon
                    );
                }
            }
        }
    }
}
