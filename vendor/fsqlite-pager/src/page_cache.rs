//! Zero-copy page cache backed by [`PageBufPool`] (§1.5 Mechanical Sympathy, bd-22n.2).
//!
//! The cache stores pages as [`PageBuf`] handles indexed by [`PageNumber`].
//! Page reads go directly from VFS into a pool-allocated buffer with no
//! intermediate heap allocation.  Callers receive `&[u8]` references into
//! the cached buffer — never copies.
//!
//! This module is the *plumbing layer* for zero-copy I/O; the full ARC
//! eviction policy lives in a higher-level module (bd-7pu).
//!
//! # Sharded Page Cache (bd-3wop3.2)
//!
//! [`ShardedPageCache`] partitions the page-number space across 128 shards,
//! each protected by its own mutex. This eliminates the global lock contention
//! that limited concurrent writer throughput to 8-16 threads.
//!
//! Shard selection uses a multiplicative hash of the page number to ensure
//! good distribution even for sequential page access patterns (common during
//! B-tree scans). Each shard is cache-line aligned (64 bytes) to prevent
//! false sharing between adjacent shards.

use std::cell::Cell;

use fsqlite_error::{FrankenError, Result};
use fsqlite_types::cx::Cx;
use fsqlite_types::sync_primitives::Mutex;
use fsqlite_types::{PageNumber, PageSize};
use fsqlite_vfs::VfsFile;

use crate::page_buf::{PageBuf, PageBufPool};

// ---------------------------------------------------------------------------
// PageCache
// ---------------------------------------------------------------------------

/// Point-in-time page-cache counters and gauges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageCacheMetricsSnapshot {
    /// Number of successful cache probes.
    pub hits: u64,
    /// Number of failed cache probes.
    pub misses: u64,
    /// Number of fresh pages admitted into the cache.
    pub admits: u64,
    /// Number of pages evicted from the cache.
    pub evictions: u64,
    /// Number of pages currently resident in the cache.
    pub cached_pages: usize,
    /// Configured buffer-pool capacity (max page buffers).
    pub pool_capacity: usize,
    /// Percent of cached pages currently dirty (0-100).
    pub dirty_ratio_pct: u64,
    /// Adaptive cache "recent" list size (ARC-compatible gauge).
    pub t1_size: usize,
    /// Adaptive cache "frequent" list size (ARC-compatible gauge).
    pub t2_size: usize,
    /// ARC ghost list B1 size (compatibility gauge).
    pub b1_size: usize,
    /// ARC ghost list B2 size (compatibility gauge).
    pub b2_size: usize,
    /// ARC adaptive target parameter P (compatibility gauge).
    pub p_target: usize,
    /// Number of pages that currently have multiple visible MVCC versions.
    pub mvcc_multi_version_pages: usize,
}

impl PageCacheMetricsSnapshot {
    /// Total cache probes (`hits + misses`).
    #[must_use]
    pub fn total_accesses(self) -> u64 {
        self.hits.saturating_add(self.misses)
    }

    /// Hit-rate as a percentage in `[0.0, 100.0]`.
    #[must_use]
    pub fn hit_rate_percent(self) -> f64 {
        let total = self.total_accesses();
        if total == 0 {
            0.0
        } else {
            (self.hits as f64 * 100.0) / total as f64
        }
    }
}

/// Simple page cache: `PageNumber → PageBuf`.
///
/// All buffers are drawn from a shared [`PageBufPool`].  On eviction the
/// backing allocation is returned to the pool for reuse, avoiding hot-path
/// heap allocations.
///
/// The cache does **not** implement an eviction policy — that is the
/// responsibility of the higher-level ARC cache (§6).  This type is the
/// low-level storage layer that proves the zero-copy invariant.
pub struct PageCache {
    pool: PageBufPool,
    pages: std::collections::HashMap<PageNumber, PageBuf, foldhash::fast::FixedState>,
    page_size: PageSize,
    hits: Cell<u64>,
    misses: Cell<u64>,
    admits: Cell<u64>,
    evictions: Cell<u64>,
}

impl PageCache {
    /// Create a new, empty `PageCache` configured for the given `page_size`.
    pub fn new(page_size: PageSize) -> Self {
        Self::with_pool(PageBufPool::new(page_size, 65_536), page_size)
    }

    /// Create a new `PageCache` using an existing `PageBufPool`.
    pub fn with_pool(pool: PageBufPool, page_size: PageSize) -> Self {
        Self {
            pool,
            pages: std::collections::HashMap::with_hasher(foldhash::fast::FixedState::default()),
            page_size,
            hits: Cell::new(0),
            misses: Cell::new(0),
            admits: Cell::new(0),
            evictions: Cell::new(0),
        }
    }

    /// Access the underlying page pool.
    pub fn pool(&self) -> &PageBufPool {
        &self.pool
    }

    /// Number of pages currently in the cache.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Retrieve a page from the cache, updating eviction metrics.
    pub fn get(&self, page_no: PageNumber) -> Option<&[u8]> {
        if let Some(page) = self.pages.get(&page_no) {
            self.hits.set(self.hits.get().saturating_add(1));
            Some(page.as_slice())
        } else {
            self.misses.set(self.misses.get().saturating_add(1));
            None
        }
    }

    /// Get a mutable reference to a cached page.
    ///
    /// Returns `None` if the page is not in the cache.  Callers can modify
    /// the page in place; the dirty-tracking flag is managed by the higher
    /// layer.
    #[inline]
    pub fn get_mut(&mut self, page_no: PageNumber) -> Option<&mut [u8]> {
        if let Some(page) = self.pages.get_mut(&page_no) {
            self.hits.set(self.hits.get().saturating_add(1));
            Some(page.as_mut_slice())
        } else {
            self.misses.set(self.misses.get().saturating_add(1));
            None
        }
    }

    /// Returns `true` if the page is present in the cache.
    #[inline]
    #[must_use]
    pub fn contains(&self, page_no: PageNumber) -> bool {
        self.pages.contains_key(&page_no)
    }

    // --- Read / Write through VFS ---

    /// Read a page from a VFS file into the cache.
    ///
    /// If the page is already cached, this is a no-op and returns the
    /// existing reference.  Otherwise a buffer is acquired from the pool,
    /// the page is read directly into it via [`VfsFile::read`], and a
    /// reference to the cached data is returned.
    ///
    /// **Zero-copy guarantee:** the buffer passed to `VfsFile::read` is the
    /// same memory that the returned `&[u8]` points into.
    pub fn read_page(
        &mut self,
        cx: &Cx,
        file: &mut impl VfsFile,
        page_no: PageNumber,
    ) -> Result<&[u8]> {
        if !self.contains(page_no) {
            let mut buf = self.pool.acquire()?;
            let offset = page_offset(page_no, self.page_size);
            let bytes_read = file.read(cx, buf.as_mut_slice(), offset)?;
            if bytes_read < self.page_size.as_usize() {
                return Err(fsqlite_error::FrankenError::DatabaseCorrupt {
                    detail: format!(
                        "short read fetching page {page}: got {bytes_read} of {page_size}",
                        page = page_no.get(),
                        page_size = self.page_size.as_usize()
                    ),
                });
            }
            self.pages.insert(page_no, buf);
            self.admits.set(self.admits.get().saturating_add(1));
        }
        // SAFETY (logical): we just ensured the key exists above.
        Ok(self.pages.get(&page_no).expect("just inserted").as_slice())
    }

    /// Write a cached page out to a VFS file.
    ///
    /// The page data is written directly from the pool-allocated buffer —
    /// no intermediate staging copy.
    ///
    /// Returns `Err` if the page is not in the cache.
    pub fn write_page(&self, cx: &Cx, file: &mut impl VfsFile, page_no: PageNumber) -> Result<()> {
        let Some(buf) = self.pages.get(&page_no) else {
            self.misses.set(self.misses.get().saturating_add(1));
            return Err(fsqlite_error::FrankenError::internal(format!(
                "page {} not in cache",
                page_no
            )));
        };
        self.hits.set(self.hits.get().saturating_add(1));
        let offset = page_offset(page_no, self.page_size);
        file.write(cx, buf.as_slice(), offset)?;
        Ok(())
    }

    /// Insert a fresh (zeroed) page into the cache.
    ///
    /// Returns a mutable reference so the caller can populate it.
    pub fn insert_fresh(&mut self, page_no: PageNumber) -> Result<&mut [u8]> {
        // Freshly acquired buffers from the pool may contain stale data.
        // Zero the buffer to match the "new page" semantics.
        let mut buf = self.pool.acquire()?;
        buf.as_mut_slice().fill(0);

        let (out, admitted_new) = match self.pages.entry(page_no) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(buf);
                (entry.into_mut().as_mut_slice(), false)
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                (entry.insert(buf).as_mut_slice(), true)
            }
        };
        if admitted_new {
            self.admits.set(self.admits.get().saturating_add(1));
        }
        Ok(out)
    }

    /// Directly insert an existing `PageBuf` into the cache.
    pub fn insert_buffer(&mut self, page_no: PageNumber, buf: PageBuf) {
        let admitted_new = match self.pages.entry(page_no) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(buf);
                false
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(buf);
                true
            }
        };
        if admitted_new {
            self.admits.set(self.admits.get().saturating_add(1));
        }
    }

    // --- Eviction ---

    /// Evict a page from the cache, returning its buffer to the pool.
    ///
    /// Returns `true` if the page was present.
    pub fn evict(&mut self, page_no: PageNumber) -> bool {
        // Dropping the PageBuf returns it to the pool via Drop impl.
        let removed = self.pages.remove(&page_no).is_some();
        if removed {
            self.evictions.set(self.evictions.get().saturating_add(1));
        }
        removed
    }

    /// Evict an arbitrary page from the cache to free up space.
    ///
    /// Returns `true` if a page was evicted, `false` if the cache was empty.
    pub fn evict_any(&mut self) -> bool {
        let key = self.pages.keys().next().copied();
        if let Some(key) = key {
            self.pages.remove(&key);
            self.evictions.set(self.evictions.get().saturating_add(1));
            true
        } else {
            false
        }
    }

    /// Evict all pages from the cache.
    pub fn clear(&mut self) {
        let removed = self.pages.len();
        let removed_u64 = u64::try_from(removed).unwrap_or(u64::MAX);
        self.evictions
            .set(self.evictions.get().saturating_add(removed_u64));
        self.pages.clear();
    }

    /// Capture current cache metrics.
    #[must_use]
    pub fn metrics_snapshot(&self) -> PageCacheMetricsSnapshot {
        let cached_pages = self.pages.len();
        PageCacheMetricsSnapshot {
            hits: self.hits.get(),
            misses: self.misses.get(),
            admits: self.admits.get(),
            evictions: self.evictions.get(),
            cached_pages,
            pool_capacity: self.pool.capacity(),
            dirty_ratio_pct: 0,
            t1_size: cached_pages,
            t2_size: 0,
            b1_size: 0,
            b2_size: 0,
            p_target: cached_pages,
            mvcc_multi_version_pages: 0,
        }
    }

    /// Reset cache counters while preserving resident pages and configuration.
    pub fn reset_metrics(&mut self) {
        self.hits.set(0);
        self.misses.set(0);
        self.admits.set(0);
        self.evictions.set(0);
    }
}

impl std::fmt::Debug for PageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageCache")
            .field("page_size", &self.page_size)
            .field("cached_pages", &self.pages.len())
            .field("pool", &self.pool)
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .field("admits", &self.admits)
            .field("evictions", &self.evictions)
            .field("metrics", &self.metrics_snapshot())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ShardedPageCache (bd-3wop3.2)
// ---------------------------------------------------------------------------

/// Number of shards in [`ShardedPageCache`].
///
/// Must be a power of 2 for efficient masking. 128 shards provides good
/// scalability up to ~64 concurrent writers while keeping memory overhead
/// reasonable (~8KB for shard metadata on 64-byte cache lines).
///
/// Future: consider scaling with `std::thread::available_parallelism()` for
/// small embedded targets (fewer shards) or large servers (more shards).
const SHARD_COUNT: usize = 128;

/// Mask for shard index calculation (`SHARD_COUNT - 1`).
const SHARD_MASK: usize = SHARD_COUNT - 1;

/// Golden ratio constant for multiplicative hashing.
///
/// This is the 32-bit fractional part of the golden ratio (2^32 / φ).
/// Multiplicative hashing with this constant provides excellent distribution
/// even for sequential keys, which is critical for B-tree scan patterns.
const GOLDEN_RATIO_32: u32 = 2_654_435_769;

/// A single shard of the page cache.
///
/// Each shard contains its own hash map and metrics counters. The shard is
/// cache-line aligned to prevent false sharing between adjacent shards when
/// accessed by different threads.
#[repr(align(64))]
struct PageCacheShard {
    pages: std::collections::HashMap<PageNumber, PageBuf, foldhash::fast::FixedState>,
    /// Local hit counter (aggregated on metrics snapshot).
    hits: u64,
    /// Local miss counter.
    misses: u64,
    /// Local admit counter.
    admits: u64,
    /// Local eviction counter.
    evictions: u64,
}

impl PageCacheShard {
    /// Create a new empty shard.
    fn new() -> Self {
        Self {
            pages: std::collections::HashMap::with_hasher(foldhash::fast::FixedState::default()),
            hits: 0,
            misses: 0,
            admits: 0,
            evictions: 0,
        }
    }

    /// Number of pages in this shard.
    #[inline]
    fn len(&self) -> usize {
        self.pages.len()
    }

    /// Check if a page is present in this shard.
    #[inline]
    fn contains(&self, page_no: PageNumber) -> bool {
        self.pages.contains_key(&page_no)
    }

    /// Get a page from this shard, updating hit/miss metrics.
    #[inline]
    fn get(&mut self, page_no: PageNumber) -> Option<&[u8]> {
        if let Some(page) = self.pages.get(&page_no) {
            self.hits = self.hits.saturating_add(1);
            Some(page.as_slice())
        } else {
            self.misses = self.misses.saturating_add(1);
            None
        }
    }

    /// Get a mutable reference to a page in this shard.
    #[inline]
    fn get_mut(&mut self, page_no: PageNumber) -> Option<&mut [u8]> {
        if let Some(page) = self.pages.get_mut(&page_no) {
            self.hits = self.hits.saturating_add(1);
            Some(page.as_mut_slice())
        } else {
            self.misses = self.misses.saturating_add(1);
            None
        }
    }

    /// Insert a buffer into this shard.
    fn insert(&mut self, page_no: PageNumber, buf: PageBuf) -> bool {
        let admitted_new = match self.pages.entry(page_no) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(buf);
                false
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(buf);
                true
            }
        };
        if admitted_new {
            self.admits = self.admits.saturating_add(1);
        }
        admitted_new
    }

    /// Remove a page from this shard.
    fn remove(&mut self, page_no: PageNumber) -> bool {
        let removed = self.pages.remove(&page_no).is_some();
        if removed {
            self.evictions = self.evictions.saturating_add(1);
        }
        removed
    }

    /// Remove an arbitrary page from this shard (for eviction).
    fn remove_any(&mut self) -> Option<PageNumber> {
        let key = self.pages.keys().next().copied();
        if let Some(k) = key {
            self.pages.remove(&k);
            self.evictions = self.evictions.saturating_add(1);
        }
        key
    }

    /// Clear all pages from this shard.
    fn clear(&mut self) -> usize {
        let removed = self.pages.len();
        self.evictions = self.evictions.saturating_add(removed as u64);
        self.pages.clear();
        removed
    }

    /// Reset metrics counters.
    fn reset_metrics(&mut self) {
        self.hits = 0;
        self.misses = 0;
        self.admits = 0;
        self.evictions = 0;
    }
}

impl std::fmt::Debug for PageCacheShard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageCacheShard")
            .field("pages", &self.pages.len())
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .field("admits", &self.admits)
            .field("evictions", &self.evictions)
            .finish()
    }
}

/// Sharded page cache for high-concurrency workloads (bd-3wop3.2).
///
/// This cache partitions the page-number space across 128 mutex-protected
/// shards. Concurrent writers operating on different pages (or even the same
/// page with different page numbers) acquire different shard locks, enabling
/// near-linear scaling up to ~64 threads.
///
/// # Design Rationale
///
/// - **128 shards**: Balance between lock granularity and memory overhead.
///   Each shard adds ~64 bytes of cache-line-padded mutex overhead.
/// - **Multiplicative hash**: Ensures good distribution even for sequential
///   page access patterns (B-tree scans, bulk inserts).
/// - **Shared pool**: The underlying `PageBufPool` remains global because
///   buffer allocation is already lock-free (via atomic free-list).
/// - **Per-shard metrics**: Avoids false sharing on metric counters.
///
/// # Thread Safety
///
/// Each shard is protected by a `Mutex`. The shard selection is deterministic
/// (based on page number), so deadlock-free access is guaranteed as long as
/// callers don't hold multiple shard locks simultaneously. The API is designed
/// to make multi-shard locking unnecessary.
pub struct ShardedPageCache {
    /// The 128 cache shards, each cache-line aligned.
    shards: Box<[Mutex<PageCacheShard>; SHARD_COUNT]>,
    /// Shared page buffer pool (lock-free allocation).
    pool: PageBufPool,
    /// Configured page size.
    page_size: PageSize,
}

impl ShardedPageCache {
    /// Create a new sharded page cache with the given page size.
    pub fn new(page_size: PageSize) -> Self {
        Self::with_pool(PageBufPool::new(page_size, 65_536), page_size)
    }

    /// Create a new sharded page cache using an existing `PageBufPool`.
    pub fn with_pool(pool: PageBufPool, page_size: PageSize) -> Self {
        // Initialize all shards
        let shards: Box<[Mutex<PageCacheShard>; SHARD_COUNT]> =
            Box::new(std::array::from_fn(|_| Mutex::new(PageCacheShard::new())));

        Self {
            shards,
            pool,
            page_size,
        }
    }

    /// Select the shard index for a given page number.
    ///
    /// Uses multiplicative hashing with the golden ratio constant for good
    /// distribution of sequential page numbers.
    #[inline]
    fn shard_index(page_no: PageNumber) -> usize {
        let hash = page_no.get().wrapping_mul(GOLDEN_RATIO_32);
        // Multiplicative hashing requires extracting the highest bits.
        // SHARD_COUNT is 128 (2^7), so we shift right by (32 - 7) = 25.
        (hash >> 25) as usize
    }

    /// Access the underlying page pool.
    pub fn pool(&self) -> &PageBufPool {
        &self.pool
    }

    /// Total number of pages across all shards.
    ///
    /// Note: This acquires all shard locks briefly. For hot-path metrics,
    /// prefer `metrics_snapshot()` which aggregates all counters atomically.
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.lock().pages.is_empty())
    }

    /// Check if a page is present in the cache.
    #[inline]
    pub fn contains(&self, page_no: PageNumber) -> bool {
        let idx = Self::shard_index(page_no);
        self.shards[idx].lock().contains(page_no)
    }

    /// Retrieve a page from the cache.
    ///
    /// Returns `None` if the page is not cached. The returned slice is valid
    /// only while the internal lock is held, so this method returns owned data
    /// via a callback pattern for safety.
    ///
    /// For zero-copy access, use `with_page()` instead.
    #[inline]
    pub fn get(&self, page_no: PageNumber) -> Option<Vec<u8>> {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();
        shard.get(page_no).map(|slice| slice.to_vec())
    }

    /// Access a cached page via a callback (zero-copy pattern).
    ///
    /// The callback receives a reference to the page data. Returns `None` if
    /// the page is not cached.
    #[inline]
    pub fn with_page<R>(&self, page_no: PageNumber, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();
        shard.get(page_no).map(f)
    }

    /// Access a cached page mutably via a callback.
    #[inline]
    pub fn with_page_mut<R>(
        &self,
        page_no: PageNumber,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Option<R> {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();
        shard.get_mut(page_no).map(f)
    }

    /// Read a page from a VFS file into the cache.
    ///
    /// If the page is already cached, returns the cached data via the callback.
    /// Otherwise, acquires a buffer from the pool, reads from VFS, caches it,
    /// and returns via the callback.
    pub fn read_page<R>(
        &self,
        cx: &Cx,
        file: &mut impl VfsFile,
        page_no: PageNumber,
        f: impl FnOnce(&[u8]) -> R,
    ) -> Result<R> {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();

        // Check for cache hit first, then update metrics
        if shard.pages.contains_key(&page_no) {
            shard.hits = shard.hits.saturating_add(1);
            // SAFETY: we just checked contains_key, so unwrap is safe
            let data = shard.pages.get(&page_no).unwrap();
            return Ok(f(data.as_slice()));
        }

        // Cache miss — read from VFS
        shard.misses = shard.misses.saturating_add(1);

        let mut buf = self.pool.acquire()?;
        let offset = page_offset(page_no, self.page_size);
        let bytes_read = file.read(cx, buf.as_mut_slice(), offset)?;

        if bytes_read < self.page_size.as_usize() {
            return Err(FrankenError::DatabaseCorrupt {
                detail: format!(
                    "short read fetching page {page}: got {bytes_read} of {page_size}",
                    page = page_no.get(),
                    page_size = self.page_size.as_usize()
                ),
            });
        }

        let result = f(buf.as_slice());
        shard.insert(page_no, buf);
        Ok(result)
    }

    /// Write a cached page out to a VFS file.
    pub fn write_page(&self, cx: &Cx, file: &mut impl VfsFile, page_no: PageNumber) -> Result<()> {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();

        // Check for presence first, update metrics, then get the data
        if !shard.pages.contains_key(&page_no) {
            shard.misses = shard.misses.saturating_add(1);
            return Err(FrankenError::internal(format!(
                "page {} not in cache",
                page_no
            )));
        }

        shard.hits = shard.hits.saturating_add(1);
        // SAFETY: we just checked contains_key, so unwrap is safe
        let buf = shard.pages.get(&page_no).unwrap();
        let offset = page_offset(page_no, self.page_size);
        file.write(cx, buf.as_slice(), offset)?;
        Ok(())
    }

    /// Insert a fresh (zeroed) page into the cache.
    ///
    /// The callback receives a mutable reference to populate the page.
    pub fn insert_fresh<R>(
        &self,
        page_no: PageNumber,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<R> {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();

        let mut buf = self.pool.acquire()?;
        buf.as_mut_slice().fill(0);
        let result = f(buf.as_mut_slice());
        shard.insert(page_no, buf);
        Ok(result)
    }

    /// Directly insert an existing `PageBuf` into the cache.
    pub fn insert_buffer(&self, page_no: PageNumber, buf: PageBuf) {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();
        shard.insert(page_no, buf);
    }

    /// Evict a specific page from the cache.
    pub fn evict(&self, page_no: PageNumber) -> bool {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();
        shard.remove(page_no)
    }

    /// Evict an arbitrary page from the cache.
    ///
    /// Iterates through shards looking for a non-empty one to evict from.
    /// Returns `true` if a page was evicted.
    pub fn evict_any(&self) -> bool {
        // Start from a pseudo-random shard to avoid always hitting shard 0
        let start = (std::time::Instant::now().elapsed().as_nanos() as usize) & SHARD_MASK;
        for i in 0..SHARD_COUNT {
            let idx = (start + i) & SHARD_MASK;
            let mut shard = self.shards[idx].lock();
            if shard.remove_any().is_some() {
                return true;
            }
        }
        false
    }

    /// Evict all pages from the cache.
    pub fn clear(&self) {
        for shard in self.shards.iter() {
            shard.lock().clear();
        }
    }

    /// Capture current cache metrics aggregated across all shards.
    #[must_use]
    pub fn metrics_snapshot(&self) -> PageCacheMetricsSnapshot {
        let mut total_hits = 0_u64;
        let mut total_misses = 0_u64;
        let mut total_admits = 0_u64;
        let mut total_evictions = 0_u64;
        let mut total_pages = 0_usize;

        for shard in self.shards.iter() {
            let s = shard.lock();
            total_hits = total_hits.saturating_add(s.hits);
            total_misses = total_misses.saturating_add(s.misses);
            total_admits = total_admits.saturating_add(s.admits);
            total_evictions = total_evictions.saturating_add(s.evictions);
            total_pages += s.len();
        }

        PageCacheMetricsSnapshot {
            hits: total_hits,
            misses: total_misses,
            admits: total_admits,
            evictions: total_evictions,
            cached_pages: total_pages,
            pool_capacity: self.pool.capacity(),
            dirty_ratio_pct: 0,
            t1_size: total_pages,
            t2_size: 0,
            b1_size: 0,
            b2_size: 0,
            p_target: total_pages,
            mvcc_multi_version_pages: 0,
        }
    }

    /// Reset cache counters while preserving resident pages.
    pub fn reset_metrics(&self) {
        for shard in self.shards.iter() {
            shard.lock().reset_metrics();
        }
    }

    /// Get the configured page size.
    #[must_use]
    pub fn page_size(&self) -> PageSize {
        self.page_size
    }

    /// Get shard distribution statistics (for testing/debugging).
    #[must_use]
    pub fn shard_distribution(&self) -> Vec<usize> {
        self.shards.iter().map(|s| s.lock().len()).collect()
    }

    /// Read a page from VFS and return an owned copy.
    ///
    /// This is a convenience method that wraps `read_page` with a copy
    /// operation, matching the common usage pattern in pager code.
    pub fn read_page_copy(
        &self,
        cx: &Cx,
        file: &mut impl VfsFile,
        page_no: PageNumber,
    ) -> Result<Vec<u8>> {
        self.read_page(cx, file, page_no, |data| data.to_vec())
    }

    /// Get a cached page and return an owned copy.
    ///
    /// Returns `None` if the page is not cached.
    #[inline]
    pub fn get_copy(&self, page_no: PageNumber) -> Option<Vec<u8>> {
        let idx = Self::shard_index(page_no);
        let mut shard = self.shards[idx].lock();
        shard.get(page_no).map(|data| data.to_vec())
    }
}

impl std::fmt::Debug for ShardedPageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let metrics = self.metrics_snapshot();
        f.debug_struct("ShardedPageCache")
            .field("shard_count", &SHARD_COUNT)
            .field("page_size", &self.page_size)
            .field("cached_pages", &metrics.cached_pages)
            .field("hits", &metrics.hits)
            .field("misses", &metrics.misses)
            .field("admits", &metrics.admits)
            .field("evictions", &metrics.evictions)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute the byte offset of a page within the database file.
///
/// Pages are 1-indexed, so page 1 starts at offset 0.
#[inline]
fn page_offset(page_no: PageNumber, page_size: PageSize) -> u64 {
    u64::from(page_no.get() - 1) * u64::from(page_size.get())
}

/// Read a database file header from a VFS file into a stack-allocated buffer.
///
/// The 100-byte SQLite database header is small enough for a stack buffer.
/// This does NOT violate the zero-copy principle — §1.5 explicitly permits
/// "small stack buffers for fixed-size headers."
///
/// Returns the raw header bytes.
pub fn read_db_header(cx: &Cx, file: &mut impl VfsFile) -> Result<[u8; 100]> {
    let mut header = [0u8; 100];
    let bytes_read = file.read(cx, &mut header, 0)?;
    if bytes_read < 100 {
        return Err(FrankenError::DatabaseCorrupt {
            detail: format!("database header short read: expected 100 bytes, got {bytes_read}"),
        });
    }
    Ok(header)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::s3_fifo::{QueueKind, S3Fifo, S3FifoConfig, S3FifoEvent};
    use fsqlite_types::flags::VfsOpenFlags;
    use fsqlite_vfs::{MemoryVfs, Vfs};
    use std::collections::VecDeque;
    use std::hint::black_box;
    use std::path::Path;
    use std::time::{Duration, Instant};

    const BEAD_ID: &str = "bd-22n.2";

    fn setup() -> (Cx, impl VfsFile) {
        let cx = Cx::new();
        let vfs = MemoryVfs::new();
        let flags = VfsOpenFlags::MAIN_DB | VfsOpenFlags::CREATE | VfsOpenFlags::READWRITE;
        let (file, _) = vfs.open(&cx, Some(Path::new("test.db")), flags).unwrap();
        (cx, file)
    }

    #[cfg(unix)]
    #[test]
    fn test_spawn_blocking_io_read_page() {
        use asupersync::runtime::{RuntimeBuilder, spawn_blocking_io};
        use std::io::{ErrorKind, Write as _};
        use std::os::unix::fs::FileExt as _;
        use std::sync::Arc;
        use tempfile::NamedTempFile;

        fn read_exact_at(file: &std::fs::File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
            let mut total = 0_usize;
            while total < buf.len() {
                #[allow(clippy::cast_possible_truncation)]
                let off = offset + total as u64;
                let n = file.read_at(&mut buf[total..], off)?;
                if n == 0 {
                    return Err(std::io::Error::new(ErrorKind::UnexpectedEof, "short read"));
                }
                total += n;
            }
            Ok(())
        }

        let mut tmp = NamedTempFile::new().unwrap();
        let page_data: Vec<u8> = (0..4096u16)
            .map(|i| u8::try_from(i % 256).expect("i % 256 fits in u8"))
            .collect();
        tmp.as_file_mut().write_all(&page_data).unwrap();
        tmp.as_file_mut().flush().unwrap();

        let file = Arc::new(tmp.reopen().unwrap());
        let pool = PageBufPool::new(PageSize::DEFAULT, 1);

        let rt = RuntimeBuilder::low_latency()
            .worker_threads(1)
            .blocking_threads(1, 1)
            .build()
            .unwrap();

        let join = rt.handle().spawn(async move {
            let worker_tid = std::thread::current().id();

            let mut buf = pool.acquire().unwrap();
            let file2 = Arc::clone(&file);
            let (buf, io_tid) = spawn_blocking_io(move || {
                let io_tid = std::thread::current().id();
                read_exact_at(file2.as_ref(), buf.as_mut_slice(), 0)?;
                Ok::<_, std::io::Error>((buf, io_tid))
            })
            .await
            .unwrap();

            assert_ne!(
                io_tid, worker_tid,
                "spawn_blocking_io must dispatch work to a blocking thread"
            );
            assert_eq!(
                buf.as_slice(),
                page_data.as_slice(),
                "bead_id={BEAD_ID} case=spawn_blocking_io_read_page data mismatch"
            );

            drop(buf);
            assert_eq!(
                pool.available(),
                1,
                "bead_id={BEAD_ID} case=spawn_blocking_io_read_page buf must return to pool"
            );
        });

        rt.block_on(join);
    }

    #[test]
    fn test_spawn_blocking_io_no_unsafe() {
        // Workspace-wide lint gate: unsafe code is forbidden.
        let manifest = include_str!("../../../Cargo.toml");
        assert!(
            manifest.contains(r#"unsafe_code = "forbid""#),
            "workspace must keep unsafe_code=forbid for IO dispatch paths"
        );
    }

    #[test]
    fn test_blocking_pool_lab_mode_inline() {
        use asupersync::lab::{LabConfig, LabRuntime};
        use asupersync::runtime::spawn_blocking_io;
        use asupersync::types::Budget;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut rt = LabRuntime::new(LabConfig::new(42));
        let region = rt.state.create_root_region(Budget::INFINITE);

        let ok = Arc::new(AtomicBool::new(false));
        let ok_task = Arc::clone(&ok);

        let (task_id, _handle) = rt
            .state
            .create_task(region, Budget::INFINITE, async move {
                let worker_tid = std::thread::current().id();
                let io_tid =
                    spawn_blocking_io(|| Ok::<_, std::io::Error>(std::thread::current().id()))
                        .await
                        .unwrap();
                ok_task.store(worker_tid == io_tid, Ordering::Release);
            })
            .unwrap();

        rt.scheduler.lock().schedule(task_id, 0);
        rt.run_until_quiescent();

        assert!(
            ok.load(Ordering::Acquire),
            "spawn_blocking_io must execute inline when no blocking pool exists (lab determinism)"
        );
    }

    #[test]
    fn test_cancel_mid_io_returns_buf_to_pool() {
        use asupersync::runtime::{RuntimeBuilder, spawn_blocking_io, yield_now};
        use std::future::poll_fn;
        use std::task::Poll;
        use std::time::Duration;

        let rt = RuntimeBuilder::low_latency()
            .worker_threads(1)
            .blocking_threads(1, 1)
            .build()
            .unwrap();

        let pool = PageBufPool::new(PageSize::DEFAULT, 1);
        let join = rt.handle().spawn(async move {
            let buf = pool.acquire().unwrap();

            let mut fut = Box::pin(spawn_blocking_io(move || {
                std::thread::sleep(Duration::from_millis(20));
                Ok::<_, std::io::Error>(buf)
            }));

            // Poll once to ensure the blocking task is enqueued, then drop the
            // future (soft cancel). The owned PageBuf must be returned to the pool.
            let mut polled = false;
            poll_fn(|cx| {
                if !polled {
                    polled = true;
                    let _ = fut.as_mut().poll(cx);
                }
                Poll::Ready(())
            })
            .await;

            drop(fut);

            // The blocking task sleeps 20ms then drops the PageBuf.
            // Yield in a loop with a brief real-time sleep per iteration so
            // the blocking thread has time to finish and return the buffer.
            for _ in 0..200u32 {
                if pool.available() == 1 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
                yield_now().await;
            }
            assert_eq!(
                pool.available(),
                1,
                "bead_id={BEAD_ID} case=cancel_mid_io_returns_buf_to_pool"
            );
        });

        rt.block_on(join);
    }

    #[test]
    fn test_pager_reads_pages_via_pool() {
        let (cx, mut file) = setup();
        let page_data = vec![0xAB_u8; 4096];
        file.write(&cx, &page_data, 0).unwrap();

        let pool = PageBufPool::new(PageSize::DEFAULT, 4);
        let mut cache = PageCache::with_pool(pool.clone(), PageSize::DEFAULT);
        let read = cache.read_page(&cx, &mut file, PageNumber::ONE).unwrap();
        assert_eq!(read, page_data.as_slice());
        assert_eq!(pool.available(), 0, "cached page still holds the buffer");

        assert!(cache.evict(PageNumber::ONE));
        assert_eq!(
            pool.available(),
            1,
            "evicting a cached page should return its buffer to the pool"
        );
    }

    // --- test_vfs_read_no_intermediate_alloc ---

    #[test]
    fn test_vfs_read_no_intermediate_alloc() {
        // Demonstrate that VfsFile::read writes directly into the PageBuf
        // memory with no intermediate buffer.  We verify by checking that
        // the data appears at the same pointer address as the PageBuf slice.
        let (cx, mut file) = setup();

        // Write a recognizable page to the file.
        let pattern: Vec<u8> = (0..4096u16)
            .map(|i| u8::try_from(i % 256).expect("i % 256 fits in u8"))
            .collect();
        file.write(&cx, &pattern, 0).unwrap();

        // Acquire a PageBuf from the pool and read directly into it.
        let pool = PageBufPool::new(PageSize::DEFAULT, 4);
        let mut buf = pool.acquire().unwrap();
        let ptr_before = buf.as_ptr();

        // VfsFile::read takes &mut [u8] — PageBuf::as_mut_slice gives us
        // a reference to the same aligned memory.
        file.read(&cx, buf.as_mut_slice(), 0).unwrap();

        let ptr_after = buf.as_ptr();
        assert_eq!(
            ptr_before, ptr_after,
            "bead_id={BEAD_ID} case=vfs_read_no_intermediate_alloc \
             pointer must not change — read goes directly into PageBuf"
        );
        assert_eq!(
            buf.as_slice(),
            pattern.as_slice(),
            "bead_id={BEAD_ID} case=vfs_read_data_correct"
        );
    }

    // --- test_vfs_write_no_intermediate_alloc ---

    #[test]
    fn test_vfs_write_no_intermediate_alloc() {
        // Demonstrate that VfsFile::write reads directly from the PageBuf
        // memory with no intermediate staging copy.
        let (cx, mut file) = setup();

        let pool = PageBufPool::new(PageSize::DEFAULT, 4);
        let mut buf = pool.acquire().unwrap();

        // Fill with a recognizable pattern.
        for (i, b) in buf.as_mut_slice().iter_mut().enumerate() {
            *b = u8::try_from(i % 251).expect("i % 251 fits in u8"); // prime-sized pattern
        }

        let ptr_before = buf.as_ptr();

        // VfsFile::write takes &[u8] — PageBuf::as_slice gives us a
        // reference to the same aligned memory, no copy.
        file.write(&cx, buf.as_slice(), 0).unwrap();

        let ptr_after = buf.as_ptr();
        assert_eq!(
            ptr_before, ptr_after,
            "bead_id={BEAD_ID} case=vfs_write_no_intermediate_alloc \
             PageBuf pointer must be stable through write"
        );

        // Verify the data was written correctly.
        let mut verify = vec![0u8; 4096];
        file.read(&cx, &mut verify, 0).unwrap();
        assert_eq!(
            verify.as_slice(),
            buf.as_slice(),
            "bead_id={BEAD_ID} case=vfs_write_data_roundtrip"
        );
    }

    // --- test_pager_returns_ref_not_copy ---

    #[test]
    fn test_pager_returns_ref_not_copy() {
        // PageCache::get() returns &[u8] that points to the same memory
        // as the stored PageBuf — a reference, not a copy.
        let (cx, mut file) = setup();

        // Write a page to the file.
        let data = vec![0xAB_u8; 4096];
        file.write(&cx, &data, 0).unwrap();

        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        // Read the page into cache.
        let ref1 = cache.read_page(&cx, &mut file, page1).unwrap();
        let ref1_ptr = ref1.as_ptr();
        assert_eq!(
            &ref1[..4096],
            data.as_slice(),
            "bead_id={BEAD_ID} case=pager_ref_data_correct"
        );

        // Get the same page again — must be same pointer (cached).
        let ref2 = cache.get(page1).unwrap();
        let ref2_ptr = ref2.as_ptr();
        assert_eq!(
            ref1_ptr, ref2_ptr,
            "bead_id={BEAD_ID} case=pager_returns_ref_not_copy \
             get() must return reference to same memory as read_page()"
        );
    }

    // --- test_wal_uses_buffered_io_compat ---

    #[test]
    fn test_wal_uses_buffered_io_compat() {
        // Verify that WAL frame size (24 + page_size) does NOT preserve
        // sector alignment, proving that WAL I/O requires buffered I/O
        // (not O_DIRECT) in compatibility mode.
        //
        // Per §1.5: "SQLite .wal frames are 24 + page_size bytes — they
        // do NOT preserve sector alignment at frame boundaries."
        let wal_header_size: u64 = 24;

        for &size in &[512u32, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            let frame_size = wal_header_size + u64::from(size);

            // Sector alignment: 512 for HDD, 4096 for modern SSD.
            // WAL frame offset after N frames = 32 (WAL header) + N * frame_size.
            let wal_header_bytes: u64 = 32; // WAL file header
            let frame2_offset = wal_header_bytes + frame_size;

            // Frame 2 offset must NOT be sector-aligned for most page sizes.
            // 24 bytes of per-frame header breaks alignment.
            let _sector_4k_aligned = frame2_offset % 4096 == 0;

            // Even for 512-byte sector: 32 + (24+512) = 568, 568 % 512 = 56.
            let sector_512_aligned = frame2_offset % 512 == 0;
            assert!(
                !sector_512_aligned,
                "bead_id={BEAD_ID} case=wal_frame_not_512_aligned \
                 WAL frame 2 at offset {frame2_offset} should NOT be 512-byte aligned \
                 for page_size={size}"
            );
        }
    }

    // --- test_small_header_stack_buffer_ok ---

    #[test]
    fn test_small_header_stack_buffer_ok() {
        // Per §1.5: "Small stack buffers for fixed-size headers ARE permitted."
        // Demonstrate that reading the 100-byte DB header into a stack
        // buffer works correctly and does not violate zero-copy.
        let (cx, mut file) = setup();

        // Write a minimal SQLite header (first 16 bytes of magic string).
        let mut header_data = [0u8; 100];
        header_data[..16].copy_from_slice(b"SQLite format 3\0");
        header_data[16..18].copy_from_slice(&4096u16.to_be_bytes()); // page size
        file.write(&cx, &header_data, 0).unwrap();

        // Read using the stack-buffer helper.
        let header = read_db_header(&cx, &mut file).unwrap();
        assert_eq!(
            &header[..16],
            b"SQLite format 3\0",
            "bead_id={BEAD_ID} case=small_header_stack_buffer_ok"
        );

        // Verify page size field.
        let page_size = u16::from_be_bytes([header[16], header[17]]);
        assert_eq!(
            page_size, 4096,
            "bead_id={BEAD_ID} case=header_page_size_correct"
        );
    }

    // --- test_page_decode_bounds_checked ---

    #[test]
    fn test_page_decode_bounds_checked() {
        // Verify that page structures are decoded with bounds-checked reads
        // in safe Rust — no transmute of variable-length formats.
        //
        // We simulate decoding a B-tree page header from a cached page.
        let (cx, mut file) = setup();

        // Write a page with a simulated B-tree leaf header.
        let mut page_data = vec![0u8; 4096];
        page_data[0] = 0x0D; // leaf table b-tree page type
        page_data[3..5].copy_from_slice(&10u16.to_be_bytes()); // cell count = 10
        page_data[5..7].copy_from_slice(&100u16.to_be_bytes()); // cell content offset
        file.write(&cx, &page_data, 0).unwrap();

        // Read into cache.
        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page = cache.read_page(&cx, &mut file, PageNumber::ONE).unwrap();

        // Bounds-checked decode: every access goes through slice indexing.
        let page_type = page[0];
        assert_eq!(page_type, 0x0D, "bead_id={BEAD_ID} case=page_decode_type");

        let cell_count = u16::from_be_bytes([page[3], page[4]]);
        assert_eq!(
            cell_count, 10,
            "bead_id={BEAD_ID} case=page_decode_cell_count"
        );

        let content_offset = u16::from_be_bytes([page[5], page[6]]);
        assert_eq!(
            content_offset, 100,
            "bead_id={BEAD_ID} case=page_decode_content_offset"
        );

        // Out of bounds access panics (safe Rust guarantee).
        // We verify by checking the page length is exactly page_size.
        assert_eq!(
            page.len(),
            4096,
            "bead_id={BEAD_ID} case=page_decode_bounds_checked"
        );
    }

    // --- Cache operation tests ---

    #[test]
    fn test_cache_insert_fresh_zeroed() {
        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        let data = cache.insert_fresh(page1).unwrap();
        assert!(
            data.iter().all(|&b| b == 0),
            "bead_id={BEAD_ID} case=insert_fresh_zeroed"
        );
        assert_eq!(data.len(), 4096);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_cache_get_mut_modifies_in_place() {
        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        cache.insert_fresh(page1).unwrap();
        let data = cache.get_mut(page1).unwrap();
        data[0] = 0xFF;
        data[4095] = 0xEE;

        let read_back = cache.get(page1).unwrap();
        assert_eq!(read_back[0], 0xFF);
        assert_eq!(read_back[4095], 0xEE);
    }

    #[test]
    fn test_cache_evict_returns_to_pool() {
        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        assert_eq!(cache.pool().available(), 0);
        cache.insert_fresh(page1).unwrap();
        assert_eq!(cache.pool().available(), 0); // buffer is in use

        assert!(cache.evict(page1));
        assert!(!cache.contains(page1));
        // Buffer returned to pool via PageBuf::Drop.
        assert_eq!(
            cache.pool().available(),
            1,
            "bead_id={BEAD_ID} case=evict_returns_to_pool"
        );
    }

    #[test]
    fn test_cache_evict_nonexistent() {
        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;
        assert!(!cache.evict(page1));
    }

    #[test]
    fn test_cache_clear_returns_all_to_pool() {
        let mut cache = PageCache::new(PageSize::DEFAULT);

        for i in 1..=5u32 {
            let pn = PageNumber::new(i).unwrap();
            cache.insert_fresh(pn).unwrap();
        }
        assert_eq!(cache.len(), 5);
        assert_eq!(cache.pool().available(), 0);

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert_eq!(
            cache.pool().available(),
            5,
            "bead_id={BEAD_ID} case=clear_returns_all_to_pool"
        );
    }

    #[test]
    fn test_cache_multiple_pages() {
        let (cx, mut file) = setup();

        // Write 3 pages with distinct content.
        for i in 0..3u32 {
            let seed = u8::try_from(i).expect("i <= 2");
            let data = vec![(seed + 1) * 0x11; 4096];
            let offset = u64::from(i) * 4096;
            file.write(&cx, &data, offset).unwrap();
        }

        let mut cache = PageCache::new(PageSize::DEFAULT);

        for i in 1..=3u32 {
            let pn = PageNumber::new(i).unwrap();
            let page = cache.read_page(&cx, &mut file, pn).unwrap();
            let expected = u8::try_from(i).expect("i <= 3") * 0x11;
            assert!(
                page.iter().all(|&b| b == expected),
                "bead_id={BEAD_ID} case=multiple_pages page={i} expected={expected:#x}"
            );
        }

        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn test_cache_write_page_roundtrip() {
        let (cx, mut file) = setup();

        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        // Insert a fresh page, modify it, write to VFS.
        let data = cache.insert_fresh(page1).unwrap();
        data.fill(0xCD);

        cache.write_page(&cx, &mut file, page1).unwrap();

        // Read back from VFS directly (bypassing cache).
        let mut verify = vec![0u8; 4096];
        file.read(&cx, &mut verify, 0).unwrap();
        assert!(
            verify.iter().all(|&b| b == 0xCD),
            "bead_id={BEAD_ID} case=write_page_roundtrip"
        );
    }

    #[test]
    fn test_page_offset_calculation() {
        // Page 1 starts at offset 0.
        assert_eq!(
            page_offset(PageNumber::ONE, PageSize::DEFAULT),
            0,
            "bead_id={BEAD_ID} case=page_offset_page1"
        );

        // Page 2 starts at 4096.
        let p2 = PageNumber::new(2).unwrap();
        assert_eq!(
            page_offset(p2, PageSize::DEFAULT),
            4096,
            "bead_id={BEAD_ID} case=page_offset_page2"
        );

        // Page 100 with 512-byte pages starts at 99 * 512 = 50688.
        let p100 = PageNumber::new(100).unwrap();
        let ps512 = PageSize::new(512).unwrap();
        assert_eq!(
            page_offset(p100, ps512),
            50688,
            "bead_id={BEAD_ID} case=page_offset_page100_512"
        );
    }

    // --- E2E: combined zero-copy verification ---

    #[test]
    fn test_e2e_zero_copy_io_no_allocations() {
        // E2E: run a read-heavy workload (simulated point lookups) and
        // verify that steady-state reads are allocation-free by checking
        // pool reuse and pointer stability.
        let (cx, mut file) = setup();

        // Write 10 pages with distinct content.
        let num_pages: u32 = 10;
        for i in 0..num_pages {
            let byte = u8::try_from(i).expect("i <= 9").wrapping_add(0x10);
            let data = vec![byte; 4096];
            file.write(&cx, &data, u64::from(i) * 4096).unwrap();
        }

        let mut cache = PageCache::new(PageSize::DEFAULT);

        // Phase 1: Cold reads — pages load from VFS into cache.
        let mut ptrs: Vec<usize> = Vec::with_capacity(num_pages as usize);
        for i in 1..=num_pages {
            let pn = PageNumber::new(i).unwrap();
            let page = cache.read_page(&cx, &mut file, pn).unwrap();
            ptrs.push(page.as_ptr() as usize);
        }

        // Phase 2: Hot reads — all pages are cached.  Verify no new
        // allocations by checking pointer stability.
        for round in 0..5u32 {
            for i in 1..=num_pages {
                let pn = PageNumber::new(i).unwrap();
                let page = cache.get(pn).unwrap();
                let ptr = page.as_ptr() as usize;
                assert_eq!(
                    ptr,
                    ptrs[(i - 1) as usize],
                    "bead_id={BEAD_ID} case=e2e_pointer_stable \
                     round={round} page={i}"
                );

                // Verify data correctness.
                let expected = u8::try_from(i - 1).expect("i - 1 <= 9").wrapping_add(0x10);
                assert!(
                    page.iter().all(|&b| b == expected),
                    "bead_id={BEAD_ID} case=e2e_data_correct \
                     round={round} page={i}"
                );
            }
        }

        // Phase 3: Evict and re-read — pool reuse avoids new allocation.
        let pool_available_before = cache.pool().available();
        let old_ptr = ptrs[0];

        cache.evict(PageNumber::ONE);
        assert_eq!(
            cache.pool().available(),
            pool_available_before + 1,
            "bead_id={BEAD_ID} case=e2e_evict_returns_to_pool"
        );

        // Re-read page 1: should reuse pool buffer (no new heap alloc).
        let page1_reread = cache.read_page(&cx, &mut file, PageNumber::ONE).unwrap();
        let new_ptr = page1_reread.as_ptr() as usize;

        // The recycled buffer from the pool should be the same allocation.
        assert_eq!(
            new_ptr, old_ptr,
            "bead_id={BEAD_ID} case=e2e_pool_reuse_after_evict \
             Expected recycled buffer at {old_ptr:#x}, got {new_ptr:#x}"
        );

        // Summary (grep-friendly).
        eprintln!("pages_cached={}", cache.len());
        eprintln!("pool_available={}", cache.pool().available());
        eprintln!("pointer_checks_passed={}", num_pages * 5 + 1);
    }

    // --- Debug ---

    #[test]
    fn test_page_cache_debug() {
        let cache = PageCache::new(PageSize::DEFAULT);
        let debug = format!("{cache:?}");
        assert!(
            debug.contains("PageCache"),
            "bead_id={BEAD_ID} case=debug_format"
        );
    }

    #[test]
    fn test_metrics_snapshot_and_reset() {
        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        assert!(cache.get(page1).is_none());
        let fresh = cache.insert_fresh(page1).unwrap();
        fresh[0] = 7;
        assert!(cache.get(page1).is_some());
        assert!(cache.evict(page1));

        let snapshot = cache.metrics_snapshot();
        assert_eq!(snapshot.hits, 1, "bead_id={BEAD_ID} case=metrics_hits");
        assert_eq!(snapshot.misses, 1, "bead_id={BEAD_ID} case=metrics_misses");
        assert_eq!(snapshot.admits, 1, "bead_id={BEAD_ID} case=metrics_admits");
        assert_eq!(
            snapshot.evictions, 1,
            "bead_id={BEAD_ID} case=metrics_evictions"
        );
        assert_eq!(
            snapshot.total_accesses(),
            2,
            "bead_id={BEAD_ID} case=metrics_total_accesses"
        );
        assert!(
            (snapshot.hit_rate_percent() - 50.0).abs() < f64::EPSILON,
            "bead_id={BEAD_ID} case=metrics_hit_rate"
        );

        cache.reset_metrics();
        let reset = cache.metrics_snapshot();
        assert_eq!(reset.hits, 0, "bead_id={BEAD_ID} case=reset_hits");
        assert_eq!(reset.misses, 0, "bead_id={BEAD_ID} case=reset_misses");
        assert_eq!(reset.admits, 0, "bead_id={BEAD_ID} case=reset_admits");
        assert_eq!(reset.evictions, 0, "bead_id={BEAD_ID} case=reset_evictions");
    }

    #[derive(Debug, Clone, Copy)]
    enum BenchEvictionPolicy {
        Arbitrary,
        ReconstructedS3Fifo(S3FifoConfig),
    }

    #[derive(Debug, Clone)]
    struct PrototypeTrace {
        config: S3FifoConfig,
        adaptation_interval: usize,
        adaptive_bounds: (usize, usize),
        access_trace: VecDeque<PageNumber>,
        max_trace_entries: usize,
    }

    impl PrototypeTrace {
        fn new(config: S3FifoConfig) -> Self {
            let probe = S3Fifo::with_config(config);
            let max_trace_entries = config.capacity().saturating_mul(8).max(64);
            Self {
                config,
                adaptation_interval: probe.adaptation_interval(),
                adaptive_bounds: probe.adaptive_bounds(),
                access_trace: VecDeque::with_capacity(max_trace_entries),
                max_trace_entries,
            }
        }

        fn record_access(&mut self, page_no: PageNumber) {
            if self.access_trace.len() >= self.max_trace_entries {
                let _ = self.access_trace.pop_front();
            }
            self.access_trace.push_back(page_no);
        }

        fn record_admit(&mut self, page_no: PageNumber) {
            self.record_access(page_no);
        }

        fn choose_victim(&self, cache: &PageCache) -> Option<PageNumber> {
            let mut model = self.build_model(cache)?;
            let synthetic_miss = choose_synthetic_miss_page_for_bench(cache)?;
            let events = model.insert(synthetic_miss);
            events.iter().find_map(|event| match event {
                S3FifoEvent::EvictedFromSmallToGhost(page_no)
                | S3FifoEvent::EvictedFromMain(page_no)
                    if cache.pages.contains_key(page_no) =>
                {
                    Some(*page_no)
                }
                _ => None,
            })
        }

        fn build_model(&self, cache: &PageCache) -> Option<S3Fifo> {
            let resident_pages = cache.pages.len();
            if resident_pages == 0 {
                return None;
            }

            let resident_keys: std::collections::HashSet<PageNumber> =
                cache.pages.keys().copied().collect();
            let mut resident_order: Vec<PageNumber> = resident_keys.iter().copied().collect();
            resident_order.sort_unstable_by_key(|page_no| page_no.get());

            let mut model = S3Fifo::with_config(self.scaled_config(resident_pages));
            model.set_adaptation_interval(self.adaptation_interval);
            let (min_bound, max_bound) = self.scaled_bounds(resident_pages);
            model.set_adaptive_bounds(min_bound, max_bound);

            for &page_no in &self.access_trace {
                if !resident_keys.contains(&page_no) {
                    continue;
                }
                if !model.access(page_no) {
                    let _ = model.insert(page_no);
                }
            }

            let mut remaining_rounds = resident_order.len().saturating_mul(2).max(1);
            while remaining_rounds > 0 {
                let missing: Vec<PageNumber> = resident_order
                    .iter()
                    .copied()
                    .filter(|page_no| {
                        !matches!(
                            model.lookup(*page_no),
                            Some(location) if location.kind != QueueKind::Ghost
                        )
                    })
                    .collect();
                if missing.is_empty() {
                    break;
                }
                for page_no in missing {
                    let _ = model.insert(page_no);
                }
                remaining_rounds = remaining_rounds.saturating_sub(1);
            }

            Some(model)
        }

        fn scaled_config(&self, resident_pages: usize) -> S3FifoConfig {
            let capacity = resident_pages.max(1);
            let prototype_capacity = self.config.capacity().max(1);
            let small_capacity =
                scale_nonzero_for_bench(self.config.small_capacity(), prototype_capacity, capacity)
                    .clamp(1, capacity);
            let ghost_capacity =
                scale_nonzero_for_bench(self.config.ghost_capacity(), prototype_capacity, capacity)
                    .max(1);
            S3FifoConfig::with_limits(
                capacity,
                small_capacity,
                ghost_capacity,
                self.config.max_reinsert(),
            )
        }

        fn scaled_bounds(&self, resident_pages: usize) -> (usize, usize) {
            let capacity = resident_pages.max(1);
            let prototype_capacity = self.config.capacity().max(1);
            let min_bound =
                scale_nonzero_for_bench(self.adaptive_bounds.0, prototype_capacity, capacity)
                    .clamp(1, capacity);
            let max_bound =
                scale_nonzero_for_bench(self.adaptive_bounds.1, prototype_capacity, capacity)
                    .clamp(min_bound, capacity);
            (min_bound, max_bound)
        }
    }

    fn scale_nonzero_for_bench(value: usize, from_capacity: usize, to_capacity: usize) -> usize {
        if value == 0 || to_capacity == 0 {
            return 0;
        }
        let numerator = value.saturating_mul(to_capacity);
        numerator.saturating_add(from_capacity.saturating_sub(1)) / from_capacity.max(1)
    }

    fn choose_synthetic_miss_page_for_bench(cache: &PageCache) -> Option<PageNumber> {
        let mut candidate = u32::MAX;
        loop {
            let page_no = PageNumber::new(candidate)?;
            if !cache.pages.contains_key(&page_no) {
                return Some(page_no);
            }
            if candidate == 1 {
                return None;
            }
            candidate = candidate.saturating_sub(1);
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct HotColdEvictionBenchResult {
        elapsed: Duration,
        hot_hits: u64,
        hot_misses: u64,
        resident_hot_checks: u64,
        resident_hot_kept: u64,
        checksum: u64,
    }

    impl HotColdEvictionBenchResult {
        fn hot_hit_pct(self) -> f64 {
            let total = self.hot_hits.saturating_add(self.hot_misses);
            if total == 0 {
                0.0
            } else {
                (self.hot_hits as f64 * 100.0) / total as f64
            }
        }

        fn resident_hot_pct(self) -> f64 {
            if self.resident_hot_checks == 0 {
                0.0
            } else {
                (self.resident_hot_kept as f64 * 100.0) / self.resident_hot_checks as f64
            }
        }
    }

    fn benchmark_hot_cold_eviction(policy: BenchEvictionPolicy) -> HotColdEvictionBenchResult {
        const CAPACITY: usize = 64;
        const HOT_SET: usize = 8;
        const HOT_TOUCHES_PER_ROUND: usize = 6;
        const COLD_BURST: usize = 16;
        const ROUNDS: usize = 250;

        let mut cache = PageCache::new(PageSize::DEFAULT);
        let mut prototype = match policy {
            BenchEvictionPolicy::Arbitrary => None,
            BenchEvictionPolicy::ReconstructedS3Fifo(config) => Some(PrototypeTrace::new(config)),
        };
        let hot_pages = [7_u32, 113, 251, 389, 521, 659, 797, 941]
            .map(|page_no| PageNumber::new(page_no).unwrap());
        let mut next_cold_page = 10_000_u32;
        let mut hot_hits = 0_u64;
        let mut hot_misses = 0_u64;
        let mut resident_hot_checks = 0_u64;
        let mut resident_hot_kept = 0_u64;
        let mut checksum = 0_u64;

        for (idx, page_no) in hot_pages.iter().copied().enumerate() {
            let page = cache.insert_fresh(page_no).unwrap();
            page[0] = u8::try_from(idx + 1).unwrap();
            if let Some(trace) = &mut prototype {
                trace.record_admit(page_no);
            }
        }

        let started = Instant::now();
        for round in 0..ROUNDS {
            for _ in 0..HOT_TOUCHES_PER_ROUND {
                for (idx, page_no) in hot_pages.iter().copied().enumerate() {
                    if let Some(page) = cache.get_mut(page_no) {
                        hot_hits = hot_hits.saturating_add(1);
                        checksum = checksum.saturating_add(u64::from(page[0]));
                        if let Some(trace) = &mut prototype {
                            trace.record_access(page_no);
                        }
                    } else {
                        hot_misses = hot_misses.saturating_add(1);
                        let page = cache.insert_fresh(page_no).unwrap();
                        page[0] = u8::try_from((idx + round) % 251).unwrap_or(0);
                        if let Some(trace) = &mut prototype {
                            trace.record_admit(page_no);
                        }
                        if cache.len() > CAPACITY {
                            match &prototype {
                                Some(trace) => {
                                    let victim = trace
                                        .choose_victim(&cache)
                                        .or_else(|| cache.pages.keys().next().copied());
                                    assert!(
                                        victim.is_some_and(|page_no| cache.evict(page_no)),
                                        "test-only prototype eviction must free capacity"
                                    );
                                }
                                None => assert!(
                                    cache.evict_any(),
                                    "arbitrary hot-page reinsertion must free capacity"
                                ),
                            }
                        }
                    }
                }
            }

            for burst_idx in 0..COLD_BURST {
                let page_no = PageNumber::new(next_cold_page).unwrap();
                next_cold_page = next_cold_page.saturating_add(1);
                let page = cache.insert_fresh(page_no).unwrap();
                page[0] = u8::try_from((round + burst_idx) % 251).unwrap_or(0);
                if let Some(trace) = &mut prototype {
                    trace.record_admit(page_no);
                }
                if cache.len() > CAPACITY {
                    match &prototype {
                        Some(trace) => {
                            let victim = trace
                                .choose_victim(&cache)
                                .or_else(|| cache.pages.keys().next().copied());
                            assert!(
                                victim.is_some_and(|page_no| cache.evict(page_no)),
                                "test-only prototype eviction must free capacity"
                            );
                        }
                        None => {
                            assert!(cache.evict_any(), "cold-page admission must free capacity");
                        }
                    }
                }
            }

            resident_hot_checks = resident_hot_checks.saturating_add(HOT_SET as u64);
            resident_hot_kept = resident_hot_kept.saturating_add(
                hot_pages
                    .iter()
                    .filter(|page_no| cache.contains(**page_no))
                    .count() as u64,
            );
        }

        HotColdEvictionBenchResult {
            elapsed: started.elapsed(),
            hot_hits,
            hot_misses,
            resident_hot_checks,
            resident_hot_kept,
            checksum: black_box(checksum),
        }
    }

    fn median_duration(samples: &[HotColdEvictionBenchResult]) -> Duration {
        let mut elapsed_nanos: Vec<u128> = samples
            .iter()
            .map(|sample| sample.elapsed.as_nanos())
            .collect();
        elapsed_nanos.sort_unstable();
        let middle = elapsed_nanos[elapsed_nanos.len() / 2];
        let nanos = u64::try_from(middle).unwrap_or(u64::MAX);
        Duration::from_nanos(nanos)
    }

    #[test]
    #[ignore = "benchmark evidence only"]
    fn page_cache_hot_cold_eviction_microbench_report() {
        const SAMPLE_COUNT: usize = 3;

        let _ = benchmark_hot_cold_eviction(BenchEvictionPolicy::Arbitrary);
        let _ = benchmark_hot_cold_eviction(BenchEvictionPolicy::ReconstructedS3Fifo(
            S3FifoConfig::new(64),
        ));

        let arbitrary_samples: Vec<HotColdEvictionBenchResult> = (0..SAMPLE_COUNT)
            .map(|_| benchmark_hot_cold_eviction(BenchEvictionPolicy::Arbitrary))
            .collect();
        let prototype_samples: Vec<HotColdEvictionBenchResult> = (0..SAMPLE_COUNT)
            .map(|_| {
                benchmark_hot_cold_eviction(BenchEvictionPolicy::ReconstructedS3Fifo(
                    S3FifoConfig::new(64),
                ))
            })
            .collect();

        let arbitrary = arbitrary_samples[0];
        let prototype = prototype_samples[0];
        let arbitrary_median = median_duration(&arbitrary_samples);
        let prototype_median = median_duration(&prototype_samples);

        println!(
            "policy=Arbitrary median_ms={:.3} hot_hit_pct={:.2} resident_hot_pct={:.2} checksum={}",
            arbitrary_median.as_secs_f64() * 1_000.0,
            arbitrary.hot_hit_pct(),
            arbitrary.resident_hot_pct(),
            arbitrary.checksum
        );
        println!(
            "policy=ReconstructedS3Fifo median_ms={:.3} hot_hit_pct={:.2} resident_hot_pct={:.2} checksum={}",
            prototype_median.as_secs_f64() * 1_000.0,
            prototype.hot_hit_pct(),
            prototype.resident_hot_pct(),
            prototype.checksum
        );
        println!(
            "delta_hot_hit_pct={:.2} delta_resident_hot_pct={:.2} slowdown_x={:.2}",
            prototype.hot_hit_pct() - arbitrary.hot_hit_pct(),
            prototype.resident_hot_pct() - arbitrary.resident_hot_pct(),
            prototype_median.as_secs_f64() / arbitrary_median.as_secs_f64()
        );

        assert!(
            prototype.hot_hit_pct() > arbitrary.hot_hit_pct(),
            "prototype must improve hot hit rate on the focused hot/cold workload"
        );
        assert!(
            prototype.resident_hot_pct() > arbitrary.resident_hot_pct(),
            "prototype must retain more hot pages across cold bursts"
        );
    }

    // -----------------------------------------------------------------------
    // bd-22n.8 — Allocation-Free Read Path Tests (Pager Layer)
    // -----------------------------------------------------------------------

    const BEAD_22N8: &str = "bd-22n.8";

    #[test]
    fn test_cache_lookup_no_alloc() {
        // bd-22n.8: Buffer pool cache lookup is allocation-free.
        //
        // PageCache::get() returns Option<&[u8]> — a reference into the
        // pool-allocated buffer.  It does a HashMap::get + PageBuf::as_slice,
        // neither of which allocates.
        //
        // We verify by: (a) checking the returned &[u8] is the same pointer
        // as the original PageBuf, and (b) repeating the lookup many times
        // and verifying pointer stability (proves no reallocation).
        let (cx, mut file) = setup();

        let data = vec![0xBE_u8; 4096];
        file.write(&cx, &data, 0).unwrap();

        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        // Cold read — allocates from pool.
        let initial = cache.read_page(&cx, &mut file, page1).unwrap();
        let initial_ptr = initial.as_ptr();

        // Hot reads — must be allocation-free (same pointer).
        for round in 0..100u32 {
            let cached = cache.get(page1).unwrap();
            assert_eq!(
                cached.as_ptr(),
                initial_ptr,
                "bead_id={BEAD_22N8} case=cache_lookup_no_alloc \
                 round={round} pointer must be stable (no realloc)"
            );
        }
    }

    #[test]
    fn test_cache_lookup_hit_returns_reference() {
        // bd-22n.8: Verify structurally that get() returns a borrow, not a copy.
        // We insert a page, mutate it via get_mut, then verify get() sees the
        // mutation at the same pointer — proving it's a reference into the
        // same memory.
        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        // Insert a fresh page and write a sentinel.
        let fresh = cache.insert_fresh(page1).unwrap();
        fresh[0] = 0xAA;
        let ptr_after_insert = cache.get(page1).unwrap().as_ptr();

        // Mutate in place.
        let mutref = cache.get_mut(page1).unwrap();
        mutref[1] = 0xBB;

        // get() must see the mutation AND return the same pointer.
        let read_back = cache.get(page1).unwrap();
        assert_eq!(
            read_back.as_ptr(),
            ptr_after_insert,
            "bead_id={BEAD_22N8} case=cache_lookup_returns_reference \
             pointer must be stable through mutation"
        );
        assert_eq!(read_back[0], 0xAA);
        assert_eq!(read_back[1], 0xBB);
    }

    #[test]
    fn test_pool_reuse_avoids_alloc_on_reread() {
        // bd-22n.8: After eviction, re-reading a page reuses a pool buffer
        // rather than allocating fresh memory.  This ensures the read path
        // is allocation-free in steady state (pool has recycled buffers).
        let (cx, mut file) = setup();

        let data = vec![0xDD_u8; 4096];
        file.write(&cx, &data, 0).unwrap();

        let mut cache = PageCache::new(PageSize::DEFAULT);
        let page1 = PageNumber::ONE;

        // Cold read, then evict.
        let _ = cache.read_page(&cx, &mut file, page1).unwrap();
        assert_eq!(cache.pool().available(), 0);
        cache.evict(page1);
        assert_eq!(
            cache.pool().available(),
            1,
            "bead_id={BEAD_22N8} case=evicted_buffer_returned_to_pool"
        );

        // Re-read: pool has a buffer, so no new allocation needed.
        let reread = cache.read_page(&cx, &mut file, page1).unwrap();
        assert_eq!(
            reread,
            data.as_slice(),
            "bead_id={BEAD_22N8} case=pool_reuse_data_correct"
        );
        assert_eq!(
            cache.pool().available(),
            0,
            "bead_id={BEAD_22N8} case=pool_buffer_consumed_on_reread"
        );
    }

    // -----------------------------------------------------------------------
    // bd-3wop3.2 — ShardedPageCache Tests
    // -----------------------------------------------------------------------

    const BEAD_3WOP3_2: &str = "bd-3wop3.2";

    #[test]
    fn test_sharded_cache_basic_operations() {
        // Basic insert/get/evict operations on sharded cache.
        let cache = ShardedPageCache::new(PageSize::DEFAULT);

        let p1 = PageNumber::ONE;
        let p2 = PageNumber::new(2).unwrap();

        // Insert two pages
        cache.insert_fresh(p1, |data| data[0] = 0xAA).unwrap();
        cache.insert_fresh(p2, |data| data[0] = 0xBB).unwrap();

        assert_eq!(cache.len(), 2);
        assert!(cache.contains(p1));
        assert!(cache.contains(p2));

        // Read back
        cache.with_page(p1, |data| assert_eq!(data[0], 0xAA));
        cache.with_page(p2, |data| assert_eq!(data[0], 0xBB));

        // Evict one
        assert!(cache.evict(p1));
        assert!(!cache.contains(p1));
        assert!(cache.contains(p2));
        assert_eq!(cache.len(), 1);

        // Metrics
        let m = cache.metrics_snapshot();
        assert_eq!(m.admits, 2, "bead_id={BEAD_3WOP3_2} case=basic_admits");
        assert_eq!(
            m.evictions, 1,
            "bead_id={BEAD_3WOP3_2} case=basic_evictions"
        );
    }

    #[test]
    fn test_sharded_cache_shard_distribution() {
        // Verify that pages are distributed across multiple shards.
        let cache = ShardedPageCache::new(PageSize::DEFAULT);

        // Insert 256 sequential pages (should use multiple shards)
        for i in 1..=256u32 {
            let pn = PageNumber::new(i).unwrap();
            cache.insert_fresh(pn, |_| {}).unwrap();
        }

        let dist = cache.shard_distribution();
        assert_eq!(dist.len(), 128);

        // Count non-empty shards
        let non_empty = dist.iter().filter(|&&n| n > 0).count();

        // With 256 pages and 128 shards, we expect good distribution.
        // Multiplicative hashing should spread sequential keys well.
        assert!(
            non_empty >= 64,
            "bead_id={BEAD_3WOP3_2} case=shard_distribution \
             expected at least 64 non-empty shards, got {non_empty}"
        );

        // No shard should have more than ~10 pages (avg is 2)
        let max_per_shard = *dist.iter().max().unwrap();
        assert!(
            max_per_shard <= 16,
            "bead_id={BEAD_3WOP3_2} case=shard_balance \
             expected max 16 pages per shard, got {max_per_shard}"
        );
    }

    #[test]
    fn test_sharded_cache_cross_shard_eviction() {
        // Verify evict_any() can find pages across different shards.
        let cache = ShardedPageCache::new(PageSize::DEFAULT);

        // Insert pages that should land in different shards
        for i in 1..=16u32 {
            let pn = PageNumber::new(i * 100).unwrap();
            cache.insert_fresh(pn, |_| {}).unwrap();
        }

        assert_eq!(cache.len(), 16);

        // Evict all via evict_any()
        let mut evicted = 0;
        while cache.evict_any() {
            evicted += 1;
            if evicted > 100 {
                panic!("bead_id={BEAD_3WOP3_2} case=cross_shard_eviction infinite loop");
            }
        }

        assert_eq!(
            evicted, 16,
            "bead_id={BEAD_3WOP3_2} case=cross_shard_eviction_count"
        );
        assert!(cache.is_empty());
    }

    #[test]
    fn test_sharded_cache_clear() {
        let cache = ShardedPageCache::new(PageSize::DEFAULT);

        for i in 1..=100u32 {
            let pn = PageNumber::new(i).unwrap();
            cache.insert_fresh(pn, |_| {}).unwrap();
        }

        assert_eq!(cache.len(), 100);
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);

        let m = cache.metrics_snapshot();
        assert_eq!(
            m.evictions, 100,
            "bead_id={BEAD_3WOP3_2} case=clear_evictions"
        );
    }

    #[test]
    fn test_sharded_cache_metrics_aggregation() {
        let cache = ShardedPageCache::new(PageSize::DEFAULT);

        // Generate cache hits and misses across multiple shards
        for i in 1..=50u32 {
            let pn = PageNumber::new(i).unwrap();
            cache.insert_fresh(pn, |_| {}).unwrap();
        }

        // Hit existing pages
        for i in 1..=50u32 {
            let pn = PageNumber::new(i).unwrap();
            cache.with_page(pn, |_| {});
        }

        // Miss non-existent pages
        for i in 51..=100u32 {
            let pn = PageNumber::new(i).unwrap();
            cache.with_page(pn, |_| {});
        }

        let m = cache.metrics_snapshot();
        assert_eq!(m.admits, 50, "bead_id={BEAD_3WOP3_2} case=metrics_admits");
        assert_eq!(m.hits, 50, "bead_id={BEAD_3WOP3_2} case=metrics_hits");
        assert_eq!(m.misses, 50, "bead_id={BEAD_3WOP3_2} case=metrics_misses");
        assert_eq!(
            m.cached_pages, 50,
            "bead_id={BEAD_3WOP3_2} case=metrics_cached_pages"
        );

        cache.reset_metrics();
        let reset = cache.metrics_snapshot();
        assert_eq!(reset.hits, 0, "bead_id={BEAD_3WOP3_2} case=reset_metrics");
        assert_eq!(reset.misses, 0);
        assert_eq!(reset.admits, 0);
        // cached_pages should still be 50 (reset doesn't clear data)
        assert_eq!(reset.cached_pages, 50);
    }

    #[test]
    fn test_sharded_cache_shard_padding_alignment() {
        // Verify cache-line alignment by checking struct sizes.
        // PageCacheShard is #[repr(align(64))], so size must be multiple of 64.
        let shard_size = std::mem::size_of::<PageCacheShard>();
        assert!(
            shard_size >= 64,
            "bead_id={BEAD_3WOP3_2} case=shard_padding \
             PageCacheShard size {shard_size} should be >= 64 bytes"
        );
        assert_eq!(
            shard_size % 64,
            0,
            "bead_id={BEAD_3WOP3_2} case=shard_alignment \
             PageCacheShard size {shard_size} must be multiple of 64"
        );

        // Verify alignment requirement
        let shard_align = std::mem::align_of::<PageCacheShard>();
        assert_eq!(
            shard_align, 64,
            "bead_id={BEAD_3WOP3_2} case=shard_align_req \
             PageCacheShard alignment should be 64, got {shard_align}"
        );
    }

    #[test]
    fn test_sharded_cache_with_page_mut() {
        let cache = ShardedPageCache::new(PageSize::DEFAULT);
        let p1 = PageNumber::ONE;

        cache.insert_fresh(p1, |data| data.fill(0)).unwrap();

        // Mutate via callback
        cache.with_page_mut(p1, |data| {
            data[0] = 0x12;
            data[1] = 0x34;
        });

        // Verify mutation persisted
        cache.with_page(p1, |data| {
            assert_eq!(data[0], 0x12, "bead_id={BEAD_3WOP3_2} case=with_page_mut_0");
            assert_eq!(data[1], 0x34, "bead_id={BEAD_3WOP3_2} case=with_page_mut_1");
        });
    }

    #[test]
    fn test_sharded_cache_insert_buffer() {
        let cache = ShardedPageCache::new(PageSize::DEFAULT);
        let p1 = PageNumber::ONE;

        // Acquire a buffer from the pool
        let mut buf = cache.pool().acquire().unwrap();
        buf.as_mut_slice().fill(0xEE);

        cache.insert_buffer(p1, buf);

        assert!(cache.contains(p1));
        cache.with_page(p1, |data| {
            assert!(
                data.iter().all(|&b| b == 0xEE),
                "bead_id={BEAD_3WOP3_2} case=insert_buffer_data"
            );
        });
    }

    #[test]
    fn test_sharded_cache_vfs_read_write() {
        let (cx, mut file) = setup();

        // Write test data to VFS
        let test_data = vec![0xAB_u8; 4096];
        file.write(&cx, &test_data, 0).unwrap();

        let cache = ShardedPageCache::new(PageSize::DEFAULT);
        let p1 = PageNumber::ONE;

        // Read through cache
        let result = cache.read_page(&cx, &mut file, p1, |data| {
            assert_eq!(
                data,
                test_data.as_slice(),
                "bead_id={BEAD_3WOP3_2} case=vfs_read_data"
            );
            data[0]
        });
        assert_eq!(result.unwrap(), 0xAB);

        // Modify and write back
        cache.with_page_mut(p1, |data| data[0] = 0xCD);
        cache.write_page(&cx, &mut file, p1).unwrap();

        // Verify write
        let mut verify = vec![0u8; 4096];
        file.read(&cx, &mut verify, 0).unwrap();
        assert_eq!(
            verify[0], 0xCD,
            "bead_id={BEAD_3WOP3_2} case=vfs_write_verify"
        );
    }

    // --- Concurrency tests ---

    #[test]
    fn test_sharded_cache_8_threads_no_deadlock() {
        // 8 threads performing concurrent operations without deadlock.
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(ShardedPageCache::new(PageSize::DEFAULT));
        let num_threads = 8;
        let ops_per_thread = 1000;

        let handles: Vec<_> = (0..num_threads)
            .map(|tid| {
                let c = Arc::clone(&cache);
                thread::spawn(move || {
                    for i in 0..ops_per_thread {
                        // Each thread works on different page ranges to avoid conflicts
                        let base = tid * 10000 + i;
                        let pn = PageNumber::new(base as u32 + 1).unwrap();

                        // Insert
                        c.insert_fresh(pn, |data| data[0] = (tid & 0xFF) as u8)
                            .unwrap();

                        // Read back
                        c.with_page(pn, |data| {
                            assert_eq!(data[0], (tid & 0xFF) as u8);
                        });

                        // Evict every 10th
                        if i % 10 == 0 {
                            c.evict(pn);
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join()
                .expect("bead_id={BEAD_3WOP3_2} case=8t_no_deadlock thread panic");
        }

        // Verify no deadlock occurred (we reached here)
        let m = cache.metrics_snapshot();
        assert!(
            m.admits >= (num_threads * ops_per_thread) as u64,
            "bead_id={BEAD_3WOP3_2} case=8t_admits"
        );
    }

    #[test]
    fn test_sharded_cache_16_threads_no_deadlock() {
        // 16 threads performing concurrent operations without deadlock.
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(ShardedPageCache::new(PageSize::DEFAULT));
        let num_threads = 16;
        let ops_per_thread = 500;

        let handles: Vec<_> = (0..num_threads)
            .map(|tid| {
                let c = Arc::clone(&cache);
                thread::spawn(move || {
                    for i in 0..ops_per_thread {
                        let base = tid * 10000 + i;
                        let pn = PageNumber::new(base as u32 + 1).unwrap();

                        c.insert_fresh(pn, |data| data[0] = ((tid * 7) & 0xFF) as u8)
                            .unwrap();
                        c.with_page(pn, |data| {
                            assert_eq!(data[0], ((tid * 7) & 0xFF) as u8);
                        });

                        if i % 5 == 0 {
                            c.evict(pn);
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join()
                .expect("bead_id={BEAD_3WOP3_2} case=16t_no_deadlock thread panic");
        }

        let m = cache.metrics_snapshot();
        assert!(
            m.admits >= (num_threads * ops_per_thread) as u64,
            "bead_id={BEAD_3WOP3_2} case=16t_admits"
        );
    }

    #[test]
    fn test_sharded_cache_throughput_vs_single() {
        // Compare throughput of sharded vs non-sharded cache.
        // This is a smoke test to ensure sharding doesn't regress single-threaded perf.
        use std::time::Instant;

        let iterations = 10_000;

        // Non-sharded (baseline)
        let mut single = PageCache::new(PageSize::DEFAULT);
        let start = Instant::now();
        for i in 1..=iterations {
            let pn = PageNumber::new(i).unwrap();
            single.insert_fresh(pn).unwrap();
            let _ = single.get(pn);
        }
        let single_elapsed = start.elapsed();

        // Sharded
        let sharded = ShardedPageCache::new(PageSize::DEFAULT);
        let start = Instant::now();
        for i in 1..=iterations {
            let pn = PageNumber::new(i).unwrap();
            sharded.insert_fresh(pn, |_| {}).unwrap();
            sharded.with_page(pn, |_| {});
        }
        let sharded_elapsed = start.elapsed();

        // Sharded should not be more than 3x slower in single-threaded case
        // (overhead from locking + callback indirection)
        let ratio = sharded_elapsed.as_nanos() as f64 / single_elapsed.as_nanos() as f64;
        assert!(
            ratio < 3.0,
            "bead_id={BEAD_3WOP3_2} case=throughput_overhead \
             sharded cache is {ratio:.2}x slower than single (max 3x allowed)"
        );

        eprintln!(
            "bead_id={BEAD_3WOP3_2} throughput_ratio={ratio:.2}x \
             single={:?} sharded={:?}",
            single_elapsed, sharded_elapsed
        );
    }

    #[test]
    fn test_sharded_cache_concurrent_same_shard() {
        // Multiple threads hitting the same shard should work correctly.
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(ShardedPageCache::new(PageSize::DEFAULT));
        let num_threads = 4;
        let ops_per_thread = 500;

        // All threads use page numbers that hash to the same shard
        // We find pages with the same shard index
        let base_page = PageNumber::ONE;
        let base_shard = ShardedPageCache::shard_index(base_page);

        // Find other pages in the same shard
        let mut same_shard_pages = vec![1u32];
        for i in 2..10000u32 {
            let pn = PageNumber::new(i).unwrap();
            if ShardedPageCache::shard_index(pn) == base_shard {
                same_shard_pages.push(i);
                if same_shard_pages.len() >= (num_threads * ops_per_thread) {
                    break;
                }
            }
        }

        let pages = Arc::new(same_shard_pages);

        let handles: Vec<_> = (0..num_threads)
            .map(|tid| {
                let c = Arc::clone(&cache);
                let p = Arc::clone(&pages);
                thread::spawn(move || {
                    let start = tid * ops_per_thread;
                    for i in 0..ops_per_thread {
                        let idx = start + i;
                        if idx >= p.len() {
                            break;
                        }
                        let pn = PageNumber::new(p[idx]).unwrap();

                        c.insert_fresh(pn, |data| data[0] = (tid & 0xFF) as u8)
                            .unwrap();
                        c.with_page(pn, |_| {});
                    }
                })
            })
            .collect();

        for h in handles {
            h.join()
                .expect("bead_id={BEAD_3WOP3_2} case=concurrent_same_shard panic");
        }

        // Verify we inserted to the expected shard
        let dist = cache.shard_distribution();
        assert!(
            dist[base_shard] > 0,
            "bead_id={BEAD_3WOP3_2} case=same_shard_populated"
        );
    }
}
